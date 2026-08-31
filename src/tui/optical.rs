use std::{
    sync::{
        mpsc::Sender,
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use image::GrayImage;

use crate::{
    frame::Payload,
    live::{check_interrupted, encode_version_for, is_interrupted},
    optical::Optical,
    qr,
    tui::camera::CameraFeed,
    Error, Result,
};

/// How long the protocol thread sleeps between checks of the camera's latest
/// decode. Short enough that a 150 ms probe dwell is never missed.
const POLL_TICK: Duration = Duration::from_millis(5);
/// Decode attempts each displayed code should get. One would mean every missed
/// frame is a lost chunk; three leaves room for glare, focus hunting, and the
/// occasional dropped frame.
const DECODE_ATTEMPTS_PER_CODE: u64 = 3;
/// Bounds on the dwell we will ask the sender for.
const MIN_SUGGESTED_DWELL_MS: u64 = 120;
const MAX_SUGGESTED_DWELL_MS: u64 = 1200;

/// What the operator chose at a turn boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateReply {
    Proceed,
    Abort,
}

/// A message from the protocol thread to the UI thread.
pub enum UiEvent {
    /// Display `image` and acknowledge on `drawn` once it is actually on
    /// screen. The protocol sleeps for the negotiated dwell after `show`
    /// returns, so that sleep must not start before the peer could see it.
    Show {
        image: Arc<GrayImage>,
        invert: bool,
        label: String,
        drawn: Sender<()>,
    },
    Status(String),
    Log(String),
    /// Hold here until the operator answers on `reply`.
    ///
    /// When `require_lock` is set, Enter is ignored until the camera is
    /// tracking the peer's screen — that is the only turn-based step.
    Gate {
        prompt: String,
        reply: Sender<GateReply>,
        require_lock: bool,
    },
    /// The protocol finished; `summary` is the line to leave on screen.
    Finished { summary: String },
}

/// The [`Optical`] channel the protocol runs against when driven by the TUI:
/// it renders through the UI thread and reads from a shared camera feed, and
/// never touches the terminal itself.
pub struct TuiOptical {
    camera: Arc<CameraFeed>,
    events: Sender<UiEvent>,
    version: u8,
    invert: bool,
}

impl TuiOptical {
    pub fn new(camera: Arc<CameraFeed>, events: Sender<UiEvent>, invert: bool) -> Self {
        Self {
            camera,
            events,
            version: 10,
            invert,
        }
    }

    fn send(&self, event: UiEvent) -> Result<()> {
        self.events
            .send(event)
            .map_err(|_| display_closed())
    }

    fn gate_inner(&mut self, prompt: &str, require_lock: bool) -> Result<bool> {
        check_interrupted()?;
        let (reply, reply_rx) = std::sync::mpsc::channel();
        self.send(UiEvent::Gate {
            prompt: prompt.to_string(),
            reply,
            require_lock,
        })?;

        loop {
            check_interrupted()?;
            match reply_rx.recv_timeout(POLL_TICK) {
                Ok(GateReply::Proceed) => return Ok(true),
                Ok(GateReply::Abort) => return Ok(false),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(display_closed())
                }
            }
        }
    }

    fn paint(&mut self, payload: &Payload) -> Result<()> {
        check_interrupted()?;

        let bytes = crate::frame::encode(payload)?;
        let version = encode_version_for(payload, self.version);
        let image = Arc::new(qr::encode_version(&bytes, version)?);

        let (drawn, drawn_rx) = std::sync::mpsc::channel();
        self.send(UiEvent::Show {
            image,
            invert: self.invert,
            label: payload_label(payload),
            drawn,
        })?;
        // Block until the UI confirms the paint, polling the interrupt flag
        // so Ctrl-C during a long dwell still unwinds. If the UI is gone the
        // transfer is over anyway.
        loop {
            check_interrupted()?;
            match drawn_rx.recv_timeout(POLL_TICK) {
                Ok(()) => return Ok(()),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(display_closed())
                }
            }
        }
    }
}

/// The error to report when the UI side of a channel goes away.
///
/// The UI drops its queue as soon as the operator quits, which usually beats
/// the protocol thread's next interrupt check by a few milliseconds. Reporting
/// a plumbing failure there would mask a deliberate Ctrl-C or `q`.
fn display_closed() -> Error {
    if is_interrupted() {
        Error::Interrupted
    } else {
        Error::Message("display closed".into())
    }
}

/// A short human label for the payload currently on screen.
pub fn payload_label(payload: &Payload) -> String {
    match payload {
        Payload::Hello { .. } => "HELLO".into(),
        Payload::Probe { id, qr_version, .. } => format!("PROBE {id} (v{qr_version})"),
        Payload::Link { qr_version, .. } => format!("LINK (v{qr_version})"),
        Payload::Go { basename, .. } => format!("GO {basename}"),
        Payload::Ack { window_base, .. } => format!("ACK @{window_base}"),
        Payload::Data { seq, .. } => format!("DATA #{seq}"),
        Payload::Fin { .. } => "FIN".into(),
        Payload::Ok => "OK".into(),
        Payload::Fail { reason } => format!("FAIL {reason}"),
    }
}

impl Optical for TuiOptical {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        match self.paint(payload) {
            Ok(()) => Ok(()),
            Err(err) => {
                if !matches!(payload, Payload::Fail { .. }) {
                    let _ = self.paint(&Payload::Fail {
                        reason: crate::frame::FAIL_PROTOCOL,
                    });
                }
                Err(err)
            }
        }
    }

    fn set_status(&mut self, status: &str) {
        let _ = self.send(UiEvent::Status(status.to_string()));
    }

    fn set_version(&mut self, version: u8) {
        self.version = version;
    }

    fn log(&mut self, line: &str) {
        let _ = self.send(UiEvent::Log(line.to_string()));
    }

    fn suggested_dwell_ms(&mut self) -> u16 {
        let Some(gap) = self.camera.decode_gap_ms() else {
            return 0;
        };
        (gap * DECODE_ATTEMPTS_PER_CODE)
            .clamp(MIN_SUGGESTED_DWELL_MS, MAX_SUGGESTED_DWELL_MS) as u16
    }

    fn gate(&mut self, prompt: &str) -> Result<bool> {
        self.gate_inner(prompt, false)
    }

    fn gate_locked(&mut self, prompt: &str) -> Result<bool> {
        self.gate_inner(prompt, true)
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        check_interrupted()?;
        let deadline = Instant::now() + timeout;
        loop {
            check_interrupted()?;
            if let Some(err) = self.camera.failure() {
                return Err(Error::Camera(err));
            }
            if let Some(payload) = self.camera.take_next() {
                return Ok(Some(payload));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(POLL_TICK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_name_the_frame_kind() {
        assert_eq!(payload_label(&Payload::Ok), "OK");
        assert_eq!(
            payload_label(&Payload::Probe {
                id: 2,
                qr_version: 20,
                dwell_ms: 200,
                last: false
            }),
            "PROBE 2 (v20)"
        );
        assert_eq!(
            payload_label(&Payload::Data {
                seq: 7,
                chunk: vec![]
            }),
            "DATA #7"
        );
    }
}
