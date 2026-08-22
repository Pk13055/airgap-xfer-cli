use image::{GrayImage, Luma};
use qrcode::{Color, EcLevel, QrCode, Version};

use crate::{frame, Error, Result};

const QUIET_ZONE_MODULES: usize = 4;
const MODULE_SCALE: usize = 4;

/// Encodes `data` as a fixed-version, quartile-error-correction QR image.
pub fn encode_version(data: &[u8], version: u8) -> Result<GrayImage> {
    if data.len() > frame::qr_byte_capacity(version) {
        return Err(Error::Message("payload exceeds QR version".into()));
    }

    let code = QrCode::with_version(data, Version::Normal(version as i16), EcLevel::Q)
        .map_err(|_| Error::Message("payload exceeds QR version".into()))?;
    let modules = code.width() + QUIET_ZONE_MODULES * 2;
    let image_size = modules * MODULE_SCALE;
    let mut image = GrayImage::from_pixel(image_size as u32, image_size as u32, Luma([255]));

    for y in 0..code.width() {
        for x in 0..code.width() {
            let value = if code[(x, y)] == Color::Dark { 0 } else { 255 };
            let origin_x = (x + QUIET_ZONE_MODULES) * MODULE_SCALE;
            let origin_y = (y + QUIET_ZONE_MODULES) * MODULE_SCALE;
            for py in origin_y..origin_y + MODULE_SCALE {
                for px in origin_x..origin_x + MODULE_SCALE {
                    image.put_pixel(px as u32, py as u32, Luma([value]));
                }
            }
        }
    }

    Ok(image)
}

/// Total module count per side for `version`, including the quiet zone.
///
/// QR version `v` is `17 + 4v` modules wide; `encode_version` pads it with a
/// [`QUIET_ZONE_MODULES`]-wide border on every side.
pub fn modules_for_version(version: u8) -> usize {
    17 + 4 * version as usize + QUIET_ZONE_MODULES * 2
}

/// Terminal cells a `version` code occupies when drawn two modules per row:
/// `(columns, rows)`.
pub fn cell_size_for_version(version: u8) -> (u16, u16) {
    let modules = modules_for_version(version);
    (modules as u16, modules.div_ceil(2) as u16)
}

/// Every QR version the protocol can carry a frame at, smallest first.
/// Versions outside this set have no capacity entry in [`frame`] and cannot be
/// encoded at all.
pub const SUPPORTED_VERSIONS: [u8; 6] = [10, 15, 20, 25, 30, 40];

/// Largest supported QR version that renders without clipping in a
/// `cols` x `rows` terminal area, or `None` if even the smallest does not fit.
///
/// A clipped QR code is undecodable, so both peers must refuse to negotiate a
/// version their own display cannot draw in full.
pub fn max_version_for_area(cols: u16, rows: u16) -> Option<u8> {
    SUPPORTED_VERSIONS
        .iter()
        .copied()
        .rev()
        .find(|&version| {
            let (need_cols, need_rows) = cell_size_for_version(version);
            need_cols <= cols && need_rows <= rows
        })
}

/// Smallest supported QR version, and therefore the floor on terminal size.
pub fn smallest_version() -> u8 {
    SUPPORTED_VERSIONS[0]
}

/// Returns whether module `(mx, my)` of a rendered code is dark, sampling the
/// center pixel of the module so the `MODULE_SCALE` raster does not matter.
pub fn module_is_dark(img: &GrayImage, mx: usize, my: usize, invert: bool) -> bool {
    let px = (mx * MODULE_SCALE + MODULE_SCALE / 2) as u32;
    let py = (my * MODULE_SCALE + MODULE_SCALE / 2) as u32;
    is_dark(img.get_pixel(px, py)[0], invert)
}

/// Module count per side of an already-rendered code image.
pub fn modules_of(img: &GrayImage) -> usize {
    img.width() as usize / MODULE_SCALE
}

/// A decoded payload and the points at which the decoder found the code.
pub type Decoded = (Vec<u8>, Vec<(f32, f32)>);

/// Decodes a QR code image, preserving its binary payload, and reports where
/// the decoder found it.
///
/// Deliberately calls `detect_in_luma_with_hints` rather than the shorter
/// `detect_in_luma`: as of rxing 0.7.1 the latter forwards its `width` and
/// `height` arguments to the luminance source in the wrong order, so every
/// non-square image — which is to say every camera frame — is interpreted
/// transposed and nothing decodes. Both paths default `TryHarder` on, so
/// nothing is lost by going the long way round.
pub fn decode_with_points(img: &GrayImage) -> Result<Decoded> {
    let result = rxing::helpers::detect_in_luma_with_hints(
        img.as_raw().clone(),
        img.width(),
        img.height(),
        Some(rxing::BarcodeFormat::QR_CODE),
        &mut rxing::DecodeHints::default(),
    )
    .map_err(|_| Error::BadFrame)?;

    let points = result
        .getPoints()
        .iter()
        .map(|point| (point.x, point.y))
        .collect();
    Ok((result.getRawBytes().to_vec(), points))
}

/// Decodes a QR code image, preserving its binary payload.
pub fn decode_image(img: &GrayImage) -> Result<Vec<u8>> {
    decode_with_points(img).map(|(bytes, _)| bytes)
}

/// Renders two QR modules per terminal row using Unicode block characters.
///
/// `encode_version` rasters at `MODULE_SCALE` pixels per module so the rxing
/// decoder has enough resolution for a reliable round-trip. Terminals need
/// one column per *module*, not one column per pixel, so this samples the
/// center pixel of each module rather than emitting the raw pixel grid
/// (which would make a version 10 code ~260 columns wide).
///
/// Forces a light quiet zone / dark modules via ANSI SGR codes (white
/// background, black foreground) so the code renders correctly regardless
/// of the terminal's color theme.
pub fn render_terminal(img: &GrayImage, invert: bool) -> String {
    let modules_side = img.width() as usize / MODULE_SCALE;
    let sample = |mx: usize, my: usize| -> bool {
        let px = (mx * MODULE_SCALE + MODULE_SCALE / 2) as u32;
        let py = (my * MODULE_SCALE + MODULE_SCALE / 2) as u32;
        is_dark(img.get_pixel(px, py)[0], invert)
    };

    let mut output = String::new();
    output.push_str("\x1b[47m\x1b[30m");

    let blank_row = " ".repeat(modules_side);
    output.push_str(&blank_row);
    output.push('\n');

    for y in (0..modules_side).step_by(2) {
        for x in 0..modules_side {
            let upper_dark = sample(x, y);
            let lower_dark = y + 1 < modules_side && sample(x, y + 1);
            output.push(match (upper_dark, lower_dark) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        output.push('\n');
    }

    output.push_str(&blank_row);
    output.push('\n');
    output.push_str("\x1b[0m");
    output
}

fn is_dark(value: u8, invert: bool) -> bool {
    (value < 128) != invert
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame;

    #[test]
    fn cell_size_matches_rendered_output() {
        for version in SUPPORTED_VERSIONS {
            let img = encode_version(b"AX", version).unwrap();
            let (cols, rows) = cell_size_for_version(version);
            assert_eq!(modules_of(&img) as u16, cols);
            assert_eq!(cols.div_ceil(2), rows);
        }
    }

    #[test]
    fn max_version_for_area_rejects_areas_that_would_clip() {
        // Version 10 needs 65x33 cells; one column short must drop a version.
        let (cols, rows) = cell_size_for_version(10);
        assert_eq!((cols, rows), (65, 33));
        assert_eq!(max_version_for_area(cols, rows), Some(10));
        // Version 10 is the floor: one column short leaves nothing usable.
        assert_eq!(max_version_for_area(cols - 1, rows), None);
        assert_eq!(max_version_for_area(cols, rows - 1), None);
        let (big_cols, big_rows) = cell_size_for_version(20);
        assert_eq!(max_version_for_area(big_cols, big_rows), Some(20));
    }

    #[test]
    fn a_code_in_a_wide_frame_decodes_like_the_camera_sees_it() {
        // Regression: rxing's `detect_in_luma` swaps width and height, so a
        // square test image round-trips happily while every 1920x1080 camera
        // frame is read transposed and decodes as nothing.
        let payload = frame::encode(&frame::Payload::Ok).unwrap();
        let code = encode_version(&payload, 10).unwrap();
        let mut canvas = image::GrayImage::from_pixel(1920, 1080, Luma([255]));
        for y in 0..code.height() {
            for x in 0..code.width() {
                canvas.put_pixel(700 + x, 400 + y, *code.get_pixel(x, y));
            }
        }
        assert_eq!(decode_image(&canvas).unwrap(), payload);
    }

    #[test]
    fn decoding_reports_where_the_code_was_found() {
        let code = encode_version(b"AX-points", 10).unwrap();
        let (_, points) = decode_with_points(&code).unwrap();
        assert!(points.len() >= 3, "expected a locatable quad, got {points:?}");
        for (x, y) in points {
            assert!(x >= 0.0 && x <= code.width() as f32, "{x}");
            assert!(y >= 0.0 && y <= code.height() as f32, "{y}");
        }
    }

    #[test]
    fn qr_roundtrip_binary_envelope() {
        let payload = frame::Payload::Data {
            seq: 1,
            chunk: (0u8..=80).collect(),
        };
        let bytes = frame::encode(&payload).unwrap();
        let img = encode_version(&bytes, 15).unwrap();
        let got = decode_image(&img).unwrap();
        assert_eq!(got, bytes);
    }

    #[test]
    fn terminal_render_contains_blocks_and_newline() {
        let img = encode_version(b"AX-test", 10).unwrap();
        let s = render_terminal(&img, false);
        assert!(s.contains('█') || s.contains('▀') || s.contains('▄'));
        assert!(s.contains('\n'));
    }

    #[test]
    fn terminal_render_width_is_module_count_not_pixel_count_for_version_10() {
        let img = encode_version(b"AX-test", 10).unwrap();
        // encode_version rasters at MODULE_SCALE=4 px/module, so a naive
        // per-pixel render would be 260 columns wide. render_terminal must
        // downsample to one column per module: 21 + 4*9 (modules for
        // version 10) + 8 (quiet zone) = 65 columns.
        assert_eq!(img.width(), 260);
        let raw = render_terminal(&img, false);
        let plain = raw.replace("\x1b[47m\x1b[30m", "").replace("\x1b[0m", "");
        let max_width = plain.lines().map(|l| l.chars().count()).max().unwrap_or(0);
        assert_eq!(max_width, 65);
        assert!(
            max_width < 100,
            "QR render must fit a normal terminal, got {max_width} columns"
        );
    }
}
