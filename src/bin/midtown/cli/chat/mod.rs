//! Chat TUI subcommand - IRC-style interface for team communication
//!
//! This module provides a read-only chat interface showing team activity
//! and coworker status in a split-pane layout.
//!
//! Uses async I/O with the `tailf` crate for instant message updates when
//! the channel.jsonl file changes, rather than polling.

mod app;
mod kitty;
mod mermaid;
mod ui;
mod usage;

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

/// Whether the app is in fullscreen diagram viewer mode
enum ViewMode {
    /// Normal chat view
    Chat,
    /// Fullscreen diagram viewer (index into app.diagram_sources, 0-based)
    DiagramViewer(usize),
}

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

    // Auto-scroll timer (30 seconds) - brings user back to bottom if they scrolled up
    // and forgot. Prevents the chat from appearing frozen when it's just scrolled up.
    let mut auto_scroll_interval = interval(Duration::from_secs(30));

    let mut view_mode = ViewMode::Chat;

    loop {
        // Draw based on current view mode
        match &view_mode {
            ViewMode::Chat => {
                // Draw UI and collect post-render overlays (hyperlinks)
                let mut hyperlinks = Vec::new();
                terminal.draw(|f| {
                    hyperlinks = ui::draw(f, app);
                })?;

                // Write hyperlinks using OSC 8 sequences (after ratatui draws)
                // This bypasses ratatui's buffer system which doesn't support escape sequences
                render_hyperlinks(terminal.backend_mut(), &hyperlinks)?;
            }
            ViewMode::DiagramViewer(idx) => {
                // Render fullscreen diagram
                if let Some(source) = app.diagram_sources.get(*idx)
                    && let Some(image) = app.mermaid_cache.get_cached(source)
                {
                    terminal.draw(|f| {
                        let area = f.area();
                        let block = ratatui::widgets::Block::default();
                        f.render_widget(block, area);
                    })?;
                    kitty::render_fullscreen_image(terminal.backend_mut(), &image.png_data)?;
                }
            }
        }

        // Use tokio::select! to wait for either:
        // 1. Terminal events (keyboard/mouse)
        // 2. File changes from tailf
        // 3. Periodic refresh timer
        tokio::select! {
            // Handle terminal events (keyboard, mouse)
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        // Handle the first event
                        match handle_event(app, event, &view_mode) {
                            EventResult::Exit => return Ok(()),
                            EventResult::ToggleSelectionMode => {
                                if app.selection_mode {
                                    let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
                                } else {
                                    let _ = execute!(terminal.backend_mut(), EnableMouseCapture);
                                }
                            }
                            EventResult::OpenDiagram(idx) => {
                                view_mode = ViewMode::DiagramViewer(idx);
                            }
                            EventResult::CloseDiagram => {
                                view_mode = ViewMode::Chat;
                            }
                            EventResult::Continue => {}
                        }

                        // Drain any immediately available events before redrawing.
                        // This coalesces rapid mouse scroll events into a single batch,
                        // avoiding the overhead of redrawing between each scroll event.
                        // Uses async timeout(ZERO) to yield to the runtime between attempts.
                        while let Ok(Some(Ok(event))) =
                            tokio::time::timeout(Duration::ZERO, event_stream.next()).await
                        {
                            match handle_event(app, event, &view_mode) {
                                EventResult::Exit => return Ok(()),
                                EventResult::ToggleSelectionMode => {
                                    if app.selection_mode {
                                        let _ = execute!(terminal.backend_mut(), DisableMouseCapture);
                                    } else {
                                        let _ = execute!(terminal.backend_mut(), EnableMouseCapture);
                                    }
                                }
                                EventResult::OpenDiagram(idx) => {
                                    view_mode = ViewMode::DiagramViewer(idx);
                                }
                                EventResult::CloseDiagram => {
                                    view_mode = ViewMode::Chat;
                                }
                                EventResult::Continue => {}
                            }
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

            // Auto-scroll back to bottom if user scrolled up and forgot
            // Skip if in selection mode - user may be copying text
            _ = auto_scroll_interval.tick() => {
                if app.scroll_offset > 0 && !app.selection_mode {
                    app.scroll_to_bottom();
                }
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
    /// Open a diagram in fullscreen viewer (0-based index into diagram_sources)
    OpenDiagram(usize),
    /// Close the diagram viewer and return to chat
    CloseDiagram,
}

/// Handle a terminal event, returns the result.
///
/// Behavior depends on the current `ViewMode`:
/// - `Chat`: normal keybindings (scroll, quit, number keys for diagrams)
/// - `DiagramViewer`: any keypress returns to chat
fn handle_event(app: &mut App, event: Event, view_mode: &ViewMode) -> EventResult {
    match view_mode {
        ViewMode::DiagramViewer(_) => {
            // In diagram viewer: any key returns to chat
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Press
            {
                return EventResult::CloseDiagram;
            }
            EventResult::Continue
        }
        ViewMode::Chat => match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => EventResult::Exit,
                KeyCode::Char('s') => {
                    app.toggle_selection_mode();
                    EventResult::ToggleSelectionMode
                }
                // Number keys 1-9 open the corresponding diagram
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = (c as usize) - ('1' as usize); // 0-based
                    if idx < app.diagram_sources.len() {
                        EventResult::OpenDiagram(idx)
                    } else {
                        EventResult::Continue
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.scroll_up();
                    EventResult::Continue
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.scroll_down();
                    EventResult::Continue
                }
                KeyCode::PageUp => {
                    app.page_up();
                    EventResult::Continue
                }
                KeyCode::PageDown => {
                    app.page_down();
                    EventResult::Continue
                }
                KeyCode::Home | KeyCode::Char('g') => {
                    app.scroll_to_top();
                    EventResult::Continue
                }
                KeyCode::End | KeyCode::Char('G') => {
                    app.scroll_to_bottom();
                    EventResult::Continue
                }
                _ => EventResult::Continue,
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    app.scroll_up();
                    EventResult::Continue
                }
                MouseEventKind::ScrollDown => {
                    app.scroll_down();
                    EventResult::Continue
                }
                _ => EventResult::Continue,
            },
            _ => EventResult::Continue,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::tests::test_app;
    use crossterm::event::{KeyEvent, KeyModifiers};

    /// Helper to create a key press event for a given KeyCode
    fn key_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn test_number_key_opens_diagram_when_source_exists() {
        let mut app = test_app();
        app.diagram_sources = vec!["graph TD\n  A-->B".into()];

        let result = handle_event(&mut app, key_press(KeyCode::Char('1')), &ViewMode::Chat);
        assert!(
            matches!(result, EventResult::OpenDiagram(0)),
            "Pressing '1' should open diagram at index 0"
        );
    }

    #[test]
    fn test_number_key_ignored_when_no_diagram_at_index() {
        let mut app = test_app();
        app.diagram_sources = vec!["graph TD\n  A-->B".into()]; // Only 1 diagram

        let result = handle_event(&mut app, key_press(KeyCode::Char('2')), &ViewMode::Chat);
        assert!(
            matches!(result, EventResult::Continue),
            "Pressing '2' with only 1 diagram should be Continue"
        );
    }

    #[test]
    fn test_number_keys_beyond_9_cannot_open_diagrams() {
        let mut app = test_app();
        // Populate 12 diagram sources
        app.diagram_sources = (0..12).map(|i| format!("graph TD\n  A{}-->B", i)).collect();

        // Keys 1-9 should work
        let result = handle_event(&mut app, key_press(KeyCode::Char('9')), &ViewMode::Chat);
        assert!(
            matches!(result, EventResult::OpenDiagram(8)),
            "Pressing '9' should open diagram at index 8"
        );

        // There is no single key for index 9+ (diagrams 10, 11, 12)
        // Key '0' is not in the 1-9 range
        let result = handle_event(&mut app, key_press(KeyCode::Char('0')), &ViewMode::Chat);
        assert!(
            matches!(result, EventResult::Continue),
            "Pressing '0' should not open any diagram"
        );
    }

    #[test]
    fn test_enter_does_not_open_diagram() {
        let mut app = test_app();
        app.diagram_sources = vec!["graph TD\n  A-->B".into()];

        let result = handle_event(&mut app, key_press(KeyCode::Enter), &ViewMode::Chat);
        assert!(
            matches!(result, EventResult::Continue),
            "Enter should not open a diagram"
        );
    }

    #[test]
    fn test_any_key_closes_diagram_viewer() {
        let mut app = test_app();
        let viewer = ViewMode::DiagramViewer(0);

        // Any key (including Enter, Esc, letters) should close the viewer
        let result = handle_event(&mut app, key_press(KeyCode::Enter), &viewer);
        assert!(
            matches!(result, EventResult::CloseDiagram),
            "Enter in viewer should close it"
        );

        let result = handle_event(&mut app, key_press(KeyCode::Esc), &viewer);
        assert!(
            matches!(result, EventResult::CloseDiagram),
            "Esc in viewer should close it"
        );

        let result = handle_event(&mut app, key_press(KeyCode::Char('q')), &viewer);
        assert!(
            matches!(result, EventResult::CloseDiagram),
            "'q' in viewer should close it"
        );

        let result = handle_event(&mut app, key_press(KeyCode::Char('x')), &viewer);
        assert!(
            matches!(result, EventResult::CloseDiagram),
            "Any key in viewer should close it"
        );
    }

    #[test]
    fn test_quit_keys_exit_from_chat_mode() {
        let mut app = test_app();

        let result = handle_event(&mut app, key_press(KeyCode::Char('q')), &ViewMode::Chat);
        assert!(matches!(result, EventResult::Exit), "'q' should exit");

        let result = handle_event(&mut app, key_press(KeyCode::Esc), &ViewMode::Chat);
        assert!(matches!(result, EventResult::Exit), "Esc should exit");
    }

    #[test]
    fn test_number_key_with_empty_diagram_sources() {
        let mut app = test_app();
        // No diagrams visible
        assert!(app.diagram_sources.is_empty());

        let result = handle_event(&mut app, key_press(KeyCode::Char('1')), &ViewMode::Chat);
        assert!(
            matches!(result, EventResult::Continue),
            "Number key with no diagrams should be Continue"
        );
    }
}
