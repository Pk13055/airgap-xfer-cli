//! Process-wide interrupt state and camera error helpers shared by the TUI
//! and the CLI.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, Once,
    },
};

use crate::{frame, Error, Result};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static TEMP_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static CTRLC_INIT: Once = Once::new();

/// Registers the process-wide Ctrl-C handler. Idempotent: only the first call installs it.
pub fn install_ctrlc_handler() {
    CTRLC_INIT.call_once(|| {
        let _ = ctrlc::set_handler(interrupt);
    });
}

/// Marks the transfer as interrupted and removes the temp archive, if one is
/// registered. Safe to call from a signal handler or from the UI thread when
/// the operator quits.
pub fn interrupt() {
    INTERRUPTED.store(true, Ordering::SeqCst);
    if let Ok(guard) = TEMP_PATH.lock() {
        if let Some(path) = guard.as_ref() {
            crate::pack::remove_temp(path);
        }
    }
}

/// Sets (or clears) the temp archive path the Ctrl-C handler should delete on interrupt.
/// Callers should only set this when they intend to delete the temp file themselves
/// absent an interrupt (i.e. `keep_temp` is false).
pub fn set_temp_path(path: Option<PathBuf>) {
    if let Ok(mut guard) = TEMP_PATH.lock() {
        *guard = path;
    }
}

pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub fn check_interrupted() -> Result<()> {
    if is_interrupted() {
        return Err(Error::Interrupted);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
const CAMERA_PERMISSION_HINT: &str =
    "allow Camera for this terminal in System Settings > Privacy & Security";
#[cfg(target_os = "windows")]
const CAMERA_PERMISSION_HINT: &str = "enable camera access in Settings > Privacy > Camera";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const CAMERA_PERMISSION_HINT: &str = "check your OS camera permission settings";

pub fn camera_error(err: impl std::fmt::Display) -> Error {
    Error::Camera(format!("{err} ({CAMERA_PERMISSION_HINT})"))
}

/// Selects the QR version to encode `payload` at: probes always use their own
/// advertised version; every other payload kind uses the currently `locked` version.
pub fn encode_version_for(payload: &frame::Payload, locked: u8) -> u8 {
    match payload {
        frame::Payload::Probe { qr_version, .. } => *qr_version,
        _ => locked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_uses_probe_version() {
        assert_eq!(
            encode_version_for(
                &frame::Payload::Probe {
                    id: 2,
                    qr_version: 20,
                    dwell_ms: 200,
                    last: false
                },
                10
            ),
            20
        );
        assert_eq!(encode_version_for(&frame::Payload::Ok, 25), 25);
    }
}
