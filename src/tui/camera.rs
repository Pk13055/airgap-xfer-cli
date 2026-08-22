use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use image::{imageops::FilterType, GrayImage};
use nokhwa::{
    pixel_format::LumaFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};

use crate::{frame::Payload, live::camera_error, qr, Error, Result};

/// Width of the downscaled copy kept for the on-screen preview. Cloning full
/// 1080p luma frames into the UI thread every tick would cost megabytes a
/// second for a picture that is at most a couple of hundred cells wide.
const PREVIEW_WIDTH: u32 = 320;

#[derive(Default)]
struct Shared {
    /// Most recent successfully decoded payload, tagged with a monotonically
    /// increasing sequence number so a consumer can tell a fresh decode from
    /// one it has already taken.
    decoded: Mutex<Option<(u64, Payload)>>,
    decodes: AtomicU64,
    frames: AtomicU64,
    preview: Mutex<Option<Arc<GrayImage>>>,
    failure: Mutex<Option<String>>,
    stop: AtomicBool,
}

/// A webcam owned by one dedicated thread for its whole life, publishing both
/// a preview image for the operator and the latest decoded QR payload for the
/// protocol.
pub struct CameraFeed {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
    pub index: u32,
}

impl CameraFeed {
    /// Opens camera `index` on its own thread and blocks until the first frame
    /// arrives, so a permission or device error surfaces before the TUI takes
    /// over the terminal.
    pub fn open(index: u32) -> Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let handle = thread::spawn(move || {
            if let Err(err) = capture_loop(index, &worker) {
                if let Ok(mut slot) = worker.failure.lock() {
                    *slot = Some(err.to_string());
                }
            }
        });

        let mut feed = Self {
            shared,
            handle: Some(handle),
            index,
        };
        feed.wait_for_first_frame()?;
        Ok(feed)
    }

    fn wait_for_first_frame(&mut self) -> Result<()> {
        for _ in 0..600 {
            if let Some(err) = self.failure() {
                return Err(Error::Camera(err));
            }
            if self.shared.frames.load(Ordering::Relaxed) > 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        Err(Error::Camera(format!(
            "camera {} produced no frames within 12s",
            self.index
        )))
    }

    /// Returns the latest decode if it is newer than `seen`, advancing `seen`.
    ///
    /// Only the newest decode is kept: the camera re-reads whatever is on the
    /// peer's screen many times per displayed code, and a queue would let the
    /// protocol consume seconds-old frames while believing it was current.
    pub fn take_newer_than(&self, seen: &mut u64) -> Option<Payload> {
        let slot = self.shared.decoded.lock().ok()?;
        let (seq, payload) = slot.as_ref()?;
        if *seq <= *seen {
            return None;
        }
        *seen = *seq;
        Some(payload.clone())
    }

    pub fn preview(&self) -> Option<Arc<GrayImage>> {
        self.shared.preview.lock().ok()?.clone()
    }

    pub fn failure(&self) -> Option<String> {
        self.shared.failure.lock().ok()?.clone()
    }

    /// `(frames captured, QR codes decoded)` since the feed opened.
    pub fn counters(&self) -> (u64, u64) {
        (
            self.shared.frames.load(Ordering::Relaxed),
            self.shared.decodes.load(Ordering::Relaxed),
        )
    }
}

impl Drop for CameraFeed {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn capture_loop(index: u32, shared: &Shared) -> Result<()> {
    let format = RequestedFormat::new::<LumaFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
    let mut camera = Camera::new(CameraIndex::Index(index), format).map_err(camera_error)?;
    camera.open_stream().map_err(camera_error)?;

    let mut seq = 0u64;
    while !shared.stop.load(Ordering::SeqCst) {
        let buffer = camera.frame().map_err(camera_error)?;
        let Ok(gray) = buffer.decode_image::<LumaFormat>() else {
            continue;
        };
        shared.frames.fetch_add(1, Ordering::Relaxed);

        if let Ok(mut slot) = shared.preview.lock() {
            *slot = Some(Arc::new(downscale(&gray)));
        }

        let Ok(bytes) = qr::decode_image(&gray) else {
            continue;
        };
        let Ok(payload) = crate::frame::decode(&bytes) else {
            continue;
        };
        seq += 1;
        shared.decodes.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut slot) = shared.decoded.lock() {
            *slot = Some((seq, payload));
        }
    }
    Ok(())
}

fn downscale(frame: &GrayImage) -> GrayImage {
    if frame.width() <= PREVIEW_WIDTH {
        return frame.clone();
    }
    let height = (frame.height() * PREVIEW_WIDTH / frame.width()).max(1);
    image::imageops::resize(frame, PREVIEW_WIDTH, height, FilterType::Nearest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downscale_preserves_aspect_ratio_and_caps_width() {
        let wide = GrayImage::new(1920, 1080);
        let small = downscale(&wide);
        assert_eq!(small.width(), PREVIEW_WIDTH);
        assert_eq!(small.height(), 1080 * PREVIEW_WIDTH / 1920);

        let tiny = GrayImage::new(64, 48);
        assert_eq!(downscale(&tiny).dimensions(), (64, 48));
    }
}
