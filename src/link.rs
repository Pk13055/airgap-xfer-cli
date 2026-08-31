use std::{
    collections::HashSet,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use crate::{
    frame::{data_chunk_size, Payload, FAIL_ABORTED, ROLE_RECV},
    optical::Optical,
    pack::dest_exists,
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
    /// and QR versions capped to what this terminal can draw. Handshake phases
    /// themselves do not wait for Enter.
    pub fn gated(max_qr_version: u8) -> Self {
        Self {
            patient: true,
            max_qr_version,
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
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub qr_version: u8,
    pub dwell_ms: u16,
    pub basename: String,
    pub uncompressed_hint: u64,
    pub compressed_size: u64,
    pub chunk_count: u32,
    pub sha256: [u8; 32],
}

fn hello_timeout(cfg: LinkConfig) -> Duration {
    // `fast` is the test knob and wins: patient timeouts are minutes long
    // because a human (or a peer working through its own retry) is in the
    // loop, which no test wants to wait for.
    if cfg.fast {
        Duration::from_millis(100)
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

pub fn run_send_handshake(
    opt: &mut impl Optical,
    basename: String,
    uncompressed_hint: u64,
    blob: &[u8],
    sha256: [u8; 32],
    cfg: LinkConfig,
) -> Result<Session> {
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
    opt.log(&format!(
        "linked at QR v{qr_version}, {dwell_ms} ms dwell"
    ));

    let chunk_size = data_chunk_size(qr_version);
    if chunk_size == 0 {
        return Err(Error::HandshakeFailed);
    }
    let chunk_count = blob.len().div_ceil(chunk_size).max(1) as u32;
    let compressed_size = blob.len() as u64;
    opt.show(&Payload::Go {
        basename: basename.clone(),
        uncompressed_hint,
        compressed_size,
        chunk_count,
        sha256,
    })?;

    let ack = poll_until(opt, go_timeout(cfg), |payload| match payload {
        Payload::Ack { window_base: 0, .. } => Some(()),
        _ => None,
    })?;
    if ack.is_none() {
        return Err(Error::HandshakeFailed);
    }
    opt.log("receiver accepted the offer");

    Ok(Session {
        qr_version,
        dwell_ms,
        basename,
        uncompressed_hint,
        compressed_size,
        chunk_count,
        sha256,
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
            protocol_ver: 1,
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
        *qr_version <= cfg.max_qr_version
            && PROBES.iter().any(|(probe_id, _, _)| id == probe_id)
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

    let Some((basename, uncompressed_hint, compressed_size, chunk_count, sha256)) =
        poll_until(opt, go_timeout(cfg), |payload| match payload {
            Payload::Go {
                basename,
                uncompressed_hint,
                compressed_size,
                chunk_count,
                sha256,
            } => Some((
                basename,
                uncompressed_hint,
                compressed_size,
                chunk_count,
                sha256,
            )),
            _ => None,
        })?
    else {
        return Err(Error::HandshakeFailed);
    };

    if dest_exists(outdir, &basename) && !force {
        opt.show(&Payload::Fail {
            reason: FAIL_ABORTED,
        })?;
        return Err(Error::DestExists(outdir.join(basename)));
    }

    opt.log(&format!(
        "offer: {basename}, {compressed_size} B compressed, {chunk_count} chunks"
    ));

    opt.show(&Payload::Ack {
        window_base: 0,
        bitmap: 0,
    })?;

    Ok(Session {
        qr_version,
        dwell_ms,
        basename,
        uncompressed_hint,
        compressed_size,
        chunk_count,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optical::{pair, Optical};
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
                _ if self.link_shown => Ok(Some(Payload::Go {
                    basename: "file".into(),
                    uncompressed_hint: 0,
                    compressed_size: 1,
                    chunk_count: 1,
                    sha256: [0; 32],
                })),
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
            run_send_handshake(&mut send_opt, "dir".into(), 10, &[1u8, 2, 3], [7; 32], cfg).unwrap()
        });
        let recv_session =
            run_recv_handshake(&mut recv_opt, &scratch_dir("dwell"), false, cfg).unwrap();
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
            run_send_handshake(&mut send_opt, "dir".into(), 10, &[1u8, 2, 3], [7; 32], cfg).unwrap()
        });
        let recv_session =
            run_recv_handshake(&mut recv_opt, &scratch_dir("dwell-floor"), false, cfg).unwrap();
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
            run_send_handshake(&mut send_opt, "dir".into(), 10, &[1u8, 2, 3], [7; 32], cfg).unwrap()
        });
        let recv_t = thread::spawn(move || {
            run_recv_handshake(&mut recv_opt, &scratch_dir("gate-ok"), false, cfg).unwrap()
        });
        send_t.join().unwrap();
        recv_t.join().unwrap();

        let send_logs = send_logs.lock().unwrap().clone();
        assert!(
            send_logs.iter().any(|line| line.contains("receiver said HELLO")),
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
            run_send_handshake(&mut send_opt, "dir".into(), 10, &[1u8, 2, 3], [7; 32], cfg).unwrap()
        });
        let recv_session =
            run_recv_handshake(&mut recv_opt, &scratch_dir("gate-cap"), false, cfg).unwrap();
        let send_session = send_t.join().unwrap();

        assert_eq!(recv_session.qr_version, 15);
        assert_eq!(send_session.qr_version, 15);
        assert!(
            send_shown
                .lock()
                .unwrap()
                .iter()
                .all(|payload| !matches!(payload, Payload::Probe { qr_version, .. } if *qr_version > 15)),
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
        assert_eq!(hello_timeout(cfg), GATED_TIMEOUT);
        assert_eq!(link_timeout(cfg), GATED_TIMEOUT);
        assert_eq!(go_timeout(cfg), GATED_TIMEOUT);
        // The ungated live path keeps its original, tighter budgets.
        assert_eq!(hello_timeout(LinkConfig::default()), Duration::from_secs(30));
    }

    #[test]
    fn handshake_picks_highest_passing_probe() {
        let (mut send_opt, mut recv_opt) = pair();
        let cfg = LinkConfig::fast();
        let blob = vec![1u8, 2, 3, 4, 5];
        let sha256 = [1u8; 32];
        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, "dir".into(), 10, &blob, sha256, cfg).unwrap()
        });
        let recv_t = thread::spawn(move || {
            let out = std::env::temp_dir().join(format!("ag-hs-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            run_recv_handshake(&mut recv_opt, &out, false, cfg).unwrap()
        });
        let s = send_t.join().unwrap();
        let r = recv_t.join().unwrap();
        assert_eq!(s.qr_version, 40);
        assert_eq!(r.qr_version, 40);
        assert_eq!(s.basename, "dir");
        assert_eq!(r.chunk_count, 1);
    }

    #[test]
    fn handshake_no_probe_response_times_out() {
        // Keep the peer alive but silent: HELLO sends succeed and poll just
        // keeps timing out, so this must become HandshakeTimeout once the
        // deadline passes rather than erroring out early.
        let (_send_end, mut recv_end) = pair();
        let cfg = LinkConfig::fast();
        let out = std::env::temp_dir();
        let err = run_recv_handshake(&mut recv_end, &out, false, cfg).unwrap_err();
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
        let err = run_recv_handshake(&mut recv_end, &out, false, cfg).unwrap_err();
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
                if matches!(payload, Payload::Go { .. }) && self.version_set_before_go.is_none() {
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
        let blob = vec![1u8, 2, 3, 4, 5];
        let sha256 = [1u8; 32];

        let send_t = thread::spawn(move || {
            let result =
                run_send_handshake(&mut send_opt, "dir".into(), 5, &blob, sha256, cfg);
            (result, send_opt.version_set_before_go)
        });
        let recv_t = thread::spawn(move || {
            let out = std::env::temp_dir().join(format!("ag-hs-ver-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            run_recv_handshake(&mut recv_opt, &out, false, cfg).unwrap()
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
        let blob = vec![1u8, 2, 3, 4, 5];
        let sha256 = [1u8; 32];

        let send_t = thread::spawn(move || {
            run_send_handshake(&mut send_opt, "dir".into(), 5, &blob, sha256, cfg).unwrap()
        });
        let recv_t = thread::spawn(move || {
            let out = std::env::temp_dir().join(format!("ag-hs-ver2-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&out);
            std::fs::create_dir_all(&out).unwrap();
            let result = run_recv_handshake(&mut recv_opt, &out, false, cfg);
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

        run_recv_handshake(&mut opt, &out, false, LinkConfig::fast()).unwrap();

        assert_eq!(opt.poll_count, 2);
    }
}
