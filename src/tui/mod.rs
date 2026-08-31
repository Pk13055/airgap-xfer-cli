//! A ratatui front end for the optical link.
//!
//! Terminals differ wildly in size, font, and colour theme, and a QR code is
//! unforgiving about all three. Rendering through a fixed TUI layout instead
//! of raw escape sequences gives both peers the same picture: a code drawn at
//! exactly one cell per module on a forced white ground, a camera preview to
//! aim with, and a transcript of the phases.
//!
//! The transfer is not turn-based after aiming. Enter is required to confirm
//! that the camera is tracking the other terminal, and once more on the sender
//! to start the file. Handshake probes, ACKs, and the receive side then run
//! unattended.

pub mod camera;
pub mod optical;
pub mod widget;

use std::{
    io::{self, Stdout},
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use image::GrayImage;
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};

use crate::{
    live, qr,
    tui::{
        camera::CameraFeed,
        optical::{GateReply, TuiOptical, UiEvent},
        widget::{PreviewWidget, QrWidget},
    },
    Error, Result,
};

/// Rows the frame around the QR pane can never give up: the title line and a
/// single line for the prompt. Everything else — the QR block's border, the
/// footer hints, the prompt's own border, the transcript — is dropped in that
/// order as the terminal gets shorter, because the QR code is the one element
/// that cannot shrink without becoming undecodable.
const FLOOR_CHROME_ROWS: u16 = 2;
/// Rows the transcript takes when there is room to spare.
const LOG_ROWS: u16 = 6;
/// Rows (and columns) a bordered block consumes.
const BORDER: u16 = 2;
/// Below this the preview is dropped and the QR pane takes the full width.
const MIN_PREVIEW_COLS: u16 = 24;
/// How many transcript lines to keep.
const LOG_LINES: usize = 200;

/// Largest QR version this terminal can display in full, given its size.
///
/// Measured against [`FLOOR_CHROME_ROWS`] rather than the full frame: the
/// layout sheds its borders and transcript to make room, so refusing a code
/// that fits the stripped-down frame would reject terminals that work fine.
pub fn max_qr_version(cols: u16, rows: u16) -> Result<u8> {
    let pane_rows = rows.saturating_sub(FLOOR_CHROME_ROWS);
    qr::max_version_for_area(cols, pane_rows).ok_or_else(|| {
        let (need_cols, need_rows) = qr::cell_size_for_version(qr::smallest_version());
        Error::TerminalTooSmall {
            need_cols,
            need_rows: need_rows + FLOOR_CHROME_ROWS,
            have_cols: cols,
            have_rows: rows,
        }
    })
}

/// How much of the frame this terminal can afford around a `qr_rows` x
/// `qr_cols` code, in rows (columns only for the QR block's border).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Chrome {
    /// Border around the QR pane: 0 or [`BORDER`].
    qr_border: u16,
    /// Transcript height including its border, or 0.
    log: u16,
    /// Prompt height: 1 bare, or 1 + [`BORDER`] when boxed.
    prompt: u16,
    /// Key-hint footer: 0 or 1.
    footer: u16,
}

impl Chrome {
    /// Adds decoration in descending order of usefulness, stopping as soon as
    /// the next piece would eat into the QR code.
    ///
    /// The transcript comes first because it is the only piece that carries
    /// information the operator cannot get elsewhere; the title line already
    /// names the frame on screen, and the key hints fold into the prompt when
    /// there is no footer row, so both of those are decoration.
    fn fit(width: u16, height: u16, qr_cols: u16, qr_rows: u16) -> Self {
        let mut chrome = Chrome {
            qr_border: 0,
            log: 0,
            prompt: 1,
            footer: 0,
        };
        let mut spare = height.saturating_sub(FLOOR_CHROME_ROWS + qr_rows);

        // A boxed transcript needs its border plus at least one line.
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
        // Anything still going spare grows the transcript back up.
        if chrome.log > 0 {
            chrome.log += spare.min(LOG_ROWS.saturating_sub(chrome.log));
        }
        chrome
    }
}

struct Ui {
    title: String,
    camera_index: u32,
    /// The largest code this terminal agreed to display. The frame is sized
    /// for it from the first draw, so the layout does not lurch when the first
    /// QR appears or when a smaller version is negotiated.
    max_version: u8,
    image: Option<Arc<GrayImage>>,
    invert: bool,
    label: String,
    status: String,
    log: Vec<String>,
    gate: Option<(String, mpsc::Sender<GateReply>, bool)>,
    done: Option<String>,
}

impl Ui {
    fn push_log(&mut self, line: String) {
        self.log.push(line);
        if self.log.len() > LOG_LINES {
            self.log.remove(0);
        }
    }
}

/// Runs `work` on a protocol thread with the TUI on the main thread.
///
/// `work` receives an [`Optical`](crate::optical::Optical) bound to this
/// display and the largest QR version the terminal can draw, and returns the
/// line to leave on screen when it finishes.
pub fn run<F>(title: &str, camera_index: u32, no_invert: bool, work: F) -> Result<()>
where
    F: FnOnce(TuiOptical, u8) -> Result<String> + Send + 'static,
{
    live::install_ctrlc_handler();

    let (cols, rows) = crossterm::terminal::size()?;
    let max_version = max_qr_version(cols, rows)?;

    // Open the camera before taking over the screen so a permission prompt or
    // device error is readable in the normal terminal.
    let feed = Arc::new(CameraFeed::open(camera_index)?);

    let (events_tx, events_rx) = mpsc::channel::<UiEvent>();
    let worker_feed = Arc::clone(&feed);
    let worker = std::thread::spawn(move || {
        let mut optical = TuiOptical::new(worker_feed, events_tx.clone(), no_invert);
        let confirmed = {
            use crate::optical::Optical;
            optical.gate_locked(&format!(
                "Aim camera {camera_index} at the other terminal until tracking locks, then press Enter"
            ))
        };
        let result = match confirmed {
            Ok(true) => work(optical, max_version),
            Ok(false) => Err(Error::Aborted),
            Err(err) => Err(err),
        };
        let summary = match &result {
            Ok(summary) => format!("done — {summary}"),
            Err(err) => format!("failed — {err}"),
        };
        let _ = events_tx.send(UiEvent::Finished { summary });
        result
    });

    let mut terminal = enter()?;
    let ui_result = event_loop(
        &mut terminal,
        &feed,
        Ui {
            title: title.to_string(),
            camera_index,
            max_version,
            image: None,
            invert: no_invert,
            label: String::new(),
            status: String::new(),
            log: Vec::new(),
            gate: None,
            done: None,
        },
        &events_rx,
    );
    leave(&mut terminal);

    // Drop the receiver before joining. If the operator quit while a frame or
    // a gate was in flight, the protocol thread is parked on the reply channel
    // for a message the UI will never answer; dropping the queue disconnects
    // those senders and lets it unwind instead of deadlocking the join.
    drop(events_rx);

    let work_result = worker.join().unwrap_or_else(|_| {
        Err(Error::Message("protocol thread panicked".into()))
    });
    // The protocol's own error is the interesting one; a UI error only
    // matters if the protocol somehow succeeded anyway.
    work_result.and_then(|summary| {
        ui_result?;
        println!("{summary}");
        Ok(())
    })
}

fn enter() -> Result<Terminal<ratatui::backend::CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;

    // A panic between here and `leave` would otherwise leave the operator in
    // raw mode on the alternate screen with no cursor.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));

    Ok(Terminal::new(ratatui::backend::CrosstermBackend::new(
        io::stdout(),
    ))?)
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

fn leave<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) {
    restore();
    let _ = terminal.show_cursor();
}

fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    feed: &CameraFeed,
    mut ui: Ui,
    events: &mpsc::Receiver<UiEvent>,
) -> Result<()> {
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    loop {
        let mut dirty = false;

        while event::poll(Duration::ZERO)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                dirty = true;
                let quit = matches!(key.code, KeyCode::Char('q'))
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c')));
                if ui.done.is_some() {
                    return Ok(());
                }
                if quit {
                    // Quitting is an interrupt, not a decline: leave the gate
                    // unanswered so the protocol thread unwinds through
                    // `check_interrupted` and the process exits 130.
                    live::interrupt();
                    ui.gate = None;
                    return Ok(());
                }
                match key.code {
                    KeyCode::Enter => {
                        let allow = match &ui.gate {
                            Some((_, _, true)) => feed.locked(),
                            Some((_, _, false)) => true,
                            None => false,
                        };
                        if allow {
                            if let Some((_, reply, _)) = ui.gate.take() {
                                let _ = reply.send(GateReply::Proceed);
                            }
                        }
                    }
                    KeyCode::Esc => {
                        if let Some((_, reply, _)) = ui.gate.take() {
                            let _ = reply.send(GateReply::Abort);
                        }
                    }
                    _ => {}
                }
            } else {
                dirty = true;
            }
        }

        // Drain messages, but stop at a Show: its acknowledgement must follow
        // the draw that actually put it on screen.
        let mut pending_draw_ack = None;
        loop {
            match events.try_recv() {
                Ok(UiEvent::Show {
                    image,
                    invert,
                    label,
                    drawn,
                }) => {
                    ui.image = Some(image);
                    ui.invert = invert;
                    ui.label = label;
                    pending_draw_ack = Some(drawn);
                    dirty = true;
                    break;
                }
                Ok(UiEvent::Status(status)) => {
                    ui.status = status;
                    dirty = true;
                }
                Ok(UiEvent::Log(line)) => {
                    ui.push_log(line);
                    dirty = true;
                }
                Ok(UiEvent::Gate {
                    prompt,
                    reply,
                    require_lock,
                }) => {
                    ui.gate = Some((prompt, reply, require_lock));
                    dirty = true;
                }
                Ok(UiEvent::Finished { summary }) => {
                    ui.push_log(summary.clone());
                    ui.done = Some(summary);
                    ui.gate = None;
                    dirty = true;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if ui.done.is_none() {
                        return Ok(());
                    }
                    break;
                }
            }
        }

        if dirty || last_draw.elapsed() >= Duration::from_millis(80) {
            let preview = feed.preview();
            let status = FeedStatus {
                counters: feed.counters(),
                locked: feed.locked(),
            };
            terminal.draw(|frame| draw(frame, &ui, preview.as_deref(), status))?;
            last_draw = Instant::now();
        }
        if let Some(ack) = pending_draw_ack {
            let _ = ack.send(());
        }
        if !dirty {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

/// What the camera thread has to say about itself, for the title line.
#[derive(Clone, Copy)]
struct FeedStatus {
    counters: (u64, u64),
    locked: bool,
}

fn draw(
    frame: &mut ratatui::Frame,
    ui: &Ui,
    preview: Option<&GrayImage>,
    status: FeedStatus,
) {
    let area = frame.area();
    let (qr_cols, qr_rows) = qr::cell_size_for_version(ui.max_version);
    let chrome = Chrome::fit(area.width, area.height, qr_cols, qr_rows);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(qr_rows + chrome.qr_border),
            Constraint::Length(chrome.log),
            Constraint::Length(chrome.prompt),
            Constraint::Length(chrome.footer),
        ])
        .split(area);

    // The title line always carries what is on screen, because the QR block's
    // own title disappears with its border on a short terminal.
    let (frames, decodes) = status.counters;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} ", ui.title),
                Style::new()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("  cam {}  ·  ", ui.camera_index)),
            // Whether the peer's screen has been located is the single most
            // useful thing to know while aiming a laptop lid.
            if status.locked {
                Span::styled("● tracking", Style::new().fg(Color::Green))
            } else {
                Span::styled("○ searching", Style::new().fg(Color::Yellow))
            },
            Span::raw(format!(
                "  ·  {frames} frames  ·  {decodes} decodes{}",
                if ui.label.is_empty() {
                    String::new()
                } else {
                    format!("  ·  showing {}", ui.label)
                }
            )),
        ])),
        rows[0],
    );

    // Give the QR pane exactly the cells its code needs; whatever is left goes
    // to the preview, which is dropped entirely when the terminal is narrow.
    let needed_cols = qr_cols + chrome.qr_border;
    let spare = rows[1].width.saturating_sub(needed_cols);
    let middle = if spare >= MIN_PREVIEW_COLS {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(needed_cols), Constraint::Min(0)])
            .split(rows[1])
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0)])
            .split(rows[1])
    };

    let qr_area = if chrome.qr_border > 0 {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" showing {} ", ui.label));
        let inner = block.inner(middle[0]);
        frame.render_widget(block, middle[0]);
        inner
    } else {
        middle[0]
    };
    frame.render_widget(
        QrWidget {
            image: ui.image.as_deref(),
            invert: ui.invert,
        },
        qr_area,
    );

    if middle.len() > 1 {
        let preview_area = if chrome.qr_border > 0 {
            let block = Block::default().borders(Borders::ALL).title(" camera ");
            let inner = block.inner(middle[1]);
            frame.render_widget(block, middle[1]);
            inner
        } else {
            middle[1]
        };
        frame.render_widget(PreviewWidget { frame: preview }, preview_area);
    }

    if chrome.log > 0 {
        let visible = chrome.log.saturating_sub(BORDER) as usize;
        let start = ui.log.len().saturating_sub(visible);
        frame.render_widget(
            Paragraph::new(
                ui.log[start..]
                    .iter()
                    .map(|line| Line::raw(line.clone()))
                    .collect::<Vec<_>>(),
            )
            .block(Block::default().borders(Borders::ALL).title(" transcript ")),
            rows[2],
        );
    }

    let hints = match (&ui.done, &ui.gate) {
        (Some(_), _) => "any key: exit",
        (None, Some((_, _, true))) if !status.locked => {
            "keep aiming until ● tracking   Esc: abort   q: quit"
        }
        (None, Some(_)) => "Enter: continue   Esc: abort   q: quit",
        (None, None) => "q: quit",
    };
    let (prompt_style, mut prompt) = match (&ui.done, &ui.gate) {
        (Some(summary), _) => (Style::new().fg(Color::Cyan), summary.clone()),
        (None, Some((_, _, true))) if !status.locked => (
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            "▶ Aim at the other terminal until tracking locks, then press Enter".into(),
        ),
        (None, Some((text, _, true))) if status.locked => (
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            format!("▶ Tracking locked. {text}"),
        ),
        (None, Some((text, _, _))) => (
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            format!("▶ {text}"),
        ),
        (None, None) => (
            Style::new().fg(Color::DarkGray),
            if ui.status.is_empty() {
                "working…".to_string()
            } else {
                ui.status.clone()
            },
        ),
    };
    if chrome.footer == 0 {
        // No footer row to spare: fold the key hints into the prompt so the
        // operator can still see that Enter is what moves things along.
        prompt = format!("{prompt}   [{hints}]");
    }
    let prompt_widget = Paragraph::new(prompt)
        .style(prompt_style)
        .wrap(Wrap { trim: true });
    frame.render_widget(
        if chrome.prompt > 1 {
            prompt_widget.block(Block::default().borders(Borders::ALL).title(" prompt "))
        } else {
            prompt_widget
        },
        rows[3],
    );

    if chrome.footer > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(hints, Style::new().fg(Color::DarkGray))),
            rows[4],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_qr_version_only_reserves_rows_the_layout_cannot_give_up() {
        let (cols, rows) = qr::cell_size_for_version(10);
        assert_eq!((cols, rows), (65, 33));
        assert_eq!(max_qr_version(cols, rows + FLOOR_CHROME_ROWS).unwrap(), 10);

        // The reported failure: 148x38 is short of the fully decorated frame
        // but has ample room for a v10 code once the chrome collapses.
        assert_eq!(max_qr_version(148, 38).unwrap(), 10);
    }

    #[test]
    fn max_qr_version_reports_the_minimum_terminal_size() {
        let err = max_qr_version(20, 10).unwrap_err();
        match err {
            Error::TerminalTooSmall {
                need_cols,
                need_rows,
                have_cols,
                have_rows,
            } => {
                let (cols, rows) = qr::cell_size_for_version(qr::smallest_version());
                assert_eq!((need_cols, need_rows), (cols, rows + FLOOR_CHROME_ROWS));
                assert_eq!((have_cols, have_rows), (20, 10));
            }
            other => panic!("expected TerminalTooSmall, got {other:?}"),
        }
    }

    #[test]
    fn chrome_sheds_decoration_from_the_outside_in_and_never_squeezes_the_code() {
        let (cols, rows) = qr::cell_size_for_version(10);

        // Roomy: everything on.
        let roomy = Chrome::fit(200, 60, cols, rows);
        assert_eq!(roomy.qr_border, BORDER);
        assert_eq!(roomy.footer, 1);
        assert_eq!(roomy.prompt, 1 + BORDER);
        assert_eq!(roomy.log, LOG_ROWS);

        // The reported terminal has three rows to spare: they go to the
        // transcript rather than to a border, because only the transcript
        // says anything the rest of the frame does not.
        let reported = Chrome::fit(148, 38, cols, rows);
        assert_eq!(reported.log, BORDER + 1);
        assert_eq!(reported.qr_border, 0);
        assert_eq!(reported.footer, 0);
        assert_eq!(reported.prompt, 1);

        // Exactly at the floor: nothing but the title and one prompt line.
        let tight = Chrome::fit(cols, rows + FLOOR_CHROME_ROWS, cols, rows);
        assert_eq!(
            tight,
            Chrome {
                qr_border: 0,
                log: 0,
                prompt: 1,
                footer: 0
            }
        );

        // Wide but short: the border costs rows it does not have.
        assert_eq!(Chrome::fit(400, rows + FLOOR_CHROME_ROWS, cols, rows).qr_border, 0);
        // Tall but narrow: the border costs columns it does not have.
        assert_eq!(Chrome::fit(cols, 100, cols, rows).qr_border, 0);
    }

    #[test]
    fn every_chrome_layout_leaves_the_code_its_full_height() {
        let (cols, rows) = qr::cell_size_for_version(10);
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
