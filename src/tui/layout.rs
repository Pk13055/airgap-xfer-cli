//! Where everything goes on screen, computed once and shared.
//!
//! The renderer and the protocol thread's tile allocation both read
//! [`plan`], so the number of DATA codes the sender pages through is exactly
//! the number the grid draws — never one more that would land clipped.
//! Outgoing codes always sit in a left-hand column; the side pane on the
//! right gets only columns the codes cannot use and is hidden, never
//! squeezed, when there are too few.

use std::sync::atomic::{AtomicU32, Ordering};

use ratatui::layout::Rect;

/// Rows the frame can never give up: the title line and one prompt line.
pub(crate) const FLOOR_CHROME_ROWS: u16 = 2;
/// Rows the transcript takes when there is room to spare.
const LOG_ROWS: u16 = 6;
/// Rows (and columns) a bordered block consumes.
pub(crate) const BORDER: u16 = 2;
/// Narrowest side pane worth drawing.
pub const MIN_SIDE_COLS: u16 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Before the link: one code at a time, camera preview alongside.
    Aiming,
    /// After the link: codes tiled, peer monitor alongside.
    Linked,
}

/// How many codes this side ever paints at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tiles {
    /// The receiver: one progress report at a time.
    One,
    /// The sender: as many DATA codes as fit.
    Fill,
}

/// How much decoration this terminal can afford around the codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    /// Border around the QR pane: 0 or [`BORDER`].
    pub qr_border: u16,
    /// Transcript height including its border, or 0.
    pub log: u16,
    /// Prompt height: 1 bare, or 1 + [`BORDER`] when boxed.
    pub prompt: u16,
    /// Key-hint footer: 0 or 1.
    pub footer: u16,
}

impl Chrome {
    /// Title and one prompt line, nothing else.
    pub fn bare() -> Self {
        Chrome {
            qr_border: 0,
            log: 0,
            prompt: 1,
            footer: 0,
        }
    }

    /// Adds decoration in descending order of usefulness, stopping as soon
    /// as the next piece would eat into the code.
    pub fn fit(width: u16, height: u16, qr_cols: u16, qr_rows: u16) -> Self {
        let mut chrome = Chrome::bare();
        let mut spare = height.saturating_sub(FLOOR_CHROME_ROWS + qr_rows);
        if spare > BORDER {
            chrome.log = BORDER + 1;
            spare -= BORDER + 1;
        }
        if spare >= BORDER && width >= qr_cols + BORDER {
            chrome.qr_border = BORDER;
            spare -= BORDER;
        }
        if spare >= 1 {
            chrome.footer = 1;
            spare -= 1;
        }
        if spare >= BORDER {
            chrome.prompt += BORDER;
            spare -= BORDER;
        }
        if chrome.log > 0 {
            chrome.log += spare.min(LOG_ROWS.saturating_sub(chrome.log));
        }
        chrome
    }
}

/// Codes packed row-major from the top-left of the QR pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileGrid {
    pub across: u16,
    pub down: u16,
    pub tile_cols: u16,
    pub tile_rows: u16,
}

impl TileGrid {
    pub fn count(&self) -> usize {
        usize::from(self.across) * usize::from(self.down)
    }

    /// Cell of tile `index` inside `area`.
    pub fn cell(&self, area: Rect, index: usize) -> Rect {
        let across = usize::from(self.across.max(1));
        let col = (index % across) as u16;
        let row = (index / across) as u16;
        Rect::new(
            area.x + col * self.tile_cols,
            area.y + row * self.tile_rows,
            self.tile_cols,
            self.tile_rows,
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub title: Rect,
    /// Where codes are drawn, inside any border.
    pub qr: Rect,
    /// The left column including its border.
    pub qr_frame: Rect,
    pub grid: TileGrid,
    /// Camera preview while aiming, peer monitor once linked; `None` when
    /// the terminal cannot spare [`MIN_SIDE_COLS`].
    pub side: Option<Rect>,
    pub log: Rect,
    pub prompt: Rect,
    pub footer: Rect,
    pub chrome: Chrome,
}

/// Lays out one frame for codes of `tile` cells (columns, rows).
pub fn plan(area: Rect, phase: Phase, tiles: Tiles, tile: (u16, u16)) -> Screen {
    let (tile_cols, tile_rows) = tile;
    let chrome = match phase {
        Phase::Aiming => Chrome::fit(area.width, area.height, tile_cols, tile_rows),
        Phase::Linked => Chrome::bare(),
    };

    let title_h = area.height.min(1);
    let below = area.height - title_h;
    let body_h = below - (chrome.log + chrome.prompt + chrome.footer).min(below);
    let title = Rect::new(area.x, area.y, area.width, title_h);
    let body = Rect::new(area.x, area.y + title_h, area.width, body_h);
    let mut y = body.bottom();
    let mut stack = |height: u16| {
        let height = height.min(area.bottom().saturating_sub(y));
        let rect = Rect::new(area.x, y, area.width, height);
        y += height;
        rect
    };
    let log = stack(chrome.log);
    let prompt = stack(chrome.prompt);
    let footer = stack(chrome.footer);

    let border = chrome.qr_border;
    let inner_w = body.width.saturating_sub(border);
    let inner_h = body.height.saturating_sub(border);
    let fit_across = inner_w.checked_div(tile_cols).unwrap_or(0);
    let fit_down = inner_h.checked_div(tile_rows).unwrap_or(0);
    let single = phase == Phase::Aiming || tiles == Tiles::One;
    let (want_across, want_down) = if single {
        (fit_across.min(1), fit_down.min(1))
    } else {
        (fit_across, fit_down)
    };
    // A pane too short for even one row of codes has no code column at all,
    // so it must not hand a side pane the space either.
    let want_across = if want_down == 0 { 0 } else { want_across };
    let (across, side_w) = split_columns(body.width, border, tile_cols, want_across, !single);
    let grid = TileGrid {
        across,
        down: if across == 0 { 0 } else { want_down },
        tile_cols,
        tile_rows,
    };

    let frame_w = body.width - side_w.unwrap_or(0);
    let qr_frame = Rect::new(body.x, body.y, frame_w, body.height);
    let inset = border / 2;
    let qr = Rect::new(
        qr_frame.x + inset,
        qr_frame.y + inset,
        qr_frame.width.saturating_sub(border),
        qr_frame.height.saturating_sub(border),
    );
    let side = side_w.map(|width| Rect::new(body.x + frame_w, body.y, width, body.height));
    Screen {
        title,
        qr,
        qr_frame,
        grid,
        side,
        log,
        prompt,
        footer,
        chrome,
    }
}

/// Splits the body between the code column and the side pane. Codes come
/// first; the side pane gets only what is left over, except that a sender
/// filling three or more columns gives one up so the operator can still
/// watch the peer — never more than a third of the codes.
fn split_columns(
    width: u16,
    border: u16,
    tile_cols: u16,
    across: u16,
    may_yield: bool,
) -> (u16, Option<u16>) {
    if across == 0 {
        return (0, None);
    }
    let leftover = width.saturating_sub(across * tile_cols + border);
    if leftover >= MIN_SIDE_COLS {
        return (across, Some(leftover));
    }
    if may_yield && across >= 3 {
        return (across - 1, Some(leftover + tile_cols));
    }
    (across, None)
}

/// DATA codes the sender should page per dwell on a screen of `area`:
/// exactly what [`plan`] draws once linked, and never zero.
pub fn data_tiles(area: Rect, tiles: Tiles, version: u8) -> usize {
    plan(
        area,
        Phase::Linked,
        tiles,
        crate::qr::cell_size_for_version(version),
    )
    .grid
    .count()
    .max(1)
}

/// The terminal size the UI last drew at, published for the protocol thread
/// so its tile count is computed from the same area the renderer used.
#[derive(Debug, Default)]
pub struct ScreenSize(AtomicU32);

impl ScreenSize {
    pub fn set(&self, area: Rect) {
        self.0.store(
            (u32::from(area.width) << 16) | u32::from(area.height),
            Ordering::Relaxed,
        );
    }

    pub fn get(&self) -> Option<Rect> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            packed => Some(Rect::new(
                0,
                0,
                (packed >> 16) as u16,
                (packed & 0xffff) as u16,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qr::{cell_size_for_version, SUPPORTED_VERSIONS};

    fn sizes() -> impl Iterator<Item = (u16, u16)> {
        (40..=330u16)
            .step_by(7)
            .flat_map(|w| (20..=100u16).step_by(4).map(move |h| (w, h)))
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.right() <= outer.right()
            && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn no_layout_ever_clips_a_code_or_lets_the_side_pane_overlap_it() {
        for (w, h) in sizes() {
            let area = Rect::new(0, 0, w, h);
            for version in SUPPORTED_VERSIONS {
                let tile = cell_size_for_version(version);
                for phase in [Phase::Aiming, Phase::Linked] {
                    for tiles in [Tiles::One, Tiles::Fill] {
                        let s = plan(area, phase, tiles, tile);
                        let why = format!("{w}x{h} v{version} {phase:?} {tiles:?}: {s:?}");
                        assert!(s.grid.across * tile.0 <= s.qr.width, "{why}");
                        assert!(s.grid.down * tile.1 <= s.qr.height, "{why}");
                        for index in 0..s.grid.count() {
                            assert!(inside(s.qr, s.grid.cell(s.qr, index)), "{why}");
                        }
                        assert_eq!(s.qr_frame.x, area.x, "codes stay in the left column: {why}");
                        if let Some(side) = s.side {
                            assert!(side.x >= s.qr_frame.right(), "{why}");
                            assert!(side.width >= MIN_SIDE_COLS, "{why}");
                            assert!(s.grid.count() > 0, "{why}");
                        }
                        for rect in [s.title, s.qr_frame, s.log, s.prompt, s.footer] {
                            assert!(inside(area, rect), "{why}");
                        }
                        if phase == Phase::Aiming || tiles == Tiles::One {
                            assert!(s.grid.count() <= 1, "{why}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn narrow_terminals_hide_the_side_pane_rather_than_squeeze_the_code() {
        let tile = cell_size_for_version(10);
        let narrow = plan(
            Rect::new(0, 0, tile.0 + MIN_SIDE_COLS - 1, tile.1 + 2),
            Phase::Aiming,
            Tiles::Fill,
            tile,
        );
        assert!(narrow.side.is_none());
        assert_eq!(narrow.grid.count(), 1);
        let wide_enough = plan(
            Rect::new(0, 0, tile.0 + MIN_SIDE_COLS, tile.1 + 2),
            Phase::Aiming,
            Tiles::Fill,
            tile,
        );
        assert_eq!(wide_enough.side.map(|r| r.width), Some(MIN_SIDE_COLS));
    }

    #[test]
    fn a_sender_gives_up_a_third_column_of_codes_for_the_monitor_but_never_its_second() {
        let tile = cell_size_for_version(10);
        let three = plan(Rect::new(0, 0, 200, 70), Phase::Linked, Tiles::Fill, tile);
        assert_eq!((three.grid.across, three.grid.down), (2, 2));
        assert_eq!(three.side.map(|r| r.width), Some(70));
        let two = plan(Rect::new(0, 0, 140, 70), Phase::Linked, Tiles::Fill, tile);
        assert_eq!(two.grid.across, 2);
        assert!(two.side.is_none());
    }

    #[test]
    fn the_receiver_keeps_one_code_and_room_to_watch_the_sender() {
        let s = plan(
            Rect::new(0, 0, 148, 38),
            Phase::Linked,
            Tiles::One,
            cell_size_for_version(10),
        );
        assert_eq!(s.grid.count(), 1);
        assert_eq!(s.side.map(|r| r.width), Some(148 - 65));
    }

    #[test]
    fn data_tiles_is_exactly_what_the_linked_plan_draws() {
        for (w, h) in sizes() {
            let area = Rect::new(0, 0, w, h);
            for version in SUPPORTED_VERSIONS {
                let drawn = plan(
                    area,
                    Phase::Linked,
                    Tiles::Fill,
                    cell_size_for_version(version),
                )
                .grid
                .count();
                assert_eq!(data_tiles(area, Tiles::Fill, version), drawn.max(1));
                assert!(data_tiles(area, Tiles::One, version) <= 1);
            }
        }
    }

    #[test]
    fn the_published_screen_size_round_trips() {
        let size = ScreenSize::default();
        assert_eq!(size.get(), None);
        size.set(Rect::new(0, 0, 148, 38));
        assert_eq!(size.get(), Some(Rect::new(0, 0, 148, 38)));
    }

    #[test]
    fn chrome_sheds_decoration_from_the_outside_in_and_never_squeezes_the_code() {
        let (cols, rows) = cell_size_for_version(10);
        let roomy = Chrome::fit(200, 60, cols, rows);
        assert_eq!(roomy.qr_border, BORDER);
        assert_eq!(roomy.footer, 1);
        assert_eq!(roomy.prompt, 1 + BORDER);
        assert_eq!(roomy.log, LOG_ROWS);
        let reported = Chrome::fit(148, 38, cols, rows);
        assert_eq!(reported.log, BORDER + 1);
        assert_eq!(reported.qr_border, 0);
        assert_eq!(reported.footer, 0);
        assert_eq!(reported.prompt, 1);
        assert_eq!(
            Chrome::fit(cols, rows + FLOOR_CHROME_ROWS, cols, rows),
            Chrome::bare()
        );
        assert_eq!(
            Chrome::fit(400, rows + FLOOR_CHROME_ROWS, cols, rows).qr_border,
            0
        );
        assert_eq!(Chrome::fit(cols, 100, cols, rows).qr_border, 0);
    }

    #[test]
    fn every_chrome_layout_leaves_the_code_its_full_height() {
        let (cols, rows) = cell_size_for_version(10);
        for height in (rows + FLOOR_CHROME_ROWS)..=(rows + 30) {
            let chrome = Chrome::fit(148, height, cols, rows);
            let used = 1 + chrome.log + chrome.prompt + chrome.footer + chrome.qr_border;
            assert!(
                used + rows <= height,
                "chrome {chrome:?} overflows a {height}-row terminal"
            );
        }
    }
}
