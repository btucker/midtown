//! Chat TUI subcommand - IRC-style interface for team communication
//!
//! This module provides an interactive chat interface showing team activity,
//! coworker status, and an input bar for posting messages.
//!
//! Uses async I/O with the `tailf` crate for instant message updates when
//! the channel.jsonl file changes, rather than polling.

mod app;
mod mermaid;
mod ui;

use std::io::{self, Write};
use std::time::Duration;

use crossterm::{
    cursor::MoveTo,
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
        KeyModifiers, KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
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

/// Keyboard enhancement flags for the kitty keyboard protocol.
///
/// `DISAMBIGUATE_ESCAPE_CODES` ensures special keys (Enter, Esc, etc.) use
/// unique CSI u sequences so crossterm can reliably decode modifier combinations
/// like Shift+Enter. Without this flag, terminals send ambiguous escape sequences
/// that crossterm may decode as incorrect characters (e.g., 'j' for Shift+Enter).
///
/// Note: `REPORT_ALL_KEYS_AS_ESCAPE_CODES` was intentionally removed because it
/// causes ALL keys (including regular text) to report as escape codes with the
/// base (lowercase) key plus modifier flags. This breaks shifted character input
/// (capitals, symbols) since the terminal no longer performs character translation.
const KEYBOARD_ENHANCEMENT_FLAGS: KeyboardEnhancementFlags =
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;

/// Convert a character index to a byte index in a UTF-8 string.
///
/// Returns the byte offset where the nth character starts.
/// If char_idx exceeds the character count, returns the string's byte length.
fn char_index_to_byte_index(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(byte_idx, _)| byte_idx)
        .unwrap_or(s.len())
}

/// Run the chat TUI
pub fn run() -> Result<(), String> {
    run_with_ready_hook(|| ())
}

/// Run the chat TUI and invoke a hook once terminal setup is complete.
pub fn run_with_ready_hook<F>(on_ready: F) -> Result<(), String>
where
    F: FnOnce(),
{
    // Setup terminal
    enable_raw_mode().map_err(|e| format!("Failed to enable raw mode: {}", e))?;
    let mut stdout = io::stdout();

    // Enable keyboard enhancement flags for proper Shift+Enter detection.
    // See KEYBOARD_ENHANCEMENT_FLAGS for details.
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        PushKeyboardEnhancementFlags(KEYBOARD_ENHANCEMENT_FLAGS)
    )
    .map_err(|e| format!("Failed to enter alternate screen: {}", e))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal =
        Terminal::new(backend).map_err(|e| format!("Failed to create terminal: {}", e))?;

    // Create app state
    let mut app = App::new();
    on_ready();

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
        DisableMouseCapture,
        PopKeyboardEnhancementFlags
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

    // Track previous hyperlinks to skip redundant OSC 8 rendering
    let mut last_hyperlinks: Vec<Hyperlink> = Vec::new();

    loop {
        // Draw UI and collect post-render overlays (hyperlinks)
        let mut hyperlinks = Vec::new();
        terminal.draw(|f| {
            hyperlinks = ui::draw(f, app);
        })?;

        // Write hyperlinks using OSC 8 sequences only when they've changed.
        // This avoids cursor-moving escape sequences on every keystroke when
        // only the input bar changed.
        if hyperlinks != last_hyperlinks {
            render_hyperlinks(terminal.backend_mut(), &hyperlinks)?;
            last_hyperlinks = hyperlinks;
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
                        match handle_event(app, event) {
                            EventResult::Exit => return Ok(()),
                            EventResult::OpenDiagramInBrowser(idx) => {
                                open_diagram_in_browser(app, idx);
                            }
                            EventResult::ToggleMouseCapture => {
                                toggle_mouse_capture(app, terminal.backend_mut());
                            }
                            EventResult::ToggleArchivedChannels => {
                                app.show_archived_channels = !app.show_archived_channels;
                            }
                            EventResult::AttachLead => {
                                attach_lead_split();
                            }
                            EventResult::AttachCoworker(coworker_name) => {
                                attach_coworker_split(&coworker_name);
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
                            match handle_event(app, event) {
                                EventResult::Exit => return Ok(()),
                                EventResult::OpenDiagramInBrowser(idx) => {
                                    open_diagram_in_browser(app, idx);
                                }
                                EventResult::ToggleMouseCapture => {
                                    toggle_mouse_capture(app, terminal.backend_mut());
                                }
                                EventResult::ToggleArchivedChannels => {
                                    app.show_archived_channels = !app.show_archived_channels;
                                }
                                EventResult::AttachLead => {
                                    attach_lead_split();
                                }
                                EventResult::AttachCoworker(coworker_name) => {
                                    attach_coworker_split(&coworker_name);
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
            _ = auto_scroll_interval.tick() => {
                if app.scroll_offset > 0 {
                    app.scroll_to_bottom();
                }
            }
        }
    }
}

/// Save SVG to disk and open it in the default browser
fn open_diagram_in_browser(app: &App, idx: usize) {
    let source = match app.diagram_sources.get(idx) {
        Some(s) => s,
        None => return,
    };
    let diagram = match app.mermaid_cache.get_cached(source) {
        Some(d) => d,
        None => return,
    };

    // Get the project diagrams directory
    let repo = midtown::paths::detect_repo_name().unwrap_or_else(|| "default".to_string());
    let diagrams_dir = midtown::paths::projects_dir_for_repo(&repo).join("diagrams");
    let _ = std::fs::create_dir_all(&diagrams_dir);

    // Save SVG to file using content hash
    let hash = mermaid::content_hash(source);
    let svg_path = diagrams_dir.join(format!("{:x}.svg", hash));
    if std::fs::write(&svg_path, &diagram.svg).is_err() {
        return;
    }

    // Open in browser using platform-appropriate command.
    // Spawn a thread to wait on the child process so it doesn't become a zombie.
    #[cfg(target_os = "macos")]
    let child = std::process::Command::new("open").arg(&svg_path).spawn();
    #[cfg(target_os = "linux")]
    let child = std::process::Command::new("xdg-open")
        .arg(&svg_path)
        .spawn();
    if let Ok(mut child) = child {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
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
/// - tmux 3.4+ with `allow-passthrough on` (if running in tmux)
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
    /// Open a diagram in the browser (0-based index into diagram_sources)
    OpenDiagramInBrowser(usize),
    /// Toggle mouse capture (selection mode)
    ToggleMouseCapture,
    /// Toggle showing archived channels
    ToggleArchivedChannels,
    /// Attach to the lead session in a split pane
    AttachLead,
    /// Attach to a coworker session in a split pane (coworker name)
    AttachCoworker(String),
}

/// Handle a terminal event, returns the result.
fn handle_event(app: &mut App, event: Event) -> EventResult {
    use app::FocusedPane;

    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            // Handle Ctrl+key combinations first (before character input catch-all)
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match key.code {
                    KeyCode::Char('q') => return EventResult::Exit,
                    KeyCode::Char('s') => return EventResult::ToggleMouseCapture,
                    KeyCode::Char('k') => {
                        app.toggle_channel_switcher();
                        return EventResult::Continue;
                    }
                    KeyCode::Char('a') => return EventResult::ToggleArchivedChannels,
                    KeyCode::Char('l') => return EventResult::AttachLead,
                    _ => {}
                }
            }
            match key.code {
                KeyCode::Esc => {
                    // Esc dismisses channel switcher if showing
                    if app.channel_switcher.show {
                        app.dismiss_channel_switcher();
                        EventResult::Continue
                    // Esc dismisses autocomplete if showing
                    } else if app.autocomplete.show {
                        app.dismiss_autocomplete();
                        EventResult::Continue
                    } else if app.focused_pane == FocusedPane::InputBar {
                        // Esc clears input when in InputBar
                        app.input_text.clear();
                        app.input_cursor = 0;
                        EventResult::Continue
                    } else {
                        // Esc exits when in Board or Chat
                        EventResult::Exit
                    }
                }
                // Number keys 1-9 open diagrams - only when diagrams exist
                // This is checked BEFORE auto-focus to input to preserve diagram shortcuts
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = (c as usize) - ('1' as usize); // 0-based
                    if idx < app.diagram_sources.len() {
                        // Diagram exists, open it (don't insert into input)
                        EventResult::OpenDiagramInBrowser(idx)
                    } else {
                        // No diagram at this index - treat as regular character input
                        auto_focus_and_insert_char(app, c);
                        EventResult::Continue
                    }
                }
                // Arrow keys for scrolling - don't auto-focus input
                // BUT if channel switcher or autocomplete is showing, navigate those instead
                KeyCode::Up => {
                    if app.channel_switcher.show {
                        app.channel_switcher_select_prev();
                        EventResult::Continue
                    } else if app.autocomplete.show {
                        app.autocomplete_select_prev();
                        EventResult::Continue
                    } else {
                        match app.focused_pane {
                            FocusedPane::Board => {
                                app.board_selection_up();
                                EventResult::Continue
                            }
                            FocusedPane::Chat | FocusedPane::InputBar => {
                                app.scroll_up();
                                EventResult::Continue
                            }
                        }
                    }
                }
                KeyCode::Down => {
                    if app.channel_switcher.show {
                        app.channel_switcher_select_next();
                        EventResult::Continue
                    } else if app.autocomplete.show {
                        app.autocomplete_select_next();
                        EventResult::Continue
                    } else {
                        match app.focused_pane {
                            FocusedPane::Board => {
                                app.board_selection_down();
                                EventResult::Continue
                            }
                            FocusedPane::Chat | FocusedPane::InputBar => {
                                app.scroll_down();
                                EventResult::Continue
                            }
                        }
                    }
                }
                KeyCode::PageUp => {
                    app.page_up();
                    EventResult::Continue
                }
                KeyCode::PageDown => {
                    app.page_down();
                    EventResult::Continue
                }
                KeyCode::Home => {
                    app.scroll_to_top();
                    EventResult::Continue
                }
                KeyCode::End => {
                    app.scroll_to_bottom();
                    EventResult::Continue
                }
                // Enter: select channel switcher item if showing, or autocomplete item if showing,
                // execute /channel create command, auto-focus InputBar, or send message
                // Shift+Enter: insert newline
                KeyCode::Enter => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        // Shift+Enter inserts a newline
                        auto_focus_and_insert_char(app, '\n');
                        EventResult::Continue
                    } else if app.channel_switcher.show {
                        app.channel_switcher_select();
                        EventResult::Continue
                    } else if app.autocomplete.show {
                        app.insert_autocomplete_item();
                        EventResult::Continue
                    } else if app.focused_pane == FocusedPane::Board {
                        // When board is focused and Enter is pressed, check if a task is selected
                        if let Some(app::BoardSelection::Task(_channel, task_id)) =
                            &app.board_selection
                            && let Some(task) = app.tasks.iter().find(|t| &t.id == task_id)
                            && let Some(ref owner) = task.owner
                        {
                            // Attach to the coworker owning this task
                            return EventResult::AttachCoworker(owner.clone());
                        }
                        // If no task selected or task has no owner, focus InputBar
                        app.focused_pane = FocusedPane::InputBar;
                        EventResult::Continue
                    } else if app.focused_pane != FocusedPane::InputBar {
                        app.focused_pane = FocusedPane::InputBar;
                        EventResult::Continue
                    } else if !app.input_text.is_empty() {
                        let message = app.input_text.clone();

                        // Check for /channel create <name> command
                        if message.starts_with("/channel create ") {
                            let channel_name =
                                message.trim_start_matches("/channel create ").trim();
                            if !channel_name.is_empty() {
                                // Create the channel and switch to it
                                if app.create_channel(channel_name) {
                                    app.input_text.clear();
                                    app.input_cursor = 0;
                                }
                                // TODO: Show error if creation failed
                            }
                            EventResult::Continue
                        } else {
                            // Post message to the selected channel
                            let channel_name = app.selected_channel.clone();

                            // Post via daemon RPC with fallback to direct channel write
                            let posted = app.post_message(&message, "user", Some(&channel_name));

                            // Only clear input if message was successfully posted
                            if posted {
                                app.input_text.clear();
                                app.input_cursor = 0;
                            }
                            // TODO: When error display is implemented, show error here if !posted
                            EventResult::Continue
                        }
                    } else {
                        EventResult::Continue
                    }
                }
                // Tab: select autocomplete item if showing
                KeyCode::Tab => {
                    if app.autocomplete.show {
                        app.insert_autocomplete_item();
                        EventResult::Continue
                    } else {
                        // Tab cycles focus: Board → Chat → InputBar → Board
                        app.cycle_focus();
                        EventResult::Continue
                    }
                }
                // Backspace: if channel switcher is showing, backspace in its input
                // Otherwise auto-focus if input has text, then delete
                KeyCode::Backspace => {
                    if app.channel_switcher.show {
                        app.channel_switcher_backspace();
                        EventResult::Continue
                    } else if !app.input_text.is_empty() && app.input_cursor == 0 {
                        // Input has text but cursor is at start - auto-focus but don't delete
                        app.focused_pane = FocusedPane::InputBar;
                        EventResult::Continue
                    } else if !app.input_text.is_empty()
                        || app.focused_pane == FocusedPane::InputBar
                    {
                        // Either input has text or already focused - auto-focus and delete
                        app.focused_pane = FocusedPane::InputBar;
                        if app.input_cursor > 0 {
                            app.input_cursor -= 1;
                            let byte_idx =
                                char_index_to_byte_index(&app.input_text, app.input_cursor);
                            app.input_text.remove(byte_idx);
                            // Detect autocomplete trigger after deletion
                            app.detect_autocomplete_trigger();
                        }
                        EventResult::Continue
                    } else {
                        EventResult::Continue
                    }
                }
                // Delete: auto-focus if input has text, then delete forward
                KeyCode::Delete => {
                    if !app.input_text.is_empty() || app.focused_pane == FocusedPane::InputBar {
                        app.focused_pane = FocusedPane::InputBar;
                        if app.input_cursor < app.input_text.chars().count() {
                            let byte_idx =
                                char_index_to_byte_index(&app.input_text, app.input_cursor);
                            app.input_text.remove(byte_idx);
                            // Detect autocomplete trigger after deletion
                            app.detect_autocomplete_trigger();
                        }
                    }
                    EventResult::Continue
                }
                // Left/Right for cursor movement - only when in InputBar
                KeyCode::Left => {
                    if app.focused_pane == FocusedPane::InputBar && app.input_cursor > 0 {
                        app.input_cursor -= 1;
                    }
                    EventResult::Continue
                }
                KeyCode::Right => {
                    if app.focused_pane == FocusedPane::InputBar
                        && app.input_cursor < app.input_text.chars().count()
                    {
                        app.input_cursor += 1;
                    }
                    EventResult::Continue
                }
                // All other character input: if channel switcher is showing, input to it
                // Otherwise auto-focus InputBar and insert
                KeyCode::Char(c) => {
                    // With the kitty keyboard protocol, Shift+letter reports the
                    // lowercase base key plus a SHIFT modifier. Convert alphabetic
                    // chars to uppercase when SHIFT is the only modifier held.
                    let c = if key.modifiers == KeyModifiers::SHIFT && c.is_ascii_lowercase() {
                        c.to_ascii_uppercase()
                    } else {
                        c
                    };
                    if app.channel_switcher.show {
                        app.channel_switcher_input(c);
                        EventResult::Continue
                    } else {
                        auto_focus_and_insert_char(app, c);
                        EventResult::Continue
                    }
                }
                _ => EventResult::Continue,
            }
        }
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => {
                app.mouse_scroll_up();
                EventResult::Continue
            }
            MouseEventKind::ScrollDown => {
                app.mouse_scroll_down();
                EventResult::Continue
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                // Handle left mouse button clicks
                let x = mouse.column;
                let y = mouse.row;

                // Check if click is on the sidebar/chat divider
                if let Some(div_x) = app.divider_x
                    && x == div_x
                {
                    app.dragging_divider = true;
                    return EventResult::Continue;
                }

                // Check if click is in the input area
                if let Some(input_rect) = app.input_area
                    && x >= input_rect.x
                    && x < input_rect.x + input_rect.width
                    && y >= input_rect.y
                    && y < input_rect.y + input_rect.height
                {
                    // Click in input area - focus it
                    app.focused_pane = FocusedPane::InputBar;
                    return EventResult::Continue;
                }

                // Check if click is in the board area
                if let Some(board_rect) = app.board_area
                    && x >= board_rect.x
                    && x < board_rect.x + board_rect.width
                    && y >= board_rect.y
                    && y < board_rect.y + board_rect.height
                {
                    // Board rect includes border (1 line), so subtract 1 to get content-relative line
                    let content_y = y.saturating_sub(board_rect.y + 1);

                    // Check if click is on a channel header (for selection)
                    if let Some(channel_name) = app.channel_line_map.get(&content_y) {
                        // Update board selection to this channel
                        app.board_selection =
                            Some(app::BoardSelection::Channel(channel_name.clone()));
                        app.update_selected_channel();
                        return EventResult::Continue;
                    }

                    // Check if click is on a task line (for attach)
                    if let Some((_task_id, task_owner)) = app.task_line_map.get(&content_y)
                        && let Some(owner) = task_owner
                    {
                        // Task has an owner - attach to their session
                        attach_coworker_split(owner);
                        return EventResult::Exit;
                    }

                    // Click in board area (but not on a task with owner) - focus it
                    app.focused_pane = FocusedPane::Board;
                    return EventResult::Continue;
                }

                EventResult::Continue
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                if app.dragging_divider {
                    app.resize_sidebar_to(mouse.column, app.layout_width);
                }
                EventResult::Continue
            }
            MouseEventKind::Up(_) => {
                app.dragging_divider = false;
                EventResult::Continue
            }
            _ => EventResult::Continue,
        },
        _ => EventResult::Continue,
    }
}

/// Toggle mouse capture for text selection mode.
/// When selection mode is on, mouse capture is disabled so the terminal handles
/// text selection natively. Scrollwheel won't work in the TUI during selection mode.
fn toggle_mouse_capture(app: &mut App, backend: &mut CrosstermBackend<io::Stdout>) {
    app.selection_mode = !app.selection_mode;
    if app.selection_mode {
        let _ = execute!(backend, DisableMouseCapture);
    } else {
        let _ = execute!(backend, EnableMouseCapture);
    }
}

/// Auto-focus the InputBar and insert a character at the cursor position
fn auto_focus_and_insert_char(app: &mut App, c: char) {
    use app::FocusedPane;

    // Switch focus to InputBar if not already there
    if app.focused_pane != FocusedPane::InputBar {
        app.focused_pane = FocusedPane::InputBar;
    }

    // Insert character at cursor position
    let byte_idx = char_index_to_byte_index(&app.input_text, app.input_cursor);
    app.input_text.insert(byte_idx, c);
    app.input_cursor += 1;

    // Detect autocomplete trigger
    app.detect_autocomplete_trigger();
}

/// Attach to a session in a split pane.
///
/// Connects to the daemon, pauses the headless session, builds an interactive
/// attach command, and launches it in a terminal split. If any step fails,
/// the session is detached so it resumes headless execution.
fn attach_session_split(session_name: &str) {
    let client = match crate::client::DaemonClient::connect() {
        Ok(c) => c,
        Err(_) => return,
    };

    // Pause the headless session and get session info.
    let info = match client.session_attach(&format!("name/{}", session_name)) {
        Ok(info) => info,
        Err(_) => return,
    };

    let session_id = match info.get("session_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            let _ = client.session_detach(session_name);
            return;
        }
    };
    let cwd = match info.get("cwd").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            let _ = client.session_detach(session_name);
            return;
        }
    };
    let provider = info
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("claude")
        .parse::<midtown::auth::AuthProvider>()
        .unwrap_or(midtown::auth::AuthProvider::Claude);

    let _ = midtown::platform_launch::run_platform_prelaunch_hook(provider);

    let cwd = match super::session::ensure_attach_worktree(session_name, cwd) {
        Ok(c) => c,
        Err(_) => {
            let _ = client.session_detach(session_name);
            return;
        }
    };

    let coworker_type = info
        .get("coworker_type")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let shell_command = match super::session::build_attach_shell_command(
        &cwd,
        session_name,
        provider,
        session_id,
        coworker_type.as_deref(),
    ) {
        Ok(cmd) => cmd,
        Err(_) => {
            let _ = client.session_detach(session_name);
            return;
        }
    };

    let host = super::daemon::AttachHost::detect();
    if super::daemon::launch_lead_split(host, &cwd, &shell_command).is_err() {
        let _ = client.session_detach(session_name);
    }
}

/// Attach to a coworker session in a split pane.
fn attach_coworker_split(coworker_name: &str) {
    attach_session_split(coworker_name);
}

/// Attach to the lead session in a split pane (Ctrl+L handler).
fn attach_lead_split() {
    attach_session_split("lead");
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::tests::test_app;
    use crossterm::event::{KeyEvent, KeyModifiers};

    /// Helper to create a key press event for a given KeyCode
    pub(super) fn key_press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn test_number_key_opens_diagram_when_source_exists() {
        let mut app = test_app();
        app.diagram_sources = vec!["graph TD\n  A-->B".into()];

        let result = handle_event(&mut app, key_press(KeyCode::Char('1')));
        assert!(
            matches!(result, EventResult::OpenDiagramInBrowser(0)),
            "Pressing '1' should open diagram at index 0"
        );
    }

    #[test]
    fn test_number_key_ignored_when_no_diagram_at_index() {
        let mut app = test_app();
        app.diagram_sources = vec!["graph TD\n  A-->B".into()]; // Only 1 diagram

        let result = handle_event(&mut app, key_press(KeyCode::Char('2')));
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
        let result = handle_event(&mut app, key_press(KeyCode::Char('9')));
        assert!(
            matches!(result, EventResult::OpenDiagramInBrowser(8)),
            "Pressing '9' should open diagram at index 8"
        );

        // There is no single key for index 9+ (diagrams 10, 11, 12)
        // Key '0' is not in the 1-9 range
        let result = handle_event(&mut app, key_press(KeyCode::Char('0')));
        assert!(
            matches!(result, EventResult::Continue),
            "Pressing '0' should not open any diagram"
        );
    }

    #[test]
    fn test_enter_does_not_open_diagram() {
        let mut app = test_app();
        app.diagram_sources = vec!["graph TD\n  A-->B".into()];

        let result = handle_event(&mut app, key_press(KeyCode::Enter));
        assert!(
            matches!(result, EventResult::Continue),
            "Enter should not open a diagram"
        );
    }

    #[test]
    fn test_esc_exits() {
        use app::FocusedPane;
        let mut app = test_app();

        // Esc should exit when not in InputBar
        let result = handle_event(&mut app, key_press(KeyCode::Esc));
        assert!(
            matches!(result, EventResult::Exit),
            "Esc should exit when not in InputBar"
        );

        // Esc should clear input when in InputBar
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "test".to_string();
        let result = handle_event(&mut app, key_press(KeyCode::Esc));
        assert!(
            matches!(result, EventResult::Continue),
            "Esc should continue when in InputBar"
        );
        assert_eq!(app.input_text, "", "Esc should clear input text");
    }

    #[test]
    fn test_q_key_inserts_into_input() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;

        // 'q' should now auto-focus and insert, not exit
        let result = handle_event(&mut app, key_press(KeyCode::Char('q')));
        assert!(
            matches!(result, EventResult::Continue),
            "'q' should continue, not exit"
        );
        assert_eq!(
            app.focused_pane,
            FocusedPane::InputBar,
            "'q' should auto-focus InputBar"
        );
        assert_eq!(app.input_text, "q", "'q' should insert into input");
    }

    #[test]
    fn test_number_key_with_empty_diagram_sources() {
        let mut app = test_app();
        // No diagrams visible
        assert!(app.diagram_sources.is_empty());

        let result = handle_event(&mut app, key_press(KeyCode::Char('1')));
        assert!(
            matches!(result, EventResult::Continue),
            "Number key with no diagrams should be Continue"
        );
    }

    #[test]
    fn test_type_anywhere_letter_keys_auto_focus_input() {
        use app::FocusedPane;
        let mut app = test_app();

        // Start with focus on Chat pane
        app.focused_pane = FocusedPane::Chat;
        assert!(app.input_text.is_empty(), "Input should start empty");

        // Type a letter - should auto-focus InputBar and insert character
        let result = handle_event(&mut app, key_press(KeyCode::Char('h')));
        assert!(
            matches!(result, EventResult::Continue),
            "Letter key should continue"
        );
        assert_eq!(
            app.focused_pane,
            FocusedPane::InputBar,
            "Focus should switch to InputBar"
        );
        assert_eq!(app.input_text, "h", "Character should be inserted");
        assert_eq!(app.input_cursor, 1, "Cursor should advance");
    }

    #[test]
    fn test_type_anywhere_from_board_pane() {
        use app::FocusedPane;
        let mut app = test_app();

        // Start with focus on Board pane
        app.focused_pane = FocusedPane::Board;

        // Type characters - should auto-focus and insert
        handle_event(&mut app, key_press(KeyCode::Char('t')));
        assert_eq!(app.focused_pane, FocusedPane::InputBar);
        assert_eq!(app.input_text, "t");

        handle_event(&mut app, key_press(KeyCode::Char('e')));
        assert_eq!(app.input_text, "te");

        handle_event(&mut app, key_press(KeyCode::Char('s')));
        assert_eq!(app.input_text, "tes");

        handle_event(&mut app, key_press(KeyCode::Char('t')));
        assert_eq!(app.input_text, "test");
    }

    #[test]
    fn test_punctuation_and_numbers_auto_focus() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;

        // Zero (not 1-9, so not diagram shortcut)
        handle_event(&mut app, key_press(KeyCode::Char('0')));
        assert_eq!(app.focused_pane, FocusedPane::InputBar);
        assert_eq!(app.input_text, "0");

        // Punctuation
        app.input_text.clear();
        app.input_cursor = 0;
        app.focused_pane = FocusedPane::Chat;

        handle_event(&mut app, key_press(KeyCode::Char('!')));
        assert_eq!(app.focused_pane, FocusedPane::InputBar);
        assert_eq!(app.input_text, "!");
    }

    #[test]
    fn test_backspace_auto_focuses_when_input_has_text() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;
        app.input_text = "hello".to_string();
        app.input_cursor = 5;

        // Backspace should auto-focus and delete
        handle_event(&mut app, key_press(KeyCode::Backspace));
        assert_eq!(app.focused_pane, FocusedPane::InputBar);
        assert_eq!(app.input_text, "hell");
        assert_eq!(app.input_cursor, 4);
    }

    #[test]
    fn test_backspace_no_effect_when_input_empty_and_not_focused() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;
        assert!(app.input_text.is_empty());

        // Backspace with empty input should not change focus
        handle_event(&mut app, key_press(KeyCode::Backspace));
        assert_eq!(app.focused_pane, FocusedPane::Chat);
    }

    #[test]
    fn test_enter_auto_focuses_input_bar() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;

        // Enter should auto-focus InputBar
        handle_event(&mut app, key_press(KeyCode::Enter));
        assert_eq!(app.focused_pane, FocusedPane::InputBar);
    }

    #[test]
    fn test_enter_sends_message_when_input_has_text() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "test message".to_string();

        handle_event(&mut app, key_press(KeyCode::Enter));
        // In test mode, posting fails because test_app() has no channel
        // Input should be preserved when posting fails
        assert_eq!(app.input_text, "test message");
        assert!(app.input_cursor <= app.input_text.len());
    }

    #[test]
    fn test_enter_does_nothing_when_input_is_empty() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "".to_string();
        app.input_cursor = 0;

        handle_event(&mut app, key_press(KeyCode::Enter));
        // Empty input should not trigger any posting logic
        assert_eq!(app.input_text, "");
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn test_enter_preserves_input_when_posting_unavailable() {
        use app::FocusedPane;
        // Create app with no channel (simulates posting unavailable)
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "test message".to_string();
        app.input_cursor = 12;

        // In test mode, daemon communication is skipped and posting fails
        // because test_app() has no channel. Input should be preserved.
        handle_event(&mut app, key_press(KeyCode::Enter));

        // Input should be preserved when posting fails
        assert_eq!(app.input_text, "test message");
        assert_eq!(app.input_cursor, 12);
    }

    #[test]
    fn test_arrow_keys_still_scroll_when_not_in_input() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;

        // Arrow keys should not auto-focus, they navigate
        handle_event(&mut app, key_press(KeyCode::Up));
        assert_eq!(
            app.focused_pane,
            FocusedPane::Chat,
            "Up arrow should not change focus"
        );

        handle_event(&mut app, key_press(KeyCode::Down));
        assert_eq!(
            app.focused_pane,
            FocusedPane::Chat,
            "Down arrow should not change focus"
        );
    }

    #[test]
    fn test_page_keys_still_work_when_not_in_input() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;

        // PageUp/PageDown should not auto-focus
        handle_event(&mut app, key_press(KeyCode::PageUp));
        assert_eq!(app.focused_pane, FocusedPane::Chat);

        handle_event(&mut app, key_press(KeyCode::PageDown));
        assert_eq!(app.focused_pane, FocusedPane::Chat);
    }

    #[test]
    fn test_diagram_shortcuts_still_work_without_auto_focus() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;
        app.diagram_sources = vec!["graph TD\n  A-->B".into()];

        // Number keys 1-9 for diagrams should NOT auto-focus input
        let result = handle_event(&mut app, key_press(KeyCode::Char('1')));
        assert!(matches!(result, EventResult::OpenDiagramInBrowser(0)));
        assert_eq!(
            app.focused_pane,
            FocusedPane::Chat,
            "Diagram shortcut should not change focus"
        );
        assert_eq!(
            app.input_text, "",
            "Diagram shortcut should not insert text"
        );
    }

    #[test]
    fn test_esc_clears_input_when_in_input_bar() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "test".to_string();
        app.input_cursor = 4;

        let result = handle_event(&mut app, key_press(KeyCode::Esc));
        assert!(matches!(result, EventResult::Continue));
        assert_eq!(app.input_text, "");
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn test_esc_exits_when_not_in_input_bar() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;

        let result = handle_event(&mut app, key_press(KeyCode::Esc));
        assert!(matches!(result, EventResult::Exit));
    }

    #[test]
    fn test_space_auto_focuses_input() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::Chat;

        handle_event(&mut app, key_press(KeyCode::Char(' ')));
        assert_eq!(app.focused_pane, FocusedPane::InputBar);
        assert_eq!(app.input_text, " ");
    }

    #[test]
    fn test_autocomplete_trigger_detection_at_start() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Type @ at start of input
        auto_focus_and_insert_char(&mut app, '@');
        assert!(
            app.autocomplete.show || app.autocomplete.trigger_type == Some('@'),
            "Autocomplete should detect @ trigger"
        );
        assert_eq!(app.autocomplete.query, "");
    }

    #[test]
    fn test_autocomplete_trigger_detection_after_space() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Type "hello @"
        for ch in "hello @".chars() {
            auto_focus_and_insert_char(&mut app, ch);
        }
        assert_eq!(app.autocomplete.trigger_type, Some('@'));
        assert_eq!(app.autocomplete.query, "");
    }

    #[test]
    fn test_autocomplete_with_query() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Type "@par" (partial coworker name)
        for ch in "@par".chars() {
            auto_focus_and_insert_char(&mut app, ch);
        }
        assert_eq!(app.autocomplete.trigger_type, Some('@'));
        assert_eq!(app.autocomplete.query, "par");
    }

    #[test]
    fn test_autocomplete_navigation() {
        use app::{AutocompleteItem, FocusedPane};
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Manually set up autocomplete state with multiple items
        app.autocomplete.show = true;
        app.autocomplete.items = vec![
            AutocompleteItem {
                value: "@lead".to_string(),
                description: None,
            },
            AutocompleteItem {
                value: "@park".to_string(),
                description: Some("Working on task 5".to_string()),
            },
            AutocompleteItem {
                value: "@madison".to_string(),
                description: None,
            },
        ];
        app.autocomplete.selected_index = 0;

        // Arrow down should move to next item
        app.autocomplete_select_next();
        assert_eq!(app.autocomplete.selected_index, 1);

        // Arrow down again
        app.autocomplete_select_next();
        assert_eq!(app.autocomplete.selected_index, 2);

        // Arrow down should wrap to first item
        app.autocomplete_select_next();
        assert_eq!(app.autocomplete.selected_index, 0);

        // Arrow up should wrap to last item
        app.autocomplete_select_prev();
        assert_eq!(app.autocomplete.selected_index, 2);
    }

    #[test]
    fn test_autocomplete_dismiss_on_escape() {
        use app::{AutocompleteItem, FocusedPane};
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Set up autocomplete
        app.autocomplete.show = true;
        app.autocomplete.items = vec![AutocompleteItem {
            value: "@lead".to_string(),
            description: None,
        }];

        // Press Escape
        handle_event(&mut app, key_press(KeyCode::Esc));
        assert!(!app.autocomplete.show, "Escape should dismiss autocomplete");
    }

    #[test]
    fn test_autocomplete_insert() {
        use app::{AutocompleteItem, FocusedPane};
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Set up input and autocomplete
        app.input_text = "@pa".to_string();
        app.input_cursor = 3;
        app.autocomplete.show = true;
        app.autocomplete.trigger_start_pos = 0;
        app.autocomplete.trigger_type = Some('@');
        app.autocomplete.query = "pa".to_string();
        app.autocomplete.selected_index = 0;
        app.autocomplete.items = vec![AutocompleteItem {
            value: "@park".to_string(),
            description: Some("Working on task 5".to_string()),
        }];

        // Insert autocomplete item
        app.insert_autocomplete_item();

        // Check that text was replaced and cursor moved
        assert_eq!(app.input_text, "@park ");
        assert_eq!(app.input_cursor, 6); // "@park " is 6 characters
        assert!(
            !app.autocomplete.show,
            "Autocomplete should be hidden after insert"
        );
    }

    #[test]
    fn test_autocomplete_no_trigger_in_middle_of_word() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Type "email@example" - @ in middle of word should not trigger
        for ch in "email@".chars() {
            auto_focus_and_insert_char(&mut app, ch);
        }
        assert!(
            !app.autocomplete.show,
            "Autocomplete should not trigger for @ in middle of word"
        );
    }

    /// Regression test for web UI binding-timing bug (verify TUI doesn't have it)
    ///
    /// The web UI had a bug where typing '@m' then 'a' then 'd' caused autocomplete
    /// to disappear because oninput fired before bind:value updated the inputText
    /// variable. The cursor was at position 4 but text was still "@m" (length 2),
    /// causing out-of-bounds access and detection failure.
    ///
    /// This test verifies the TUI doesn't have an analogous issue. In Rust, state
    /// updates are synchronous: input_text.insert() and input_cursor += 1 happen
    /// before detect_autocomplete_trigger() is called, so cursor and text are
    /// always in sync.
    #[test]
    fn test_autocomplete_no_binding_timing_bug() {
        use app::{CoworkerInfo, FocusedPane};
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Add test coworkers so autocomplete has items to show
        app.coworkers = vec![
            CoworkerInfo {
                name: "madison".to_string(),
                task_id: None,
                phase: None,
                pr_number: None,
                health: "green".to_string(),
                provider: "claude".to_string(),
                profile: "default".to_string(),
                progress: None,
                time_estimate: None,
            },
            CoworkerInfo {
                name: "lexington".to_string(),
                task_id: None,
                phase: None,
                pr_number: None,
                health: "green".to_string(),
                provider: "claude".to_string(),
                profile: "default".to_string(),
                progress: None,
                time_estimate: None,
            },
        ];

        // Type '@' - should trigger autocomplete for @lead
        auto_focus_and_insert_char(&mut app, '@');
        assert!(app.autocomplete.show, "Autocomplete should show after '@'");
        assert_eq!(app.autocomplete.trigger_type, Some('@'));
        assert_eq!(app.autocomplete.query, "");
        assert_eq!(app.input_text, "@");
        assert_eq!(app.input_cursor, 1);

        // Type 'm' - autocomplete detection should work (text and cursor in sync)
        auto_focus_and_insert_char(&mut app, 'm');
        // The key assertion: autocomplete detection didn't crash or fail due to
        // cursor being ahead of text (the web UI bug pattern)
        assert!(
            app.autocomplete.show,
            "Autocomplete should remain visible after 'm'"
        );
        assert_eq!(app.autocomplete.query, "m");
        assert_eq!(app.input_text, "@m");
        assert_eq!(app.input_cursor, 2);

        // Type 'a'
        auto_focus_and_insert_char(&mut app, 'a');
        assert!(
            app.autocomplete.show,
            "Autocomplete should remain visible after 'a'"
        );
        assert_eq!(app.autocomplete.query, "ma");
        assert_eq!(app.input_text, "@ma");
        assert_eq!(app.input_cursor, 3);

        // Type 'd' - this is where the web UI bug manifested
        // (cursor at 4, text still "@m" length 2 → out of bounds)
        // In TUI, both are updated synchronously before detection
        auto_focus_and_insert_char(&mut app, 'd');
        assert!(
            app.autocomplete.show,
            "Autocomplete should remain visible after 'd'"
        );
        assert_eq!(app.autocomplete.query, "mad");
        assert_eq!(app.input_text, "@mad");
        assert_eq!(app.input_cursor, 4);

        // Autocomplete detection succeeded without crashing
        assert_eq!(app.autocomplete.trigger_type, Some('@'));
        assert_eq!(app.autocomplete.trigger_start_pos, 0);
    }

    #[test]
    fn test_autocomplete_maintains_dropdown_when_typing_more_chars() {
        use app::{CoworkerInfo, FocusedPane};
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;

        // Populate test coworkers including "madison"
        app.coworkers = vec![
            CoworkerInfo {
                name: "madison".to_string(),
                task_id: Some(42),
                phase: Some("dev".to_string()),
                pr_number: None,
                health: "green".to_string(),
                provider: "claude".to_string(),
                profile: "default".to_string(),
                progress: Some(50),
                time_estimate: None,
            },
            CoworkerInfo {
                name: "park".to_string(),
                task_id: Some(43),
                phase: Some("PR".to_string()),
                pr_number: Some(123),
                health: "green".to_string(),
                provider: "claude".to_string(),
                profile: "default".to_string(),
                progress: Some(90),
                time_estimate: None,
            },
        ];

        // Type "@m" - autocomplete should appear
        for ch in "@m".chars() {
            auto_focus_and_insert_char(&mut app, ch);
            app.detect_autocomplete_trigger();
        }
        assert!(
            app.autocomplete.show,
            "Autocomplete should show after typing '@m'"
        );
        assert_eq!(app.autocomplete.trigger_type, Some('@'));
        assert_eq!(app.autocomplete.query, "m");

        // Type "a" to make it "@ma" - autocomplete should still be visible
        auto_focus_and_insert_char(&mut app, 'a');
        app.detect_autocomplete_trigger();
        assert!(
            app.autocomplete.show,
            "Autocomplete should still show after typing '@ma'"
        );
        assert_eq!(app.autocomplete.query, "ma");

        // Type "d" to make it "@mad" - autocomplete should STILL be visible
        auto_focus_and_insert_char(&mut app, 'd');
        app.detect_autocomplete_trigger();
        assert!(
            app.autocomplete.show,
            "Autocomplete should still show after typing '@mad' (this was the bug)"
        );
        assert_eq!(app.autocomplete.query, "mad");
    }

    #[test]
    fn test_enter_key_uses_selected_channel_not_hardcoded_midtown() {
        // This test verifies that messages are posted to app.selected_channel,
        // not hardcoded "midtown"

        let mut app = test_app();
        app.selected_channel = "custom-channel".to_string();
        app.input_text = "Test message".to_string();
        app.focused_pane = app::FocusedPane::InputBar;

        // Verify selected channel is not "midtown"
        assert_eq!(app.selected_channel, "custom-channel");

        // Press Enter to post the message
        let event = key_press(KeyCode::Enter);
        let result = handle_event(&mut app, event);

        assert!(matches!(result, EventResult::Continue));

        // Verify that post_message was called with "custom-channel", not "midtown"
        assert_eq!(
            app.last_posted_channel,
            Some("custom-channel".to_string()),
            "post_message should use selected_channel, not hardcoded 'midtown'"
        );
    }

    #[test]
    fn test_channel_create_command_clears_input() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "/channel create test-channel".to_string();
        app.input_cursor = app.input_text.len();

        // Press Enter to execute the command
        let event = key_press(KeyCode::Enter);
        let result = handle_event(&mut app, event);

        assert!(matches!(result, EventResult::Continue));
        // Input should be cleared after successful channel creation
        assert_eq!(app.input_text, "");
        assert_eq!(app.input_cursor, 0);
    }

    #[test]
    fn test_channel_create_command_switches_channel() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "/channel create new-channel".to_string();
        app.selected_channel = "midtown".to_string();

        // Press Enter to execute the command
        let event = key_press(KeyCode::Enter);
        let result = handle_event(&mut app, event);

        assert!(matches!(result, EventResult::Continue));
        // Should switch to the newly created channel
        assert_eq!(app.selected_channel, "new-channel");
    }

    #[test]
    fn test_channel_create_command_with_empty_name_does_nothing() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "/channel create   ".to_string();
        let original_channel = app.selected_channel.clone();

        // Press Enter to execute the command
        let event = key_press(KeyCode::Enter);
        let result = handle_event(&mut app, event);

        assert!(matches!(result, EventResult::Continue));
        // Should not create a channel or clear input with empty name
        assert_eq!(app.selected_channel, original_channel);
    }

    #[test]
    fn test_regular_message_not_treated_as_command() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "Let's /channel create a test later".to_string();

        // Press Enter to post the message
        let event = key_press(KeyCode::Enter);
        let result = handle_event(&mut app, event);

        assert!(matches!(result, EventResult::Continue));
        // Should NOT be treated as a channel create command
        // because /channel create is not at the start
        #[cfg(test)]
        assert_eq!(app.last_posted_channel, Some("midtown".to_string()));
    }

    #[test]
    fn test_channel_create_rejects_path_traversal() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "/channel create ../../etc/passwd".to_string();
        app.input_cursor = app.input_text.len();
        let original_channel = app.selected_channel.clone();

        let event = key_press(KeyCode::Enter);
        let result = handle_event(&mut app, event);

        assert!(matches!(result, EventResult::Continue));
        // Path traversal name should be rejected — input preserved, channel unchanged
        assert_eq!(
            app.input_text, "/channel create ../../etc/passwd",
            "Input should be preserved when channel name is invalid"
        );
        assert_eq!(
            app.selected_channel, original_channel,
            "Selected channel should not change on invalid name"
        );
    }

    #[test]
    fn test_channel_create_rejects_name_with_slashes() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        app.input_text = "/channel create foo/bar".to_string();
        app.input_cursor = app.input_text.len();
        let original_channel = app.selected_channel.clone();

        let event = key_press(KeyCode::Enter);
        handle_event(&mut app, event);

        // Slash in name should be rejected
        assert_eq!(
            app.input_text, "/channel create foo/bar",
            "Input should be preserved when channel name contains slash"
        );
        assert_eq!(app.selected_channel, original_channel);
    }

    #[test]
    fn test_channel_create_preserves_input_on_failure() {
        use app::FocusedPane;
        let mut app = test_app();
        app.focused_pane = FocusedPane::InputBar;
        // Use a name with backslash which should be rejected by validation
        app.input_text = "/channel create bad\\name".to_string();
        app.input_cursor = app.input_text.len();
        let original_cursor = app.input_cursor;

        let event = key_press(KeyCode::Enter);
        handle_event(&mut app, event);

        // On failure, input text and cursor should be preserved
        assert_eq!(
            app.input_text, "/channel create bad\\name",
            "Input text should be preserved on creation failure"
        );
        assert_eq!(
            app.input_cursor, original_cursor,
            "Input cursor should be preserved on creation failure"
        );
    }

    /// Regression test: Shift+letter should insert uppercase character.
    ///
    /// With the kitty keyboard protocol (REPORT_ALL_KEYS_AS_ESCAPE_CODES),
    /// Shift+A is reported as KeyCode::Char('a') + KeyModifiers::SHIFT.
    /// The handler must convert to uppercase rather than inserting lowercase.
    #[test]
    fn test_shift_letter_inserts_uppercase() {
        let mut app = test_app();

        // Simulate kitty protocol: Shift+A = lowercase 'a' + SHIFT modifier
        let event = Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT));
        handle_event(&mut app, event);

        assert_eq!(app.input_text, "A", "Shift+a should insert uppercase 'A'");
    }

    /// Regression test: plain letter without Shift should insert lowercase.
    #[test]
    fn test_plain_letter_inserts_lowercase() {
        let mut app = test_app();

        let event = key_press(KeyCode::Char('a'));
        handle_event(&mut app, event);

        assert_eq!(app.input_text, "a", "Plain 'a' should insert lowercase 'a'");
    }

    /// Helper to create a mouse click event at the given coordinates
    fn mouse_click(column: u16, row: u16) -> Event {
        use crossterm::event::{MouseButton, MouseEvent};
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Test that clicking on a channel header in the board panel selects that channel
    #[test]
    fn test_click_channel_header_selects_channel() {
        use app::{BoardSelection, KanbanTask, TaskStatus};
        use midtown::ChannelInfo;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        // Set up multiple channels with tasks
        app.available_channels = vec![
            ChannelInfo {
                name: "midtown".to_string(),
                is_archived: false,
            },
            ChannelInfo {
                name: "feature-x".to_string(),
                is_archived: false,
            },
        ];
        app.tasks = vec![
            KanbanTask {
                id: "1".to_string(),
                subject: "Task in midtown".to_string(),
                owner: None,
                status: TaskStatus::Pending,
                modified_at: None,
                channel: Some("midtown".to_string()),
                blocked_by: vec![],
            },
            KanbanTask {
                id: "2".to_string(),
                subject: "Task in feature-x".to_string(),
                owner: None,
                status: TaskStatus::Pending,
                modified_at: None,
                channel: Some("feature-x".to_string()),
                blocked_by: vec![],
            },
        ];
        app.selected_channel = "midtown".to_string();

        // First draw to populate channel_line_map and board_area
        terminal
            .draw(|f| {
                // Use ui::draw to populate both channel_line_map and board_area
                ui::draw(f, &mut app);
            })
            .unwrap();

        // Verify channel_line_map was populated
        assert!(
            !app.channel_line_map.is_empty(),
            "channel_line_map should be populated after render"
        );

        // Find which line number corresponds to the "feature-x" channel header
        let feature_x_line = app
            .channel_line_map
            .iter()
            .find(|(_, name)| *name == "feature-x")
            .map(|(line, _)| *line)
            .expect("feature-x should be in channel_line_map");

        // Click on the feature-x channel header
        // board_area.y is the top of the board (with border), so add 1 for border + feature_x_line for content
        let board_area = app.board_area.unwrap();
        let click_y = board_area.y + 1 + feature_x_line;
        let click_x = board_area.x + 5; // Somewhere in the middle of the channel header

        let click_event = mouse_click(click_x, click_y);
        let result = handle_event(&mut app, click_event);

        assert!(
            matches!(result, EventResult::Continue),
            "Mouse click should continue"
        );

        // Verify board selection was updated to the clicked channel
        assert_eq!(
            app.board_selection,
            Some(BoardSelection::Channel("feature-x".to_string())),
            "Clicking channel header should select that channel"
        );

        // Verify selected_channel was updated
        assert_eq!(
            app.selected_channel, "feature-x",
            "Clicking channel header should update selected_channel"
        );
    }

    /// Test that clicking on a channel header when already selected keeps it selected
    #[test]
    fn test_click_already_selected_channel_maintains_selection() {
        use app::{BoardSelection, KanbanTask, TaskStatus};
        use midtown::ChannelInfo;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        app.available_channels = vec![ChannelInfo {
            name: "midtown".to_string(),
            is_archived: false,
        }];
        app.tasks = vec![KanbanTask {
            id: "1".to_string(),
            subject: "Task in midtown".to_string(),
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: Some("midtown".to_string()),
            blocked_by: vec![],
        }];
        app.selected_channel = "midtown".to_string();
        app.board_selection = Some(BoardSelection::Channel("midtown".to_string()));

        // Render to populate maps and board_area
        terminal
            .draw(|f| {
                ui::draw(f, &mut app);
            })
            .unwrap();

        let midtown_line = app
            .channel_line_map
            .iter()
            .find(|(_, name)| *name == "midtown")
            .map(|(line, _)| *line)
            .unwrap();

        let board_area = app.board_area.unwrap();
        let click_y = board_area.y + 1 + midtown_line;
        let click_x = board_area.x + 5;

        // Click on already-selected channel
        handle_event(&mut app, mouse_click(click_x, click_y));

        // Should still be selected
        assert_eq!(
            app.board_selection,
            Some(BoardSelection::Channel("midtown".to_string()))
        );
        assert_eq!(app.selected_channel, "midtown");
    }

    /// Test divider drag: clicking the divider column starts dragging
    #[test]
    fn test_click_divider_starts_drag() {
        let mut app = test_app();
        app.divider_x = Some(32);

        let event = mouse_click(32, 5);
        let result = handle_event(&mut app, event);

        assert!(
            matches!(result, EventResult::Continue),
            "Clicking divider should continue"
        );
        assert!(
            app.dragging_divider,
            "dragging_divider should be set after clicking divider"
        );
    }

    /// Test that clicking adjacent to but not on the divider does not start drag
    #[test]
    fn test_click_near_divider_does_not_start_drag() {
        let mut app = test_app();
        app.divider_x = Some(32);

        // Click one column away from the divider
        handle_event(&mut app, mouse_click(31, 5));
        assert!(
            !app.dragging_divider,
            "Clicking adjacent to divider should not start drag"
        );

        handle_event(&mut app, mouse_click(33, 5));
        assert!(
            !app.dragging_divider,
            "Clicking adjacent to divider (right) should not start drag"
        );
    }

    /// Helper to create a mouse drag event
    fn mouse_drag(column: u16, row: u16) -> Event {
        use crossterm::event::{MouseButton, MouseEvent};
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Helper to create a mouse up event
    fn mouse_up(column: u16, row: u16) -> Event {
        use crossterm::event::{MouseButton, MouseEvent};
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Test full drag sequence: mousedown on divider → drag → release
    #[test]
    fn test_drag_divider_resizes_sidebar() {
        let mut app = test_app();
        app.divider_x = Some(40); // divider at column 40
        app.layout_width = 100; // 100-column terminal
        app.sidebar_width_pct = 40;

        // Click divider to start drag
        handle_event(&mut app, mouse_click(40, 5));
        assert!(app.dragging_divider);

        // Drag to column 50 → should set sidebar to 50%
        handle_event(&mut app, mouse_drag(50, 5));
        assert_eq!(
            app.sidebar_width_pct, 50,
            "Dragging to column 50 in 100-wide terminal should set sidebar to 50%"
        );

        // Drag to column 30 → should set sidebar to 30%
        handle_event(&mut app, mouse_drag(30, 5));
        assert_eq!(app.sidebar_width_pct, 30);

        // Release mouse → dragging should stop
        handle_event(&mut app, mouse_up(30, 5));
        assert!(!app.dragging_divider, "Mouse up should stop dragging");

        // Drag after release should not change width
        handle_event(&mut app, mouse_drag(60, 5));
        assert_eq!(
            app.sidebar_width_pct, 30,
            "Drag after mouse up should not resize"
        );
    }

    /// Test that drag is clamped to min width (20%)
    #[test]
    fn test_drag_clamps_to_min_width() {
        let mut app = test_app();
        app.divider_x = Some(40);
        app.layout_width = 100;
        app.dragging_divider = true;

        // Drag to column 5 → should clamp to 20%
        handle_event(&mut app, mouse_drag(5, 5));
        assert_eq!(
            app.sidebar_width_pct, 20,
            "Sidebar should clamp to 20% minimum"
        );
    }

    /// Test that drag is clamped to max width (60%)
    #[test]
    fn test_drag_clamps_to_max_width() {
        let mut app = test_app();
        app.divider_x = Some(40);
        app.layout_width = 100;
        app.dragging_divider = true;

        // Drag to column 90 → should clamp to 60%
        handle_event(&mut app, mouse_drag(90, 5));
        assert_eq!(
            app.sidebar_width_pct, 60,
            "Sidebar should clamp to 60% maximum"
        );
    }

    /// Test that non-dragging drag events are ignored
    #[test]
    fn test_drag_without_mousedown_is_ignored() {
        let mut app = test_app();
        app.layout_width = 100;
        app.sidebar_width_pct = 40;
        app.dragging_divider = false;

        handle_event(&mut app, mouse_drag(60, 5));
        assert_eq!(
            app.sidebar_width_pct, 40,
            "Drag without prior mousedown on divider should not resize"
        );
    }

    /// Test that clicking outside the board area does not trigger channel selection
    #[test]
    fn test_click_outside_board_does_not_select_channel() {
        use app::{KanbanTask, TaskStatus};
        use midtown::ChannelInfo;
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();

        let mut app = test_app();
        app.available_channels = vec![ChannelInfo {
            name: "midtown".to_string(),
            is_archived: false,
        }];
        app.tasks = vec![KanbanTask {
            id: "1".to_string(),
            subject: "Task in midtown".to_string(),
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: Some("midtown".to_string()),
            blocked_by: vec![],
        }];
        app.selected_channel = "midtown".to_string();
        app.board_selection = None;

        // Render to populate maps and board_area
        terminal
            .draw(|f| {
                ui::draw(f, &mut app);
            })
            .unwrap();

        let board_area = app.board_area.unwrap();
        // Click far outside the board area
        let click_x = board_area.x + board_area.width + 10;
        let click_y = board_area.y + 5;

        handle_event(&mut app, mouse_click(click_x, click_y));

        // Board selection should remain None
        assert_eq!(app.board_selection, None);
        // Selected channel should not change
        assert_eq!(app.selected_channel, "midtown");
    }
}

#[path = "channel_switcher_tests.rs"]
#[cfg(test)]
mod channel_switcher_tests;

#[path = "keyboard_protocol_tests.rs"]
#[cfg(test)]
mod keyboard_protocol_tests;
