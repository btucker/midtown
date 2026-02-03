//! Chat TUI subcommand - IRC-style interface for team communication
//!
//! This module provides a read-only chat interface showing team activity
//! and coworker status in a split-pane layout.
//!
//! Uses async I/O with the `tailf` crate for instant message updates when
//! the channel.jsonl file changes, rather than polling.

mod app;
mod ui;

use std::io::{self, Write};
use std::time::Duration;

use crossterm::{
    cursor::MoveTo,
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
        MouseEventKind,
    },
    execute,
    style::{Color as CrosstermColor, Print, ResetColor, SetForegroundColor},
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, prelude::CrosstermBackend};
use tokio::time::interval;

use app::App;
use ratatui::style::Color as RatatuiColor;
use ui::Hyperlink;

/// Run the chat TUI
pub fn run() -> Result<(), String> {
    // Setup terminal
    enable_raw_mode().map_err(|e| format!("Failed to enable raw mode: {}", e))?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| format!("Failed to enter alternate screen: {}", e))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| format!("Failed to create terminal: {}", e))?;

    // Create app state
    let mut app = App::new();

    // Run the async main loop using tokio
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Failed to create tokio runtime: {}", e))?
        .block_on(run_app_async(&mut terminal, &mut app));

    // Restore terminal (always attempt cleanup)
    let _ = disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    );
    let _ = terminal.show_cursor();

    result.map_err(|e| format!("TUI error: {}", e))
}

async fn run_app_async(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<()> {
    // Set up async event stream for terminal events
    let mut event_stream = EventStream::new();

    // Set up file tailer for channel.jsonl if available
    // We use num_lines=None (no -n flag) to avoid a race condition on macOS where
    // `tail -n 0 -f` seeks to EOF before registering its kqueue file watcher,
    // causing the first write after tailer creation to be lost forever.
    // The actual content from tail is ignored; we only use it as a change notification.
    let mut tailer = app
        .channel_file_path()
        .and_then(|path| tailf::tailf(&path, None).ok());

    // Fallback timer for message/kanban/repo status refresh (1 second)
    // This ensures responsive updates if tailf isn't triggering
    let mut refresh_interval = interval(Duration::from_secs(1));

    loop {
        // Draw UI and collect hyperlinks
        let mut hyperlinks = Vec::new();
        terminal.draw(|f| {
            hyperlinks = ui::draw(f, app);
        })?;

        // Write hyperlinks using OSC 8 sequences (after ratatui draws)
        // This bypasses ratatui's buffer system which doesn't support escape sequences
        render_hyperlinks(terminal.backend_mut(), &hyperlinks)?;

        // Use tokio::select! to wait for either:
        // 1. Terminal events (keyboard/mouse)
        // 2. File changes from tailf
        // 3. Periodic refresh timer
        tokio::select! {
            // Handle terminal events (keyboard, mouse)
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        match handle_event(app, event) {
                            EventResult::Exit => return Ok(()),
                            EventResult::ToggleSelectionMode => {
                                // Toggle mouse capture based on selection mode
                                if app.selection_mode {
                                    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
                                } else {
                                    let _ = execute!(terminal.backend_mut(), EnableMouseCapture);
                                }
                            }
                            EventResult::Continue => {}
                        }
                    }
                    Some(Err(_)) => {
                        // Event stream error, continue
                    }
                    None => {
                        // Event stream closed
                        return Ok(());
                    }
                }
            }

            // Handle file changes from tailf - instant message updates
            Some(result) = async {
                match &mut tailer {
                    Some(t) => Some(t.next().await),
                    None => None,
                }
            } => {
                if let Ok(Some(_)) = result {
                    // New content in channel.jsonl - refresh messages
                    app.refresh();
                }
            }

            // Periodic refresh for kanban and repo status
            _ = refresh_interval.tick() => {
                app.refresh();
            }
        }
    }
}

/// Render hyperlinks using OSC 8 escape sequences
///
/// This function writes hyperlinks directly to the terminal, bypassing ratatui's
/// buffer system. OSC 8 hyperlinks work by wrapping text in escape sequences:
/// - Start: \x1b]8;;URL\x07
/// - End: \x1b]8;;\x07
///
/// Requirements for clickable links:
/// - tmux 3.4+ with `allow-passthrough on`
/// - Terminal with OSC 8 support (iTerm2, kitty, WezTerm, etc.)
fn render_hyperlinks<W: io::Write>(
    backend: &mut CrosstermBackend<W>,
    hyperlinks: &[Hyperlink],
) -> io::Result<()> {
    for link in hyperlinks {
        // Move cursor to the hyperlink position
        execute!(backend, MoveTo(link.x, link.y))?;

        // Write OSC 8 start sequence
        write!(backend, "\x1b]8;;{}\x07", link.url)?;

        // Write the text with optional first-char coloring for CI status dots
        let mut chars = link.text.chars().peekable();
        if let Some(first_char) = chars.next() {
            // Handle CI status dot coloring for first character
            if let Some(color) = link.first_char_color {
                if first_char == '●' || first_char == '○' {
                    let crossterm_color = ratatui_to_crossterm_color(color);
                    execute!(
                        backend,
                        SetForegroundColor(crossterm_color),
                        Print(first_char),
                        ResetColor
                    )?;
                } else {
                    write!(backend, "{}", first_char)?;
                }
            } else {
                write!(backend, "{}", first_char)?;
            }
            // Write remaining characters
            for ch in chars {
                write!(backend, "{}", ch)?;
            }
        }

        // Write OSC 8 end sequence
        write!(backend, "\x1b]8;;\x07")?;
    }

    // Flush to ensure sequences are written
    backend.flush()?;

    Ok(())
}

/// Convert ratatui Color to crossterm Color
fn ratatui_to_crossterm_color(color: RatatuiColor) -> CrosstermColor {
    match color {
        RatatuiColor::Reset => CrosstermColor::Reset,
        RatatuiColor::Black => CrosstermColor::Black,
        RatatuiColor::Red => CrosstermColor::DarkRed,
        RatatuiColor::Green => CrosstermColor::DarkGreen,
        RatatuiColor::Yellow => CrosstermColor::DarkYellow,
        RatatuiColor::Blue => CrosstermColor::DarkBlue,
        RatatuiColor::Magenta => CrosstermColor::DarkMagenta,
        RatatuiColor::Cyan => CrosstermColor::DarkCyan,
        RatatuiColor::Gray => CrosstermColor::Grey,
        RatatuiColor::DarkGray => CrosstermColor::DarkGrey,
        RatatuiColor::LightRed => CrosstermColor::Red,
        RatatuiColor::LightGreen => CrosstermColor::Green,
        RatatuiColor::LightYellow => CrosstermColor::Yellow,
        RatatuiColor::LightBlue => CrosstermColor::Blue,
        RatatuiColor::LightMagenta => CrosstermColor::Magenta,
        RatatuiColor::LightCyan => CrosstermColor::Cyan,
        RatatuiColor::White => CrosstermColor::White,
        RatatuiColor::Rgb(r, g, b) => CrosstermColor::Rgb { r, g, b },
        RatatuiColor::Indexed(i) => CrosstermColor::AnsiValue(i),
    }
}

/// Result of handling an event
enum EventResult {
    /// Continue running
    Continue,
    /// Exit the app
    Exit,
    /// Toggle selection mode (needs to update terminal mouse capture)
    ToggleSelectionMode,
}

/// Handle a terminal event, returns the result
fn handle_event(app: &mut App, event: Event) -> EventResult {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return EventResult::Exit,
            KeyCode::Char('s') => {
                app.toggle_selection_mode();
                return EventResult::ToggleSelectionMode;
            }
            KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
            KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
            KeyCode::PageUp => app.page_up(),
            KeyCode::PageDown => app.page_down(),
            KeyCode::Home | KeyCode::Char('g') => app.scroll_to_top(),
            KeyCode::End | KeyCode::Char('G') => app.scroll_to_bottom(),
            _ => {}
        },
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => app.scroll_up(),
            MouseEventKind::ScrollDown => app.scroll_down(),
            _ => {}
        },
        _ => {}
    }
    EventResult::Continue
}
