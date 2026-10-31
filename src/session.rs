//! One complete transfer attempt, and the fallback ladder around it.
//!
//! A failed attempt that still moved chunks was an *interruption* — a
//! covered camera, a bumped lid — not evidence that the code is too dense.
//! The next attempt reconnects at the same rung, the receiver's in-memory
//! chunks carry over, and it costs nothing against the retry budget. An
//! attempt that moved nothing steps down: a smaller QR version (fewer,
//! larger modules) held on screen longer (more camera frames per code).
//!
//! Both sides walk the ladder independently. They need not agree on the
//! rung: the handshake re-runs every attempt, and the negotiated version is
//! whatever the *more* constrained side can manage.

use std::path::Path;

use crate::{
    link::{self, Attempt, LinkConfig, Linked, Session, MAX_ATTEMPTS},
    optical::{Optical, Replay},
    resume::{Offer, ReceiveCache},
    transport::{self, TransportConfig},
    Error, Result,
};

/// What an attempt settled on, for the operator-facing summary.
#[derive(Clone, Debug)]
pub struct Completed {
    pub session: Session,
    /// Handshakes before the one that finished.
    pub attempt: usize,
}

/// Where the fallback ladder stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Ladder {
    /// Index into [`link::attempt_plan`]: higher is smaller and slower.
    pub rung: usize,
    /// Attempts that moved nothing; bounded by [`MAX_ATTEMPTS`].
    pub failures: usize,
    /// Every attempt so far, including ones that made progress.
    pub attempts: usize,
}

impl Ladder {
    /// Records a failed attempt. One that moved chunks keeps the rung and
    /// the budget; one that moved nothing steps down and spends the budget.
    pub fn record_failure(&mut self, progressed: bool) {
        self.attempts += 1;
        if !progressed {
            self.rung += 1;
            self.failures += 1;
        }
    }

    pub fn exhausted(&self) -> bool {
        self.failures >= MAX_ATTEMPTS
    }
}

type Configs = fn(&Ladder, u8) -> (LinkConfig, TransportConfig, Attempt);

/// The link and transport settings for the ladder's current position.
///
/// Only the very first attempt is interactive: it waits for a human to aim
/// and has the sender confirm once before GO. Every later attempt —
/// whether it reconnects at the same rung or steps down — runs unattended.
pub fn configs_for(ladder: &Ladder, terminal_max: u8) -> (LinkConfig, TransportConfig, Attempt) {
    let plan = link::attempt_plan(ladder.rung, terminal_max);
    let link_cfg = if ladder.attempts == 0 {
        LinkConfig::gated(plan.max_qr_version)
    } else {
        LinkConfig::retry(plan)
    };
    (link_cfg, TransportConfig { fast: false }, plan)
}

fn note_retry(
    opt: &mut impl Optical,
    ladder: &Ladder,
    terminal_max: u8,
    err: &Error,
    progressed: bool,
) {
    let next = link::attempt_plan(ladder.rung, terminal_max);
    opt.log(&format!("attempt {} interrupted: {err}", ladder.attempts));
    if progressed {
        opt.log(&format!(
            "reconnecting at QR v{} max; everything received so far is kept",
            next.max_qr_version
        ));
    } else {
        opt.log(&format!(
            "retry {}/{}: QR v{} max, {} ms per code",
            ladder.failures + 1,
            MAX_ATTEMPTS,
            next.max_qr_version,
            next.dwell_floor_ms
        ));
    }
}

/// Runs `attempt` until it succeeds, fails in a way no retry can fix, or
/// the ladder runs out. `attempt` reports whether it moved any chunks.
///
/// A handshake start the transport read on its way out is pushed back, so
/// the next attempt's handshake reads it first.
fn climb<O: Optical, T>(
    opt: &mut O,
    terminal_max: u8,
    configs: Configs,
    mut attempt: impl FnMut(&mut Replay<'_, O>, LinkConfig, TransportConfig) -> (Result<T>, bool),
) -> Result<(T, usize)> {
    let mut opt = Replay::new(opt);
    let mut ladder = Ladder::default();
    loop {
        let (link_cfg, transport_cfg, _) = configs(&ladder, terminal_max);
        let (outcome, progressed) = attempt(&mut opt, link_cfg, transport_cfg);
        let err = match outcome {
            Ok(value) => return Ok((value, ladder.attempts)),
            Err(err) if link::is_retryable(&err) => err,
            Err(err) => return Err(err),
        };
        if let Error::Restart(payload) = &err {
            opt.push_back(payload.as_ref().clone());
        }
        ladder.record_failure(progressed);
        if ladder.exhausted() {
            return Err(err);
        }
        note_retry(&mut opt, &ladder, terminal_max, &err, progressed);
    }
}

fn climb_send<O: Optical>(
    opt: &mut O,
    offer: &Offer,
    blob: &[u8],
    terminal_max: u8,
    configs: Configs,
) -> Result<Completed> {
    let (session, attempt) = climb(
        opt,
        terminal_max,
        configs,
        |opt, link_cfg, transport_cfg| {
            let mut progressed = false;
            let outcome = link::run_send_handshake(opt, offer, link_cfg).and_then(
                |Linked { session, mut acked }| {
                    let before = acked.count();
                    let sent = transport::send_blob(opt, &session, blob, &mut acked, transport_cfg);
                    progressed = acked.count() > before;
                    sent.map(|()| session)
                },
            );
            (outcome, progressed)
        },
    )?;
    Ok(Completed { session, attempt })
}

fn climb_recv<O: Optical>(
    opt: &mut O,
    outdir: &Path,
    force: bool,
    terminal_max: u8,
    configs: Configs,
) -> Result<(Completed, Vec<u8>)> {
    let mut cache = ReceiveCache::default();
    let ((session, blob), attempt) = climb(
        opt,
        terminal_max,
        configs,
        |opt, link_cfg, transport_cfg| {
            let held_before = cache.held();
            let outcome = link::run_recv_handshake(opt, outdir, force, link_cfg, &mut cache)
                .and_then(|session| {
                    let store = cache.store(&session.offer).ok_or(Error::HandshakeFailed)?;
                    let blob = transport::recv_blob(opt, &session, store, transport_cfg)?;
                    Ok((session, blob))
                });
            if matches!(outcome, Err(Error::HashMismatch)) {
                // Some accepted chunk was wrong and there is no telling which;
                // resuming would fail the same way.
                cache.clear();
            }
            let progressed = cache.held() > held_before;
            (outcome, progressed)
        },
    )?;
    Ok((Completed { session, attempt }, blob))
}

/// Sends `blob`, reconnecting after interruptions and stepping down the
/// ladder after attempts that moved nothing.
pub fn send_with_fallback(
    opt: &mut impl Optical,
    offer: &Offer,
    blob: &[u8],
    terminal_max: u8,
) -> Result<Completed> {
    climb_send(opt, offer, blob, terminal_max, configs_for)
}

/// Receives a blob into `outdir`'s namespace. Chunks already assembled
/// survive every retry for an identical offer.
///
/// Returns the blob rather than writing it: the caller owns the durable
/// write and is the only thing that may tell the sender OK.
pub fn recv_with_fallback(
    opt: &mut impl Optical,
    outdir: &Path,
    force: bool,
    terminal_max: u8,
) -> Result<(Completed, Vec<u8>)> {
    climb_recv(opt, outdir, force, terminal_max, configs_for)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        chunks::ChunkStore,
        frame::{Payload, PROTOCOL_VERSION, ROLE_RECV},
        optical::{pair, Optical},
    };
    use sha2::{Digest, Sha256};
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, Instant},
    };

    fn fast_configs(ladder: &Ladder, terminal_max: u8) -> (LinkConfig, TransportConfig, Attempt) {
        let plan = link::attempt_plan(ladder.rung, terminal_max);
        (
            LinkConfig {
                fast: true,
                max_qr_version: plan.max_qr_version,
                dwell_floor_ms: plan.dwell_floor_ms,
                ..LinkConfig::default()
            },
            TransportConfig::fast(),
            plan,
        )
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let out = std::env::temp_dir().join(format!("ag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        out
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

    fn hello() -> Payload {
        Payload::Hello {
            protocol_ver: PROTOCOL_VERSION,
            role: ROLE_RECV,
        }
    }

    type Shown = Arc<Mutex<Vec<(u8, Payload)>>>;
    type Logs = Arc<Mutex<Vec<String>>>;

    /// Records what one side shows (with the version locked at the time)
    /// and what it logs.
    struct Tap<T> {
        inner: T,
        version: u8,
        shown: Shown,
        logs: Logs,
    }

    impl<T> Tap<T> {
        fn new(inner: T, shown: Shown, logs: Logs) -> Self {
            Self {
                inner,
                version: 0,
                shown,
                logs,
            }
        }
    }

    impl<T: Optical> Optical for Tap<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown
                .lock()
                .unwrap()
                .push((self.version, payload.clone()));
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.version = version;
            self.inner.set_version(version)
        }

        fn log(&mut self, line: &str) {
            self.logs.lock().unwrap().push(line.to_string());
        }
    }

    /// The receiver's camera reads `open` more DATA frames, then is covered:
    /// every later DATA frame goes unseen until `open` is raised again.
    struct Covered<T> {
        inner: T,
        open: Arc<AtomicUsize>,
    }

    impl<T: Optical> Optical for Covered<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            let deadline = Instant::now() + timeout;
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match self.inner.poll(remaining)? {
                    Some(Payload::Data { .. })
                        if self
                            .open
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
                            .is_err() =>
                    {
                        continue
                    }
                    other => return Ok(other),
                }
            }
        }

        fn set_version(&mut self, version: u8) {
            self.inner.set_version(version)
        }
    }

    /// A link that carries `budget` DATA frames above `ceiling` and then no
    /// more: the lid slipped, big codes stopped reading, small ones still do.
    struct Degrading<T> {
        inner: T,
        ceiling: u8,
        version: u8,
        budget: usize,
    }

    impl<T: Optical> Optical for Degrading<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(payload, Payload::Data { .. }) && self.version > self.ceiling {
                if self.budget == 0 {
                    return Ok(());
                }
                self.budget -= 1;
            }
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.version = version;
            self.inner.set_version(version)
        }
    }

    #[test]
    fn an_attempt_that_moved_chunks_reconnects_at_the_same_rung_for_free() {
        let mut ladder = Ladder::default();
        ladder.record_failure(true);
        assert_eq!(
            ladder,
            Ladder {
                rung: 0,
                failures: 0,
                attempts: 1
            }
        );
        ladder.record_failure(false);
        assert_eq!(
            ladder,
            Ladder {
                rung: 1,
                failures: 1,
                attempts: 2
            }
        );
        for _ in 1..MAX_ATTEMPTS {
            assert!(!ladder.exhausted());
            ladder.record_failure(false);
        }
        assert!(ladder.exhausted());
    }

    #[test]
    fn only_the_very_first_attempt_asks_the_operator_anything() {
        let (first, _, _) = configs_for(&Ladder::default(), 40);
        assert!(first.patient && first.confirm_offer);
        let resumed = Ladder {
            rung: 0,
            failures: 0,
            attempts: 1,
        };
        let (link_cfg, _, _) = configs_for(&resumed, 40);
        assert!(
            !link_cfg.confirm_offer,
            "a reconnect on the same rung must not ask again"
        );
        assert!(link_cfg.patient);
        for failures in 1..MAX_ATTEMPTS {
            let ladder = Ladder {
                rung: failures,
                failures,
                attempts: failures,
            };
            let (link_cfg, _, _) = configs_for(&ladder, 40);
            assert!(
                !link_cfg.confirm_offer,
                "attempt {failures} must not prompt"
            );
            assert!(link_cfg.patient);
        }
    }

    #[test]
    fn a_handshake_start_seen_mid_transfer_is_replayed_to_the_next_attempt() {
        let (mut end, _peer) = pair();
        let mut calls = 0;
        let (seen, attempt) = climb(&mut end, 40, fast_configs, |opt, _, _| {
            calls += 1;
            if calls == 1 {
                return (Err(Error::Restart(Box::new(hello()))), true);
            }
            (opt.poll(Duration::ZERO), false)
        })
        .unwrap();
        assert_eq!(seen, Some(hello()));
        assert_eq!(attempt, 1);
    }

    #[test]
    fn errors_no_retry_can_fix_end_the_climb_at_once() {
        let (mut end, _peer) = pair();
        let mut calls = 0;
        let err = climb(&mut end, 40, fast_configs, |_, _, _| {
            calls += 1;
            (Err::<(), _>(Error::Aborted), false)
        })
        .unwrap_err();
        assert!(matches!(err, Error::Aborted));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_covered_camera_mid_transfer_resumes_instead_of_restarting() {
        let blob: Vec<u8> = (0..4000u32).map(|i| (i * 7) as u8).collect();
        let offer = offer_for(&blob);
        assert_eq!(offer.chunk_count, 42);
        let (send_end, recv_end) = pair();
        let open = Arc::new(AtomicUsize::new(1));
        let shown: Shown = Arc::default();
        let mut sender = Tap::new(send_end, Arc::clone(&shown), Arc::default());
        let mut receiver = Covered {
            inner: recv_end,
            open: Arc::clone(&open),
        };
        let out = scratch_dir("covered");
        let mut cache = ReceiveCache::default();
        let link_cfg = LinkConfig::fast();
        let transport_cfg = TransportConfig::fast();

        // Round 1: the camera is covered after one span (several chunks); both sides give up.
        let send_t = thread::spawn({
            let (offer, blob) = (offer.clone(), blob.clone());
            move || {
                let result = link::run_send_handshake(&mut sender, &offer, link_cfg).and_then(
                    |Linked { session, mut acked }| {
                        transport::send_blob(
                            &mut sender,
                            &session,
                            &blob,
                            &mut acked,
                            transport_cfg,
                        )
                    },
                );
                (sender, result)
            }
        });
        let session =
            link::run_recv_handshake(&mut receiver, &out, false, link_cfg, &mut cache).unwrap();
        let store: &mut ChunkStore = cache.store(&session.offer).unwrap();
        let err = transport::recv_blob(&mut receiver, &session, store, transport_cfg).unwrap_err();
        assert!(matches!(err, Error::Stalled(_)), "{err:?}");
        let (mut sender, first) = send_t.join().unwrap();
        assert!(matches!(first, Err(Error::Stalled(_))), "{first:?}");
        let held = cache.held();
        assert!(
            held > 0 && held < 42,
            "everything read before the cover is kept: {held}"
        );

        // Round 2: uncovered. Only the missing chunks travel.
        open.store(usize::MAX, Ordering::SeqCst);
        shown.lock().unwrap().clear();
        let send_t = thread::spawn({
            let (offer, blob) = (offer.clone(), blob.clone());
            move || {
                link::run_send_handshake(&mut sender, &offer, link_cfg).and_then(
                    |Linked { session, mut acked }| {
                        assert_eq!(acked.count(), held, "the sender learns what is held");
                        transport::send_blob(
                            &mut sender,
                            &session,
                            &blob,
                            &mut acked,
                            transport_cfg,
                        )
                    },
                )
            }
        });
        let session =
            link::run_recv_handshake(&mut receiver, &out, false, link_cfg, &mut cache).unwrap();
        let store = cache.store(&session.offer).unwrap();
        let got = transport::recv_blob(&mut receiver, &session, store, transport_cfg).unwrap();
        receiver
            .show(&Payload::Ok {
                transfer_id: offer.transfer_id,
            })
            .unwrap();
        send_t.join().unwrap().unwrap();
        assert_eq!(got, blob);
        assert!(
            !shown
                .lock()
                .unwrap()
                .iter()
                .any(|(_, p)| matches!(p, Payload::Data { first, .. } if *first < held)),
            "chunks the receiver already held must not be sent again"
        );
    }

    #[test]
    fn a_link_that_degrades_mid_transfer_falls_back_and_keeps_what_arrived() {
        let blob: Vec<u8> = (0..4000u32).map(|i| (i * 7) as u8).collect();
        let offer = offer_for(&blob);
        let (send_end, recv_end) = pair();
        let shown: Shown = Arc::default();
        let recv_logs: Logs = Arc::default();
        let mut sender = Tap::new(
            Degrading {
                inner: send_end,
                ceiling: 15,
                version: 0,
                budget: 3,
            },
            Arc::clone(&shown),
            Arc::default(),
        );
        let mut receiver = Tap::new(recv_end, Arc::default(), Arc::clone(&recv_logs));

        let send_t = thread::spawn({
            let (offer, blob) = (offer.clone(), blob.clone());
            move || climb_send(&mut sender, &offer, &blob, 20, fast_configs)
        });
        let out = scratch_dir("degrade");
        let (done, got) = climb_recv(&mut receiver, &out, false, 20, fast_configs)
            .expect("the receiver converges");
        receiver
            .show(&Payload::Ok {
                transfer_id: offer.transfer_id,
            })
            .unwrap();
        let sent = send_t.join().unwrap().expect("the sender converges");

        assert_eq!(got, blob);
        assert!(
            done.session.qr_version <= 15,
            "got v{}",
            done.session.qr_version
        );
        assert!(sent.session.qr_version <= 15);
        let logs = recv_logs.lock().unwrap().clone();
        assert!(
            logs.iter().any(|line| line.contains("resuming with 12/42")),
            "the fallback must resume, not restart: {logs:?}"
        );
        assert!(
            !shown
                .lock()
                .unwrap()
                .iter()
                .any(|(version, p)| *version <= 15
                    && matches!(p, Payload::Data { first, .. } if *first < 12)),
            "chunks received at v20 must not be resent after the fallback"
        );
    }

    #[test]
    fn the_ladder_shrinks_the_code_then_slows_it_down() {
        let plans: Vec<Attempt> = (0..MAX_ATTEMPTS)
            .map(|attempt| link::attempt_plan(attempt, 40))
            .collect();
        assert_eq!(plans[0].max_qr_version, 40);
        assert_eq!(plans[0].dwell_floor_ms, 0, "the first try uses probe dwell");
        for pair in plans.windows(2) {
            assert!(pair[1].max_qr_version <= pair[0].max_qr_version);
            assert!(pair[1].dwell_floor_ms >= pair[0].dwell_floor_ms);
        }
        assert_eq!(
            plans[MAX_ATTEMPTS - 1].max_qr_version,
            crate::qr::smallest_version()
        );
        assert!(plans[MAX_ATTEMPTS - 1].dwell_floor_ms >= 1000);
    }

    #[test]
    fn a_small_terminal_starts_lower_and_still_bottoms_out_safely() {
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
        assert!(!link::is_retryable(&Error::Aborted));
        assert!(!link::is_retryable(&Error::Interrupted));
        assert!(!link::is_retryable(&Error::DestExists("x".into())));
        assert!(!link::is_retryable(&Error::Refused("x")));
        assert!(!link::is_retryable(&Error::ProtocolMismatch {
            peer: 1,
            ours: 2
        }));
        assert!(link::is_retryable(&Error::Stalled(vec![3])));
        assert!(link::is_retryable(&Error::HandshakeTimeout));
        assert!(link::is_retryable(&Error::HashMismatch));
        assert!(link::is_retryable(&Error::Restart(Box::new(hello()))));
    }
}
