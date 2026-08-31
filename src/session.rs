//! One complete transfer attempt, and the fallback ladder around it.
//!
//! A stall means the receiver's camera did not get a readable look at every
//! code the sender put on screen. Nothing about that is fixable by trying the
//! same settings again, so each retry makes the optical channel easier to
//! read: a smaller QR version (fewer, larger modules) held on screen for
//! longer (more camera frames per code).
//!
//! Both sides walk the same ladder independently. They do not need to agree on
//! which rung they are on — the handshake re-runs from scratch every attempt,
//! and the negotiated version is whatever the *more* constrained side can
//! manage, so one peer stepping down is enough to bring the other with it.

use std::path::Path;

use crate::{
    link::{self, Attempt, LinkConfig, Session, MAX_ATTEMPTS},
    optical::Optical,
    transport::{self, TransportConfig},
    Error, Result,
};

/// What an attempt settled on, for the operator-facing summary.
#[derive(Clone, Debug)]
pub struct Completed {
    pub session: Session,
    /// Zero-based index of the attempt that worked.
    pub attempt: usize,
}

/// The link and transport settings for attempt `n`.
///
/// The first attempt is the interactive one: it waits for the operator to
/// confirm aiming, then (on the sender) to start the file. Handshake phases
/// and the receive side run unattended. Every retry is fully unattended —
/// having committed to the transfer once, being asked to re-confirm each
/// rung of the ladder would defeat the point of retrying automatically.
pub fn configs_for(attempt: usize, terminal_max: u8) -> (LinkConfig, TransportConfig, Attempt) {
    let plan = link::attempt_plan(attempt, terminal_max);
    let link_cfg = if attempt == 0 {
        LinkConfig::gated(plan.max_qr_version)
    } else {
        LinkConfig::retry(plan)
    };
    let transport_cfg = TransportConfig {
        fast: false,
        gated: attempt == 0,
    };
    (link_cfg, transport_cfg, plan)
}

fn note_retry(opt: &mut impl Optical, attempt: usize, next: Attempt, err: &Error) {
    opt.log(&format!("attempt {} failed: {err}", attempt + 1));
    opt.log(&format!(
        "retry {}/{}: QR v{} max, {} ms per code",
        attempt + 2,
        MAX_ATTEMPTS,
        next.max_qr_version,
        next.dwell_floor_ms
    ));
}

/// Sends `blob`, stepping down the ladder until it lands or the attempts run
/// out.
pub fn send_with_fallback(
    opt: &mut impl Optical,
    basename: &str,
    uncompressed_hint: u64,
    blob: &[u8],
    sha256: [u8; 32],
    terminal_max: u8,
) -> Result<Completed> {
    let mut last = None;
    for attempt in 0..MAX_ATTEMPTS {
        let (link_cfg, transport_cfg, _) = configs_for(attempt, terminal_max);
        let outcome = link::run_send_handshake(
            opt,
            basename.to_string(),
            uncompressed_hint,
            blob,
            sha256,
            link_cfg,
        )
        .and_then(|session| {
            transport::send_blob(opt, &session, blob, transport_cfg)?;
            Ok(session)
        });

        match outcome {
            Ok(session) => return Ok(Completed { session, attempt }),
            Err(err) if link::is_retryable(&err) && attempt + 1 < MAX_ATTEMPTS => {
                note_retry(opt, attempt, link::attempt_plan(attempt + 1, terminal_max), &err);
                last = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or(Error::HandshakeFailed))
}

/// Receives a blob into `outdir`'s namespace, stepping down the same ladder.
///
/// Returns the blob rather than writing it: the caller owns the durable write
/// and is the only thing that may tell the sender OK.
pub fn recv_with_fallback(
    opt: &mut impl Optical,
    outdir: &Path,
    force: bool,
    terminal_max: u8,
) -> Result<(Completed, Vec<u8>)> {
    let mut last = None;
    for attempt in 0..MAX_ATTEMPTS {
        let (link_cfg, transport_cfg, _) = configs_for(attempt, terminal_max);
        let outcome = link::run_recv_handshake(opt, outdir, force, link_cfg).and_then(|session| {
            let blob = transport::recv_blob(opt, &session, transport_cfg)?;
            Ok((session, blob))
        });

        match outcome {
            Ok((session, blob)) => return Ok((Completed { session, attempt }, blob)),
            Err(err) if link::is_retryable(&err) && attempt + 1 < MAX_ATTEMPTS => {
                note_retry(opt, attempt, link::attempt_plan(attempt + 1, terminal_max), &err);
                last = Some(err);
            }
            Err(err) => return Err(err),
        }
    }
    Err(last.unwrap_or(Error::HandshakeFailed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        frame::Payload,
        optical::{pair, Optical},
    };
    use sha2::{Digest, Sha256};
    use std::{
        sync::{
            atomic::{AtomicU8, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    };

    /// A link whose probes all get through but which cannot sustain DATA above
    /// `data_ceiling` — the real failure the ladder exists for. Probing says
    /// the big code is readable, then the transfer misses chunks anyway.
    struct Fussy<T> {
        inner: T,
        data_ceiling: u8,
        version: Arc<AtomicU8>,
    }

    impl<T: Optical> Optical for Fussy<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(payload, Payload::Data { .. })
                && self.version.load(Ordering::SeqCst) > self.data_ceiling
            {
                // Swallowed: the camera never resolved this code.
                return Ok(());
            }
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.version.store(version, Ordering::SeqCst);
            self.inner.set_version(version);
        }
    }

    /// The ladder with test-speed timeouts, so a stall costs milliseconds.
    fn fast_configs(attempt: usize, terminal_max: u8) -> (LinkConfig, TransportConfig) {
        let plan = link::attempt_plan(attempt, terminal_max);
        (
            LinkConfig {
                fast: true,
                max_qr_version: plan.max_qr_version,
                dwell_floor_ms: plan.dwell_floor_ms,
                ..LinkConfig::default()
            },
            TransportConfig::fast(),
        )
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let out = std::env::temp_dir().join(format!("ag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        out
    }

    /// Mirrors `send_with_fallback`/`recv_with_fallback` at test speed.
    fn drive<F, T>(mut once: F) -> (Result<T>, usize)
    where
        F: FnMut(usize, LinkConfig, TransportConfig) -> Result<T>,
    {
        let mut last = None;
        for attempt in 0..MAX_ATTEMPTS {
            let (link_cfg, transport_cfg) = fast_configs(attempt, 40);
            match once(attempt, link_cfg, transport_cfg) {
                Ok(value) => return (Ok(value), attempt),
                Err(err) if link::is_retryable(&err) && attempt + 1 < MAX_ATTEMPTS => {
                    last = Some(err)
                }
                Err(err) => return (Err(err), attempt),
            }
        }
        (Err(last.unwrap_or(Error::HandshakeFailed)), MAX_ATTEMPTS)
    }

    #[test]
    fn a_link_that_cannot_sustain_big_codes_is_walked_down_until_it_can() {
        let blob: Vec<u8> = (0..1200u32).map(|i| i as u8).collect();
        let sha256: [u8; 32] = Sha256::digest(&blob).into();
        // Probes pass at every version, but DATA only survives at v15 or less.
        const CEILING: u8 = 15;

        let (send_end, recv_end) = pair();
        let send_version = Arc::new(AtomicU8::new(10));
        let recv_version = Arc::new(AtomicU8::new(10));
        let mut sender = Fussy {
            inner: send_end,
            data_ceiling: CEILING,
            version: Arc::clone(&send_version),
        };
        let mut receiver = Fussy {
            inner: recv_end,
            data_ceiling: CEILING,
            version: Arc::clone(&recv_version),
        };

        let send_blob = blob.clone();
        let send_t = thread::spawn(move || {
            drive(|_, link_cfg, transport_cfg| {
                let session = link::run_send_handshake(
                    &mut sender,
                    "dir".into(),
                    send_blob.len() as u64,
                    &send_blob,
                    sha256,
                    link_cfg,
                )?;
                transport::send_blob(&mut sender, &session, &send_blob, transport_cfg)?;
                Ok(session)
            })
        });

        let out = scratch_dir("ladder");
        let (received, recv_attempt) = drive(|_, link_cfg, transport_cfg| {
            let session = link::run_recv_handshake(&mut receiver, &out, false, link_cfg)?;
            let got = transport::recv_blob(&mut receiver, &session, transport_cfg)?;
            receiver.show(&Payload::Ok)?;
            Ok((session, got))
        });

        let (session, got) = received.expect("the ladder should reach a workable rung");
        assert_eq!(got, blob, "the file must arrive intact");
        assert!(
            session.qr_version <= CEILING,
            "should have stepped down to a version the link carries, got v{}",
            session.qr_version
        );
        assert!(
            recv_attempt > 0,
            "the first attempt was supposed to stall and trigger a retry"
        );
        let (sent, _) = send_t.join().unwrap();
        assert!(sent.is_ok(), "sender should converge too: {sent:?}");
    }

    #[test]
    fn the_ladder_shrinks_the_code_then_slows_it_down() {
        // Six usable versions, so the version bottoms out after five steps and
        // later attempts buy time on screen instead.
        let plans: Vec<Attempt> = (0..MAX_ATTEMPTS)
            .map(|attempt| link::attempt_plan(attempt, 40))
            .collect();

        assert_eq!(plans[0].max_qr_version, 40);
        assert_eq!(plans[0].dwell_floor_ms, 0, "the first try uses probe dwell");
        for pair in plans.windows(2) {
            assert!(
                pair[1].max_qr_version <= pair[0].max_qr_version,
                "the code must never grow between attempts"
            );
            assert!(
                pair[1].dwell_floor_ms >= pair[0].dwell_floor_ms,
                "the dwell must never shrink between attempts"
            );
        }
        assert_eq!(plans[MAX_ATTEMPTS - 1].max_qr_version, crate::qr::smallest_version());
        assert!(plans[MAX_ATTEMPTS - 1].dwell_floor_ms >= 1000);
    }

    #[test]
    fn a_small_terminal_starts_lower_and_still_bottoms_out_safely() {
        // A terminal that can only draw v15 has two rungs, not six.
        let plans: Vec<Attempt> = (0..MAX_ATTEMPTS)
            .map(|attempt| link::attempt_plan(attempt, 15))
            .collect();
        assert_eq!(plans[0].max_qr_version, 15);
        assert_eq!(plans[1].max_qr_version, 10);
        assert!(plans[2..]
            .iter()
            .all(|plan| plan.max_qr_version == crate::qr::smallest_version()));
    }

    #[test]
    fn operator_decisions_are_never_retried_away() {
        // Aborting or interrupting must end the transfer, not walk the ladder.
        assert!(!link::is_retryable(&Error::Aborted));
        assert!(!link::is_retryable(&Error::Interrupted));
        assert!(!link::is_retryable(&Error::DestExists("x".into())));
        // Whereas everything the ladder exists for is fair game.
        assert!(link::is_retryable(&Error::Stalled(vec![3])));
        assert!(link::is_retryable(&Error::HandshakeTimeout));
        assert!(link::is_retryable(&Error::HashMismatch));
    }

    #[test]
    fn only_the_first_attempt_asks_the_operator_anything() {
        let (first_link, first_transport, _) = configs_for(0, 40);
        assert!(
            first_link.patient,
            "the first attempt still waits for a human to aim"
        );
        assert!(
            first_transport.gated,
            "the sender confirms once before the file goes"
        );
        for attempt in 1..MAX_ATTEMPTS {
            let (link_cfg, transport_cfg, _) = configs_for(attempt, 40);
            assert!(!transport_cfg.gated, "attempt {attempt} must not prompt");
            assert!(
                link_cfg.patient,
                "attempt {attempt} still needs long timeouts: the peer may be \
                 finishing its own stall detection"
            );
        }
    }
}
