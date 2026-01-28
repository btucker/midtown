//! Chat TUI subcommand - IRC-style interface for team communication
//!
//! This module provides a read-only chat interface showing team activity
//! and coworker status in a split-pane layout.
//!
//! Uses async I/O with the `tailf` crate for instant message updates when
//! the channel.jsonl file changes, rather than polling.

mod app;
mod ui;

use std::io;
use std::time::Duration;

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
        MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures::StreamExt;
use ratatui::{Terminal, prelude::CrosstermBackend};
use tokio::time::interval;

use app::App;

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
        // Draw UI
        terminal.draw(|f| ui::draw(f, app))?;

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
