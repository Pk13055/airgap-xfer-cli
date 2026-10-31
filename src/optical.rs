use crate::{
    frame::{decode, encode, Payload},
    Error, Result,
};
use std::{
    collections::{HashSet, VecDeque},
    sync::mpsc::{self, Receiver, Sender},
    time::Duration,
};

pub trait Optical {
    fn show(&mut self, payload: &Payload) -> Result<()>;
    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>>;

    /// Updates the status line shown under the QR code (e.g. window
    /// progress, holes, throughput). No-op by default.
    fn set_status(&mut self, _status: &str) {}

    /// Sets the QR version used to encode non-probe payloads. No-op by
    /// default.
    fn set_version(&mut self, _version: u8) {}

    /// Appends a line to the operator-visible transcript of the transfer.
    /// No-op by default.
    fn log(&mut self, _line: &str) {}

    /// How long this side needs each code held on screen, in milliseconds,
    /// based on how often its camera actually lands a decode.
    ///
    /// The probe table's dwells assume a camera that resolves a code within a
    /// couple of frames. A real webcam decoding 1080p does far worse, and a
    /// code that goes by in fewer frames than that is simply missed — which
    /// arrives as a hole in the window rather than as anything diagnosable.
    /// Zero means "no opinion".
    fn suggested_dwell_ms(&mut self) -> u16 {
        0
    }

    /// Blocks until the operator confirms the upcoming phase described by
    /// `prompt`, returning `Ok(true)` to proceed and `Ok(false)` to abort.
    ///
    /// Used for the sender's one "send the file" confirmation after LINK,
    /// immediately before GO. Aiming uses [`Optical::gate_locked`] or
    /// [`Optical::wait_locked`].
    fn gate(&mut self, _prompt: &str) -> Result<bool> {
        Ok(true)
    }

    /// Like [`Optical::gate`], but the UI ignores Enter until the camera is
    /// tracking the peer. Default is an ordinary gate, which in-process tests
    /// auto-accept.
    fn gate_locked(&mut self, prompt: &str) -> Result<bool> {
        self.gate(prompt)
    }

    /// Blocks until the camera is tracking the peer, then proceeds without
    /// Enter. Receiver aiming uses this so there is no "Enter to receive"
    /// after the sender has already started. Default auto-accepts.
    fn wait_locked(&mut self, prompt: &str) -> Result<bool> {
        self.gate(prompt)
    }

    /// How many DATA frames this side can paint in one dwell. Handshake
    /// frames are always a single code. Default 1.
    fn tile_count(&self) -> usize {
        1
    }

    /// Paint `payloads` together for one dwell. Default shows them in
    /// sequence; the TUI draws a grid and waits once.
    fn show_many(&mut self, payloads: &[Payload]) -> Result<()> {
        for payload in payloads {
            self.show(payload)?;
        }
        Ok(())
    }
}

pub struct PairEnd {
    sender: Sender<Vec<u8>>,
    receiver: Receiver<Vec<u8>>,
}

pub fn pair() -> (PairEnd, PairEnd) {
    let (a_to_b_sender, a_to_b_receiver) = mpsc::channel();
    let (b_to_a_sender, b_to_a_receiver) = mpsc::channel();

    (
        PairEnd {
            sender: a_to_b_sender,
            receiver: b_to_a_receiver,
        },
        PairEnd {
            sender: b_to_a_sender,
            receiver: a_to_b_receiver,
        },
    )
}

impl Optical for PairEnd {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        let frame = encode(payload)?;
        self.sender
            .send(frame)
            .map_err(|_| Error::Message("optical peer disconnected".into()))
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        match self.receiver.recv_timeout(timeout) {
            Ok(frame) => decode(&frame).map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(Error::Message("optical peer disconnected".into()))
            }
        }
    }
}

pub struct Lossy<T> {
    pub inner: T,
    /// DATA spans to lose once each, by first chunk.
    pub drop_first: HashSet<u32>,
}

impl<T: Optical> Optical for Lossy<T> {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        self.inner.show(payload)
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        let payload = self.inner.poll(timeout)?;
        if let Some(Payload::Data { first, .. }) = payload.as_ref() {
            if self.drop_first.remove(first) {
                return Ok(None);
            }
        }
        Ok(payload)
    }
}

/// Hands back payloads one protocol layer read but the next one must see.
///
/// The transport stops as soon as it sees the peer restart the handshake,
/// and the frame that told it so — a HELLO, PROBE or LINK — is still the one
/// on the peer's screen. A live camera reports an unchanged code only once,
/// so if that read were dropped the next handshake would wait for a HELLO
/// that never arrives "again".
pub struct Replay<'a, T> {
    inner: &'a mut T,
    pending: VecDeque<Payload>,
}

impl<'a, T: Optical> Replay<'a, T> {
    pub fn new(inner: &'a mut T) -> Self {
        Self {
            inner,
            pending: VecDeque::new(),
        }
    }

    /// Queues `payload` to be the next thing [`Optical::poll`] returns.
    pub fn push_back(&mut self, payload: Payload) {
        self.pending.push_back(payload);
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

impl<T: Optical> Optical for Replay<'_, T> {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        self.inner.show(payload)
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        match self.pending.pop_front() {
            Some(payload) => Ok(Some(payload)),
            None => self.inner.poll(timeout),
        }
    }

    fn set_status(&mut self, status: &str) {
        self.inner.set_status(status)
    }

    fn set_version(&mut self, version: u8) {
        self.inner.set_version(version)
    }

    fn log(&mut self, line: &str) {
        self.inner.log(line)
    }

    fn suggested_dwell_ms(&mut self) -> u16 {
        self.inner.suggested_dwell_ms()
    }

    fn gate(&mut self, prompt: &str) -> Result<bool> {
        self.inner.gate(prompt)
    }

    fn gate_locked(&mut self, prompt: &str) -> Result<bool> {
        self.inner.gate_locked(prompt)
    }

    fn wait_locked(&mut self, prompt: &str) -> Result<bool> {
        self.inner.wait_locked(prompt)
    }

    fn tile_count(&self) -> usize {
        self.inner.tile_count()
    }

    fn show_many(&mut self, payloads: &[Payload]) -> Result<()> {
        self.inner.show_many(payloads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::Payload;
    use std::time::Duration;

    #[test]
    fn pair_delivers_hello() {
        let (mut a, mut b) = pair();
        a.show(&Payload::Hello {
            protocol_ver: crate::frame::PROTOCOL_VERSION,
            role: 2,
        })
        .unwrap();
        match b.poll(Duration::from_millis(1)).unwrap() {
            Some(Payload::Hello { role, .. }) => assert_eq!(role, 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lossy_drops_configured_span() {
        let (mut send, recv) = pair();
        let mut recv = Lossy {
            inner: recv,
            drop_first: [3].into_iter().collect(),
        };
        send.show(&Payload::Data {
            transfer_id: 1,
            first: 3,
            count: 1,
            bytes: vec![1],
        })
        .unwrap();
        assert!(recv.poll(Duration::from_millis(1)).unwrap().is_none());
        send.show(&Payload::Data {
            transfer_id: 1,
            first: 4,
            count: 1,
            bytes: vec![2],
        })
        .unwrap();
        match recv.poll(Duration::from_millis(1)).unwrap() {
            Some(Payload::Data { first, .. }) => assert_eq!(first, 4),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn replay_hands_back_what_was_pushed_before_anything_new() {
        let (mut a, mut b) = pair();
        b.show(&Payload::Ok { transfer_id: 2 }).unwrap();
        let mut replay = Replay::new(&mut a);
        replay.push_back(Payload::Hello {
            protocol_ver: crate::frame::PROTOCOL_VERSION,
            role: 2,
        });
        assert_eq!(replay.pending(), 1);
        assert!(matches!(
            replay.poll(Duration::from_millis(1)).unwrap(),
            Some(Payload::Hello { .. })
        ));
        assert_eq!(
            replay.poll(Duration::from_millis(1)).unwrap(),
            Some(Payload::Ok { transfer_id: 2 })
        );
    }
}
