//! Locating a QR code on the other laptop's screen and rectifying it.
//!
//! Both machines are laptops, so the camera sits in a lid at one angle and the
//! screen it is reading sits in a lid at another. The code therefore lands in
//! the frame as a small, rotated trapezoid rather than a square, and it is
//! surrounded by a room the binarizer has to threshold along with it.
//!
//! The decoder already samples through a perspective transform of its own once
//! it has found the finder patterns, so the win here is not perspective
//! handling — it is *resolution and isolation*: lock onto the quadrilateral the
//! code occupies, warp only that region back to a square, and hand the decoder
//! an image that is nothing but code.

use image::{GrayImage, Luma};

use crate::qr;

/// Fraction of the code's own size added on each side when cropping, so the
/// quiet zone the sender draws around the symbol comes along with it.
const CROP_MARGIN: f32 = 0.12;
/// Rectified images are never smaller than this, so a distant screen is
/// upsampled rather than handed to the decoder at a few pixels per module.
const MIN_RECTIFIED: u32 = 320;
/// ...and never larger, to bound the per-frame cost.
const MAX_RECTIFIED: u32 = 1024;
/// Slight upsample relative to the tracked region's longest edge.
const RECTIFY_SCALE: f32 = 1.25;
/// Consecutive misses inside the locked region before falling back to a full
/// frame scan. Kept small: at a 150 ms dwell only a handful of frames cover
/// each displayed code, so a stale lock costs chunks quickly.
const MAX_MISSES: u32 = 2;

/// A convex quadrilateral in frame pixel coordinates, ordered clockwise from
/// the top-left as the camera sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    pub corners: [(f32, f32); 4],
}

impl Quad {
    /// Longest edge in pixels, used to size the rectified image.
    pub fn longest_edge(&self) -> f32 {
        (0..4)
            .map(|i| {
                let (x0, y0) = self.corners[i];
                let (x1, y1) = self.corners[(i + 1) % 4];
                ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()
            })
            .fold(0.0, f32::max)
    }
}

/// A 3x3 projective transform in homogeneous coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography(pub [[f32; 3]; 3]);

impl Homography {
    /// Maps the unit square — (0,0), (1,0), (1,1), (0,1) — onto `quad`,
    /// matching its corner order.
    ///
    /// Heckbert's closed form for the square-to-quad case; no general solver
    /// needed because the source corners are fixed.
    pub fn square_to_quad(quad: &Quad) -> Self {
        let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = quad.corners;
        let dx1 = x1 - x2;
        let dx2 = x3 - x2;
        let dx3 = x0 - x1 + x2 - x3;
        let dy1 = y1 - y2;
        let dy2 = y3 - y2;
        let dy3 = y0 - y1 + y2 - y3;

        let denom = dx1 * dy2 - dx2 * dy1;
        let (g, h) = if dx3 == 0.0 && dy3 == 0.0 {
            // The quad is a parallelogram: the map is affine.
            (0.0, 0.0)
        } else if denom.abs() < f32::EPSILON {
            (0.0, 0.0)
        } else {
            (
                (dx3 * dy2 - dx2 * dy3) / denom,
                (dx1 * dy3 - dx3 * dy1) / denom,
            )
        };

        Homography([
            [x1 - x0 + g * x1, x3 - x0 + h * x3, x0],
            [y1 - y0 + g * y1, y3 - y0 + h * y3, y0],
            [g, h, 1.0],
        ])
    }

    /// Applies the transform to `(u, v)`, returning `None` if the point maps
    /// to the plane at infinity.
    pub fn map(&self, u: f32, v: f32) -> Option<(f32, f32)> {
        let m = &self.0;
        let w = m[2][0] * u + m[2][1] * v + m[2][2];
        if w.abs() < 1e-9 {
            return None;
        }
        Some((
            (m[0][0] * u + m[0][1] * v + m[0][2]) / w,
            (m[1][0] * u + m[1][1] * v + m[1][2]) / w,
        ))
    }

    /// Inverse transform, or `None` when the matrix is singular.
    pub fn invert(&self) -> Option<Self> {
        let m = &self.0;
        let cof = |r: usize, c: usize| -> f32 {
            let rows: Vec<usize> = (0..3).filter(|&i| i != r).collect();
            let cols: Vec<usize> = (0..3).filter(|&i| i != c).collect();
            let minor = m[rows[0]][cols[0]] * m[rows[1]][cols[1]]
                - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
            if (r + c).is_multiple_of(2) {
                minor
            } else {
                -minor
            }
        };
        let det = m[0][0] * cof(0, 0) + m[0][1] * cof(0, 1) + m[0][2] * cof(0, 2);
        if det.abs() < 1e-12 {
            return None;
        }
        let mut out = [[0.0f32; 3]; 3];
        for (r, row) in out.iter_mut().enumerate() {
            for (c, value) in row.iter_mut().enumerate() {
                // Transposed cofactor matrix over the determinant.
                *value = cof(c, r) / det;
            }
        }
        Some(Homography(out))
    }
}

/// Orders up to four detected points into a clockwise quad starting at the
/// top-left, deriving the fourth corner when the detector only reports three.
///
/// The decoder is free to hand back three finder centres or four symbol
/// corners in whatever order it likes, so nothing about the input order is
/// assumed.
pub fn order_quad(points: &[(f32, f32)]) -> Option<Quad> {
    let mut pts: Vec<(f32, f32)> = points.to_vec();
    if pts.len() == 3 {
        // The right-angled vertex is the corner between the two short edges;
        // the missing corner is its reflection through the other two.
        let right_angle = (0..3).min_by(|&a, &b| {
            cos_at(&pts, a)
                .abs()
                .partial_cmp(&cos_at(&pts, b).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let others: Vec<usize> = (0..3).filter(|&i| i != right_angle).collect();
        pts.push((
            pts[others[0]].0 + pts[others[1]].0 - pts[right_angle].0,
            pts[others[0]].1 + pts[others[1]].1 - pts[right_angle].1,
        ));
    }
    if pts.len() != 4 {
        return None;
    }

    let cx = pts.iter().map(|p| p.0).sum::<f32>() / 4.0;
    let cy = pts.iter().map(|p| p.1).sum::<f32>() / 4.0;
    // Image y grows downward, so ascending atan2 walks the corners clockwise
    // as seen on screen.
    pts.sort_by(|a, b| {
        (a.1 - cy)
            .atan2(a.0 - cx)
            .partial_cmp(&(b.1 - cy).atan2(b.0 - cx))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let start = (0..4).min_by(|&a, &b| {
        (pts[a].0 + pts[a].1)
            .partial_cmp(&(pts[b].0 + pts[b].1))
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;
    let corners = [
        pts[start],
        pts[(start + 1) % 4],
        pts[(start + 2) % 4],
        pts[(start + 3) % 4],
    ];
    Some(Quad { corners })
}

/// Cosine of the angle at vertex `i` of a triangle.
fn cos_at(pts: &[(f32, f32)], i: usize) -> f32 {
    let a = pts[(i + 1) % 3];
    let b = pts[(i + 2) % 3];
    let v = pts[i];
    let (ux, uy) = (a.0 - v.0, a.1 - v.1);
    let (wx, wy) = (b.0 - v.0, b.1 - v.1);
    let norms = (ux * ux + uy * uy).sqrt() * (wx * wx + wy * wy).sqrt();
    if norms == 0.0 {
        1.0
    } else {
        (ux * wx + uy * wy) / norms
    }
}

/// Maps a pixel of the rectified image back to a point on the expanded unit
/// square, so decoded positions can be projected into frame coordinates.
fn rectified_to_unit(pixel: f32, size: u32) -> f32 {
    -CROP_MARGIN + (pixel / size as f32) * (1.0 + 2.0 * CROP_MARGIN)
}

/// Warps the region of `frame` under `quad` into a `size` x `size` image,
/// widened by [`CROP_MARGIN`] on every side to carry the quiet zone along.
///
/// Samples bilinearly, and paints anything falling outside the frame white so
/// a code near the edge still gets a light quiet zone rather than a black wall
/// the binarizer would read as modules.
pub fn rectify(frame: &GrayImage, quad: &Quad, size: u32) -> GrayImage {
    let homography = Homography::square_to_quad(quad);
    let mut out = GrayImage::from_pixel(size, size, Luma([255]));
    for j in 0..size {
        let v = rectified_to_unit(j as f32 + 0.5, size);
        for i in 0..size {
            let u = rectified_to_unit(i as f32 + 0.5, size);
            if let Some((x, y)) = homography.map(u, v) {
                out.put_pixel(i, j, Luma([sample_bilinear(frame, x, y)]));
            }
        }
    }
    out
}

fn sample_bilinear(frame: &GrayImage, x: f32, y: f32) -> u8 {
    let (w, h) = (frame.width() as i64, frame.height() as i64);
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    if x0 < -1 || y0 < -1 || x0 >= w || y0 >= h {
        return 255;
    }
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let at = |px: i64, py: i64| -> f32 {
        if px < 0 || py < 0 || px >= w || py >= h {
            255.0
        } else {
            frame.get_pixel(px as u32, py as u32).0[0] as f32
        }
    };
    let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
    let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
    (top * (1.0 - fy) + bottom * fy).round().clamp(0.0, 255.0) as u8
}

/// Keeps a lock on where the peer's code sits in the frame across frames.
///
/// A full-frame scan is the fallback, not the steady state: once the region is
/// known, every later frame is decoded from a rectified crop of it, which both
/// cuts the work and gives the binarizer an image containing nothing but code.
#[derive(Default)]
pub struct Tracker {
    quad: Option<Quad>,
    misses: u32,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the peer's screen is currently being tracked.
    pub fn locked(&self) -> bool {
        self.quad.is_some()
    }

    /// Decodes whatever QR code is in `frame`, tracking its position.
    ///
    /// The tracked region is a *place on the other screen*, not a particular
    /// code: during a transfer the payload changes every dwell, so a lock is
    /// only ever confirmed by "something decoded inside it", never by decoding
    /// the same bytes twice.
    pub fn decode(&mut self, frame: &GrayImage) -> Option<Vec<u8>> {
        if let Some(quad) = self.quad {
            let size = rectified_size(&quad);
            let cropped = rectify(frame, &quad, size);
            if let Ok((bytes, points)) = qr::decode_with_points(&cropped) {
                self.quad = project_back(&quad, &points, size).or(Some(quad));
                self.misses = 0;
                return Some(bytes);
            }
            self.misses += 1;
            if self.misses < MAX_MISSES {
                return None;
            }
            self.quad = None;
            self.misses = 0;
        }

        let (bytes, points) = qr::decode_with_points(frame).ok()?;
        self.quad = order_quad(&points);
        Some(bytes)
    }

    /// Finds every QR in `frame`. After the link, the peer may tile several
    /// codes; a single-quad crop would only ever see one of them.
    pub fn decode_all(&mut self, frame: &GrayImage) -> Vec<Vec<u8>> {
        let found = qr::decode_all(frame);
        if !found.is_empty() {
            if let Some((_, points)) = found.first() {
                self.quad = order_quad(points).or(self.quad);
            }
            self.misses = 0;
            return found.into_iter().map(|(bytes, _)| bytes).collect();
        }
        self.decode(frame).into_iter().collect()
    }
}

fn rectified_size(quad: &Quad) -> u32 {
    ((quad.longest_edge() * RECTIFY_SCALE) as u32).clamp(MIN_RECTIFIED, MAX_RECTIFIED)
}

/// Re-expresses points found in a rectified crop as frame coordinates, so the
/// lock follows the screen as the lids move.
fn project_back(quad: &Quad, points: &[(f32, f32)], size: u32) -> Option<Quad> {
    let homography = Homography::square_to_quad(quad);
    let mapped: Vec<(f32, f32)> = points
        .iter()
        .filter_map(|&(x, y)| {
            homography.map(rectified_to_unit(x, size), rectified_to_unit(y, size))
        })
        .collect();
    if mapped.len() != points.len() {
        return None;
    }
    order_quad(&mapped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_quad(corners: [(f32, f32); 4]) -> Quad {
        Quad { corners }
    }

    /// Paints `code` into a `w` x `h` canvas under the perspective that maps
    /// the code's own bounds onto `quad` — a stand-in for a laptop screen seen
    /// from an angled lid.
    fn project_onto_canvas(code: &GrayImage, quad: &Quad, w: u32, h: u32) -> GrayImage {
        let forward = Homography::square_to_quad(quad);
        let inverse = forward.invert().expect("quad must be non-degenerate");
        let mut canvas = GrayImage::from_pixel(w, h, Luma([190]));
        for y in 0..h {
            for x in 0..w {
                let Some((u, v)) = inverse.map(x as f32 + 0.5, y as f32 + 0.5) else {
                    continue;
                };
                if (0.0..1.0).contains(&u) && (0.0..1.0).contains(&v) {
                    let sx = (u * code.width() as f32) as u32;
                    let sy = (v * code.height() as f32) as u32;
                    let pixel = *code.get_pixel(
                        sx.min(code.width() - 1),
                        sy.min(code.height() - 1),
                    );
                    canvas.put_pixel(x, y, pixel);
                }
            }
        }
        canvas
    }

    /// A square screen of `size` pixels seen with its far edge `tilt` shorter
    /// than its near edge — the keystone a laptop lid produces.
    fn keystone(size: f32, tilt: f32) -> Quad {
        let (cx, cy) = (960.0, 540.0);
        let far = size / 2.0 * (1.0 - tilt);
        unit_quad([
            (cx - far, cy - size / 2.0),
            (cx + far, cy - size / 2.0),
            (cx + size / 2.0, cy + size / 2.0),
            (cx - size / 2.0, cy + size / 2.0),
        ])
    }

    #[test]
    fn rectifying_reads_lids_angled_past_where_a_plain_scan_gives_up() {
        let code = qr::encode_version(b"AX-keystone", 10).unwrap();
        let want = qr::decode_image(&code).unwrap();

        // A mild angle needs no help.
        let mild = keystone(400.0, 0.25);
        assert!(qr::decode_image(&project_onto_canvas(&code, &mild, 1920, 1080)).is_ok());

        // Past roughly 45% keystone the decoder cannot find the code in the
        // full frame at all -- this is the case the user hit -- but the
        // rectified crop of the tracked region reads it cleanly.
        for (size, tilt) in [(700.0, 0.45), (400.0, 0.60), (200.0, 0.60)] {
            let quad = keystone(size, tilt);
            let canvas = project_onto_canvas(&code, &quad, 1920, 1080);
            assert!(
                qr::decode_image(&canvas).is_err(),
                "expected a full-frame scan to fail at size {size} tilt {tilt}"
            );
            let cropped = rectify(&canvas, &quad, rectified_size(&quad));
            assert_eq!(
                qr::decode_image(&cropped).unwrap(),
                want,
                "rectified crop should read size {size} tilt {tilt}"
            );
        }
    }

    #[test]
    fn the_lock_follows_a_lid_tilting_past_where_acquisition_would_work() {
        // The lock has to be acquired from a pose a plain scan can read. After
        // that the tracker follows the screen frame by frame -- lids move
        // slowly relative to the frame rate -- well past that point.
        let code = qr::encode_version(b"AX-follow", 10).unwrap();
        let mut tracker = Tracker::new();

        let mut hard_frames = 0;
        for step in 0..=8 {
            let tilt = 0.20 + 0.05 * step as f32;
            let quad = keystone(500.0, tilt);
            let canvas = project_onto_canvas(&code, &quad, 1920, 1080);
            if qr::decode_image(&canvas).is_err() {
                hard_frames += 1;
            }
            assert!(
                tracker.decode(&canvas).is_some(),
                "tracker lost the code at tilt {tilt:.2}"
            );
            assert!(tracker.locked(), "tilt {tilt:.2} should leave a lock");
        }
        assert!(
            hard_frames >= 3,
            "the sweep must end well past what a plain scan handles, got {hard_frames}"
        );
    }

    #[test]
    fn square_to_quad_lands_each_corner_where_it_belongs() {
        let quad = unit_quad([(10.0, 20.0), (110.0, 15.0), (120.0, 90.0), (5.0, 100.0)]);
        let h = Homography::square_to_quad(&quad);
        let got = [
            h.map(0.0, 0.0).unwrap(),
            h.map(1.0, 0.0).unwrap(),
            h.map(1.0, 1.0).unwrap(),
            h.map(0.0, 1.0).unwrap(),
        ];
        for (got, want) in got.iter().zip(quad.corners.iter()) {
            assert!(
                (got.0 - want.0).abs() < 1e-3 && (got.1 - want.1).abs() < 1e-3,
                "{got:?} != {want:?}"
            );
        }
    }

    #[test]
    fn inverting_a_homography_round_trips_points() {
        let quad = unit_quad([(10.0, 20.0), (110.0, 15.0), (120.0, 90.0), (5.0, 100.0)]);
        let h = Homography::square_to_quad(&quad);
        let inv = h.invert().unwrap();
        for &(u, v) in &[(0.25f32, 0.4f32), (0.9, 0.1), (0.5, 0.5)] {
            let (x, y) = h.map(u, v).unwrap();
            let (bu, bv) = inv.map(x, y).unwrap();
            assert!((bu - u).abs() < 1e-3 && (bv - v).abs() < 1e-3, "{bu},{bv}");
        }
    }

    #[test]
    fn corners_are_ordered_clockwise_from_top_left_whatever_order_they_arrive_in() {
        let want = [(10.0, 20.0), (110.0, 15.0), (120.0, 90.0), (5.0, 100.0)];
        for rotation in 0..4 {
            let shuffled: Vec<(f32, f32)> =
                (0..4).map(|i| want[(i + rotation) % 4]).collect();
            assert_eq!(order_quad(&shuffled).unwrap().corners, want);
        }
        // Reversed input must still come back clockwise.
        let mut reversed = want.to_vec();
        reversed.reverse();
        assert_eq!(order_quad(&reversed).unwrap().corners, want);
    }

    #[test]
    fn three_finder_points_yield_the_missing_fourth_corner() {
        // Top-left is the right-angled vertex; the detector omits bottom-right.
        let quad = order_quad(&[(100.0, 300.0), (100.0, 100.0), (300.0, 100.0)]).unwrap();
        assert_eq!(
            quad.corners,
            [(100.0, 100.0), (300.0, 100.0), (300.0, 300.0), (100.0, 300.0)]
        );
    }

    #[test]
    fn a_code_seen_at_a_steep_angle_decodes_after_rectifying() {
        let payload = b"AX-oblique-lid";
        let code = qr::encode_version(payload, 10).unwrap();
        // A screen filling a modest part of a 1080p frame, tilted so the far
        // edge is ~30% shorter than the near edge.
        let quad = unit_quad([
            (760.0, 300.0),
            (1180.0, 380.0),
            (1120.0, 800.0),
            (700.0, 720.0),
        ]);
        let canvas = project_onto_canvas(&code, &quad, 1920, 1080);

        let mut tracker = Tracker::new();
        let got = tracker.decode(&canvas).expect("should decode the tilted code");
        assert_eq!(got, qr::decode_image(&code).unwrap());
        assert!(tracker.locked(), "a successful decode must leave a lock");

        // The next frame goes through the rectified crop, not a full scan.
        let again = tracker.decode(&canvas).expect("locked decode");
        assert_eq!(again, got);
        assert!(tracker.locked());
    }

    #[test]
    fn the_lock_is_dropped_after_a_short_run_of_misses() {
        let code = qr::encode_version(b"AX-lock", 10).unwrap();
        let quad = unit_quad([
            (760.0, 300.0),
            (1180.0, 380.0),
            (1120.0, 800.0),
            (700.0, 720.0),
        ]);
        let canvas = project_onto_canvas(&code, &quad, 1920, 1080);
        let blank = GrayImage::from_pixel(1920, 1080, Luma([190]));

        let mut tracker = Tracker::new();
        tracker.decode(&canvas).unwrap();
        assert!(tracker.locked());
        for _ in 0..MAX_MISSES {
            assert!(tracker.decode(&blank).is_none());
        }
        assert!(!tracker.locked(), "a stale lock must not survive");

        // And it re-acquires once the code is back.
        assert!(tracker.decode(&canvas).is_some());
        assert!(tracker.locked());
    }
}
