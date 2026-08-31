use std::{
    collections::VecDeque,
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

use crate::{detect::Tracker, frame::Payload, live::camera_error, Error, Result};

/// Width of the downscaled copy kept for the on-screen preview. Cloning full
/// 1080p luma frames into the UI thread every tick would cost megabytes a
/// second for a picture that is at most a couple of hundred cells wide.
const PREVIEW_WIDTH: u32 = 320;
/// Unique payloads the protocol thread can lag by before we drop the oldest.
/// Two ACK windows fit, so a slow paint cannot wipe a burst that already
/// decoded.
const DECODE_QUEUE: usize = 64;

/// Consecutive identical camera reads collapse to one entry: the webcam
/// re-reads whatever is on the peer's screen many times per displayed code.
fn push_unique(slot: &mut DecodeSlot, payload: Payload) {
    if slot.last.as_ref() == Some(&payload) {
        return;
    }
    if slot.pending.len() >= DECODE_QUEUE {
        slot.pending.pop_front();
    }
    slot.pending.push_back(payload.clone());
    slot.last = Some(payload);
}

struct DecodeSlot {
    pending: VecDeque<Payload>,
    last: Option<Payload>,
}

impl Default for DecodeSlot {
    fn default() -> Self {
        Self {
            pending: VecDeque::new(),
            last: None,
        }
    }
}

#[derive(Default)]
struct Shared {
    /// Unique payloads in decode order. Consecutive identical reads are
    /// collapsed so the protocol never consumes a seconds-old duplicate of
    /// the code still on screen, but a new seq that arrived while it was
    /// painting an ACK is still waiting.
    decoded: Mutex<DecodeSlot>,
    decodes: AtomicU64,
    frames: AtomicU64,
    /// Exponential moving average of the gap between successful decodes, in
    /// milliseconds. Zero until at least two decodes have landed.
    decode_gap_ms: AtomicU64,
    preview: Mutex<Option<Arc<GrayImage>>>,
    failure: Mutex<Option<String>>,
    /// Whether the peer's screen is currently being tracked in the frame.
    locked: AtomicBool,
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

    /// Returns the next unique payload the camera has decoded, if any.
    ///
    /// Consecutive identical reads of the same code are collapsed at enqueue
    /// time, so this is a change in what the peer is showing, not another
    /// look at the same QR.
    pub fn take_next(&self) -> Option<Payload> {
        self.shared.decoded.lock().ok()?.pending.pop_front()
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

    /// Whether the peer's code has been located in the frame and is being
    /// followed, rather than hunted for from scratch each frame.
    pub fn locked(&self) -> bool {
        self.shared.locked.load(Ordering::Relaxed)
    }

    /// Typical milliseconds between successful decodes, or `None` before
    /// enough have landed to say.
    pub fn decode_gap_ms(&self) -> Option<u64> {
        match self.shared.decode_gap_ms.load(Ordering::Relaxed) {
            0 => None,
            gap => Some(gap),
        }
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

    let mut last_decode: Option<std::time::Instant> = None;
    // The lids sit at different angles, so the peer's screen is a small
    // trapezoid somewhere in a wide frame. The tracker finds it once and then
    // decodes rectified crops of that region.
    let mut tracker = Tracker::new();
    while !shared.stop.load(Ordering::SeqCst) {
        let buffer = camera.frame().map_err(camera_error)?;
        let Ok(gray) = buffer.decode_image::<LumaFormat>() else {
            continue;
        };
        shared.frames.fetch_add(1, Ordering::Relaxed);

        if let Ok(mut slot) = shared.preview.lock() {
            *slot = Some(Arc::new(downscale(&gray)));
        }

        let decoded = tracker.decode(&gray);
        shared.locked.store(tracker.locked(), Ordering::Relaxed);
        let Some(bytes) = decoded else {
            continue;
        };
        let Ok(payload) = crate::frame::decode(&bytes) else {
            continue;
        };
        shared.decodes.fetch_add(1, Ordering::Relaxed);
        let now = std::time::Instant::now();
        if let Some(previous) = last_decode.replace(now) {
            let gap = now.duration_since(previous).as_millis() as u64;
            let smoothed = match shared.decode_gap_ms.load(Ordering::Relaxed) {
                0 => gap,
                // Weighted toward history so one slow frame does not swing it.
                current => (current * 3 + gap) / 4,
            };
            shared.decode_gap_ms.store(smoothed.max(1), Ordering::Relaxed);
        }
        if let Ok(mut slot) = shared.decoded.lock() {
            push_unique(&mut slot, payload);
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

    #[test]
    fn consecutive_identical_payloads_collapse_but_a_change_is_queued() {
        let mut slot = DecodeSlot::default();
        let hello = Payload::Hello {
            protocol_ver: 1,
            role: 2,
        };
        let data0 = Payload::Data {
            seq: 0,
            chunk: vec![1],
        };
        let data1 = Payload::Data {
            seq: 1,
            chunk: vec![2],
        };

        push_unique(&mut slot, hello.clone());
        push_unique(&mut slot, hello.clone());
        push_unique(&mut slot, data0.clone());
        push_unique(&mut slot, data0.clone());
        push_unique(&mut slot, data1.clone());

        assert_eq!(slot.pending, VecDeque::from([hello, data0, data1]));
    }

    #[test]
    fn a_full_queue_drops_the_oldest_unique_payload() {
        let mut slot = DecodeSlot::default();
        for seq in 0..=DECODE_QUEUE as u32 {
            push_unique(
                &mut slot,
                Payload::Data {
                    seq,
                    chunk: vec![seq as u8],
                },
            );
        }
        assert_eq!(slot.pending.len(), DECODE_QUEUE);
        assert_eq!(
            slot.pending.front(),
            Some(&Payload::Data {
                seq: 1,
                chunk: vec![1]
            })
        );
    }
}
