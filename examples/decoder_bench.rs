//! Synthetic decoder benchmark: how long the camera thread spends per frame
//! on each decode path, measured on generated 1080p frames instead of a
//! webcam, so the numbers are repeatable on any machine.
//!
//!     cargo run --release --example decoder_bench
//!
//! Set BENCH_ITERS to change the sample count (default 15).

use std::time::{Duration, Instant};

use airgap_xfer::{
    chunks::CHUNK_BYTES,
    detect::{self, Homography, Quad, Tracker},
    frame::{self, Payload},
    qr,
};
use image::{GrayImage, Luma};

fn main() {
    let iters = std::env::var("BENCH_ITERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(15usize)
        .max(1);
    println!("{iters} iterations per row, 1920x1080 synthetic frames");
    println!(
        "{:<40} {:>10} {:>10} {:>8}",
        "path", "median ms", "p90 ms", "ok"
    );
    for version in [10u8, 25, 40] {
        let code = qr::encode_version(&full_data(version), version).unwrap();
        let quad = keystone(520.0, 0.25);
        let canvas = project(&code, &quad, 1920, 1080);
        report(&format!("v{version} full-frame scan"), iters, || {
            qr::decode_with_points(&canvas).is_ok()
        });
        let mut tracker = Tracker::new();
        let _ = tracker.decode(&canvas);
        report(&format!("v{version} tracked crop"), iters, || {
            tracker.decode(&canvas).is_some()
        });
        report(
            &format!("v{version} monitor crop ({} px)", 96),
            iters,
            || detect::rectify(&canvas, &quad, 96).width() == 96,
        );
    }
    let tile = qr::encode_version(&full_data(15), 15).unwrap();
    let grid = tiled(&tile, 2, 2, 1920, 1080);
    report("4 x v15 tiles, decode_all", iters, || {
        qr::decode_all(&grid).len() == 4
    });
}

fn full_data(version: u8) -> Vec<u8> {
    let count = frame::chunks_per_frame(version);
    frame::encode(&Payload::Data {
        transfer_id: 0x0123_4567_89ab_cdef,
        first: 0,
        count: count as u8,
        bytes: (0..count as usize * CHUNK_BYTES)
            .map(|i| (i * 37) as u8)
            .collect(),
    })
    .unwrap()
}

fn report(name: &str, iters: usize, mut op: impl FnMut() -> bool) {
    let mut times: Vec<Duration> = Vec::with_capacity(iters);
    let mut ok = 0;
    for _ in 0..iters {
        let start = Instant::now();
        if op() {
            ok += 1;
        }
        times.push(start.elapsed());
    }
    times.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let median = times[times.len() / 2];
    let p90 = times[(times.len() * 9 / 10).min(times.len() - 1)];
    println!(
        "{name:<40} {:>10.2} {:>10.2} {:>5}/{iters}",
        ms(median),
        ms(p90),
        ok
    );
}

/// A square screen of `size` pixels whose far edge is `tilt` shorter than
/// its near edge — the keystone two laptop lids produce.
fn keystone(size: f32, tilt: f32) -> Quad {
    let (cx, cy) = (960.0, 540.0);
    let far = size / 2.0 * (1.0 - tilt);
    Quad {
        corners: [
            (cx - far, cy - size / 2.0),
            (cx + far, cy - size / 2.0),
            (cx + size / 2.0, cy + size / 2.0),
            (cx - size / 2.0, cy + size / 2.0),
        ],
    }
}

fn project(code: &GrayImage, quad: &Quad, w: u32, h: u32) -> GrayImage {
    let inverse = Homography::square_to_quad(quad)
        .invert()
        .expect("non-degenerate quad");
    let mut canvas = GrayImage::from_pixel(w, h, Luma([190]));
    for y in 0..h {
        for x in 0..w {
            let Some((u, v)) = inverse.map(x as f32 + 0.5, y as f32 + 0.5) else {
                continue;
            };
            if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                let sx = ((u * code.width() as f32) as u32).min(code.width() - 1);
                let sy = ((v * code.height() as f32) as u32).min(code.height() - 1);
                canvas.put_pixel(x, y, *code.get_pixel(sx, sy));
            }
        }
    }
    canvas
}

fn tiled(tile: &GrayImage, across: u32, down: u32, w: u32, h: u32) -> GrayImage {
    let mut canvas = GrayImage::from_pixel(w, h, Luma([190]));
    let gap = 24;
    for row in 0..down {
        for col in 0..across {
            let (x0, y0) = (
                100 + col * (tile.width() + gap),
                60 + row * (tile.height() + gap),
            );
            for y in 0..tile.height() {
                for x in 0..tile.width() {
                    canvas.put_pixel(x0 + x, y0 + y, *tile.get_pixel(x, y));
                }
            }
        }
    }
    canvas
}
