use std::{
    sync::{mpsc::Sender, Arc},
    thread,
    time::{Duration, Instant},
};

use image::GrayImage;
use ratatui::layout::Rect;

use crate::{
    frame::{Payload, PROTOCOL_VERSION},
    live::{check_interrupted, encode_version_for, is_interrupted},
    optical::Optical,
    qr,
    resume::UNSCOPED,
    tui::{
        camera::CameraFeed,
        layout::{self, ScreenSize, Tiles},
    },
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
        images: Vec<Arc<GrayImage>>,
        invert: bool,
        label: String,
        /// After the link: the side pane becomes the peer monitor and the
        /// codes are tiled.
        linked: bool,
        /// The locked QR version, which sizes the tile grid when no image is
        /// on screen yet.
        version: u8,
        drawn: Sender<()>,
    },
    Status(String),
    Log(String),
    /// Hold here until the operator answers on `reply`.
    ///
    /// When `require_lock` is set, Enter is ignored until the camera is
    /// tracking the peer's screen. When `auto_on_lock` is also set, lock
    /// itself is the answer — used by the receiver so there is no extra Enter.
    Gate {
        prompt: String,
        reply: Sender<GateReply>,
        require_lock: bool,
        auto_on_lock: bool,
    },
    /// The protocol finished; `summary` is the line to leave on screen.
    Finished {
        summary: String,
    },
}

/// The [`Optical`] channel the protocol runs against when driven by the TUI:
/// it renders through the UI thread and reads from a shared camera feed, and
/// never touches the terminal itself.
pub struct TuiOptical {
    camera: Arc<CameraFeed>,
    events: Sender<UiEvent>,
    version: u8,
    invert: bool,
    /// Set once the link is up.
    linked: bool,
    /// The area the UI last drew, for the tile count.
    screen: Arc<ScreenSize>,
    tiles: Tiles,
}

impl TuiOptical {
    pub fn new(
        camera: Arc<CameraFeed>,
        events: Sender<UiEvent>,
        invert: bool,
        screen: Arc<ScreenSize>,
        tiles: Tiles,
    ) -> Self {
        Self {
            camera,
            events,
            version: 10,
            invert,
            linked: false,
            screen,
            tiles,
        }
    }

    fn send(&self, event: UiEvent) -> Result<()> {
        self.events.send(event).map_err(|_| display_closed())
    }

    fn gate_inner(&mut self, prompt: &str, require_lock: bool, auto_on_lock: bool) -> Result<bool> {
        check_interrupted()?;
        let (reply, reply_rx) = std::sync::mpsc::channel();
        self.send(UiEvent::Gate {
            prompt: prompt.to_string(),
            reply,
            require_lock,
            auto_on_lock,
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
        self.paint_images(vec![image], payload_label(payload))
    }

    fn paint_images(&mut self, images: Vec<Arc<GrayImage>>, label: String) -> Result<()> {
        check_interrupted()?;
        let (drawn, drawn_rx) = std::sync::mpsc::channel();
        self.send(UiEvent::Show {
            images,
            invert: self.invert,
            label,
            linked: self.linked,
            version: self.version,
            drawn,
        })?;
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
        Payload::Go(offer) => format!("GO {}", offer.basename),
        Payload::Progress { contiguous, .. } => format!("PROGRESS @{contiguous}"),
        Payload::Data { first, count, .. } if *count > 1 => {
            format!(
                "DATA #{first}-#{}",
                first.saturating_add(u32::from(*count) - 1)
            )
        }
        Payload::Data { first, .. } => format!("DATA #{first}"),
        Payload::Fin { .. } => "FIN".into(),
        Payload::Ok { .. } => "OK".into(),
        Payload::Fail { reason, .. } => format!("FAIL {reason}"),
    }
}

impl Optical for TuiOptical {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        match self.paint(payload) {
            Ok(()) => Ok(()),
            Err(err) => {
                if !matches!(payload, Payload::Fail { .. }) {
                    let _ = self.paint(&Payload::Fail {
                        transfer_id: UNSCOPED,
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
        self.linked = true;
        self.camera.set_linked(true);
        // Only the receiver watches a peer that tiles DATA. The sender watches
        // one progress code, which tracked single-code decoding reads far
        // more cheaply than a full-frame multi-code scan every frame.
        self.camera.set_multi(self.tiles == Tiles::One);
    }

    fn log(&mut self, line: &str) {
        let _ = self.send(UiEvent::Log(line.to_string()));
    }

    fn suggested_dwell_ms(&mut self) -> u16 {
        let Some(gap) = self.camera.decode_gap_ms() else {
            return 0;
        };
        (gap * DECODE_ATTEMPTS_PER_CODE).clamp(MIN_SUGGESTED_DWELL_MS, MAX_SUGGESTED_DWELL_MS)
            as u16
    }

    fn gate(&mut self, prompt: &str) -> Result<bool> {
        self.gate_inner(prompt, false, false)
    }

    fn gate_locked(&mut self, prompt: &str) -> Result<bool> {
        self.gate_inner(prompt, true, false)
    }

    fn wait_locked(&mut self, prompt: &str) -> Result<bool> {
        self.gate_inner(prompt, true, true)
    }

    fn tile_count(&self) -> usize {
        if !self.linked {
            return 1;
        }
        let area = self.screen.get().or_else(|| {
            crossterm::terminal::size()
                .ok()
                .map(|(cols, rows)| Rect::new(0, 0, cols, rows))
        });
        area.map_or(1, |area| layout::data_tiles(area, self.tiles, self.version))
    }

    fn show_many(&mut self, payloads: &[Payload]) -> Result<()> {
        if payloads.len() <= 1 {
            return payloads
                .first()
                .map(|payload| self.show(payload))
                .unwrap_or(Ok(()));
        }
        let mut images = Vec::with_capacity(payloads.len());
        let mut labels = Vec::new();
        for payload in payloads {
            let bytes = crate::frame::encode(payload)?;
            let version = encode_version_for(payload, self.version);
            images.push(Arc::new(qr::encode_version(&bytes, version)?));
            labels.push(payload_label(payload));
        }
        let label = match labels.as_slice() {
            [one] => one.clone(),
            [first, .., last] => format!("{first}–{last}"),
            [] => String::new(),
        };
        match self.paint_images(images, label) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = self.paint(&Payload::Fail {
                    transfer_id: UNSCOPED,
                    reason: crate::frame::FAIL_PROTOCOL,
                });
                Err(err)
            }
        }
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        check_interrupted()?;
        let deadline = Instant::now() + timeout;
        loop {
            check_interrupted()?;
            if let Some(err) = self.camera.failure() {
                return Err(Error::Camera(err));
            }
            if let Some(peer) = self.camera.foreign_version() {
                return Err(Error::ProtocolMismatch {
                    peer,
                    ours: PROTOCOL_VERSION,
                });
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
        assert_eq!(payload_label(&Payload::Ok { transfer_id: 1 }), "OK");
        assert_eq!(
            payload_label(&Payload::Probe {
                id: 2,
                qr_version: 20,
                dwell_ms: 200,
                last: false
            }),
            "PROBE 2 (v20)"
        );
        let data = |count| Payload::Data {
            transfer_id: 1,
            first: 7,
            count,
            bytes: vec![],
        };
        assert_eq!(payload_label(&data(1)), "DATA #7");
        assert_eq!(payload_label(&data(3)), "DATA #7-#9");
    }
}
