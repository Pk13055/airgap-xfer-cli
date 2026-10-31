//! The peer monitor: a fuzzy ASCII view of what this camera actually sees of
//! the other screen, once the link is up.
//!
//! It is drawn from the rectified camera crop, never from the decoded
//! payload, so glare, blur and misalignment show as they are. It is
//! deliberately *not* a QR code: blurred ASCII density on the terminal's own
//! background, so the peer's camera — pointed straight at this screen — can
//! never decode it as one of our codes.
//!
//! Its tint changes only when the camera decodes a payload it has not just
//! seen, so a code held on screen does not look like progress. When tracking
//! drops or decodes stop, the last crop stays up, dimmed and labelled STALE.

use std::time::{Duration, Instant};

use image::GrayImage;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::Widget,
};

use crate::tui::camera::PeerCrop;

/// Density ramp, lightest first.
pub const RAMP: &[u8] = b" .:-=+*#%@";
/// A crop older than this, or any crop while tracking is lost, is stale.
pub const STALE_AFTER: Duration = Duration::from_millis(1200);
/// Blur applied before sampling, so modules smear into each other.
const BLUR_SIGMA: f32 = 1.2;
/// Tallest the view is drawn; beyond this it only gets coarser to look at.
const MAX_ROWS: u16 = 24;
const TINTS: [Color; 6] = [
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::LightBlue,
    Color::LightRed,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    Live,
    Stale,
}

pub fn freshness(crop_at: Instant, now: Instant, locked: bool) -> Freshness {
    if locked && now.saturating_duration_since(crop_at) <= STALE_AFTER {
        Freshness::Live
    } else {
        Freshness::Stale
    }
}

/// The tint for a crop taken at `epoch`.
pub fn tint(epoch: u64) -> Color {
    TINTS[(epoch % TINTS.len() as u64) as usize]
}

/// Blurred, area-averaged ASCII rendering of `crop` in at most `cols` x
/// `rows` cells, kept square on screen (cells are about twice as tall as
/// they are wide).
pub fn ascii_rows(crop: &GrayImage, cols: u16, rows: u16) -> Vec<String> {
    let rows = rows.min(cols / 2).min(MAX_ROWS);
    let cols = rows * 2;
    if rows == 0 || crop.width() == 0 || crop.height() == 0 {
        return Vec::new();
    }
    let soft = image::imageops::blur(crop, BLUR_SIGMA);
    let (w, h) = soft.dimensions();
    let (rows, cols) = (u32::from(rows), u32::from(cols));
    (0..rows)
        .map(|row| {
            let y0 = row * h / rows;
            let y1 = ((row + 1) * h / rows).max(y0 + 1).min(h);
            (0..cols)
                .map(|col| {
                    let x0 = col * w / cols;
                    let x1 = ((col + 1) * w / cols).max(x0 + 1).min(w);
                    let mut sum = 0u32;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            sum += u32::from(soft.get_pixel(x, y)[0]);
                        }
                    }
                    let mean = sum / ((x1 - x0) * (y1 - y0));
                    let darkness = 255 - mean as usize;
                    RAMP[darkness * (RAMP.len() - 1) / 255] as char
                })
                .collect()
        })
        .collect()
}

fn clip(text: &str, width: u16) -> String {
    text.chars().take(usize::from(width)).collect()
}

pub struct MonitorWidget<'a> {
    pub crop: Option<&'a PeerCrop>,
    pub locked: bool,
    pub now: Instant,
}

impl Widget for MonitorWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(crop) = self.crop else {
            buf.set_string(
                area.x,
                area.y,
                clip("peer view · waiting for the first decode", area.width),
                Style::new().fg(Color::DarkGray),
            );
            return;
        };
        let (label, label_style, body_style) = match freshness(crop.at, self.now, self.locked) {
            Freshness::Live => (
                "● peer view · live".to_string(),
                Style::new()
                    .fg(tint(crop.epoch))
                    .add_modifier(Modifier::BOLD),
                Style::new().fg(tint(crop.epoch)),
            ),
            Freshness::Stale => (
                format!(
                    "◌ STALE {}s · reacquiring",
                    self.now.saturating_duration_since(crop.at).as_secs()
                ),
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
                Style::new().fg(Color::DarkGray).add_modifier(Modifier::DIM),
            ),
        };
        buf.set_string(area.x, area.y, clip(&label, area.width), label_style);
        let rows = ascii_rows(&crop.image, area.width, area.height.saturating_sub(1));
        let width = rows.first().map_or(0, |row| row.len() as u16);
        let x = area.x + (area.width - width) / 2;
        for (i, line) in rows.iter().enumerate() {
            buf.set_string(x, area.y + 1 + i as u16, line, body_style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr;
    use image::Luma;
    use std::sync::Arc;

    fn crop(value: u8, epoch: u64, at: Instant) -> PeerCrop {
        PeerCrop {
            image: Arc::new(GrayImage::from_pixel(96, 96, Luma([value]))),
            epoch,
            at,
        }
    }

    #[test]
    fn the_rendering_is_ascii_density_never_a_scannable_block_code() {
        let code = qr::encode_version(b"AX-monitor", 10).unwrap();
        let rows = ascii_rows(&code, 60, 30);
        assert_eq!(rows.len(), 24);
        assert!(rows.iter().all(|row| row.len() == 48));
        assert!(rows
            .iter()
            .flat_map(|row| row.bytes())
            .all(|b| RAMP.contains(&b)));
        assert!(!rows.concat().chars().any(|c| matches!(c, '█' | '▀' | '▄')));
    }

    #[test]
    fn dark_pixels_draw_dense_and_light_pixels_draw_blank() {
        let dark = GrayImage::from_pixel(96, 96, Luma([0]));
        let light = GrayImage::from_pixel(96, 96, Luma([255]));
        assert!(ascii_rows(&dark, 20, 10).concat().chars().all(|c| c == '@'));
        assert!(ascii_rows(&light, 20, 10)
            .concat()
            .chars()
            .all(|c| c == ' '));
        assert!(ascii_rows(&dark, 1, 10).is_empty(), "no room for a square");
    }

    #[test]
    fn the_tint_changes_only_when_the_epoch_does() {
        assert_eq!(tint(3), tint(3));
        assert_ne!(tint(3), tint(4));
    }

    #[test]
    fn the_view_goes_stale_when_tracking_is_lost_or_decodes_stop() {
        let t0 = Instant::now();
        assert_eq!(
            freshness(t0, t0 + Duration::from_millis(100), true),
            Freshness::Live
        );
        assert_eq!(
            freshness(t0, t0 + Duration::from_millis(100), false),
            Freshness::Stale
        );
        assert_eq!(
            freshness(t0, t0 + STALE_AFTER + Duration::from_millis(1), true),
            Freshness::Stale
        );
    }

    #[test]
    fn a_stale_view_keeps_the_last_crop_dimmed_and_says_so() {
        let t0 = Instant::now();
        let last = crop(0, 5, t0);
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        MonitorWidget {
            crop: Some(&last),
            locked: false,
            now: t0 + Duration::from_secs(3),
        }
        .render(area, &mut buf);
        let header: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            header.contains("STALE") && header.contains("reacquiring"),
            "{header}"
        );
        let body = &buf[(20, 2)];
        assert_eq!(body.symbol(), "@", "the last crop is still drawn");
        assert_eq!(body.fg, Color::DarkGray);
        assert!(body.modifier.contains(Modifier::DIM));
    }

    #[test]
    fn a_live_view_is_tinted_by_epoch() {
        let t0 = Instant::now();
        let current = crop(0, 5, t0);
        let area = Rect::new(0, 0, 40, 12);
        let mut buf = Buffer::empty(area);
        MonitorWidget {
            crop: Some(&current),
            locked: true,
            now: t0,
        }
        .render(area, &mut buf);
        assert_eq!(buf[(20, 2)].fg, tint(5));
        assert!(!buf[(20, 2)].modifier.contains(Modifier::DIM));
    }
}
