use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use image::{imageops::FilterType, GrayImage};
use nokhwa::{
    pixel_format::LumaFormat,
    utils::{CameraIndex, RequestedFormat, RequestedFormatType},
    Camera,
};

use crate::{
    detect::{self, Tracker},
    frame::{self, Payload},
    live::camera_error,
    Error, Result,
};

/// Width of the downscaled copy kept for the on-screen preview.
const PREVIEW_WIDTH: u32 = 320;
/// Payloads the protocol thread can lag by before the oldest is dropped.
const DECODE_QUEUE: usize = 64;
/// Side of the square, rectified crop of the peer's code kept for the
/// monitor. Small on purpose: it is drawn a few dozen cells wide, and
/// warping it costs next to nothing per frame.
pub const MONITOR_CROP: u32 = 96;
/// How long a decoded code counts as still on the peer's screen. Reads of
/// it within this window are the camera looking again, not news: they
/// neither queue the payload twice nor change the monitor's tint.
pub const SCREEN_HOLD: Duration = Duration::from_millis(1500);

/// Payload fingerprints read recently, and when each was last read.
#[derive(Default)]
struct ScreenMemory {
    seen: HashMap<u32, Instant>,
}

impl ScreenMemory {
    /// Whether `bytes` is news: not read at any time in the last
    /// [`SCREEN_HOLD`]. Every read refreshes the entry, so a code that stays
    /// on screen stays old however long it is held, while one that left and
    /// came back (a resend) is news again.
    fn is_fresh(&mut self, bytes: &[u8], now: Instant) -> bool {
        self.seen
            .retain(|_, at| now.saturating_duration_since(*at) < SCREEN_HOLD);
        self.seen.insert(crc32fast::hash(bytes), now).is_none()
    }
}

/// The monitor's view of the peer: rectified camera pixels around the
/// peer's code, never a regenerated code.
#[derive(Clone)]
pub struct PeerCrop {
    pub image: Arc<GrayImage>,
    /// Distinct payloads decoded when the crop was taken. Changes only when
    /// the peer shows something new.
    pub epoch: u64,
    /// When the frame it came from was decoded.
    pub at: Instant,
}

/// What one camera frame produced.
#[derive(Default)]
struct FrameOutcome {
    /// Payloads that are news, in decode order.
    fresh: Vec<Payload>,
    /// Successful decodes of any kind, repeats included.
    decodes: u64,
    /// Version byte of a frame from a peer speaking another protocol.
    foreign: Option<u8>,
    /// Rectified camera pixels around what decoded, if anything did.
    crop: Option<GrayImage>,
}

fn process_frame(
    gray: &GrayImage,
    tracker: &mut Tracker,
    multi: bool,
    memory: &mut ScreenMemory,
    now: Instant,
) -> FrameOutcome {
    let decoded: Vec<Vec<u8>> = if multi {
        tracker.decode_all(gray)
    } else {
        tracker.decode(gray).into_iter().collect()
    };
    let mut outcome = FrameOutcome::default();
    for bytes in &decoded {
        match frame::decode(bytes) {
            Ok(payload) => {
                outcome.decodes += 1;
                if memory.is_fresh(bytes, now) {
                    outcome.fresh.push(payload);
                }
            }
            Err(_) => {
                if let Some(version) = frame::foreign_version(bytes) {
                    outcome.foreign = Some(version);
                }
            }
        }
    }
    if outcome.decodes > 0 {
        outcome.crop = tracker
            .region()
            .map(|quad| detect::rectify(gray, &quad, MONITOR_CROP));
    }
    outcome
}

fn enqueue(queue: &mut VecDeque<Payload>, payload: Payload) {
    if queue.len() >= DECODE_QUEUE {
        queue.pop_front();
    }
    queue.push_back(payload);
}

#[derive(Default)]
struct Shared {
    /// Payloads that were news, in decode order.
    decoded: Mutex<VecDeque<Payload>>,
    decodes: AtomicU64,
    frames: AtomicU64,
    /// Exponential moving average of the gap between frames that decoded
    /// anything, in milliseconds. Zero until two such frames have landed.
    decode_gap_ms: AtomicU64,
    /// Distinct payloads decoded so far.
    epoch: AtomicU64,
    preview: Mutex<Option<Arc<GrayImage>>>,
    crop: Mutex<Option<PeerCrop>>,
    failure: Mutex<Option<String>>,
    /// Version byte last seen on a frame this build cannot decode; 0 = none.
    foreign: AtomicU8,
    /// Whether the peer's screen is currently being tracked in the frame.
    locked: AtomicBool,
    /// Decode every QR in the frame: the peer is tiling.
    multi: AtomicBool,
    /// The link is up: the preview is no longer drawn.
    linked: AtomicBool,
    stop: AtomicBool,
}

/// A webcam owned by one dedicated thread for its whole life, publishing a
/// preview, a crop of the peer's code, and decoded payloads.
pub struct CameraFeed {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
    pub index: u32,
}

impl CameraFeed {
    /// Opens camera `index` on its own thread and blocks until the first
    /// frame arrives, so a permission or device error surfaces before the
    /// TUI takes over the terminal.
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

    /// The next payload that was news when the camera read it.
    pub fn take_next(&self) -> Option<Payload> {
        self.shared.decoded.lock().ok()?.pop_front()
    }

    pub fn preview(&self) -> Option<Arc<GrayImage>> {
        self.shared.preview.lock().ok()?.clone()
    }

    /// The last crop of the peer's code, however old.
    pub fn peer_crop(&self) -> Option<PeerCrop> {
        self.shared.crop.lock().ok()?.clone()
    }

    pub fn epoch(&self) -> u64 {
        self.shared.epoch.load(Ordering::Relaxed)
    }

    /// The protocol version of a peer this build cannot talk to, if one has
    /// been seen.
    pub fn foreign_version(&self) -> Option<u8> {
        match self.shared.foreign.load(Ordering::Relaxed) {
            0 => None,
            version => Some(version),
        }
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

    pub fn locked(&self) -> bool {
        self.shared.locked.load(Ordering::Relaxed)
    }

    /// Switch between single-code tracking and decoding every QR in the
    /// frame (a tiling peer).
    pub fn set_multi(&self, multi: bool) {
        self.shared.multi.store(multi, Ordering::Relaxed);
    }

    /// Once linked the preview is not drawn, so stop paying to downscale it.
    pub fn set_linked(&self, linked: bool) {
        self.shared.linked.store(linked, Ordering::Relaxed);
    }

    /// Typical milliseconds between frames that decoded something.
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

    // The lids sit at different angles, so the peer's screen is a small
    // trapezoid somewhere in a wide frame. The tracker finds it once and then
    // decodes rectified crops of that region.
    let mut tracker = Tracker::new();
    let mut memory = ScreenMemory::default();
    let mut last_decode: Option<Instant> = None;
    while !shared.stop.load(Ordering::SeqCst) {
        let buffer = camera.frame().map_err(camera_error)?;
        let Ok(gray) = buffer.decode_image::<LumaFormat>() else {
            continue;
        };
        shared.frames.fetch_add(1, Ordering::Relaxed);

        if !shared.linked.load(Ordering::Relaxed) {
            if let Ok(mut slot) = shared.preview.lock() {
                *slot = Some(Arc::new(downscale(&gray)));
            }
        }

        let now = Instant::now();
        let multi = shared.multi.load(Ordering::Relaxed);
        let outcome = process_frame(&gray, &mut tracker, multi, &mut memory, now);
        shared.locked.store(tracker.locked(), Ordering::Relaxed);
        if let Some(version) = outcome.foreign {
            shared.foreign.store(version, Ordering::Relaxed);
        }
        if outcome.decodes > 0 {
            shared.decodes.fetch_add(outcome.decodes, Ordering::Relaxed);
            if let Some(previous) = last_decode.replace(now) {
                let gap = now.duration_since(previous).as_millis() as u64;
                let smoothed = match shared.decode_gap_ms.load(Ordering::Relaxed) {
                    0 => gap,
                    current => (current * 3 + gap) / 4,
                };
                shared
                    .decode_gap_ms
                    .store(smoothed.max(1), Ordering::Relaxed);
            }
        }
        if !outcome.fresh.is_empty() {
            shared
                .epoch
                .fetch_add(outcome.fresh.len() as u64, Ordering::Relaxed);
            if let Ok(mut queue) = shared.decoded.lock() {
                for payload in outcome.fresh {
                    enqueue(&mut queue, payload);
                }
            }
        }
        if let Some(crop) = outcome.crop {
            let peer = PeerCrop {
                image: Arc::new(crop),
                epoch: shared.epoch.load(Ordering::Relaxed),
                at: now,
            };
            if let Ok(mut slot) = shared.crop.lock() {
                *slot = Some(peer);
            }
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
    use crate::qr;
    use image::Luma;

    /// A camera frame of a code on a mid-grey desk. Every pixel carries the
    /// small offsets a real sensor has: nothing is pure black or white.
    fn camera_frame(bytes: &[u8], version: u8) -> GrayImage {
        let code = qr::encode_version(bytes, version).unwrap();
        let mut frame = GrayImage::from_pixel(900, 700, Luma([150]));
        for y in 0..code.height() {
            for x in 0..code.width() {
                let sensor = if code.get_pixel(x, y)[0] < 128 {
                    20 + (x % 5) as u8
                } else {
                    225 + (y % 5) as u8
                };
                frame.put_pixel(200 + x, 120 + y, Luma([sensor]));
            }
        }
        frame
    }

    #[test]
    fn downscale_preserves_aspect_ratio_and_caps_width() {
        let wide = GrayImage::new(1920, 1080);
        let small = downscale(&wide);
        assert_eq!(small.width(), PREVIEW_WIDTH);
        assert_eq!(small.height(), 1080 * PREVIEW_WIDTH / 1920);
        assert_eq!(downscale(&GrayImage::new(64, 48)).dimensions(), (64, 48));
    }

    #[test]
    fn repeated_reads_of_an_unchanged_code_are_not_news() {
        let mut memory = ScreenMemory::default();
        let t0 = Instant::now();
        assert!(memory.is_fresh(b"A", t0));
        assert!(!memory.is_fresh(b"A", t0 + Duration::from_millis(100)));
        assert!(
            memory.is_fresh(b"B", t0 + Duration::from_millis(100)),
            "a different code is news"
        );
        assert!(
            !memory.is_fresh(b"A", t0 + Duration::from_millis(1400)),
            "still on screen: each read refreshes it"
        );
        assert!(
            memory.is_fresh(b"A", t0 + Duration::from_millis(1400) + SCREEN_HOLD),
            "gone for a whole hold and back again is a resend"
        );
    }

    #[test]
    fn a_frame_yields_its_payload_once_and_a_crop_of_real_camera_pixels() {
        let payload = Payload::Ok { transfer_id: 9 };
        let frame = camera_frame(&frame::encode(&payload).unwrap(), 10);
        let mut tracker = Tracker::new();
        let mut memory = ScreenMemory::default();
        let t0 = Instant::now();

        let first = process_frame(&frame, &mut tracker, false, &mut memory, t0);
        assert_eq!(first.fresh, vec![payload]);
        assert_eq!(first.decodes, 1);
        let crop = first.crop.expect("a decode leaves a crop");
        assert_eq!(crop.dimensions(), (MONITOR_CROP, MONITOR_CROP));
        assert!(
            crop.pixels().all(|p| p[0] != 0 && p[0] != 255),
            "a regenerated code would be pure black and white; this must be camera pixels"
        );
        assert!(crop.pixels().any(|p| p[0] < 80) && crop.pixels().any(|p| p[0] > 180));

        let again = process_frame(
            &frame,
            &mut tracker,
            false,
            &mut memory,
            t0 + Duration::from_millis(50),
        );
        assert!(
            again.fresh.is_empty(),
            "the same code still on screen is not news"
        );
        assert_eq!(again.decodes, 1);
        assert!(
            again.crop.is_some(),
            "the monitor still refreshes from the live frame"
        );
    }

    #[test]
    fn a_peer_on_another_protocol_version_is_reported_not_decoded() {
        let mut bytes = frame::encode(&Payload::Ok { transfer_id: 9 }).unwrap();
        bytes[2] = 1;
        let frame = camera_frame(&bytes, 10);
        let outcome = process_frame(
            &frame,
            &mut Tracker::new(),
            false,
            &mut ScreenMemory::default(),
            Instant::now(),
        );
        assert!(outcome.fresh.is_empty());
        assert_eq!(outcome.foreign, Some(1));
    }

    #[test]
    fn a_full_queue_drops_the_oldest_payload() {
        let mut queue = VecDeque::new();
        for first in 0..=DECODE_QUEUE as u32 {
            enqueue(
                &mut queue,
                Payload::Data {
                    transfer_id: 1,
                    first,
                    count: 1,
                    bytes: vec![first as u8],
                },
            );
        }
        assert_eq!(queue.len(), DECODE_QUEUE);
        assert!(matches!(
            queue.front(),
            Some(Payload::Data { first: 1, .. })
        ));
    }
}
