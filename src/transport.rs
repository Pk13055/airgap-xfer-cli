//! Moving the archive once the link is up.
//!
//! The sender pages DATA frames, each packing as many *adjacent* missing
//! 96-byte chunks as the negotiated QR version holds, and after every burst
//! waits for a progress report. The receiver assembles chunks into a
//! [`ChunkStore`] that outlives this attempt: a stall here costs time, never
//! data, because the next attempt resumes from whatever the store holds.

use std::{
    thread,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};

use crate::{
    chunks::{plan_spans, report_limit, span_bytes, ChunkSet, ChunkStore},
    frame::{chunks_per_frame, fail_error, progress_windows, Payload, FAIL_HASH, FAIL_PROTOCOL},
    link::Session,
    optical::Optical,
    resume::{Offer, UNSCOPED},
    Error, Result,
};

/// DATA frames painted between progress reports.
pub const BURST_FRAMES: usize = 32;
/// Rounds in a row without a new chunk before this attempt gives up and
/// hands the link back to the fallback ladder.
const STALL_LIMIT: u32 = 20;
/// Missing chunk numbers quoted in a stall error.
const STALL_SAMPLE: usize = 32;

#[derive(Clone, Copy, Debug, Default)]
pub struct TransportConfig {
    pub fast: bool,
}

impl TransportConfig {
    /// Millisecond timeouts, for in-process tests.
    pub fn fast() -> Self {
        Self { fast: true }
    }
}

fn ack_timeout(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        // Must outlast the receiver's post-burst quiet wait (1.5× dwell, up to
        // ~1.8s) plus the time to encode and paint the progress QR.
        Duration::from_secs(5)
    }
}

/// How long the receiver waits for another *new* chunk before concluding the
/// sender has finished the burst and wants a progress report.
fn burst_quiet(cfg: TransportConfig, dwell_ms: u16) -> Duration {
    if cfg.fast {
        Duration::ZERO
    } else {
        Duration::from_millis((u64::from(dwell_ms) * 3 / 2).max(80))
    }
}

/// Upper bound on waiting for the first DATA chunk, in both modes. The
/// receiver only enters `recv_blob` after reading GO, so the sender is already
/// sending; an unbounded wait would mean a receiver that never decodes the
/// first DATA never stalls and the fallback ladder never runs.
fn first_data_budget(cfg: TransportConfig) -> Option<Duration> {
    Some(ack_timeout(cfg) * STALL_LIMIT)
}

/// How long the sender waits for OK after FIN. The receiver writes, syncs
/// and unpacks the archive before answering, which for a large tree takes a
/// while; the sender must not give up and report failure meanwhile.
fn fin_timeout(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(120)
    }
}

/// Least time between unprompted progress reports while only duplicates
/// arrive (the sender is resending because it missed a report).
fn report_refresh(cfg: TransportConfig) -> Duration {
    ack_timeout(cfg) / 2
}

fn status_line(have: &ChunkSet, new_this_attempt: u32, elapsed: Duration) -> String {
    let rate = f64::from(new_this_attempt) / elapsed.as_secs_f64().max(0.001);
    format!(
        "{}/{} chunks · next hole #{} · {rate:.1} chunks/s",
        have.count(),
        have.total(),
        have.contiguous()
    )
}

fn missing_sample(have: &ChunkSet) -> Vec<u32> {
    (have.contiguous()..have.total())
        .filter(|index| !have.contains(*index))
        .take(STALL_SAMPLE)
        .collect()
}

/// HELLO / PROBE / LINK mean the peer gave up on this attempt and started
/// another handshake. The payload goes back to the caller so that handshake
/// can still read it.
fn is_handshake_restart(payload: &Payload) -> bool {
    matches!(
        payload,
        Payload::Hello { .. } | Payload::Probe { .. } | Payload::Link { .. }
    )
}

/// Whether a FAIL addressed to `transfer_id` concerns `offer`. Unscoped FAILs
/// come from a peer whose display broke before it knew any offer.
fn concerns(offer: &Offer, transfer_id: u64) -> bool {
    transfer_id == offer.transfer_id || transfer_id == UNSCOPED
}

/// The receiver's next progress report for `offer`.
pub fn progress_payload(offer: &Offer, store: &mut ChunkStore, windows: usize) -> Payload {
    let (contiguous, bitmaps) = store.have().report(windows);
    Payload::Progress {
        transfer_id: offer.transfer_id,
        serial: store.next_serial(),
        contiguous,
        bitmaps,
    }
}

fn show_spans(
    opt: &mut impl Optical,
    blob: &[u8],
    offer: &Offer,
    spans: &[(u32, u32)],
    cfg: TransportConfig,
    dwell_ms: u16,
) -> Result<()> {
    let tiles = opt.tile_count().max(1);
    for batch in spans.chunks(tiles) {
        let payloads: Vec<Payload> = batch
            .iter()
            .map(|&(first, count)| {
                let range = span_bytes(offer.compressed_size, first, count)
                    .expect("planned spans lie inside the archive");
                Payload::Data {
                    transfer_id: offer.transfer_id,
                    first,
                    count: count as u8,
                    bytes: blob[range].to_vec(),
                }
            })
            .collect();
        opt.show_many(&payloads)?;
        if !cfg.fast {
            thread::sleep(Duration::from_millis(dwell_ms.into()));
        }
    }
    Ok(())
}

/// Waits up to `timeout` for a progress report not seen before and folds it
/// into `acked`, then folds in every report already queued behind it so the
/// caller plans from the newest picture rather than the oldest.
fn await_progress(
    opt: &mut impl Optical,
    offer: &Offer,
    acked: &mut ChunkSet,
    last_serial: &mut Option<u16>,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut got_one = false;
    loop {
        let wait = if got_one {
            Duration::ZERO
        } else {
            match deadline.checked_duration_since(Instant::now()) {
                Some(remaining) => remaining,
                None => break,
            }
        };
        match opt.poll(wait)? {
            Some(Payload::Progress {
                transfer_id,
                serial,
                contiguous,
                bitmaps,
            }) if transfer_id == offer.transfer_id && *last_serial != Some(serial) => {
                *last_serial = Some(serial);
                if acked.apply_report(contiguous, &bitmaps).is_some() {
                    got_one = true;
                }
            }
            Some(Payload::Fail {
                transfer_id,
                reason,
            }) if concerns(offer, transfer_id) => return Err(fail_error(reason)),
            Some(payload) if is_handshake_restart(&payload) => {
                return Err(Error::Restart(Box::new(payload)))
            }
            Some(_) => {}
            None => break,
        }
    }
    Ok(())
}

/// Sends every chunk `acked` does not already cover, then FIN, and returns
/// once the receiver says OK. `acked` is updated in place, so a caller can
/// tell whether this attempt moved anything even when it fails.
pub fn send_blob(
    opt: &mut impl Optical,
    session: &Session,
    blob: &[u8],
    acked: &mut ChunkSet,
    cfg: TransportConfig,
) -> Result<()> {
    let offer = &session.offer;
    let per_frame = chunks_per_frame(session.qr_version);
    if per_frame == 0
        || blob.len() as u64 != offer.compressed_size
        || acked.total() != offer.chunk_count
    {
        return Err(Error::HandshakeFailed);
    }
    let windows = progress_windows(session.qr_version);
    let start = Instant::now();
    let held_at_start = acked.count();
    let mut last_serial = None;
    let mut stalls = 0;
    while !acked.is_complete() {
        let limit = report_limit(acked.contiguous(), windows, acked.total());
        let spans = plan_spans(acked, per_frame, limit, BURST_FRAMES);
        opt.set_status(&status_line(
            acked,
            acked.count() - held_at_start,
            start.elapsed(),
        ));
        show_spans(opt, blob, offer, &spans, cfg, session.dwell_ms)?;

        let before = acked.count();
        await_progress(opt, offer, acked, &mut last_serial, ack_timeout(cfg))?;
        if acked.count() > before {
            stalls = 0;
        } else {
            stalls += 1;
            if stalls >= STALL_LIMIT {
                return Err(Error::Stalled(missing_sample(acked)));
            }
        }
    }

    opt.set_status("all chunks acknowledged; waiting for the receiver to verify, sync and unpack");
    opt.show(&Payload::Fin {
        transfer_id: offer.transfer_id,
        sha256: offer.sha256,
    })?;
    let deadline = Instant::now() + fin_timeout(cfg);
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match opt.poll(remaining)? {
            Some(Payload::Ok { transfer_id }) if transfer_id == offer.transfer_id => return Ok(()),
            Some(Payload::Fail {
                transfer_id,
                reason,
            }) if concerns(offer, transfer_id) => return Err(fail_error(reason)),
            Some(payload) if is_handshake_restart(&payload) => {
                return Err(Error::Restart(Box::new(payload)))
            }
            Some(_) => {}
            None => break,
        }
    }
    Err(Error::HandshakeFailed)
}

/// Assembles the archive into `store` until FIN arrives with every chunk
/// held and the SHA-256 matching, and returns a copy of it.
///
/// Never shows OK: the caller must persist, sync and unpack first (see
/// `crate::deliver`). On any error the chunks already in `store` stay there
/// for the next attempt.
pub fn recv_blob(
    opt: &mut impl Optical,
    session: &Session,
    store: &mut ChunkStore,
    cfg: TransportConfig,
) -> Result<Vec<u8>> {
    let offer = &session.offer;
    if store.size() != offer.compressed_size || store.have().total() != offer.chunk_count {
        return Err(Error::HandshakeFailed);
    }
    let windows = progress_windows(session.qr_version);
    let quiet = burst_quiet(cfg, session.dwell_ms);
    let start = Instant::now();
    let held_at_start = store.have().count();
    let mut last_new = Instant::now();
    let mut last_report = Instant::now();
    let mut unreported = false;
    let mut seen_data = false;
    let mut idle_rounds = 0;
    let mut frames_since_report = 0;
    let mut report_floor = store.have().contiguous();
    opt.set_status(&format!(
        "holding {held_at_start}/{} chunks; waiting for DATA",
        offer.chunk_count
    ));

    loop {
        let poll_for = if unreported && !cfg.fast {
            quiet
                .saturating_sub(last_new.elapsed())
                .max(Duration::from_millis(5))
        } else {
            ack_timeout(cfg)
        };

        let report_now = match opt.poll(poll_for)? {
            Some(Payload::Data {
                transfer_id,
                first,
                count,
                bytes,
            }) if transfer_id == offer.transfer_id => {
                seen_data = true;
                idle_rounds = 0;
                frames_since_report += 1;
                // The frame CRC passed and the id matches, so a span that
                // does not fit the offer is a protocol bug, not line noise.
                let accepted = store
                    .accept(first, u32::from(count), &bytes)
                    .map_err(|_| Error::HandshakeFailed)?;
                if accepted.conflict > 0 {
                    opt.log(&format!(
                        "kept the first copy of {} chunk(s) near #{first} that arrived again with different bytes",
                        accepted.conflict
                    ));
                }
                if accepted.new > 0 {
                    last_new = Instant::now();
                    unreported = true;
                    opt.set_status(&status_line(
                        store.have(),
                        store.have().count() - held_at_start,
                        start.elapsed(),
                    ));
                    let last = first + u32::from(count) - 1;
                    // Report at the end of a burst, not when a window inside
                    // it fills: the sender plans from the first report it
                    // sees, so a mid-burst one makes it resend what is still
                    // arriving. A burst ends at the edge of what one report
                    // can describe or after BURST_FRAMES frames.
                    store.is_complete()
                        || frames_since_report >= BURST_FRAMES
                        || last + 1 >= report_limit(report_floor, windows, offer.chunk_count)
                } else {
                    // Only repeats: the sender is resending what it thinks is
                    // missing, so it probably missed the last report.
                    !unreported && last_report.elapsed() >= quiet.max(report_refresh(cfg))
                }
            }
            // Another transfer's traffic, e.g. from before a restart.
            Some(Payload::Data { .. }) => false,
            Some(Payload::Fin {
                transfer_id,
                sha256,
            }) if transfer_id == offer.transfer_id => {
                if !store.is_complete() {
                    true
                } else {
                    if sha256 != offer.sha256 {
                        opt.show(&Payload::Fail {
                            transfer_id,
                            reason: FAIL_PROTOCOL,
                        })?;
                        return Err(Error::HandshakeFailed);
                    }
                    let digest: [u8; 32] = Sha256::digest(store.blob()).into();
                    if digest != offer.sha256 {
                        opt.show(&Payload::Fail {
                            transfer_id,
                            reason: FAIL_HASH,
                        })?;
                        return Err(Error::HashMismatch);
                    }
                    return Ok(store.blob().to_vec());
                }
            }
            Some(Payload::Fail {
                transfer_id,
                reason,
            }) if concerns(offer, transfer_id) => return Err(fail_error(reason)),
            Some(payload) if is_handshake_restart(&payload) => {
                return Err(Error::Restart(Box::new(payload)))
            }
            Some(_) => unreported && last_new.elapsed() >= quiet,
            None => {
                if unreported && last_new.elapsed() >= quiet {
                    true
                } else if !seen_data {
                    if first_data_budget(cfg).is_some_and(|budget| start.elapsed() >= budget) {
                        return Err(Error::Stalled(missing_sample(store.have())));
                    }
                    false
                } else {
                    idle_rounds += 1;
                    if idle_rounds >= STALL_LIMIT {
                        return Err(Error::Stalled(missing_sample(store.have())));
                    }
                    // A whole timeout with nothing new: repaint the report so
                    // a reconnecting sender, or one that missed the last
                    // report, has a fresh one to act on.
                    true
                }
            }
        };

        if report_now {
            opt.show(&progress_payload(offer, store, windows))?;
            unreported = false;
            frames_since_report = 0;
            report_floor = store.have().contiguous();
            last_report = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chunks::WINDOW_CHUNKS,
        frame::{PROTOCOL_VERSION, ROLE_RECV},
        optical::{pair, Lossy},
    };
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    #[test]
    fn first_data_wait_is_bounded_in_both_modes() {
        let live = first_data_budget(TransportConfig::default()).expect("live mode is bounded");
        assert!(live >= Duration::from_secs(60), "live budget {live:?}");
        let fast = first_data_budget(TransportConfig::fast()).expect("fast mode is bounded");
        assert!(fast <= Duration::from_secs(5), "fast budget {fast:?}");
    }

    fn offer_for(blob: &[u8]) -> Offer {
        Offer::new(
            "dir",
            blob.len() as u64,
            blob.len() as u64,
            Sha256::digest(blob).into(),
            1,
        )
        .unwrap()
    }

    fn session_for(blob: &[u8], version: u8) -> Session {
        Session {
            qr_version: version,
            dwell_ms: 1,
            offer: offer_for(blob),
        }
    }

    fn ramp(len: u32) -> Vec<u8> {
        (0..len).map(|i| i as u8).collect()
    }

    /// Runs both ends over `s`/`r`. The receiver shows OK once `recv_blob`
    /// returns, standing in for `deliver`.
    fn transfer<S, R>(
        mut s: S,
        mut r: R,
        session: &Session,
        blob: &[u8],
    ) -> (Result<()>, Result<Vec<u8>>, ChunkStore)
    where
        S: Optical + Send + 'static,
        R: Optical,
    {
        let cfg = TransportConfig::fast();
        let sender = thread::spawn({
            let session = session.clone();
            let blob = blob.to_vec();
            move || {
                let mut acked = ChunkSet::new(session.offer.chunk_count);
                send_blob(&mut s, &session, &blob, &mut acked, cfg)
            }
        });
        let mut store = ChunkStore::new(session.offer.compressed_size).unwrap();
        let got = recv_blob(&mut r, session, &mut store, cfg);
        if got.is_ok() {
            r.show(&Payload::Ok {
                transfer_id: session.offer.transfer_id,
            })
            .unwrap();
        }
        (sender.join().unwrap(), got, store)
    }

    struct Recorder<T> {
        inner: T,
        shown: Arc<Mutex<Vec<Payload>>>,
    }

    impl<T: Optical> Optical for Recorder<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown.lock().unwrap().push(payload.clone());
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }
    }

    /// Loses the receiver's first "everything is here" report.
    struct DropTheCompletionReport<T> {
        inner: T,
        dropped: bool,
    }

    impl<T: Optical> Optical for DropTheCompletionReport<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(payload, Payload::Progress { bitmaps, .. } if bitmaps.is_empty())
                && !self.dropped
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

    /// Drops anything the peer sent while this side was painting a frame —
    /// the live camera's behaviour while the UI thread is busy.
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

    fn assert_delivered((sent, got, _): (Result<()>, Result<Vec<u8>>, ChunkStore), blob: &[u8]) {
        sent.unwrap();
        assert_eq!(got.unwrap(), blob);
    }

    #[test]
    fn transfer_zero_loss() {
        let blob = ramp(2000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn the_first_burst_packs_adjacent_chunks_into_each_frame() {
        let blob = ramp(3000);
        let session = session_for(&blob, 40);
        assert_eq!(session.offer.chunk_count, 32);
        let (s, r) = pair();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            inner: s,
            shown: Arc::clone(&shown),
        };
        assert_delivered(transfer(recorder, r, &session, &blob), &blob);
        let spans: Vec<(u32, u8)> = shown
            .lock()
            .unwrap()
            .iter()
            .filter_map(|p| match p {
                Payload::Data { first, count, .. } => Some((*first, *count)),
                _ => None,
            })
            .collect();
        assert_eq!(&spans[..2], &[(0, 17), (17, 15)]);
    }

    #[test]
    fn zero_loss_transfers_send_each_chunk_once_at_every_version() {
        let blob = ramp(640 * crate::chunks::CHUNK_BYTES as u32);
        for version in crate::qr::SUPPORTED_VERSIONS {
            let session = session_for(&blob, version);
            assert_eq!(session.offer.chunk_count, 640);
            let (s, r) = pair();
            let shown = Arc::new(Mutex::new(Vec::new()));
            let recorder = Recorder {
                inner: s,
                shown: Arc::clone(&shown),
            };
            assert_delivered(transfer(recorder, r, &session, &blob), &blob);
            let sent: u32 = shown
                .lock()
                .unwrap()
                .iter()
                .filter_map(|p| match p {
                    Payload::Data { count, .. } => Some(u32::from(*count)),
                    _ => None,
                })
                .sum();
            assert_eq!(sent, 640, "QR v{version} resent chunks on a clean link");
        }
    }

    #[test]
    fn a_dropped_span_is_sent_again() {
        let blob = ramp(2000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = Lossy {
            inner: r,
            drop_first: [0].into_iter().collect(),
        };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn a_burst_of_drops_is_sent_again() {
        let blob = ramp(4000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = Lossy {
            inner: r,
            drop_first: (2..10).collect(),
        };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn transfers_span_many_progress_windows() {
        let blob = ramp(6401);
        let session = session_for(&blob, 10);
        assert!(session.offer.chunk_count > 2 * WINDOW_CHUNKS);
        let (s, r) = pair();
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn a_lost_completion_report_is_recovered() {
        let blob = ramp(6401);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = DropTheCompletionReport {
            inner: r,
            dropped: false,
        };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn reporting_progress_must_not_drop_the_rest_of_the_burst() {
        let blob = ramp(2000);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let r = DropInboundWhileShowing { inner: r };
        assert_delivered(transfer(s, r, &session, &blob), &blob);
    }

    #[test]
    fn duplicates_and_conflicts_never_override_accepted_bytes() {
        let good: Vec<u8> = (0..96).collect();
        let session = session_for(&good, 10);
        let tid = session.offer.transfer_id;
        let (mut s, mut r) = pair();
        for bytes in [good.clone(), good.clone(), vec![0xEE; 96]] {
            s.show(&Payload::Data {
                transfer_id: tid,
                first: 0,
                count: 1,
                bytes,
            })
            .unwrap();
        }
        s.show(&Payload::Fin {
            transfer_id: tid,
            sha256: session.offer.sha256,
        })
        .unwrap();
        let mut store = ChunkStore::new(96).unwrap();
        let got = recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap();
        assert_eq!(got, good);
        assert_eq!(store.have().count(), 1, "repeats must not inflate progress");
        assert_eq!(store.conflicts(), 1);
    }

    #[test]
    fn traffic_for_another_transfer_is_ignored() {
        let good: Vec<u8> = (0..96).collect();
        let session = session_for(&good, 10);
        let tid = session.offer.transfer_id;
        let (mut s, mut r) = pair();
        s.show(&Payload::Data {
            transfer_id: tid ^ 1,
            first: 0,
            count: 1,
            bytes: vec![0xEE; 96],
        })
        .unwrap();
        s.show(&Payload::Data {
            transfer_id: tid,
            first: 0,
            count: 1,
            bytes: good.clone(),
        })
        .unwrap();
        s.show(&Payload::Fin {
            transfer_id: tid,
            sha256: session.offer.sha256,
        })
        .unwrap();
        let mut store = ChunkStore::new(96).unwrap();
        assert_eq!(
            recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap(),
            good
        );
        assert_eq!(store.conflicts(), 0);
    }

    #[test]
    fn recv_rejects_a_span_of_the_wrong_length() {
        let blob = ramp(160);
        let session = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        s.show(&Payload::Data {
            transfer_id: session.offer.transfer_id,
            first: 0,
            count: 1,
            bytes: vec![1; 95],
        })
        .unwrap();
        let mut store = ChunkStore::new(160).unwrap();
        assert!(matches!(
            recv_blob(&mut r, &session, &mut store, TransportConfig::fast()),
            Err(Error::HandshakeFailed)
        ));
    }

    #[test]
    fn hash_mismatch_does_not_return_blob() {
        let blob = vec![1, 2, 3, 4];
        let session = Session {
            qr_version: 10,
            dwell_ms: 1,
            offer: Offer::new("dir", 4, 4, [0; 32], 1).unwrap(),
        };
        let (s, r) = pair();
        let (sent, got, _) = transfer(s, r, &session, &blob);
        assert!(matches!(got, Err(Error::HashMismatch)), "{got:?}");
        assert!(matches!(sent, Err(Error::HashMismatch)), "{sent:?}");
    }

    #[test]
    fn recv_blob_stops_when_the_sender_reports_fail() {
        let blob = ramp(10);
        let session = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        s.show(&Payload::Fail {
            transfer_id: session.offer.transfer_id,
            reason: FAIL_PROTOCOL,
        })
        .unwrap();
        let mut store = ChunkStore::new(10).unwrap();
        let err = recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap_err();
        assert!(matches!(err, Error::HandshakeFailed), "{err:?}");
    }

    #[test]
    fn a_handshake_start_seen_mid_transfer_is_handed_back_and_data_is_kept() {
        let blob = ramp(500);
        let session = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        r.show(&Payload::Hello {
            protocol_ver: PROTOCOL_VERSION,
            role: ROLE_RECV,
        })
        .unwrap();
        let mut acked = ChunkSet::new(session.offer.chunk_count);
        let err =
            send_blob(&mut s, &session, &blob, &mut acked, TransportConfig::fast()).unwrap_err();
        assert!(
            matches!(&err, Error::Restart(p) if matches!(**p, Payload::Hello { .. })),
            "{err:?}"
        );

        let probe = Payload::Probe {
            id: 0,
            qr_version: 10,
            dwell_ms: 250,
            last: true,
        };
        s.show(&probe).unwrap();
        let mut store = ChunkStore::new(session.offer.compressed_size).unwrap();
        let err = recv_blob(&mut r, &session, &mut store, TransportConfig::fast()).unwrap_err();
        assert!(matches!(&err, Error::Restart(p) if **p == probe), "{err:?}");
        assert!(
            store.is_complete(),
            "chunks that arrived before the restart are kept"
        );
    }

    #[test]
    fn recv_blob_gives_up_after_sustained_silence() {
        let blob = ramp(10);
        let session = session_for(&blob, 10);
        let (_s, mut r) = pair();
        let mut store = ChunkStore::new(10).unwrap();
        assert!(matches!(
            recv_blob(&mut r, &session, &mut store, TransportConfig::fast()),
            Err(Error::Stalled(_))
        ));
    }

    #[test]
    fn the_receiver_never_acknowledges_success_itself() {
        let blob = ramp(500);
        let session = session_for(&blob, 10);
        let (s, r) = pair();
        let shown = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder {
            inner: r,
            shown: Arc::clone(&shown),
        };
        assert_delivered(transfer(s, recorder, &session, &blob), &blob);
        let shown = shown.lock().unwrap();
        let ok_at = shown
            .iter()
            .position(|p| matches!(p, Payload::Ok { .. }))
            .expect("the harness shows OK");
        assert_eq!(
            ok_at,
            shown.len() - 1,
            "recv_blob itself must never show OK"
        );
    }
}
