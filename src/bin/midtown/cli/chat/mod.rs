//! Chat TUI subcommand - IRC-style interface for team communication
//!
//! This module provides a read-only chat interface showing team activity
//! and coworker status in a split-pane layout.

mod app;
mod ui;

use std::io;
use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, prelude::CrosstermBackend};

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

    // Run the main loop
    let result = run_app(&mut terminal, &mut app);

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

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        // Draw UI
        terminal.draw(|f| ui::draw(f, app))?;

        // Poll for events with a timeout to allow periodic updates
        if event::poll(Duration::from_millis(250))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
                KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
                KeyCode::PageUp => app.page_up(),
                KeyCode::PageDown => app.page_down(),
                KeyCode::Home | KeyCode::Char('g') => app.scroll_to_top(),
                KeyCode::End | KeyCode::Char('G') => app.scroll_to_bottom(),
                _ => {}
            }
        }

        // Check for new messages
        app.refresh();
    }
}
