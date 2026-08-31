use std::collections::HashSet;

use image::{GrayImage, Luma};
use qrcode::bits::Bits;
use qrcode::{Color, EcLevel, QrCode, Version};

use crate::{frame, Error, Result};

const QUIET_ZONE_MODULES: usize = 4;
const MODULE_SCALE: usize = 4;

fn too_long(version: u8, len: usize, why: &str) -> Error {
    Error::Message(format!(
        "payload exceeds QR v{version} ({len} bytes, {why})"
    ))
}

fn qr_code_byte_mode(data: &[u8], version: u8) -> Result<QrCode> {
    let cap = frame::qr_byte_capacity(version);
    if data.len() > cap {
        return Err(too_long(version, data.len(), &format!("max {cap}")));
    }

    let mut bits = Bits::new(Version::Normal(version as i16));
    bits.push_byte_data(data)
        .map_err(|_| too_long(version, data.len(), "byte mode"))?;
    bits.push_terminator(EcLevel::Q)
        .map_err(|_| too_long(version, data.len(), "terminator"))?;
    QrCode::with_bits(bits, EcLevel::Q).map_err(|_| too_long(version, data.len(), "encoder"))
}

/// Encodes `data` as a fixed-version, quartile-error-correction QR image.
///
/// Always uses byte mode. The payload is a binary envelope, and the optimizer
/// in `QrCode::with_version` can pick a mixed-mode layout that no longer fits
/// the bit budget we sized the chunks for.
pub fn encode_version(data: &[u8], version: u8) -> Result<GrayImage> {
    let code = qr_code_byte_mode(data, version)?;
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

/// Checks that a full-size DATA envelope fits in `version` under byte mode.
///
/// Call this after the link is locked and before GO, so a capacity mistake
/// fails the handshake instead of crashing the sender while the receiver
/// waits for DATA and reports a stall on sequences 0–31.
pub fn ensure_full_data_frame_fits(version: u8) -> Result<()> {
    let chunk = vec![0xA5u8; frame::data_chunk_size(version)];
    let bytes = frame::encode(&frame::Payload::Data { seq: 0, chunk })?;
    let cap = frame::qr_byte_capacity(version);
    if bytes.len() > cap {
        return Err(too_long(version, bytes.len(), &format!("max {cap}")));
    }
    let mut bits = Bits::new(Version::Normal(version as i16));
    bits.push_byte_data(&bytes)
        .map_err(|_| too_long(version, bytes.len(), "byte mode"))?;
    bits.push_terminator(EcLevel::Q)
        .map_err(|_| too_long(version, bytes.len(), "terminator"))?;
    Ok(())
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

/// How many copies of `version` fit in a `cols` x `rows` pane, packed in a
/// row-major grid. Always at least 1: a pane smaller than one code is a
/// clip (handled at draw time), not zero tiles.
pub fn tiles_for_area(cols: u16, rows: u16, version: u8) -> usize {
    let (w, h) = cell_size_for_version(version);
    if w == 0 || h == 0 {
        return 1;
    }
    let across = (cols / w).max(1);
    let down = (rows / h).max(1);
    across as usize * down as usize
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

/// Finds every QR code in `img`. Used after the link is up, when the sender
/// may be tiling several DATA frames into one screen.
pub fn decode_all(img: &GrayImage) -> Vec<Decoded> {
    let mut hints = rxing::DecodeHints::default();
    hints.PossibleFormats = Some(HashSet::from([rxing::BarcodeFormat::QR_CODE]));
    let Ok(results) = rxing::helpers::detect_multiple_in_luma_with_hints(
        img.as_raw().clone(),
        img.width(),
        img.height(),
        &mut hints,
    ) else {
        return Vec::new();
    };
    results
        .into_iter()
        .map(|result| {
            let points = result
                .getPoints()
                .iter()
                .map(|point| (point.x, point.y))
                .collect();
            (result.getRawBytes().to_vec(), points)
        })
        .collect()
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
    fn a_full_data_chunk_fits_every_supported_version() {
        for version in SUPPORTED_VERSIONS {
            let cap = frame::qr_byte_capacity(version);
            let chunk = vec![0xA5u8; frame::data_chunk_size(version)];
            let bytes = frame::encode(&frame::Payload::Data { seq: 0, chunk }).unwrap();
            assert!(
                bytes.len() <= cap,
                "v{version}: DATA envelope {} > capacity {cap}",
                bytes.len()
            );
            encode_version(&bytes, version).unwrap_or_else(|err| {
                panic!("v{version} full DATA chunk must encode, got {err}")
            });
            encode_version(&vec![0xFFu8; cap], version).unwrap_or_else(|err| {
                panic!("v{version} capacity {cap} must encode, got {err}")
            });
            ensure_full_data_frame_fits(version).unwrap_or_else(|err| {
                panic!("v{version} handshake preflight must pass, got {err}")
            });
            assert!(
                encode_version(&vec![0xFFu8; cap + 1], version).is_err(),
                "v{version} must reject capacity+1"
            );
        }
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

    #[test]
    fn two_v10_codes_fit_side_by_side_on_a_wide_pane() {
        let (w, h) = cell_size_for_version(10);
        assert_eq!(tiles_for_area(w, h, 10), 1);
        assert_eq!(tiles_for_area(w * 2, h, 10), 2);
        assert_eq!(tiles_for_area(w * 2, h * 2, 10), 4);
        assert_eq!(tiles_for_area(w - 1, h, 10), 1);
    }

    fn blit(dst: &mut GrayImage, src: &GrayImage, x0: u32, y0: u32) {
        for y in 0..src.height() {
            for x in 0..src.width() {
                dst.put_pixel(x0 + x, y0 + y, *src.get_pixel(x, y));
            }
        }
    }

    #[test]
    fn decode_all_finds_two_codes_on_one_canvas() {
        let left = frame::encode(&frame::Payload::Ok).unwrap();
        let right = frame::encode(&frame::Payload::Fin { sha256: [9; 32] }).unwrap();
        let a = encode_version(&left, 10).unwrap();
        let b = encode_version(&right, 10).unwrap();
        let gap = 48;
        let mut canvas = GrayImage::from_pixel(a.width() + gap + b.width(), a.height(), Luma([255]));
        blit(&mut canvas, &a, 0, 0);
        blit(&mut canvas, &b, a.width() + gap, 0);
        let found: HashSet<Vec<u8>> = decode_all(&canvas).into_iter().map(|(bytes, _)| bytes).collect();
        assert!(found.contains(&left), "missing left code, got {found:?}");
        assert!(found.contains(&right), "missing right code, got {found:?}");
    }
}
