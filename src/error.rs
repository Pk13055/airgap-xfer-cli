use crate::frame::Payload;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("point the other webcam at this terminal")]
    HandshakeTimeout,
    #[error("handshake failed")]
    HandshakeFailed,
    #[error("no usable QR version (lighting, distance, or font)")]
    NoUsableProbe,
    #[error("camera: {0}")]
    Camera(String),
    #[error("destination exists: {0} (use --force)")]
    DestExists(PathBuf),
    #[error("empty archive")]
    EmptyArchive,
    #[error("hash mismatch")]
    HashMismatch,
    #[error("stalled: missing seqs {0:?}")]
    Stalled(Vec<u32>),
    #[error("bad frame")]
    BadFrame,
    #[error("interrupted")]
    Interrupted,
    #[error("aborted by operator")]
    Aborted,
    /// The peer started a new handshake (HELLO, PROBE or LINK) while this
    /// side was mid-transfer. The payload is kept so the next handshake can
    /// consume it: the peer will not show it "again".
    #[error("peer restarted the handshake")]
    Restart(Box<Payload>),
    #[error("the other side runs protocol v{peer}; this build speaks v{ours}. Install the same airgap-xfer on both machines")]
    ProtocolMismatch { peer: u8, ours: u8 },
    #[error("receiver refused the offer: {0}")]
    Refused(&'static str),
    #[error(
        "terminal too small: need at least {need_cols}x{need_rows}, have {have_cols}x{have_rows}"
    )]
    TerminalTooSmall {
        need_cols: u16,
        need_rows: u16,
        have_cols: u16,
        have_rows: u16,
    },
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, Error>;
