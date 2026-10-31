//! A ratatui front end for the optical link.
//!
//! Terminals differ wildly in size, font, and colour theme, and a QR code is
//! unforgiving about all three. Every frame is laid out by [`layout::plan`]:
//! outgoing codes in a left-hand column at exactly one cell per module on a
//! forced white ground, and a side pane on the right — the camera preview
//! while aiming, then a view of the peer once linked — that is dropped
//! entirely, never squeezed into the codes, when the terminal is too
//! narrow. The protocol thread sizes its DATA tile grid from the same plan.
//!
//! The transfer is not turn-based after aiming. The receiver shows HELLO
//! straight away so the sender has a code to lock onto. The sender presses
//! Enter after lock to begin the handshake, then once more to send the file.
//! Handshake probes, progress reports, reconnects and the receive side then
//! run unattended.

pub mod camera;
pub mod gate;
pub mod layout;
pub mod monitor;
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
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Terminal,
};

use crate::{
    live, qr,
    tui::{
        camera::{CameraFeed, PeerCrop},
        gate::{Gate, GateSpec, KeyAction},
        layout::{Phase, Screen, ScreenSize, Tiles, BORDER, FLOOR_CHROME_ROWS},
        monitor::{freshness, tint, Freshness, MonitorWidget},
        optical::{GateReply, TuiOptical, UiEvent},
        widget::{PreviewWidget, QrGridWidget, QrWidget},
    },
    Error, Result,
};

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

/// Waits until the terminal can draw the smallest usable QR, telling the
/// operator to zoom out or stretch the window. Returns the largest version
/// that now fits.
fn wait_for_terminal_size<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) -> Result<u8> {
    loop {
        live::check_interrupted()?;
        let (cols, rows) = crossterm::terminal::size()?;
        match max_qr_version(cols, rows) {
            Ok(version) => return Ok(version),
            Err(Error::TerminalTooSmall {
                need_cols,
                need_rows,
                have_cols,
                have_rows,
            }) => {
                terminal.draw(|frame| {
                    draw_resize_prompt(frame, have_cols, have_rows, need_cols, need_rows);
                })?;
            }
            Err(err) => return Err(err),
        }

        if !event::poll(Duration::from_millis(80))? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    || (key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('c')));
                if quit {
                    live::interrupt();
                    return Err(Error::Aborted);
                }
            }
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
}

fn draw_resize_prompt(
    frame: &mut ratatui::Frame,
    have_cols: u16,
    have_rows: u16,
    need_cols: u16,
    need_rows: u16,
) {
    let col_note = if have_cols >= need_cols {
        "ok"
    } else {
        "too narrow"
    };
    let row_note = if have_rows >= need_rows {
        "ok"
    } else {
        "too short"
    };
    let zoom = if cfg!(target_os = "macos") {
        "Cmd/-  or  Ctrl/-    zoom out (more cells)\n    Cmd/+  or  Ctrl/+    zoom in  (fewer cells)"
    } else {
        "Ctrl/-    zoom out (more cells)\n    Ctrl/+    zoom in  (fewer cells)"
    };
    let body = format!(
        "This terminal cannot yet draw a readable QR code.\n\n\
         \u{00a0} now     {have_cols} × {have_rows}    columns {col_note}, rows {row_note}\n\
         \u{00a0} need    {need_cols} × {need_rows}    (or larger)\n\n\
         Stretch the window, or change the font size:\n    {zoom}\n\n\
         The numbers update as you resize. Esc or q aborts."
    );
    let area = frame.area();
    frame.render_widget(
        Paragraph::new(body)
            .style(Style::new().fg(Color::Yellow))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" enlarge the terminal "),
            ),
        area,
    );
}

/// How aiming ends before the protocol thread starts the handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
    /// Sender: tracking must lock, then Enter starts the handshake.
    EnterWhenLocked,
    /// Receiver: handshake starts at once. Its HELLO is the only code on
    /// either screen before the link, so it must not wait for a lock of its
    /// own: the sender shows nothing until it reads HELLO, and tracking only
    /// locks on a decoded code. The HELLO loop waits for probes on its own.
    Immediate,
}

impl Aim {
    /// The sender fills the screen with DATA codes; the receiver only ever
    /// shows one progress report at a time.
    fn tiles(self) -> Tiles {
        match self {
            Aim::EnterWhenLocked => Tiles::Fill,
            Aim::Immediate => Tiles::One,
        }
    }
}

struct Ui {
    title: String,
    camera_index: u32,
    /// The largest code this terminal agreed to display. The aiming layout
    /// is sized for it from the first draw, so it does not lurch when the
    /// first QR appears or when a smaller version is negotiated.
    max_version: u8,
    images: Vec<Arc<GrayImage>>,
    invert: bool,
    label: String,
    status: String,
    log: Vec<String>,
    gate: Option<Gate>,
    done: Option<String>,
    /// After the link: tiled codes and the peer monitor.
    linked: bool,
    /// The locked QR version once linked.
    version: u8,
    tiles: Tiles,
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
pub fn run<F>(title: &str, camera_index: u32, no_invert: bool, aim: Aim, work: F) -> Result<()>
where
    F: FnOnce(TuiOptical, u8) -> Result<String> + Send + 'static,
{
    live::install_ctrlc_handler();

    // Open the camera before taking over the screen so a permission prompt or
    // device error is readable in the normal terminal.
    let feed = Arc::new(CameraFeed::open(camera_index)?);

    let mut terminal = enter()?;
    let max_version = match wait_for_terminal_size(&mut terminal) {
        Ok(version) => version,
        Err(err) => {
            leave(&mut terminal);
            return Err(err);
        }
    };

    let tiles = aim.tiles();
    let screen = Arc::new(ScreenSize::default());
    let (events_tx, events_rx) = mpsc::channel::<UiEvent>();
    let worker_feed = Arc::clone(&feed);
    let worker_screen = Arc::clone(&screen);
    let worker = std::thread::spawn(move || {
        let mut optical = TuiOptical::new(
            worker_feed,
            events_tx.clone(),
            no_invert,
            worker_screen,
            tiles,
        );
        let confirmed = {
            use crate::optical::Optical;
            match aim {
                Aim::EnterWhenLocked => optical.gate_locked(&format!(
                    "Aim camera {camera_index} at the other terminal until tracking locks, then press Enter"
                )),
                Aim::Immediate => {
                    optical.set_status(&format!(
                        "Showing HELLO. Aim camera {camera_index} at the sender; receive starts on its own"
                    ));
                    Ok(true)
                }
            }
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

    let ui_result = event_loop(
        &mut terminal,
        &feed,
        &screen,
        Ui {
            title: title.to_string(),
            camera_index,
            max_version,
            images: Vec::new(),
            invert: no_invert,
            label: String::new(),
            status: String::new(),
            log: Vec::new(),
            gate: None,
            done: None,
            linked: false,
            version: max_version,
            tiles,
        },
        &events_rx,
    );
    leave(&mut terminal);

    // Drop the receiver before joining. If the operator quit while a frame or
    // a gate was in flight, the protocol thread is parked on the reply channel
    // for a message the UI will never answer; dropping the queue disconnects
    // those senders and lets it unwind instead of deadlocking the join.
    drop(events_rx);

    let work_result = worker
        .join()
        .unwrap_or_else(|_| Err(Error::Message("protocol thread panicked".into())));
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
    let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
}

fn leave<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) {
    restore();
    let _ = terminal.show_cursor();
}

fn event_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    feed: &CameraFeed,
    screen: &ScreenSize,
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
                let action = ui.gate.as_ref().map_or(KeyAction::Ignore, |gate| {
                    gate::on_key(&gate.spec, key.code, feed.locked(), Instant::now())
                });
                let reply = match action {
                    KeyAction::Proceed => Some(GateReply::Proceed),
                    KeyAction::Abort => Some(GateReply::Abort),
                    KeyAction::Ignore => None,
                };
                if let Some(reply) = reply {
                    if let Some(gate) = ui.gate.take() {
                        let _ = gate.reply.send(reply);
                    }
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
                    images,
                    invert,
                    label,
                    linked,
                    version,
                    drawn,
                }) => {
                    ui.images = images;
                    ui.invert = invert;
                    ui.label = label;
                    ui.linked = linked;
                    ui.version = version;
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
                    auto_on_lock,
                }) => {
                    ui.gate = Some(Gate {
                        prompt,
                        reply,
                        spec: GateSpec {
                            require_lock,
                            auto_on_lock,
                            armed_at: Instant::now(),
                        },
                    });
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
            let view = FeedView {
                counters: feed.counters(),
                locked: feed.locked(),
                preview: feed.preview(),
                crop: feed.peer_crop(),
                now: Instant::now(),
            };
            terminal.draw(|frame| {
                screen.set(frame.area());
                draw(frame, &ui, &view);
            })?;
            last_draw = Instant::now();
        }
        if ui.done.is_none()
            && ui
                .gate
                .as_ref()
                .is_some_and(|gate| gate::auto_proceed(&gate.spec, feed.locked()))
        {
            if let Some(gate) = ui.gate.take() {
                let _ = gate.reply.send(GateReply::Proceed);
            }
        }
        if let Some(ack) = pending_draw_ack {
            let _ = ack.send(());
        }
        if !dirty {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

/// What the camera thread has to say, sampled once per draw.
struct FeedView {
    counters: (u64, u64),
    locked: bool,
    preview: Option<Arc<GrayImage>>,
    crop: Option<PeerCrop>,
    now: Instant,
}

/// Terminal cells a rendered code occupies: one column per module, two
/// modules per row.
fn cells_of(image: &GrayImage) -> (u16, u16) {
    let modules = qr::modules_of(image) as u16;
    (modules, modules.div_ceil(2))
}

fn draw(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView) {
    // Linked frames are sized by what is actually on screen: a re-handshake
    // after a fallback shows probes larger than the locked version.
    let (phase, tile) = if ui.linked {
        let tile = ui
            .images
            .first()
            .map(|image| cells_of(image))
            .unwrap_or_else(|| qr::cell_size_for_version(ui.version));
        (Phase::Linked, tile)
    } else {
        (Phase::Aiming, qr::cell_size_for_version(ui.max_version))
    };
    let screen = layout::plan(frame.area(), phase, ui.tiles, tile);
    draw_title(frame, ui, view, &screen);
    draw_codes(frame, ui, &screen);
    draw_side(frame, ui, view, &screen);
    draw_log(frame, ui, &screen);
    draw_prompt(frame, ui, view, &screen);
}

fn draw_title(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView, screen: &Screen) {
    let (frames, decodes) = view.counters;
    let mut spans = vec![
        Span::styled(
            format!(" {} ", ui.title),
            Style::new()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  cam {}  ·  ", ui.camera_index)),
        if view.locked {
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
    ];
    if ui.linked {
        spans.push(match &view.crop {
            Some(crop) if freshness(crop.at, view.now, view.locked) == Freshness::Live => {
                Span::styled("  ·  peer view live", Style::new().fg(tint(crop.epoch)))
            }
            _ => Span::styled(
                "  ·  peer view STALE, reacquiring",
                Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
        });
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), screen.title);
}

fn draw_codes(frame: &mut ratatui::Frame, ui: &Ui, screen: &Screen) {
    if screen.chrome.qr_border > 0 {
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" showing {} ", ui.label)),
            screen.qr_frame,
        );
    }
    if ui.linked {
        frame.render_widget(
            QrGridWidget {
                images: &ui.images,
                invert: ui.invert,
                grid: screen.grid,
            },
            screen.qr,
        );
    } else {
        frame.render_widget(
            QrWidget {
                image: ui.images.first().map(|image| image.as_ref()),
                invert: ui.invert,
            },
            screen.qr,
        );
    }
}

fn draw_side(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView, screen: &Screen) {
    let Some(side) = screen.side else {
        return;
    };
    if ui.linked {
        // After the link the full preview gives way to a view of just the
        // peer's code, and it stays this way through any reconnect.
        frame.render_widget(
            MonitorWidget {
                crop: view.crop.as_ref(),
                locked: view.locked,
                now: view.now,
            },
            side,
        );
        return;
    }
    let inner = if screen.chrome.qr_border > 0 {
        let block = Block::default().borders(Borders::ALL).title(" camera ");
        let inner = block.inner(side);
        frame.render_widget(block, side);
        inner
    } else {
        side
    };
    frame.render_widget(
        PreviewWidget {
            frame: view.preview.as_deref(),
        },
        inner,
    );
}

fn draw_log(frame: &mut ratatui::Frame, ui: &Ui, screen: &Screen) {
    if screen.chrome.log == 0 {
        return;
    }
    let visible = screen.log.height.saturating_sub(BORDER) as usize;
    let start = ui.log.len().saturating_sub(visible);
    frame.render_widget(
        Paragraph::new(
            ui.log[start..]
                .iter()
                .map(|line| Line::raw(line.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" transcript ")),
        screen.log,
    );
}

fn draw_prompt(frame: &mut ratatui::Frame, ui: &Ui, view: &FeedView, screen: &Screen) {
    let hints = match (&ui.done, &ui.gate) {
        (Some(_), _) => "any key: exit",
        (None, Some(gate)) if gate.spec.require_lock && !view.locked => {
            "keep aiming until ● tracking   Esc: abort   q: quit"
        }
        (None, Some(gate)) if gate.spec.auto_on_lock => "Esc: abort   q: quit",
        (None, Some(_)) => "Enter: continue   Esc: abort   q: quit",
        (None, None) => "q: quit",
    };
    let (prompt_style, mut prompt) = match (&ui.done, &ui.gate) {
        (Some(summary), _) => (Style::new().fg(Color::Cyan), summary.clone()),
        (None, Some(gate)) if gate.spec.require_lock && !view.locked => (
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            if gate.spec.auto_on_lock {
                "▶ Aim at the other terminal until tracking locks — receive starts on its own"
                    .into()
            } else {
                "▶ Aim at the other terminal until tracking locks, then press Enter".into()
            },
        ),
        (None, Some(gate)) if gate.spec.require_lock && view.locked => (
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
            format!("▶ Tracking locked. {}", gate.prompt),
        ),
        (None, Some(gate)) => (
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            format!("▶ {}", gate.prompt),
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
    if screen.chrome.footer == 0 {
        // No footer row to spare: fold the key hints into the prompt so the
        // operator can still see that Enter is what moves things along.
        prompt = format!("{prompt}   [{hints}]");
    }
    let prompt_widget = Paragraph::new(prompt)
        .style(prompt_style)
        .wrap(Wrap { trim: true });
    frame.render_widget(
        if screen.chrome.prompt > 1 {
            prompt_widget.block(Block::default().borders(Borders::ALL).title(" prompt "))
        } else {
            prompt_widget
        },
        screen.prompt,
    );
    if screen.chrome.footer > 0 {
        frame.render_widget(
            Paragraph::new(Span::styled(hints, Style::new().fg(Color::DarkGray))),
            screen.footer,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

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

    fn ui_for(linked: bool, images: Vec<Arc<GrayImage>>, tiles: Tiles) -> Ui {
        Ui {
            title: "t".into(),
            camera_index: 0,
            max_version: 10,
            images,
            invert: false,
            label: String::new(),
            status: String::new(),
            log: Vec::new(),
            gate: None,
            done: None,
            linked,
            version: 10,
            tiles,
        }
    }

    fn render(cols: u16, rows: u16, ui: &Ui) -> ratatui::buffer::Buffer {
        let view = FeedView {
            counters: (0, 0),
            locked: false,
            preview: None,
            crop: None,
            now: Instant::now(),
        };
        render_with(cols, rows, ui, &view)
    }

    fn render_with(cols: u16, rows: u16, ui: &Ui, view: &FeedView) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
        terminal.draw(|frame| draw(frame, ui, view)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn text(buf: &ratatui::buffer::Buffer) -> String {
        let area = buf.area;
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn code_at(buf: &ratatui::buffer::Buffer, x: u16) -> bool {
        (0..buf.area.height).any(|y| {
            let cell = &buf[(x, y)];
            cell.bg == Color::White && matches!(cell.symbol(), "█" | "▀" | "▄")
        })
    }

    #[test]
    fn while_aiming_the_code_is_on_the_left_and_the_camera_on_the_right() {
        let image = Arc::new(qr::encode_version(b"AX-aim", 10).unwrap());
        let buf = render(148, 38, &ui_for(false, vec![image], Tiles::Fill));
        assert!(code_at(&buf, 10));
        assert!(
            (65..148).all(|x| !code_at(&buf, x)),
            "nothing scannable in the side pane"
        );
        assert!(text(&buf).contains("waiting for camera"));
    }

    #[test]
    fn a_narrow_terminal_drops_the_side_pane_and_keeps_the_code_whole() {
        let image = Arc::new(qr::encode_version(b"AX-narrow", 10).unwrap());
        let buf = render(80, 38, &ui_for(false, vec![image], Tiles::Fill));
        let text = text(&buf);
        assert!(!text.contains("waiting for camera"));
        assert!(!text.contains("QR needs"), "the code must not be clipped");
        assert!((0..80).any(|x| code_at(&buf, x)), "the code is still drawn");
    }

    #[test]
    fn once_linked_the_preview_becomes_a_monitor_that_goes_stale_visibly() {
        let image = Arc::new(qr::encode_version(b"AX-linked", 10).unwrap());
        let ui = ui_for(true, vec![image], Tiles::One);
        let t0 = Instant::now();
        let view = FeedView {
            counters: (0, 0),
            locked: false,
            preview: None,
            crop: Some(PeerCrop {
                image: Arc::new(GrayImage::from_pixel(96, 96, image::Luma([0]))),
                epoch: 2,
                at: t0,
            }),
            now: t0 + Duration::from_secs(2),
        };
        let buf = render_with(148, 38, &ui, &view);
        let text = text(&buf);
        assert!(
            !text.contains("waiting for camera"),
            "no full preview after the link"
        );
        assert!(text.contains("STALE"), "{text}");
        assert!(text.contains("reacquiring"));
        assert!(
            (65..148).all(|x| !code_at(&buf, x)),
            "the monitor is never a scannable code"
        );
    }
}
