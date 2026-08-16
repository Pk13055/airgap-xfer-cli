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

/// Decodes a QR code image, preserving its binary payload.
pub fn decode_image(img: &GrayImage) -> Result<Vec<u8>> {
    rxing::helpers::detect_in_luma(
        img.as_raw().clone(),
        img.width(),
        img.height(),
        Some(rxing::BarcodeFormat::QR_CODE),
    )
    .map(|result| result.getRawBytes().to_vec())
    .map_err(|_| Error::BadFrame)
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
