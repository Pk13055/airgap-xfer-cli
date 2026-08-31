use std::{
    collections::HashSet,
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    frame::{data_chunk_size, Payload, FAIL_HASH},
    link::Session,
    optical::Optical,
    Error, Result,
};

pub const WINDOW: u32 = 32;
const STALL_LIMIT: u32 = 20;

#[derive(Clone, Copy, Debug, Default)]
pub struct TransportConfig {
    pub fast: bool,
    /// Pauses for an operator confirmation before the first DATA frame. The
    /// chunk loop itself is never gated: a human cannot arbitrate thousands of
    /// frames, and the ACK protocol already paces the window.
    pub gated: bool,
}

impl TransportConfig {
    /// Millisecond timeouts, for in-process tests.
    pub fn fast() -> Self {
        Self {
            fast: true,
            gated: false,
        }
    }

    /// Interactive settings: one confirmation before the transfer starts.
    pub fn gated() -> Self {
        Self {
            fast: false,
            gated: true,
        }
    }
}

fn ack_timeout(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        // Must outlast the receiver's post-burst quiet wait (1.5× dwell, up to
        // ~1.8s) plus the time to encode and paint the ACK QR.
        Duration::from_secs(5)
    }
}

/// How long the receiver waits for another *new* sequence before concluding
/// the sender has finished the burst and wants an ACK.
///
/// Live cameras re-decode the code currently on screen many times a second, so
/// "poll returned None" never happens while a DATA frame is held. The quiet
/// timer is measured from the last new seq instead. Fast in-process tests have
/// no duplicates and ACK as soon as the channel goes idle.
fn burst_quiet(cfg: TransportConfig, dwell_ms: u16) -> Duration {
    if cfg.fast {
        Duration::ZERO
    } else {
        Duration::from_millis((u64::from(dwell_ms) * 3 / 2).max(80))
    }
}

/// Live transfers wait this long for the first DATA: the sender still has one
/// Enter to press after the handshake, and that is not a stall.
fn first_data_budget(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        ack_timeout(cfg) * STALL_LIMIT
    } else {
        Duration::from_secs(600)
    }
}

fn fin_timeout(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(15)
    }
}

fn expected_chunk_count(blob_len: usize, chunk_size: usize) -> Option<u32> {
    if chunk_size == 0 {
        return None;
    }
    u32::try_from(blob_len.div_ceil(chunk_size)).ok()
}

/// Builds the status line shown under the QR code: window progress, holes,
/// and throughput.
fn status_line(base: u32, end: u32, chunk_count: u32, holes: usize, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64().max(0.001);
    let kbps = (base as f64 / secs).max(0.0);
    format!("window {base}-{end}/{chunk_count} holes:{holes} {kbps:.1} chunks/s")
}

fn missing_seqs(base: u32, end: u32, bitmap: u32) -> Vec<u32> {
    (base..end)
        .filter(|seq| bitmap & (1 << (seq - base)) == 0)
        .collect()
}

fn window_bitmap(got: &HashSet<u32>, base: u32, end: u32) -> u32 {
    (base..end).fold(0, |bits, candidate| {
        bits | if got.contains(&candidate) {
            1 << (candidate - base)
        } else {
            0
        }
    })
}

fn show_data(
    opt: &mut impl Optical,
    blob: &[u8],
    chunk_size: usize,
    base: u32,
    end: u32,
    cfg: TransportConfig,
    dwell_ms: u16,
) -> Result<()> {
    for seq in base..end {
        let start = seq as usize * chunk_size;
        let end = (start + chunk_size).min(blob.len());
        opt.show(&Payload::Data {
            seq,
            chunk: blob[start..end].to_vec(),
        })?;
        if !cfg.fast {
            thread::sleep(Duration::from_millis(dwell_ms.into()));
        }
    }
    Ok(())
}

pub fn send_blob(
    opt: &mut impl Optical,
    session: &Session,
    blob: &[u8],
    cfg: TransportConfig,
) -> Result<()> {
    let chunk_size = data_chunk_size(session.qr_version);
    let Some(chunk_count) = expected_chunk_count(blob.len(), chunk_size) else {
        return Err(Error::HandshakeFailed);
    };
    if chunk_count != session.chunk_count {
        return Err(Error::HandshakeFailed);
    }

    if cfg.gated && !opt.gate(&format!(
        "Ready to send {} in {chunk_count} chunks. Enter to start the transfer",
        session.basename
    ))? {
        return Err(Error::Aborted);
    }

    let start_time = Instant::now();
    let mut base = 0;
    while base < chunk_count {
        let end = (base + WINDOW).min(chunk_count);
        opt.set_status(&status_line(base, end, chunk_count, 0, start_time.elapsed()));
        show_data(
            opt,
            blob,
            chunk_size,
            base,
            end,
            cfg,
            session.dwell_ms,
        )?;

        let mut bitmap = 0;
        let mut previous_bitmap = 0;
        let mut stalls = 0;
        loop {
            let deadline = Instant::now() + ack_timeout(cfg);
            let mut received_ack = false;
            while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
                match opt.poll(remaining)? {
                    Some(Payload::Ack {
                        window_base,
                        bitmap: ack_bitmap,
                    }) if window_base == base => {
                        bitmap |= ack_bitmap;
                        received_ack = true;
                        break;
                    }
                    Some(_) => {}
                    None => break,
                }
            }

            let missing = missing_seqs(base, end, bitmap);
            if missing.is_empty() {
                break;
            }

            if !received_ack || bitmap == previous_bitmap {
                stalls += 1;
                if stalls == STALL_LIMIT {
                    return Err(Error::Stalled(missing));
                }
            } else {
                stalls = 0;
            }
            previous_bitmap = bitmap;

            opt.set_status(&status_line(
                base,
                end,
                chunk_count,
                missing.len(),
                start_time.elapsed(),
            ));
            for seq in missing {
                let start = seq as usize * chunk_size;
                let end = (start + chunk_size).min(blob.len());
                opt.show(&Payload::Data {
                    seq,
                    chunk: blob[start..end].to_vec(),
                })?;
                if !cfg.fast {
                    thread::sleep(Duration::from_millis(session.dwell_ms.into()));
                }
            }
        }
        base = end;
    }

    opt.set_status(&status_line(chunk_count, chunk_count, chunk_count, 0, start_time.elapsed()));
    opt.show(&Payload::Fin {
        sha256: session.sha256,
    })?;
    let deadline = Instant::now() + fin_timeout(cfg);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match opt.poll(remaining)? {
            Some(Payload::Ok) => return Ok(()),
            Some(Payload::Fail { reason: FAIL_HASH }) => return Err(Error::HashMismatch),
            Some(Payload::Fail { .. }) => return Err(Error::HandshakeFailed),
            Some(_) => {}
            None => break,
        }
    }
    Err(Error::HandshakeFailed)
}

fn emit_ack(
    opt: &mut impl Optical,
    got: &HashSet<u32>,
    base: u32,
    end: u32,
) -> Result<()> {
    opt.show(&Payload::Ack {
        window_base: base,
        bitmap: window_bitmap(got, base, end),
    })
}

pub fn recv_blob(
    opt: &mut impl Optical,
    session: &Session,
    cfg: TransportConfig,
) -> Result<Vec<u8>> {
    let chunk_size = data_chunk_size(session.qr_version);
    if chunk_size == 0 {
        return Err(Error::HandshakeFailed);
    }
    let chunk_count = session.chunk_count;
    let compressed_size =
        usize::try_from(session.compressed_size).map_err(|_| Error::HandshakeFailed)?;
    if expected_chunk_count(compressed_size, chunk_size) != Some(chunk_count) {
        return Err(Error::HandshakeFailed);
    }
    let mut got = HashSet::new();
    let mut blob = vec![0; compressed_size];
    let mut base = 0;
    let start_time = Instant::now();
    let mut idle_polls: u32 = 0;
    let mut last_new = Instant::now();
    let mut unacked = false;
    let mut seen_data = false;
    let mut last_old_window: Option<u32> = None;
    let quiet = burst_quiet(cfg, session.dwell_ms);

    loop {
        let end = (base + WINDOW).min(chunk_count);
        let poll_for = if seen_data && unacked && !cfg.fast {
            quiet
                .saturating_sub(last_new.elapsed())
                .max(Duration::from_millis(5))
        } else {
            ack_timeout(cfg)
        };

        match opt.poll(poll_for)? {
            Some(Payload::Data { seq, chunk }) => {
                idle_polls = 0;
                if seq < base {
                    if got.contains(&seq) {
                        let window_base = seq / WINDOW * WINDOW;
                        if last_old_window != Some(window_base) {
                            let window_end = (window_base + WINDOW).min(chunk_count);
                            emit_ack(opt, &got, window_base, window_end)?;
                            last_old_window = Some(window_base);
                        }
                    }
                    continue;
                }
                if seq >= end || got.contains(&seq) {
                    if unacked && last_new.elapsed() >= quiet {
                        emit_ack(opt, &got, base, end)?;
                        unacked = false;
                    }
                    continue;
                }

                let start = seq as usize * chunk_size;
                let expected_len = (compressed_size - start).min(chunk_size);
                if chunk.len() != expected_len {
                    return Err(Error::HandshakeFailed);
                }
                got.insert(seq);
                blob[start..start + expected_len].copy_from_slice(&chunk);
                seen_data = true;
                unacked = true;
                last_new = Instant::now();
                let holes = (base..end).filter(|c| !got.contains(c)).count();
                opt.set_status(&status_line(
                    base,
                    end,
                    chunk_count,
                    holes,
                    start_time.elapsed(),
                ));

                if (base..end).all(|candidate| got.contains(&candidate)) {
                    emit_ack(opt, &got, base, end)?;
                    unacked = false;
                    base = end;
                }
            }
            Some(Payload::Fail { reason: FAIL_HASH }) => return Err(Error::HashMismatch),
            Some(Payload::Fail { .. }) => return Err(Error::HandshakeFailed),
            Some(Payload::Fin { .. }) if got.len() == chunk_count as usize => {
                if blob.len() != compressed_size {
                    return Err(Error::HandshakeFailed);
                }
                let digest: [u8; 32] = Sha256::digest(&blob).into();
                if digest != session.sha256 {
                    opt.show(&Payload::Fail { reason: FAIL_HASH })?;
                    return Err(Error::HashMismatch);
                }
                // Do not show OK yet: the caller still needs to durably
                // write (and, on the CLI path, unpack) the blob. OK is only
                // sent once that succeeds, so the sender never sees OK
                // before the data is safely on disk.
                return Ok(blob);
            }
            Some(_) | None => {
                if unacked && last_new.elapsed() >= quiet {
                    emit_ack(opt, &got, base, end)?;
                    unacked = false;
                    continue;
                }
                if !seen_data {
                    if start_time.elapsed() >= first_data_budget(cfg) {
                        return Err(Error::Stalled(missing_seqs(
                            base,
                            end,
                            window_bitmap(&got, base, end),
                        )));
                    }
                    continue;
                }
                if !unacked {
                    idle_polls += 1;
                    if idle_polls >= STALL_LIMIT {
                        return Err(Error::Stalled(missing_seqs(
                            base,
                            end,
                            window_bitmap(&got, base, end),
                        )));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::{pair, Lossy};
    use sha2::{Digest, Sha256};
    use std::{thread, time::Duration};

    struct DropOneCompleteAck<T> {
        inner: T,
        dropped: bool,
    }

    impl<T: Optical> Optical for DropOneCompleteAck<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(
                payload,
                Payload::Ack {
                    bitmap: u32::MAX,
                    ..
                }
            ) && !self.dropped
            {
                self.dropped = true;
                return Ok(());
            }
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }
    }

    struct Gated<T> {
        inner: T,
        prompts: Vec<String>,
        allow: bool,
        data_shown: usize,
    }

    impl<T: Optical> Optical for Gated<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(payload, Payload::Data { .. }) {
                self.data_shown += 1;
            }
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn gate(&mut self, prompt: &str) -> Result<bool> {
            self.prompts.push(prompt.to_string());
            Ok(self.allow)
        }
    }

    fn session_for(blob: &[u8], version: u8) -> Session {
        let cs = data_chunk_size(version);
        let chunk_count = ((blob.len() + cs - 1) / cs) as u32;
        let mut hasher = Sha256::new();
        hasher.update(blob);
        let sha256: [u8; 32] = hasher.finalize().into();
        Session {
            qr_version: version,
            dwell_ms: 1,
            basename: "dir".into(),
            uncompressed_hint: blob.len() as u64,
            compressed_size: blob.len() as u64,
            chunk_count,
            sha256,
        }
    }

    /// Drops anything the peer sent while this side was encoding a frame —
    /// the live camera's "only the latest decode" behaviour, which used to
    /// throw away a DATA burst every time the receiver painted an ACK.
    struct DropInboundWhileShowing<T> {
        inner: T,
    }

    impl<T: Optical> Optical for DropInboundWhileShowing<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            while matches!(self.inner.poll(Duration::ZERO), Ok(Some(_))) {}
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }
    }

    #[test]
    fn the_operator_confirms_once_before_the_first_chunk_not_once_per_chunk() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (s, mut r) = pair();
        let mut s = Gated {
            inner: s,
            prompts: Vec::new(),
            allow: true,
            data_shown: 0,
        };
        let recv_sess = sess.clone();
        let recv = thread::spawn(move || {
            let got = recv_blob(&mut r, &recv_sess, TransportConfig::fast()).unwrap();
            // recv_blob leaves OK to its caller (which must durably write
            // first); stand in for that here so send_blob unblocks.
            r.show(&Payload::Ok).unwrap();
            got
        });
        send_blob(
            &mut s,
            &sess,
            &blob,
            TransportConfig {
                fast: true,
                gated: true,
            },
        )
        .unwrap();
        assert_eq!(recv.join().unwrap(), blob);

        assert_eq!(s.prompts.len(), 1, "{:?}", s.prompts);
        assert!(s.prompts[0].contains(&format!("{} chunks", sess.chunk_count)));
        assert!(s.data_shown >= sess.chunk_count as usize);
    }

    #[test]
    fn declining_the_start_gate_sends_no_data_at_all() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (s, _r) = pair();
        let mut s = Gated {
            inner: s,
            prompts: Vec::new(),
            allow: false,
            data_shown: 0,
        };
        let err = send_blob(
            &mut s,
            &sess,
            &blob,
            TransportConfig {
                fast: true,
                gated: true,
            },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Aborted), "{err:?}");
        assert_eq!(
            s.data_shown, 0,
            "a declined transfer must not blast frames at a receiver that moved on"
        );
    }

    #[test]
    fn acking_must_not_drop_the_rest_of_the_window() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, r) = pair();
        let mut r = DropInboundWhileShowing { inner: r };
        let cfg = TransportConfig::fast();
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || {
            let blob = recv_blob(&mut r, &sess, cfg).unwrap();
            r.show(&Payload::Ok).unwrap();
            blob
        });
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn transfer_zero_loss() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        let cfg = TransportConfig::fast();
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || {
            let blob = recv_blob(&mut r, &sess, cfg).unwrap();
            // recv_blob no longer sends OK itself (the caller must durably
            // write/unpack first); simulate that here so send_blob unblocks.
            r.show(&Payload::Ok).unwrap();
            blob
        });
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn transfer_drops_seq_zero_then_recovers() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, r) = pair();
        let mut r = Lossy {
            inner: r,
            drop_data_seq: [0].into_iter().collect(),
        };
        let cfg = TransportConfig::fast();
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || {
            let blob = recv_blob(&mut r, &sess, cfg).unwrap();
            r.show(&Payload::Ok).unwrap();
            blob
        });
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn transfer_burst_of_eight_drops() {
        let blob: Vec<u8> = (0..4000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, r) = pair();
        let mut r = Lossy {
            inner: r,
            drop_data_seq: (2..10).collect(),
        };
        let cfg = TransportConfig::fast();
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || {
            let blob = recv_blob(&mut r, &sess, cfg).unwrap();
            r.show(&Payload::Ok).unwrap();
            blob
        });
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn transfer_crosses_ack_window_boundary() {
        let blob: Vec<u8> = (0..6401u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        assert!(sess.chunk_count > WINDOW);
        let (mut s, mut r) = pair();
        let cfg = TransportConfig::fast();
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || {
            let blob = recv_blob(&mut r, &sess, cfg).unwrap();
            r.show(&Payload::Ok).unwrap();
            blob
        });
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn transfer_recovers_when_completed_window_ack_is_lost() {
        let blob: Vec<u8> = (0..6401u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, r) = pair();
        let mut r = DropOneCompleteAck {
            inner: r,
            dropped: false,
        };
        let cfg = TransportConfig::fast();
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || {
            let blob = recv_blob(&mut r, &sess, cfg).unwrap();
            r.show(&Payload::Ok).unwrap();
            blob
        });
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn recv_rejects_wrong_sized_data_chunk() {
        let blob = vec![1; 159];
        let mut sess = session_for(&blob, 10);
        sess.compressed_size = 160;
        let (mut s, mut r) = pair();
        s.show(&Payload::Data {
            seq: 0,
            chunk: blob,
        })
        .unwrap();
        s.show(&Payload::Fin {
            sha256: sess.sha256,
        })
        .unwrap();

        assert!(matches!(
            recv_blob(&mut r, &sess, TransportConfig::fast()),
            Err(Error::HandshakeFailed)
        ));
    }

    #[test]
    fn hash_mismatch_does_not_return_blob() {
        let blob = vec![1, 2, 3, 4];
        let mut sess = session_for(&blob, 10);
        sess.sha256 = [0u8; 32];
        let (mut s, mut r) = pair();
        let cfg = TransportConfig::fast();
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg)
        });
        let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg));
        let recv_res = rt.join().unwrap();
        assert!(matches!(recv_res, Err(crate::Error::HashMismatch)));
        assert!(matches!(st.join().unwrap(), Err(crate::Error::HashMismatch)));
    }

    #[test]
    fn recv_blob_stops_when_the_sender_reports_fail() {
        let blob = vec![1u8; 10];
        let sess = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        s.show(&Payload::Fail {
            reason: crate::frame::FAIL_PROTOCOL,
        })
        .unwrap();
        let err = recv_blob(&mut r, &sess, TransportConfig::fast()).unwrap_err();
        assert!(matches!(err, Error::HandshakeFailed), "{err:?}");
    }

    #[test]
    fn recv_blob_aborts_after_sustained_idle_polls_instead_of_looping_forever() {
        let blob = vec![1u8; 10];
        let sess = session_for(&blob, 10);
        // Keep the peer alive but silent: poll always times out (Ok(None)),
        // which used to loop forever. It must now abort with a bounded
        // number of idle polls instead of hanging.
        let (_send_end, mut recv_end) = pair();
        let cfg = TransportConfig::fast();
        let err = recv_blob(&mut recv_end, &sess, cfg).unwrap_err();
        assert!(matches!(err, Error::Stalled(_)));
    }
}
