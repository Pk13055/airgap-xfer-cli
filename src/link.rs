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

#[derive(Clone, Copy, Debug, Default)]
pub struct LinkConfig {
    /// Shortens HELLO/LINK/GO timeouts to 100/50/50 ms, quiet time to 20 ms,
    /// and skips probe dwell sleeps.
    pub fast: bool,
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
    if cfg.fast {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(30)
    }
}

fn link_timeout(cfg: LinkConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(5)
    }
}

fn go_timeout(cfg: LinkConfig) -> Duration {
    if cfg.fast {
        Duration::from_millis(50)
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

    for (id, qr_version, dwell_ms) in PROBES {
        opt.show(&Payload::Probe {
            id,
            qr_version,
            dwell_ms,
            last: id == 5,
        })?;
        if !cfg.fast {
            thread::sleep(Duration::from_millis(dwell_ms.into()));
        }
    }

    let Some((qr_version, dwell_ms)) =
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

    // Lock the QR version before GO is shown so GO (and everything after
    // it) is encoded at the negotiated version instead of the probe default.
    opt.set_version(qr_version);

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

pub fn run_recv_handshake(
    opt: &mut impl Optical,
    outdir: &Path,
    force: bool,
    cfg: LinkConfig,
) -> Result<Session> {
    let hello_deadline = Instant::now() + hello_timeout(cfg);
    let mut probes = Vec::new();
    let mut saw_last;

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
                }
                Some(_) => {}
                None => break,
            }
        }
    }

    probes.retain(|(id, _, _)| PROBES.iter().any(|(probe_id, _, _)| id == probe_id));
    let successful_count = probes
        .iter()
        .map(|(id, _, _)| *id)
        .collect::<HashSet<_>>()
        .len();
    let (_, qr_version, dwell_ms) = probes
        .iter()
        .copied()
        .max_by_key(|(_, qr_version, _)| *qr_version)
        .ok_or(Error::NoUsableProbe)?;
    let probe_loss = (100 * (6 - successful_count) / 6) as u8;

    // Lock the QR version as soon as it's chosen so LINK (and GO) are
    // encoded at the negotiated version rather than the probe default.
    opt.set_version(qr_version);

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

    #[test]
    fn handshake_picks_highest_passing_probe() {
        let (mut send_opt, mut recv_opt) = pair();
        let cfg = LinkConfig { fast: true };
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
        let cfg = LinkConfig { fast: true };
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
            LinkConfig { fast: true },
        )
        .unwrap_err();
        assert!(matches!(err, Error::Message(m) if m == "boom"));
    }

    #[test]
    fn hello_loop_propagates_disconnect_instead_of_masking_as_timeout() {
        let (send_end, mut recv_end) = pair();
        drop(send_end);
        let cfg = LinkConfig { fast: true };
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
        let cfg = LinkConfig { fast: true };
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
        let cfg = LinkConfig { fast: true };
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
            LinkConfig { fast: true },
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

        run_recv_handshake(&mut opt, &out, false, LinkConfig { fast: true }).unwrap();

        assert_eq!(opt.poll_count, 2);
    }
}
