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

/// Renders two image rows per terminal row using Unicode block characters.
pub fn render_terminal(img: &GrayImage, invert: bool) -> String {
    let mut output = String::new();
    let width = img.width() as usize;
    let height = img.height() as usize;
    let side_quiet_zone = " ".repeat(width);

    output.push_str(&side_quiet_zone);
    output.push('\n');

    for y in (0..height).step_by(2) {
        output.push(' ');
        for x in 0..width {
            let upper_dark = is_dark(img.get_pixel(x as u32, y as u32)[0], invert);
            let lower_dark = y + 1 < height
                && is_dark(img.get_pixel(x as u32, (y + 1) as u32)[0], invert);
            output.push(match (upper_dark, lower_dark) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        output.push(' ');
        output.push('\n');
    }

    output.push_str(&side_quiet_zone);
    output.push('\n');
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
}
