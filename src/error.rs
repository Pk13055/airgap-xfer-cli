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
    #[error("terminal too small: need at least {need_cols}x{need_rows}, have {have_cols}x{have_rows}")]
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
