use std::{
    collections::HashSet,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use crate::{
    chunks::ChunkSet,
    frame::{
        chunks_per_frame, fail_error, progress_windows, qr_byte_capacity, Payload, FAIL_ABORTED,
        FAIL_DISK, FAIL_PROTOCOL, PROTOCOL_VERSION, ROLE_RECV,
    },
    optical::Optical,
    pack::dest_exists,
    resume::{Offer, ReceiveCache, Reuse, UNSCOPED},
    transport::progress_payload,
    Error, Result,
};

/// Probe id, QR version, and display dwell time in milliseconds.
pub const PROBES: [(u8, u8, u16); 6] = [
    (0, 10, 250),
    (1, 15, 250),
    (2, 20, 200),
    (3, 25, 200),
    (4, 30, 150),
    (5, 40, 150),
];

#[derive(Clone, Copy, Debug)]
pub struct LinkConfig {
    /// Shortens HELLO/LINK/GO timeouts to 100/50/50 ms, quiet time to 20 ms,
    /// and skips probe dwell sleeps.
    pub fast: bool,
    /// Stretches the HELLO/LINK/GO timeouts to [`GATED_TIMEOUT`]. Set whenever
    /// the peer might be slow to arrive — a human reading the screen, or a
    /// stalled peer still working through its own retry — which is every
    /// interactive path, gated or not.
    pub patient: bool,
    /// Highest QR version this side can display without clipping. Probes above
    /// it are never offered, and never chosen from the other peer's offers: a
    /// clipped code is undecodable.
    pub max_qr_version: u8,
    /// Lower bound on how long each code stays on screen, overriding the probe
    /// table. Raised on retries: the receiver's camera needs several frames per
    /// displayed code to have a fair chance at every one of them.
    pub dwell_floor_ms: u16,
    /// Sender only: pause for Enter after LINK and before GO, so the offer
    /// and the first DATA follow the keypress immediately. The receiver is
    /// still waiting for GO at that point and has not started stall detection.
    pub confirm_offer: bool,
}

/// Timeout used for peer replies when a human sits between the phases.
pub const GATED_TIMEOUT: Duration = Duration::from_secs(600);

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            fast: false,
            patient: false,
            max_qr_version: 40,
            dwell_floor_ms: 0,
            confirm_offer: false,
        }
    }
}

impl LinkConfig {
    /// Millisecond timeouts and no dwell sleeps, for in-process tests.
    pub fn fast() -> Self {
        Self {
            fast: true,
            ..Self::default()
        }
    }

    /// The first, interactive attempt: generous timeouts so a human can aim,
    /// QR versions capped to what this terminal can draw, and one Enter on
    /// the sender before GO. Handshake probes themselves do not wait for Enter.
    pub fn gated(max_qr_version: u8) -> Self {
        Self {
            patient: true,
            max_qr_version,
            confirm_offer: true,
            ..Self::default()
        }
    }

    /// A retry: same generous timeouts, but no prompts. Once the operator has
    /// committed to the transfer, asking them to re-confirm every rung of the
    /// fallback ladder would defeat the point of retrying automatically.
    pub fn retry(plan: Attempt) -> Self {
        Self {
            patient: true,
            max_qr_version: plan.max_qr_version,
            dwell_floor_ms: plan.dwell_floor_ms,
            ..Self::default()
        }
    }
}

/// How many times a stalled transfer is retried before giving up.
pub const MAX_ATTEMPTS: usize = 10;
/// Ceiling on the per-code dwell the ladder will climb to.
const MAX_DWELL_MS: u16 = 1500;

/// The optical settings to try on a given attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attempt {
    pub max_qr_version: u8,
    pub dwell_floor_ms: u16,
}

/// Settings for attempt `n`, each rung trading throughput for legibility.
///
/// Missing sequence numbers mean the receiver's camera did not get a readable
/// look at every code, so both levers point the same way: a smaller QR version
/// (fewer, larger modules) held on screen for longer (more camera frames per
/// code). The version steps down one rung per attempt until it bottoms out at
/// the smallest usable one; after that only the dwell keeps growing.
pub fn attempt_plan(attempt: usize, terminal_max: u8) -> Attempt {
    let rungs: Vec<u8> = crate::qr::SUPPORTED_VERSIONS
        .iter()
        .copied()
        .filter(|version| *version <= terminal_max)
        .collect();
    let top = rungs.len().saturating_sub(1);
    Attempt {
        max_qr_version: rungs
            .get(top.saturating_sub(attempt))
            .copied()
            .unwrap_or_else(crate::qr::smallest_version),
        dwell_floor_ms: if attempt == 0 {
            0
        } else {
            (150 * (attempt as u16 + 1)).min(MAX_DWELL_MS)
        },
    }
}

/// Whether `err` is the kind of failure another, more conservative attempt
/// could plausibly fix — as opposed to one that will fail identically forever.
pub fn is_retryable(err: &Error) -> bool {
    matches!(
        err,
        Error::Stalled(_)
            | Error::HandshakeTimeout
            | Error::HandshakeFailed
            | Error::NoUsableProbe
            | Error::BadFrame
            | Error::HashMismatch
            | Error::Restart(_)
    )
}

/// What both sides agreed on for one attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub qr_version: u8,
    pub dwell_ms: u16,
    pub offer: Offer,
}

/// The sender's side of a completed handshake: the session, plus what the
/// receiver reported it already holds.
#[derive(Clone, Debug)]
pub struct Linked {
    pub session: Session,
    pub acked: ChunkSet,
}

fn hello_timeout(cfg: LinkConfig) -> Duration {
    // `fast` is the test knob and wins: patient timeouts are minutes long
    // because a human (or a peer working through its own retry) is in the
    // loop, which no test wants to wait for. It is still a full second: long
    // enough to outlast the peer's fast-mode stall detection (20 rounds of
    // 50 ms), so a test peer that is still giving up on its previous attempt
    // is waited for rather than counted as another failure.
    if cfg.fast {
        Duration::from_secs(1)
    } else if cfg.patient {
        GATED_TIMEOUT
    } else {
        Duration::from_secs(30)
    }
}

fn link_timeout(cfg: LinkConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else if cfg.patient {
        GATED_TIMEOUT
    } else {
        Duration::from_secs(5)
    }
}

fn go_timeout(cfg: LinkConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else if cfg.patient {
        GATED_TIMEOUT
    } else {
        Duration::from_secs(15)
    }
}

fn quiet_timeout(cfg: LinkConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(20)
    } else {
        Duration::from_secs(2)
    }
}

fn poll_until<T>(
    opt: &mut impl Optical,
    timeout: Duration,
    mut matches: impl FnMut(Payload) -> Option<T>,
) -> Result<Option<T>> {
    let deadline = Instant::now() + timeout;
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(None);
        };
        match opt.poll(remaining)? {
            Some(payload) => {
                if let Some(value) = matches(payload) {
                    return Ok(Some(value));
                }
            }
            None => return Ok(None),
        }
    }
}

fn offer_prompt(offer: &Offer, qr_version: u8) -> String {
    format!(
        "Linked at QR v{qr_version}. Send {} ({} B, {} chunks)? Enter sends, Esc aborts. The receiver accepts on its own",
        offer.basename, offer.compressed_size, offer.chunk_count
    )
}

pub fn run_send_handshake(
    opt: &mut impl Optical,
    offer: &Offer,
    cfg: LinkConfig,
) -> Result<Linked> {
    let hello = poll_until(opt, hello_timeout(cfg), |payload| match payload {
        Payload::Hello { .. } => Some(()),
        _ => None,
    })?;
    if hello.is_none() {
        return Err(Error::HandshakeTimeout);
    }
    opt.log("receiver said HELLO");

    // Probes above what this terminal can draw would render clipped and
    // never decode, so they are dropped rather than offered.
    let probes: Vec<_> = PROBES
        .iter()
        .copied()
        .filter(|(_, qr_version, _)| *qr_version <= cfg.max_qr_version)
        .collect();
    let Some(&(last_id, _, _)) = probes.last() else {
        return Err(Error::NoUsableProbe);
    };
    for (id, qr_version, dwell_ms) in probes {
        opt.show(&Payload::Probe {
            id,
            qr_version,
            dwell_ms,
            last: id == last_id,
        })?;
        if !cfg.fast {
            thread::sleep(Duration::from_millis(dwell_ms.into()));
        }
    }

    let Some((qr_version, linked_dwell_ms)) =
        poll_until(opt, link_timeout(cfg), |payload| match payload {
            Payload::Link {
                qr_version,
                dwell_ms,
                ..
            } => Some((qr_version, dwell_ms)),
            _ => None,
        })?
    else {
        return Err(Error::HandshakeFailed);
    };
    let dwell_ms = linked_dwell_ms.max(cfg.dwell_floor_ms);
    if qr_version > cfg.max_qr_version {
        return Err(Error::NoUsableProbe);
    }

    // Lock the QR version before GO is shown so GO (and everything after
    // it) is encoded at the negotiated version instead of the probe default.
    opt.set_version(qr_version);
    opt.log(&format!("linked at QR v{qr_version}, {dwell_ms} ms dwell"));

    if chunks_per_frame(qr_version) == 0 {
        return Err(Error::HandshakeFailed);
    }
    crate::qr::ensure_full_data_frame_fits(qr_version)?;
    let go = Payload::Go(offer.clone());
    let go_len = crate::frame::encode(&go)?.len();
    let cap = qr_byte_capacity(qr_version);
    if go_len > cap {
        return Err(Error::Message(format!(
            "filename {:?} makes the offer {go_len} bytes; QR v{qr_version} holds {cap}",
            offer.basename
        )));
    }
    // Confirm here, not after the receiver has already accepted: the
    // receiver is still waiting for the offer, not timing DATA.
    if cfg.confirm_offer && !opt.gate(&offer_prompt(offer, qr_version))? {
        return Err(Error::Aborted);
    }
    opt.show(&go)?;

    // Acceptance is a progress report: empty for a new offer, or everything
    // already held when the receiver is resuming this exact offer.
    let reply = poll_until(opt, go_timeout(cfg), |payload| match payload {
        Payload::Progress {
            transfer_id,
            contiguous,
            bitmaps,
            ..
        } if transfer_id == offer.transfer_id => Some(Ok((contiguous, bitmaps))),
        Payload::Fail {
            transfer_id,
            reason,
        } if transfer_id == offer.transfer_id || transfer_id == UNSCOPED => {
            Some(Err(fail_error(reason)))
        }
        _ => None,
    })?;
    let Some(reply) = reply else {
        return Err(Error::HandshakeFailed);
    };
    let (contiguous, bitmaps) = reply?;
    let acked = ChunkSet::from_report(offer.chunk_count, contiguous, &bitmaps)
        .ok_or(Error::HandshakeFailed)?;
    if acked.count() == 0 {
        opt.log("receiver accepted the offer");
    } else {
        opt.log(&format!(
            "receiver already holds {}/{} chunks; resuming",
            acked.count(),
            offer.chunk_count
        ));
    }

    Ok(Linked {
        session: Session {
            qr_version,
            dwell_ms,
            offer: offer.clone(),
        },
        acked,
    })
}

/// Number of probes that could plausibly have been received: those the sender
/// offered (inferred from the id it flagged `last`, when that frame arrived)
/// and that this terminal is large enough to display.
fn eligible_probe_count(cfg: LinkConfig, last_offered_id: Option<u8>) -> usize {
    PROBES
        .iter()
        .filter(|(id, qr_version, _)| {
            *qr_version <= cfg.max_qr_version
                && last_offered_id.is_none_or(|last_id| *id <= last_id)
        })
        .count()
}

pub fn run_recv_handshake(
    opt: &mut impl Optical,
    outdir: &Path,
    force: bool,
    cfg: LinkConfig,
    cache: &mut ReceiveCache,
) -> Result<Session> {
    let hello_deadline = Instant::now() + hello_timeout(cfg);
    let mut probes = Vec::new();
    let mut saw_last;
    // Id of the probe the sender flagged as its last. The sender only offers
    // probes its own display can draw, so this is how many were on the table.
    let mut last_offered_id = None;

    loop {
        // Only the deadline expiring becomes HandshakeTimeout. Real errors
        // (interrupt, camera failure, peer disconnect) must propagate as-is
        // so e.g. Ctrl-C during HELLO still surfaces as an interrupt.
        let Some(remaining) = hello_deadline.checked_duration_since(Instant::now()) else {
            return Err(Error::HandshakeTimeout);
        };

        opt.show(&Payload::Hello {
            protocol_ver: PROTOCOL_VERSION,
            role: ROLE_RECV,
        })?;

        let timeout = remaining.min(Duration::from_millis(50));
        match opt.poll(timeout)? {
            Some(Payload::Probe {
                id,
                qr_version,
                dwell_ms,
                last,
            }) => {
                probes.push((id, qr_version, dwell_ms));
                saw_last = last;
                if last {
                    last_offered_id = Some(id);
                }
                break;
            }
            Some(_) | None => {}
        }
    }

    if probes.is_empty() {
        return Err(Error::NoUsableProbe);
    }

    if !saw_last {
        let quiet = quiet_timeout(cfg);
        let mut quiet_deadline = Instant::now() + quiet;
        while !saw_last {
            let Some(remaining) = quiet_deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            match opt.poll(remaining)? {
                Some(Payload::Probe {
                    id,
                    qr_version,
                    dwell_ms,
                    last,
                }) => {
                    probes.push((id, qr_version, dwell_ms));
                    quiet_deadline = Instant::now() + quiet;
                    saw_last = last;
                    if last {
                        last_offered_id = Some(id);
                    }
                }
                Some(_) => {}
                None => break,
            }
        }
    }

    probes.retain(|(id, qr_version, _)| {
        *qr_version <= cfg.max_qr_version && PROBES.iter().any(|(probe_id, _, _)| id == probe_id)
    });
    let successful_count = probes
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<HashSet<_>>()
        .len();
    let (_, qr_version, probe_dwell_ms) = probes
        .iter()
        .copied()
        .max_by_key(|(_, qr_version, _)| *qr_version)
        .ok_or(Error::NoUsableProbe)?;
    // The receiver owns the dwell: it is the side that knows how often its
    // camera actually lands a decode, and LINK carries the number to the
    // sender.
    let dwell_ms = probe_dwell_ms
        .max(cfg.dwell_floor_ms)
        .max(opt.suggested_dwell_ms());

    // Loss is measured only against probes that could have counted: ones the
    // sender actually offered *and* this terminal is large enough to draw.
    // Charging the operator for probes nobody offered would report a
    // two-thirds loss on a flawless link and send them off adjusting lamps.
    let eligible_count = eligible_probe_count(cfg, last_offered_id);
    let probe_loss = (100 * eligible_count.saturating_sub(successful_count))
        .checked_div(eligible_count)
        .unwrap_or(0) as u8;

    // Lock the QR version as soon as it's chosen so LINK (and GO) are
    // encoded at the negotiated version rather than the probe default.
    opt.set_version(qr_version);
    opt.log(&format!(
        "sender found: best readable QR v{qr_version}, {probe_loss}% probe loss"
    ));

    opt.show(&Payload::Link {
        qr_version,
        dwell_ms,
        probe_loss,
    })?;

    let Some(offer) = poll_until(opt, go_timeout(cfg), |payload| match payload {
        Payload::Go(offer) => Some(offer),
        _ => None,
    })?
    else {
        return Err(Error::HandshakeFailed);
    };

    if !offer.is_consistent() {
        opt.log("refusing an offer whose transfer id does not match its contents");
        opt.show(&Payload::Fail {
            transfer_id: offer.transfer_id,
            reason: FAIL_PROTOCOL,
        })?;
        return Err(Error::HandshakeFailed);
    }
    if dest_exists(outdir, &offer.basename) && !force {
        opt.show(&Payload::Fail {
            transfer_id: offer.transfer_id,
            reason: FAIL_ABORTED,
        })?;
        return Err(Error::DestExists(outdir.join(&offer.basename)));
    }
    let Some(reuse) = cache.adopt(&offer) else {
        opt.show(&Payload::Fail {
            transfer_id: offer.transfer_id,
            reason: FAIL_DISK,
        })?;
        return Err(Error::Message(format!(
            "{} B is too large to receive on this machine",
            offer.compressed_size
        )));
    };
    opt.log(&match reuse {
        Reuse::Fresh => format!(
            "offer: {}, {} B compressed, {} chunks",
            offer.basename, offer.compressed_size, offer.chunk_count
        ),
        Reuse::Resumed { held } => format!(
            "offer matches the interrupted transfer: resuming with {held}/{} chunks",
            offer.chunk_count
        ),
        Reuse::Replaced { dropped } => format!(
            "offer: {} replaces an interrupted transfer ({dropped} chunks discarded)",
            offer.basename
        ),
    });

    let store = cache.store(&offer).ok_or(Error::HandshakeFailed)?;
    // Accepting is reporting progress, so a resumed sender skips what is
    // already here.
    opt.show(&progress_payload(
        &offer,
        store,
        progress_windows(qr_version),
    ))?;

    Ok(Session {
        qr_version,
        dwell_ms,
        offer,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::{pair, Optical};
    use crate::resume::{Offer, ReceiveCache};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct LastProbeOptical {
        poll_count: usize,
        link_shown: bool,
    }

    impl Optical for LastProbeOptical {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            if matches!(payload, Payload::Link { .. }) {
                self.link_shown = true;
            }
            Ok(())
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.poll_count += 1;
            match self.poll_count {
                1 => Ok(Some(Payload::Probe {
                    id: 0,
                    qr_version: 10,
                    dwell_ms: 250,
                    last: true,
                })),
                _ if self.link_shown => Ok(Some(Payload::Go(test_offer(1)))),
                _ => {
                    thread::sleep(timeout);
                    Ok(None)
                }
            }
        }
    }

    /// Wraps a channel end, recording every log line and payload shown.
    struct Gated<T> {
        inner: T,
        logs: Arc<Mutex<Vec<String>>>,
        shown: Arc<Mutex<Vec<Payload>>>,
    }

    impl<T: Optical> Optical for Gated<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown.lock().unwrap().push(payload.clone());
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.inner.set_version(version)
        }

        fn log(&mut self, line: &str) {
            self.logs.lock().unwrap().push(line.to_string());
        }
    }

    type Recorded<T> = Arc<Mutex<Vec<T>>>;

    fn gated<T>(inner: T) -> (Gated<T>, Recorded<String>, Recorded<Payload>) {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let shown = Arc::new(Mutex::new(Vec::new()));
        (
            Gated {
                inner,
                logs: Arc::clone(&logs),
                shown: Arc::clone(&shown),
            },
            logs,
            shown,
        )
    }

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let out = std::env::temp_dir().join(format!("ag-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        out
    }

    fn test_offer(size: u64) -> Offer {
        Offer::new("dir", 10, size, [7; 32], 1).unwrap()
    }

    fn fresh_recv(opt: &mut impl Optical, out: &Path, cfg: LinkConfig) -> Result<Session> {
        run_recv_handshake(opt, out, false, cfg, &mut ReceiveCache::default())
    }

    /// Replays a fixed script of payloads, then goes quiet.
    struct ScriptedPeer {
        script: std::collections::VecDeque<Payload>,
        shown: Vec<Payload>,
    }

    impl Optical for ScriptedPeer {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown.push(payload.clone());
            Ok(())
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            match self.script.pop_front() {
                Some(payload) => Ok(Some(payload)),
                None => {
                    thread::sleep(timeout);
                    Ok(None)
                }
            }
        }
    }

    /// A receiver whose camera is slow enough that the probe table's dwell
    /// would give each code only one look.
    struct SlowCamera<T> {
        inner: T,
        suggest_ms: u16,
    }

    impl<T: Optical> Optical for SlowCamera<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.inner.set_version(version)
        }

        fn suggested_dwell_ms(&mut self) -> u16 {
            self.suggest_ms
        }
    }

    #[test]
    fn a_slow_camera_gets_the_dwell_it_asks_for_on_both_sides() {
        let (mut send_opt, recv_end) = pair();
        let mut recv_opt = SlowCamera {
            inner: recv_end,
            suggest_ms: 700,
        };
        let cfg = LinkConfig::fast();

        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, &test_offer(3), cfg)
                .map(|linked| linked.session)
                .unwrap()
        });
        let recv_session = fresh_recv(&mut recv_opt, &scratch_dir("dwell"), cfg).unwrap();
        let send_session = send_t.join().unwrap();

        // The probe table would have said 150 ms for v40.
        assert_eq!(recv_session.dwell_ms, 700);
        assert_eq!(
            send_session.dwell_ms, 700,
            "LINK must carry the receiver's dwell to the sender"
        );
    }

    #[test]
    fn the_retry_floor_wins_when_it_is_higher_than_the_camera_suggests() {
        let (mut send_opt, recv_end) = pair();
        let mut recv_opt = SlowCamera {
            inner: recv_end,
            suggest_ms: 200,
        };
        let cfg = LinkConfig {
            fast: true,
            dwell_floor_ms: 900,
            ..LinkConfig::default()
        };

        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, &test_offer(3), cfg)
                .map(|linked| linked.session)
                .unwrap()
        });
        let recv_session = fresh_recv(&mut recv_opt, &scratch_dir("dwell-floor"), cfg).unwrap();
        send_t.join().unwrap();
        assert_eq!(recv_session.dwell_ms, 900);
    }

    #[test]
    fn handshake_does_not_stop_for_the_operator() {
        let (send_end, recv_end) = pair();
        let (mut send_opt, send_logs, _) = gated(send_end);
        let (mut recv_opt, recv_logs, _) = gated(recv_end);
        let cfg = LinkConfig::fast();

        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, &test_offer(3), cfg)
                .map(|linked| linked.session)
                .unwrap()
        });
        let recv_t =
            thread::spawn(move || fresh_recv(&mut recv_opt, &scratch_dir("gate-ok"), cfg).unwrap());
        send_t.join().unwrap();
        recv_t.join().unwrap();

        let send_logs = send_logs.lock().unwrap().clone();
        assert!(
            send_logs
                .iter()
                .any(|line| line.contains("receiver said HELLO")),
            "{send_logs:?}"
        );
        assert!(
            send_logs.iter().any(|line| line.contains("linked at QR v")),
            "{send_logs:?}"
        );

        let recv_logs = recv_logs.lock().unwrap().clone();
        assert!(
            recv_logs.iter().any(|line| line.contains("sender found")),
            "{recv_logs:?}"
        );
        assert!(
            recv_logs.iter().any(|line| line.contains("offer: dir")),
            "{recv_logs:?}"
        );
    }

    #[test]
    fn a_terminal_that_cannot_draw_large_codes_never_offers_or_picks_them() {
        let (send_end, recv_end) = pair();
        let cfg = LinkConfig {
            fast: true,
            max_qr_version: 15,
            ..LinkConfig::default()
        };
        let (mut send_opt, _, send_shown) = gated(send_end);
        let (mut recv_opt, recv_logs, _) = gated(recv_end);

        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, &test_offer(3), cfg)
                .map(|linked| linked.session)
                .unwrap()
        });
        let recv_session = fresh_recv(&mut recv_opt, &scratch_dir("gate-cap"), cfg).unwrap();
        let send_session = send_t.join().unwrap();

        assert_eq!(recv_session.qr_version, 15);
        assert_eq!(send_session.qr_version, 15);
        assert!(
            send_shown.lock().unwrap().iter().all(
                |payload| !matches!(payload, Payload::Probe { qr_version, .. } if *qr_version > 15)
            ),
            "probes above the display limit must never be offered"
        );
        // The four probes above v15 were never on the table, so they are not
        // losses: a clean capped link must report 0%, not 66%.
        let logs = recv_logs.lock().unwrap().clone();
        assert!(
            logs.iter().any(|line| line.contains("0% probe loss")),
            "{logs:?}"
        );
    }

    #[test]
    fn losing_an_offered_probe_still_counts_against_the_link() {
        // Two probes offered (cap v15), only the second arrives.
        let cfg = LinkConfig {
            fast: true,
            max_qr_version: 15,
            ..LinkConfig::default()
        };
        assert_eq!(eligible_probe_count(cfg, Some(1)), 2);

        // Capping at v15 alone does not shrink the denominator below what the
        // sender offered, so a probe that really was lost still shows up.
        assert_eq!(eligible_probe_count(cfg, None), 2);

        // Uncapped, with the sender's `last` flag lost: fall back to the full
        // probe ladder rather than silently shrinking the denominator.
        assert_eq!(
            eligible_probe_count(LinkConfig::default(), None),
            PROBES.len()
        );
    }

    #[test]
    fn gated_configs_wait_minutes_so_a_human_can_read_the_screen() {
        let cfg = LinkConfig::gated(15);
        assert!(cfg.confirm_offer);
        assert_eq!(hello_timeout(cfg), GATED_TIMEOUT);
        assert_eq!(link_timeout(cfg), GATED_TIMEOUT);
        assert_eq!(go_timeout(cfg), GATED_TIMEOUT);
        // The ungated live path keeps its original, tighter budgets.
        assert_eq!(
            hello_timeout(LinkConfig::default()),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn handshake_picks_highest_passing_probe() {
        let (mut send_opt, mut recv_opt) = pair();
        let cfg = LinkConfig::fast();
        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, &test_offer(5), cfg)
                .map(|linked| linked.session)
                .unwrap()
        });
        let recv_t = thread::spawn(move || {
            let out = std::env::temp_dir().join(format!("ag-hs-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            fresh_recv(&mut recv_opt, &out, cfg).unwrap()
        });
        let s = send_t.join().unwrap();
        let r = recv_t.join().unwrap();
        assert_eq!(s.qr_version, 40);
        assert_eq!(r.qr_version, 40);
        assert_eq!(s.offer.basename, "dir");
        assert_eq!(r.offer.chunk_count, 1);
    }

    #[test]
    fn handshake_no_probe_response_times_out() {
        // Keep the peer alive but silent: HELLO sends succeed and poll just
        // keeps timing out, so this must become HandshakeTimeout once the
        // deadline passes rather than erroring out early.
        let (_send_end, mut recv_end) = pair();
        let cfg = LinkConfig::fast();
        let out = std::env::temp_dir();
        let err = fresh_recv(&mut recv_end, &out, cfg).unwrap_err();
        assert!(matches!(err, crate::Error::HandshakeTimeout));
    }

    #[test]
    fn hello_loop_propagates_real_errors_instead_of_masking_as_timeout() {
        struct FailingPollOptical;

        impl Optical for FailingPollOptical {
            fn show(&mut self, _: &Payload) -> Result<()> {
                Ok(())
            }

            fn poll(&mut self, _: Duration) -> Result<Option<Payload>> {
                Err(Error::Message("boom".into()))
            }
        }

        let mut opt = FailingPollOptical;
        let err = run_recv_handshake(
            &mut opt,
            &std::env::temp_dir(),
            false,
            LinkConfig::fast(),
            &mut ReceiveCache::default(),
        )
        .unwrap_err();
        assert!(matches!(err, Error::Message(m) if m == "boom"));
    }

    #[test]
    fn hello_loop_propagates_disconnect_instead_of_masking_as_timeout() {
        let (send_end, mut recv_end) = pair();
        drop(send_end);
        let cfg = LinkConfig::fast();
        let out = std::env::temp_dir();
        let err = fresh_recv(&mut recv_end, &out, cfg).unwrap_err();
        assert!(matches!(err, crate::Error::Message(_)));
    }

    #[test]
    fn send_handshake_sets_version_before_go_is_shown() {
        struct GoVersionRecorder<T> {
            inner: T,
            version_set_before_go: Option<bool>,
            version_set: bool,
        }

        impl<T: Optical> Optical for GoVersionRecorder<T> {
            fn show(&mut self, payload: &Payload) -> Result<()> {
                if matches!(payload, Payload::Go(_)) && self.version_set_before_go.is_none() {
                    self.version_set_before_go = Some(self.version_set);
                }
                self.inner.show(payload)
            }

            fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
                self.inner.poll(timeout)
            }

            fn set_version(&mut self, _version: u8) {
                self.version_set = true;
            }
        }

        let (send_opt, mut recv_opt) = pair();
        let mut send_opt = GoVersionRecorder {
            inner: send_opt,
            version_set_before_go: None,
            version_set: false,
        };
        let cfg = LinkConfig::fast();

        let send_t = thread::spawn(move || {
            let result =
                run_send_handshake(&mut send_opt, &test_offer(5), cfg).map(|linked| linked.session);
            (result, send_opt.version_set_before_go)
        });
        let recv_t = thread::spawn(move || {
            let out = std::env::temp_dir().join(format!("ag-hs-ver-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            fresh_recv(&mut recv_opt, &out, cfg).unwrap()
        });

        let (send_result, version_set_before_go) = send_t.join().unwrap();
        send_result.unwrap();
        recv_t.join().unwrap();
        assert_eq!(version_set_before_go, Some(true));
    }

    #[test]
    fn recv_handshake_sets_version_before_link_is_shown() {
        struct LinkVersionRecorder<T> {
            inner: T,
            version_set_before_link: Option<bool>,
            version_set: bool,
        }

        impl<T: Optical> Optical for LinkVersionRecorder<T> {
            fn show(&mut self, payload: &Payload) -> Result<()> {
                if matches!(payload, Payload::Link { .. }) && self.version_set_before_link.is_none()
                {
                    self.version_set_before_link = Some(self.version_set);
                }
                self.inner.show(payload)
            }

            fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
                self.inner.poll(timeout)
            }

            fn set_version(&mut self, _version: u8) {
                self.version_set = true;
            }
        }

        let (mut send_opt, recv_opt) = pair();
        let mut recv_opt = LinkVersionRecorder {
            inner: recv_opt,
            version_set_before_link: None,
            version_set: false,
        };
        let cfg = LinkConfig::fast();

        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, &test_offer(5), cfg)
                .map(|linked| linked.session)
                .unwrap()
        });
        let recv_t = thread::spawn(move || {
            let out = std::env::temp_dir().join(format!("ag-hs-ver2-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            let result = fresh_recv(&mut recv_opt, &out, cfg);
            (result, recv_opt.version_set_before_link)
        });

        send_t.join().unwrap();
        let (recv_result, version_set_before_link) = recv_t.join().unwrap();
        recv_result.unwrap();
        assert_eq!(version_set_before_link, Some(true));
    }

    #[test]
    fn receiver_rejects_only_unknown_probe() {
        struct UnknownProbeOptical;

        impl Optical for UnknownProbeOptical {
            fn show(&mut self, _: &Payload) -> Result<()> {
                Ok(())
            }

            fn poll(&mut self, _: Duration) -> Result<Option<Payload>> {
                Ok(Some(Payload::Probe {
                    id: 99,
                    qr_version: 40,
                    dwell_ms: 150,
                    last: true,
                }))
            }
        }

        let mut opt = UnknownProbeOptical;
        let err = run_recv_handshake(
            &mut opt,
            &std::env::temp_dir(),
            false,
            LinkConfig::fast(),
            &mut ReceiveCache::default(),
        )
        .unwrap_err();

        assert!(matches!(err, Error::NoUsableProbe));
    }

    #[test]
    fn receiver_skips_quiet_wait_after_last_probe() {
        let mut opt = LastProbeOptical {
            poll_count: 0,
            link_shown: false,
        };
        let out = std::env::temp_dir().join(format!("ag-last-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();

        fresh_recv(&mut opt, &out, LinkConfig::fast()).unwrap();

        assert_eq!(opt.poll_count, 2);
    }

    struct OfferGate<T> {
        inner: T,
        prompts: Vec<String>,
        allow: bool,
        shown: Vec<Payload>,
    }

    impl<T: Optical> Optical for OfferGate<T> {
        fn show(&mut self, payload: &Payload) -> Result<()> {
            self.shown.push(payload.clone());
            self.inner.show(payload)
        }

        fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
            self.inner.poll(timeout)
        }

        fn set_version(&mut self, version: u8) {
            self.inner.set_version(version);
        }

        fn log(&mut self, _line: &str) {}

        fn gate(&mut self, prompt: &str) -> Result<bool> {
            self.prompts.push(prompt.to_string());
            Ok(self.allow)
        }
    }

    #[test]
    fn the_sender_confirms_once_before_go_not_before_each_probe() {
        let (send_end, recv_end) = pair();
        let mut send_opt = OfferGate {
            inner: send_end,
            prompts: Vec::new(),
            allow: true,
            shown: Vec::new(),
        };
        let mut recv_opt = recv_end;
        let cfg = LinkConfig {
            fast: true,
            confirm_offer: true,
            ..LinkConfig::default()
        };

        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, &test_offer(3), cfg)
                .map(|linked| (linked.session, send_opt.prompts, send_opt.shown))
        });
        fresh_recv(&mut recv_opt, &scratch_dir("confirm-go"), cfg).unwrap();
        let (_, prompts, shown) = send_t.join().unwrap().unwrap();

        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert!(prompts[0].contains("Enter sends"), "{prompts:?}");
        let go_at = shown
            .iter()
            .position(|payload| matches!(payload, Payload::Go(_)))
            .expect("GO must be shown after the operator confirms");
        assert!(
            shown
                .iter()
                .take(go_at)
                .all(|payload| matches!(payload, Payload::Probe { .. })),
            "only probes should precede GO, got {shown:?}"
        );
    }

    #[test]
    fn declining_the_offer_sends_no_go_at_all() {
        let (send_end, mut recv_end) = pair();
        let mut send_opt = OfferGate {
            inner: send_end,
            prompts: Vec::new(),
            allow: false,
            shown: Vec::new(),
        };
        let cfg = LinkConfig {
            fast: true,
            confirm_offer: true,
            ..LinkConfig::default()
        };
        let recv_t =
            thread::spawn(move || fresh_recv(&mut recv_end, &scratch_dir("decline-go"), cfg));
        let err = run_send_handshake(&mut send_opt, &test_offer(3), cfg).unwrap_err();
        let _ = recv_t.join();
        assert!(matches!(err, Error::Aborted), "{err:?}");
        assert!(
            send_opt
                .shown
                .iter()
                .all(|payload| matches!(payload, Payload::Probe { .. })),
            "a declined offer must not show GO, got {:?}",
            send_opt.shown
        );
    }

    fn held_cache(offer: &Offer, chunks: u32) -> ReceiveCache {
        let mut cache = ReceiveCache::default();
        cache.adopt(offer).unwrap();
        let bytes = vec![0u8; chunks as usize * crate::chunks::CHUNK_BYTES];
        cache
            .store(offer)
            .unwrap()
            .accept(0, chunks, &bytes)
            .unwrap();
        cache
    }

    #[test]
    fn a_matching_offer_resumes_with_the_chunks_already_held() {
        let offer = test_offer(500);
        let mut cache = held_cache(&offer, 2);
        let (mut send_end, mut recv_end) = pair();
        let cfg = LinkConfig::fast();
        let sent = offer.clone();
        let send_t = thread::spawn(move || run_send_handshake(&mut send_end, &sent, cfg).unwrap());
        let session = run_recv_handshake(
            &mut recv_end,
            &scratch_dir("resume-match"),
            false,
            cfg,
            &mut cache,
        )
        .unwrap();
        let linked = send_t.join().unwrap();
        assert_eq!(session.offer, offer);
        assert_eq!(
            linked.acked.count(),
            2,
            "the sender must learn what is already held"
        );
        assert!(linked.acked.contains(0) && linked.acked.contains(1));
        assert_eq!(cache.held(), 2);
    }

    #[test]
    fn an_offer_that_differs_in_any_field_starts_from_nothing() {
        let held = test_offer(500);
        for other in [
            Offer::new("dir", 10, 500, [8; 32], 1).unwrap(),
            Offer::new("dir", 10, 500, [7; 32], 2).unwrap(),
            Offer::new("other", 10, 500, [7; 32], 1).unwrap(),
            Offer::new("dir", 11, 500, [7; 32], 1).unwrap(),
        ] {
            let mut cache = held_cache(&held, 2);
            let (mut send_end, mut recv_end) = pair();
            let cfg = LinkConfig::fast();
            let sent = other.clone();
            let send_t =
                thread::spawn(move || run_send_handshake(&mut send_end, &sent, cfg).unwrap());
            run_recv_handshake(
                &mut recv_end,
                &scratch_dir("resume-other"),
                false,
                cfg,
                &mut cache,
            )
            .unwrap();
            let linked = send_t.join().unwrap();
            assert_eq!(linked.acked.count(), 0, "{other:?} must not inherit chunks");
            assert_eq!(cache.held(), 0);
        }
    }

    #[test]
    fn an_inconsistent_offer_is_refused_before_touching_the_cache() {
        let held = test_offer(500);
        let mut forged = test_offer(500);
        forged.transfer_id ^= 1;
        let mut cache = held_cache(&held, 2);
        let mut peer = ScriptedPeer {
            script: [
                Payload::Probe {
                    id: 0,
                    qr_version: 10,
                    dwell_ms: 250,
                    last: true,
                },
                Payload::Go(forged),
            ]
            .into(),
            shown: Vec::new(),
        };
        let err = run_recv_handshake(
            &mut peer,
            &scratch_dir("forged"),
            false,
            LinkConfig::fast(),
            &mut cache,
        )
        .unwrap_err();
        assert!(matches!(err, Error::HandshakeFailed), "{err:?}");
        assert!(peer.shown.iter().any(|p| matches!(
            p,
            Payload::Fail {
                reason: FAIL_PROTOCOL,
                ..
            }
        )));
        assert_eq!(cache.held(), 2, "the interrupted transfer is untouched");
    }
}
