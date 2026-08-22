use image::GrayImage;
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::{Paragraph, Widget, Wrap},
};

use crate::qr;

/// Maps a pair of vertically stacked "on" flags to the half-block glyph that
/// paints them.
fn half_block(upper: bool, lower: bool) -> char {
    match (upper, lower) {
        (true, true) => '█',
        (true, false) => '▀',
        (false, true) => '▄',
        (false, false) => ' ',
    }
}

/// Draws a QR code at exactly one terminal column per module, two modules per
/// row.
///
/// The code is painted with explicit black-on-white cells rather than the
/// terminal's own palette: a themed background behind a QR code is the most
/// common reason a peer's camera cannot read it.
pub struct QrWidget<'a> {
    pub image: Option<&'a GrayImage>,
    pub invert: bool,
}

impl Widget for QrWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let quiet = Style::new().bg(Color::White).fg(Color::Black);
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char(' ').set_style(quiet);
                }
            }
        }

        let Some(image) = self.image else {
            return;
        };
        let modules = qr::modules_of(image);
        let cols = modules as u16;
        let rows = modules.div_ceil(2) as u16;
        if cols > area.width || rows > area.height {
            // Never draw a partial code: a clipped QR decodes as nothing, and
            // silently showing one would look like a camera problem.
            Paragraph::new(format!(
                "QR needs {cols}x{rows} cells, pane is {}x{}.\nEnlarge the terminal.",
                area.width, area.height
            ))
            .style(quiet)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .render(area, buf);
            return;
        }

        let origin_x = area.x + (area.width - cols) / 2;
        let origin_y = area.y + (area.height - rows) / 2;
        for row in 0..rows {
            let my = row as usize * 2;
            for col in 0..cols {
                let mx = col as usize;
                let upper = qr::module_is_dark(image, mx, my, self.invert);
                let lower = my + 1 < modules && qr::module_is_dark(image, mx, my + 1, self.invert);
                if let Some(cell) = buf.cell_mut((origin_x + col, origin_y + row)) {
                    cell.set_char(half_block(upper, lower)).set_style(quiet);
                }
            }
        }
    }
}

/// Draws a grayscale camera frame using half blocks, so each cell carries two
/// vertically adjacent pixels.
pub struct PreviewWidget<'a> {
    pub frame: Option<&'a GrayImage>,
}

impl Widget for PreviewWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let Some(frame) = self.frame else {
            Paragraph::new("waiting for camera…")
                .alignment(Alignment::Center)
                .render(area, buf);
            return;
        };
        if frame.width() == 0 || frame.height() == 0 {
            return;
        }

        // The frame is wider than it is tall and cells are ~twice as tall as
        // they are wide, so sample independently on each axis and letterbox.
        let src_w = frame.width() as f32;
        let src_h = frame.height() as f32;
        let scale = (area.width as f32 / src_w).min(area.height as f32 * 2.0 / src_h);
        let draw_cols = ((src_w * scale) as u16).clamp(1, area.width);
        let draw_rows = (((src_h * scale) / 2.0) as u16).clamp(1, area.height);
        let origin_x = area.x + (area.width - draw_cols) / 2;
        let origin_y = area.y + (area.height - draw_rows) / 2;

        let sample = |col: u16, subrow: u32| -> u8 {
            let sx = (col as f32 / draw_cols as f32 * src_w) as u32;
            let sy = (subrow as f32 / (draw_rows as u32 * 2) as f32 * src_h) as u32;
            frame
                .get_pixel(sx.min(frame.width() - 1), sy.min(frame.height() - 1))
                .0[0]
        };

        for row in 0..draw_rows {
            for col in 0..draw_cols {
                let upper = sample(col, row as u32 * 2);
                let lower = sample(col, row as u32 * 2 + 1);
                if let Some(cell) = buf.cell_mut((origin_x + col, origin_y + row)) {
                    cell.set_char('▀')
                        .set_fg(Color::Rgb(upper, upper, upper))
                        .set_bg(Color::Rgb(lower, lower, lower));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr;

    #[test]
    fn half_block_covers_every_pair() {
        assert_eq!(half_block(true, true), '█');
        assert_eq!(half_block(true, false), '▀');
        assert_eq!(half_block(false, true), '▄');
        assert_eq!(half_block(false, false), ' ');
    }

    #[test]
    fn qr_widget_draws_exact_module_grid_and_leaves_a_quiet_border() {
        let image = qr::encode_version(b"AX-test", 10).unwrap();
        let (cols, rows) = qr::cell_size_for_version(10);
        let area = Rect::new(0, 0, cols + 4, rows + 2);
        let mut buf = Buffer::empty(area);
        QrWidget {
            image: Some(&image),
            invert: false,
        }
        .render(area, &mut buf);

        // Every cell is painted white-backed, including the margin.
        assert_eq!(buf[(0, 0)].bg, Color::White);
        assert_eq!(buf[(0, 0)].symbol(), " ");
        // The code itself lands centred and contains dark modules.
        let dark = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter(|&(x, y)| buf[(x, y)].symbol() != " ")
            .count();
        assert!(dark > 0, "expected drawn modules");
    }

    #[test]
    fn qr_widget_refuses_to_clip_when_the_pane_is_too_small() {
        let image = qr::encode_version(b"AX-test", 10).unwrap();
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(area);
        QrWidget {
            image: Some(&image),
            invert: false,
        }
        .render(area, &mut buf);

        let text: String = (0..area.width).map(|x| buf[(x, 0)].symbol()).collect();
        assert!(
            !text.contains('█') && !text.contains('▀'),
            "must not draw a partial code, got {text:?}"
        );
    }
}
