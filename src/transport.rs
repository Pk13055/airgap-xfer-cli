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
}

fn ack_timeout(cfg: TransportConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(2)
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

fn missing_seqs(base: u32, end: u32, bitmap: u32) -> Vec<u32> {
    (base..end)
        .filter(|seq| bitmap & (1 << (seq - base)) == 0)
        .collect()
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

    let mut base = 0;
    while base < chunk_count {
        let end = (base + WINDOW).min(chunk_count);
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
    let mut got = HashSet::new();
    let mut parts = vec![None; chunk_count as usize];
    let mut base = 0;

    loop {
        match opt.poll(ack_timeout(cfg))? {
            Some(Payload::Data { seq, chunk }) => {
                let end = (base + WINDOW).min(chunk_count);
                if seq < base || seq >= end || got.contains(&seq) {
                    continue;
                }

                got.insert(seq);
                parts[seq as usize] = Some(chunk);
                let bitmap = (base..end).fold(0, |bits, candidate| {
                    bits | if got.contains(&candidate) {
                        1 << (candidate - base)
                    } else {
                        0
                    }
                });
                opt.show(&Payload::Ack {
                    window_base: base,
                    bitmap,
                })?;

                if (base..end).all(|candidate| got.contains(&candidate)) {
                    base = end;
                }
            }
            Some(Payload::Fin { .. }) if got.len() == chunk_count as usize => {
                let mut blob = Vec::with_capacity(session.compressed_size as usize);
                for part in parts {
                    blob.extend(part.expect("all chunk parts were received"));
                }
                let digest: [u8; 32] = Sha256::digest(&blob).into();
                if digest != session.sha256 {
                    opt.show(&Payload::Fail { reason: FAIL_HASH })?;
                    return Err(Error::HashMismatch);
                }
                opt.show(&Payload::Ok)?;
                return Ok(blob);
            }
            Some(_) | None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::{pair, Lossy};
    use sha2::{Digest, Sha256};
    use std::thread;

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

    #[test]
    fn transfer_zero_loss() {
        let blob: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        let sess = session_for(&blob, 10);
        let (mut s, mut r) = pair();
        let cfg = TransportConfig { fast: true };
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg).unwrap());
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
        let cfg = TransportConfig { fast: true };
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg).unwrap());
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
        let cfg = TransportConfig { fast: true };
        let st = thread::spawn({
            let blob = blob.clone();
            let sess = sess.clone();
            move || send_blob(&mut s, &sess, &blob, cfg).unwrap()
        });
        let rt = thread::spawn(move || recv_blob(&mut r, &sess, cfg).unwrap());
        st.join().unwrap();
        assert_eq!(rt.join().unwrap(), blob);
    }

    #[test]
    fn hash_mismatch_does_not_return_blob() {
        let blob = vec![1, 2, 3, 4];
        let mut sess = session_for(&blob, 10);
        sess.sha256 = [0u8; 32];
        let (mut s, mut r) = pair();
        let cfg = TransportConfig { fast: true };
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
}
