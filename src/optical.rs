use crate::{
    frame::{decode, encode, Payload},
    Error, Result,
};
use std::{
    collections::HashSet,
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
    pub drop_data_seq: HashSet<u32>,
}

impl<T: Optical> Optical for Lossy<T> {
    fn show(&mut self, payload: &Payload) -> Result<()> {
        self.inner.show(payload)
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<Payload>> {
        let payload = self.inner.poll(timeout)?;
        if let Some(Payload::Data { seq, .. }) = payload.as_ref() {
            if self.drop_data_seq.remove(seq) {
                return Ok(None);
            }
        }
        Ok(payload)
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
            protocol_ver: 1,
            role: 2,
        })
        .unwrap();
        match b.poll(Duration::from_millis(1)).unwrap() {
            Some(Payload::Hello { role, .. }) => assert_eq!(role, 2),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn lossy_drops_configured_data_seq() {
        let (mut send, recv) = pair();
        let mut recv = Lossy {
            inner: recv,
            drop_data_seq: [3].into_iter().collect(),
        };
        send.show(&Payload::Data {
            seq: 3,
            chunk: vec![1],
        })
        .unwrap();
        assert!(recv.poll(Duration::from_millis(1)).unwrap().is_none());
        send.show(&Payload::Data {
            seq: 4,
            chunk: vec![2],
        })
        .unwrap();
        match recv.poll(Duration::from_millis(1)).unwrap() {
            Some(Payload::Data { seq, .. }) => assert_eq!(seq, 4),
            other => panic!("{other:?}"),
        }
    }
}
