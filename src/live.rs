use std::{
    io::{self, Write},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, Once,
    },
    time::{Duration, Instant},
};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use nokhwa::{
    pixel_format::LumaFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};

use crate::{frame, optical::Optical, qr, Error, Result};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static TEMP_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static CTRLC_INIT: Once = Once::new();

/// Registers the process-wide Ctrl-C handler. Idempotent: only the first call installs it.
fn install_ctrlc_handler() {
    CTRLC_INIT.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            INTERRUPTED.store(true, Ordering::SeqCst);
            if let Ok(guard) = TEMP_PATH.lock() {
                if let Some(path) = guard.as_ref() {
                    crate::pack::remove_temp(path);
                }
            }
        });
    });
}

/// Sets (or clears) the temp archive path the Ctrl-C handler should delete on interrupt.
/// Callers should only set this when they intend to delete the temp file themselves
/// absent an interrupt (i.e. `keep_temp` is false).
pub fn set_temp_path(path: Option<PathBuf>) {
    if let Ok(mut guard) = TEMP_PATH.lock() {
        *guard = path;
    }
}

fn interrupted_err() -> Error {
    Error::Interrupted
}

fn check_interrupted() -> Result<()> {
    if INTERRUPTED.load(Ordering::SeqCst) {
        return Err(interrupted_err());
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

fn camera_error(err: impl std::fmt::Display) -> Error {
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

/// Live webcam + terminal optical channel: displays QR codes in the alternate
/// screen and decodes incoming QR codes from a webcam.
pub struct LiveOptical {
    camera: Camera,
    version: u8,
    invert: bool,
    status: String,
}

impl LiveOptical {
    pub fn open(camera: u32, no_invert: bool) -> Result<Self> {
        install_ctrlc_handler();

        let format =
            RequestedFormat::new::<LumaFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
        let mut cam =
            Camera::new(CameraIndex::Index(camera), format).map_err(camera_error)?;
        cam.open_stream().map_err(camera_error)?;

        execute!(io::stdout(), EnterAlternateScreen, Hide)?;

        Ok(Self {
            camera: cam,
            version: 10,
            // Plan default (invert == false): dark modules on a light quiet
            // zone. `--no-invert` (no_invert == true) flips to invert == true.
            invert: no_invert,
            status: String::new(),
        })
    }
}

impl Drop for LiveOptical {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
    }
}

impl Optical for LiveOptical {
    fn show(&mut self, payload: &frame::Payload) -> Result<()> {
        check_interrupted()?;

        let bytes = frame::encode(payload)?;
        let version = encode_version_for(payload, self.version);
        let img = qr::encode_version(&bytes, version)?;

        let mut stdout = io::stdout();
        execute!(stdout, MoveTo(0, 0), Clear(ClearType::All))?;
        write!(stdout, "{}", qr::render_terminal(&img, self.invert))?;
        writeln!(stdout, "{}", self.status)?;
        stdout.flush()?;
        Ok(())
    }

    fn set_status(&mut self, status: &str) {
        self.status = status.to_string();
    }

    fn set_version(&mut self, v: u8) {
        self.version = v;
    }

    fn poll(&mut self, timeout: Duration) -> Result<Option<frame::Payload>> {
        check_interrupted()?;
        let deadline = Instant::now() + timeout;

        loop {
            check_interrupted()?;
            if Instant::now() >= deadline {
                return Ok(None);
            }

            let buffer = self.camera.frame().map_err(camera_error)?;
            let Ok(gray) = buffer.decode_image::<LumaFormat>() else {
                continue;
            };
            let Ok(bytes) = qr::decode_image(&gray) else {
                continue;
            };
            match frame::decode(&bytes) {
                Ok(payload) => return Ok(Some(payload)),
                Err(_) => continue,
            }
        }
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
