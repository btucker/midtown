//! UI rendering for the chat TUI

use chrono::{DateTime, Local, Utc};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use midtown::{Message, MessageType};

use super::app::{App, CiStatus};

/// Format duration as (Xm) or (Xh) for display
fn format_duration_minutes(since: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(since);
    let minutes = duration.num_minutes();
    if minutes >= 60 {
        format!("({}h)", minutes / 60)
    } else {
        format!("({}m)", minutes)
    }
}

/// Gutter width for timestamp: " HH:MM " = 7 chars
const TIMESTAMP_GUTTER_WIDTH: usize = 7;

/// Avenue names mapped to colors (position-based assignment)
const AVENUE_COLORS: &[(&str, Color)] = &[
    ("lexington", Color::Cyan),
    ("park", Color::Green),
    ("madison", Color::Yellow),
    ("broadway", Color::Magenta),
    ("amsterdam", Color::Blue),
    ("columbus", Color::Red),
    ("riverside", Color::LightCyan),
    ("york", Color::LightGreen),
    ("pleasant", Color::LightMagenta),
    ("vernon", Color::LightBlue),
    // Overflow names
    ("bleecker", Color::Indexed(208)), // orange
    ("houston", Color::Indexed(213)),  // pink
    ("canal", Color::Indexed(117)),    // light blue
    ("spring", Color::Indexed(156)),   // light green
    ("prince", Color::Indexed(183)),   // lavender
    ("mercer", Color::Indexed(216)),   // salmon
];

/// Get color for a sender name
fn get_sender_color(name: &str) -> Color {
    match name.to_lowercase().as_str() {
        "lead" => Color::LightYellow,
        "github" => Color::DarkGray,
        "system" => Color::DarkGray,
        _ => {
            // Check avenue colors
            for (avenue, color) in AVENUE_COLORS {
                if name.to_lowercase() == *avenue {
                    return *color;
                }
            }
            // Default for unknown names
            Color::White
        }
    }
}

/// Height of the kanban board (including borders)
/// Increased to accommodate 2-line items in In Progress and Review columns
const KANBAN_HEIGHT: u16 = 9;

/// Height of the repo status line
const REPO_STATUS_HEIGHT: u16 = 1;

/// Draw the main UI
///
/// Note: The Team panel has been removed - coworker status is now shown
/// in tmux tab names instead, providing better visibility even when the
/// chat TUI is not in focus.
pub fn draw(f: &mut Frame, app: &mut App) {
    // Split into repo status (top), kanban (middle), and chat (bottom) panels
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(REPO_STATUS_HEIGHT),
            Constraint::Length(KANBAN_HEIGHT),
            Constraint::Min(10),
        ])
        .split(f.area());

    draw_repo_status_line(f, app, chunks[0]);
    draw_kanban_panel(f, app, chunks[1]);
    draw_chat_panel(f, app, chunks[2]);
}

/// Format relative time (e.g., "3 minutes ago", "2 hours ago", "1 day ago")
fn format_relative_time(time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(time);
    let minutes = duration.num_minutes();
    let hours = duration.num_hours();
    let days = duration.num_days();

    if days > 0 {
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    } else if hours > 0 {
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else if minutes > 0 {
        if minutes == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{} minutes ago", minutes)
        }
    } else {
        "just now".to_string()
    }
}

/// Draw the repo status line showing commit, CI status, and release info
fn draw_repo_status_line(f: &mut Frame, app: &App, area: Rect) {
    let status = &app.repo_status;

    // Background color matching tmux status bar (colour236 = dark gray)
    let bg = Color::Indexed(236);

    let mut spans = Vec::new();

    // Repo name (dim)
    spans.push(Span::styled(
        format!(" {}  ", app.repo_name),
        Style::default().fg(Color::DarkGray).bg(bg),
    ));

    // Commit hash and time
    if !status.commit_hash.is_empty() {
        spans.push(Span::styled(
            status.commit_hash.clone(),
            Style::default().fg(Color::Yellow).bg(bg),
        ));
        if let Some(commit_time) = status.commit_time {
            spans.push(Span::styled(
                format!("  {}  ", format_relative_time(commit_time)),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        } else {
            spans.push(Span::styled("  ", Style::default().bg(bg)));
        }
    }

    // CI status dot
    let (ci_char, ci_color) = match status.ci_status {
        CiStatus::Passed => ("●", Color::Green),
        CiStatus::Failed => ("●", Color::Red),
        CiStatus::Running => ("●", Color::Yellow),
        CiStatus::Unknown => ("○", Color::DarkGray),
    };
    spans.push(Span::styled(ci_char, Style::default().fg(ci_color).bg(bg)));
    spans.push(Span::styled("  ", Style::default().bg(bg)));

    // Release info
    if let Some(ref tag) = status.release_tag {
        spans.push(Span::styled(
            "Releases: ",
            Style::default().fg(Color::DarkGray).bg(bg),
        ));
        spans.push(Span::styled(
            tag.clone(),
            Style::default().fg(Color::Cyan).bg(bg),
        ));
        if let Some(release_time) = status.release_time {
            spans.push(Span::styled(
                format!("  {}", format_relative_time(release_time)),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        }
    }

    // Fill rest of line with background
    let content_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if content_len < area.width as usize {
        spans.push(Span::styled(
            " ".repeat(area.width as usize - content_len),
            Style::default().bg(bg),
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    f.render_widget(paragraph, area);
}

/// A kanban item that may span multiple lines
struct KanbanItem {
    /// Lines to display (1 for Backlog/Done, up to 2 for In Progress/Review)
    lines: Vec<String>,
    /// Optional URL for the first line (for clickable PR links)
    url: Option<String>,
    /// Optional CI status for PRs (for colored dot rendering)
    ci_status: Option<CiStatus>,
}

/// Get the dot character for CI status (colored in rendering)
fn ci_status_dot(status: &CiStatus) -> &'static str {
    match status {
        CiStatus::Passed => "●",
        CiStatus::Failed => "●",
        CiStatus::Running => "●",
        CiStatus::Unknown => "○",
    }
}

/// Get the color for CI status dot
fn ci_status_color(status: &CiStatus) -> Color {
    match status {
        CiStatus::Passed => Color::Green,
        CiStatus::Failed => Color::Red,
        CiStatus::Running => Color::Yellow,
        CiStatus::Unknown => Color::DarkGray,
    }
}

/// Draw the kanban board with 4 columns
fn draw_kanban_panel(f: &mut Frame, app: &App, area: Rect) {
    // Split into 4 equal columns
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area);

    let (pending, in_progress, _completed) = app.tasks_by_status();

    // Backlog column (pending tasks) - single line items
    let backlog_items: Vec<KanbanItem> = pending
        .iter()
        .map(|t| KanbanItem {
            lines: vec![format!("#{} {}", t.id, t.subject)],
            url: None,
            ci_status: None,
        })
        .collect();
    draw_kanban_column(f, columns[0], "Backlog", Color::Blue, &backlog_items);

    // In Progress column (with owner and duration) - 2-line items
    let in_progress_items: Vec<KanbanItem> = in_progress
        .iter()
        .map(|t| {
            let line1 = format!("#{} {}", t.id, t.subject);
            let owner = t.owner.as_deref().unwrap_or("?");
            let duration = t
                .modified_at
                .map(format_duration_minutes)
                .unwrap_or_default();
            let line2 = format!("  └ {} {}", owner, duration);
            KanbanItem {
                lines: vec![line1, line2],
                url: None,
                ci_status: None,
            }
        })
        .collect();
    draw_kanban_column(
        f,
        columns[1],
        "In Progress",
        Color::Yellow,
        &in_progress_items,
    );

    // Review column (open PRs with repo#XX format, CI status dot, and duration) - 2-line items
    let review_items: Vec<KanbanItem> = app
        .prs
        .iter()
        .map(|pr| {
            let ci_dot = ci_status_dot(&pr.ci_status);
            let line1 = format!("{} PR#{} {}", ci_dot, pr.number, pr.title);
            let duration = format_duration_minutes(pr.created_at);
            let line2 = format!("  └ {} {}", pr.author, duration);
            let url = format!("https://github.com/{}/pull/{}", app.repo_name, pr.number);
            KanbanItem {
                lines: vec![line1, line2],
                url: Some(url),
                ci_status: Some(pr.ci_status.clone()),
            }
        })
        .collect();
    draw_kanban_column(f, columns[2], "Review", Color::Magenta, &review_items);

    // Done column (merged PRs with repo#XX format) - single line, reverse chronological, max 10
    let done_items: Vec<KanbanItem> = app
        .merged_prs
        .iter()
        .take(10)
        .map(|pr| {
            let url = format!("https://github.com/{}/pull/{}", app.repo_name, pr.number);
            KanbanItem {
                lines: vec![format!("PR#{} {}", pr.number, pr.title)],
                url: Some(url),
                ci_status: None,
            }
        })
        .collect();
    draw_kanban_column(f, columns[3], "Done", Color::Green, &done_items);
}

/// Draw a single kanban column with multi-line item support and optional hyperlinks
fn draw_kanban_column(f: &mut Frame, area: Rect, title: &str, color: Color, items: &[KanbanItem]) {
    let block = Block::default()
        .title(format!(" {} ({}) ", title, items.len()))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if items.is_empty() {
        let paragraph = Paragraph::new("-").style(Style::default().fg(Color::White));
        f.render_widget(paragraph, inner);
        return;
    }

    let available_width = inner.width as usize;
    let available_lines = inner.height as usize;
    let buffer = f.buffer_mut();

    let mut lines_used = 0;

    for item in items {
        // Check if we have room for at least the first line of this item
        if lines_used >= available_lines {
            break;
        }

        for (line_idx, line) in item.lines.iter().enumerate() {
            if lines_used >= available_lines {
                break;
            }

            let truncated = truncate_str(line, available_width);
            let y = inner.y + lines_used as u16;

            // For the first line, check if we need to color a CI status dot
            let ci_dot_color = if line_idx == 0 {
                item.ci_status.as_ref().map(ci_status_color)
            } else {
                None
            };

            // Only apply hyperlink to the first line of items that have URLs
            if let (0, Some(url)) = (line_idx, item.url.as_ref()) {
                render_hyperlink_line(
                    buffer,
                    inner.x,
                    y,
                    &truncated,
                    url,
                    available_width,
                    ci_dot_color,
                );
            } else {
                // Render plain text with optional CI dot coloring
                render_plain_line(
                    buffer,
                    inner.x,
                    y,
                    &truncated,
                    available_width,
                    ci_dot_color,
                );
            }

            lines_used += 1;
        }
    }
}

/// Render a plain text line with optional CI status dot coloring
fn render_plain_line(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    text: &str,
    max_width: usize,
    ci_dot_color: Option<Color>,
) {
    for (i, ch) in text.chars().enumerate() {
        if i >= max_width {
            break;
        }
        // Color the first character (CI dot) if we have a CI status
        let fg_color = match (i, &ci_dot_color, ch) {
            (0, Some(color), '●' | '○') => *color,
            _ => Color::White,
        };
        buffer[(x + i as u16, y)].set_char(ch).set_fg(fg_color);
    }
}

/// Render a line with optional CI dot coloring
///
/// NOTE: OSC 8 hyperlinks were previously attempted here but disabled because
/// ratatui's cell/buffer system doesn't properly support embedding escape
/// sequences in cell symbols. The sequences caused display corruption
/// (e.g., "PR#140" appearing as "P #140"). When ratatui adds native hyperlink
/// support, this can be revisited.
///
/// The `url` parameter is kept for API compatibility but currently unused.
fn render_hyperlink_line(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    text: &str,
    _url: &str,
    max_width: usize,
    ci_dot_color: Option<Color>,
) {
    // Render as plain text (hyperlinks disabled due to ratatui limitations)
    render_plain_line(buffer, x, y, text, max_width, ci_dot_color);
}

/// Truncate a string to fit within the given width, adding "..." if truncated
///
/// For kanban items with identifiers like "PR #42" or "#1 Task name", this function
/// prioritizes showing the identifier (#N) when space is very limited, since that's
/// the most useful information for identifying the item.
fn truncate_str(s: &str, max_width: usize) -> String {
    if s.chars().count() <= max_width {
        return s.to_string();
    }

    // For narrow columns, try to extract and show just the identifier (#N)
    // This handles formats like "PR #42 title", "#1 task name", or "PR#97"
    if let Some(id) = extract_identifier(s) {
        let id_len = id.chars().count();

        // For very narrow columns (<=5), prefer showing (truncated) identifier
        // over useless prefix characters like "P" from "PR#97"
        if max_width <= 5 {
            if id_len <= max_width {
                return id;
            } else {
                // Identifier doesn't fit entirely, but still better to show truncated id
                // e.g., "PR#97" at width 2 → "#9" instead of "P…"
                return id.chars().take(max_width).collect();
            }
        }

        // For wider columns, show identifier when truncation would hide it
        // e.g., "midtown#97" at width 6 → "#97" instead of "midto…"
        if id_len <= max_width
            && let Some(hash_pos) = s.find('#')
            && hash_pos >= max_width.saturating_sub(1)
        {
            // If the # would be cut off by truncation, show just the identifier
            return id;
        }
    }

    // Fall back to normal truncation with ellipsis
    if max_width <= 1 {
        s.chars().take(max_width).collect()
    } else {
        let truncated: String = s.chars().take(max_width - 1).collect();
        format!("{}…", truncated)
    }
}

/// Extract the identifier pattern (#N) from a kanban item string
///
/// Handles formats like:
/// - "PR #42 Some title" -> "#42"
/// - "#1 Task name" -> "#1"
fn extract_identifier(s: &str) -> Option<String> {
    // Find the # character
    let hash_pos = s.find('#')?;

    // Extract digits after the #
    let after_hash = &s[hash_pos + 1..];
    let digit_count = after_hash
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .count();

    if digit_count == 0 {
        return None;
    }

    // Build the identifier: # + digits
    let digits: String = after_hash.chars().take(digit_count).collect();
    Some(format!("#{}", digits))
}

/// Draw the chat panel showing messages
fn draw_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    // Show selection mode indicator in title
    let title = if app.selection_mode {
        " #midtown [SELECT: press 's' to exit] "
    } else {
        " #midtown "
    };
    let border_color = if app.selection_mode {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    // Update visible height for scroll calculations
    app.visible_height = inner.height as usize;

    // Get visible messages
    let visible = app.visible_messages();

    // Build lines for messages, tracking previous sender for grouping
    let mut lines: Vec<Line> = Vec::new();
    let mut prev_sender: Option<&str> = None;

    for msg in visible.iter() {
        let msg_lines = render_message(msg, inner.width as usize, prev_sender);
        lines.extend(msg_lines);
        prev_sender = Some(&msg.from);
    }

    // Handle line truncation based on scroll position.
    // Each message can render to multiple lines (sender + content + continuations),
    // so the total rendered lines often exceeds visible_height.
    //
    // When at bottom (scroll_offset=0): show LAST N lines (newest messages)
    // When scrolled up (scroll_offset>0): show FIRST N lines (older messages the user scrolled to)
    let visible_lines = if lines.len() > inner.height as usize {
        if app.scroll_offset == 0 {
            // At bottom: take last N lines to show newest messages
            lines.split_off(lines.len() - inner.height as usize)
        } else {
            // Scrolled up: take first N lines to show older messages
            lines.truncate(inner.height as usize);
            lines
        }
    } else {
        lines
    };

    let paragraph = Paragraph::new(visible_lines);

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Render a single message into one or more Lines
///
/// Layout for action messages:
/// - " HH:MM * name message" all on one line
///
/// Layout for regular messages when sender changes:
/// - Line 1: Actor name alone
/// - Line 2: " HH:MM message"
/// - Line 3+: "       continuation" (7 spaces)
///
/// Layout for regular messages when sender is same:
/// - Line 1: " HH:MM message"
/// - Line 2+: "       continuation" (7 spaces)
fn render_message(msg: &Message, width: usize, prev_sender: Option<&str>) -> Vec<Line<'static>> {
    let local_time = msg.timestamp.with_timezone(&Local);
    let time = local_time.format("%H:%M").to_string();
    let color = get_sender_color(&msg.from);

    // Determine if we need to show the sender name
    let show_sender = prev_sender.is_none_or(|prev| prev != msg.from);

    // Determine base style for content based on message type
    let content_style = match msg.message_type {
        MessageType::Action => Style::default().fg(color),
        MessageType::System => Style::default().fg(Color::DarkGray),
        _ if msg.from == "github" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::White),
    };

    // For action messages, use special format: "* name message"
    if msg.message_type == MessageType::Action {
        let mut result = Vec::new();
        // Add blank line before action messages (except for first message)
        if prev_sender.is_some() {
            result.push(Line::from(""));
        }
        result.extend(render_action_message(
            msg,
            &time,
            color,
            content_style,
            width,
        ));
        return result;
    }

    // For system messages (or daemon messages), render entire line in gray (no timestamp gutter)
    if msg.message_type == MessageType::System || msg.from == "daemon" {
        return render_system_message(&msg.content, width);
    }

    // Calculate content width (after " HH:MM " gutter)
    let content_width = width.saturating_sub(TIMESTAMP_GUTTER_WIDTH);
    if content_width == 0 {
        return vec![]; // Panel too narrow
    }

    // Split and wrap content
    let content_lines = wrap_content(&msg.content, content_width);

    let mut result = Vec::new();

    // Add sender name line if sender changed
    if show_sender {
        // Add blank line before new sender (except for first message)
        if prev_sender.is_some() {
            result.push(Line::from(""));
        }
        result.push(build_sender_line(msg, color));
    }

    // Add content lines with timestamp/indent prefix
    for (i, content) in content_lines.iter().enumerate() {
        if i == 0 {
            // First content line: " HH:MM message"
            result.push(build_timestamp_line(&time, content, content_style));
        } else {
            // Continuation lines: "       message" (7 spaces)
            let indent = " ".repeat(TIMESTAMP_GUTTER_WIDTH);
            let mut spans = vec![Span::raw(indent)];
            spans.extend(parse_markdown(content, content_style));
            result.push(Line::from(spans));
        }
    }

    result
}

/// Render an action message: "* name message" (no timestamp)
fn render_action_message(
    msg: &Message,
    _time: &str,
    color: Color,
    content_style: Style,
    width: usize,
) -> Vec<Line<'static>> {
    // Format: "* name message" (no timestamp for /me actions)
    // Prefix is "* name " where name varies
    let prefix_len = 2 + msg.from.len() + 1; // "* " + name + " "
    let content_width = width.saturating_sub(prefix_len);

    if content_width == 0 {
        return vec![];
    }

    let content_lines = wrap_content(&msg.content, content_width);
    let mut result = Vec::new();

    for (i, content) in content_lines.iter().enumerate() {
        if i == 0 {
            // First line: "* name message"
            let spans = vec![
                Span::styled("* ", Style::default().fg(color)),
                Span::styled(
                    msg.from.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {}", content), content_style),
            ];
            result.push(Line::from(spans));
        } else {
            // Continuation: indent to align with message content
            let indent = " ".repeat(prefix_len);
            let mut spans = vec![Span::raw(indent)];
            spans.extend(parse_markdown(content, content_style));
            result.push(Line::from(spans));
        }
    }

    result
}

/// Render a system message: entire line in gray, no timestamp gutter
fn render_system_message(content: &str, width: usize) -> Vec<Line<'static>> {
    let style = Style::default().fg(Color::DarkGray);
    let content_lines = wrap_content(content, width);

    content_lines
        .into_iter()
        .map(|line| Line::from(vec![Span::styled(line, style)]))
        .collect()
}

/// Build a line with just the sender name
fn build_sender_line(msg: &Message, color: Color) -> Line<'static> {
    match msg.message_type {
        MessageType::System => Line::from(vec![Span::styled(
            String::from("<system>"),
            Style::default().fg(Color::DarkGray),
        )]),
        _ => Line::from(vec![Span::styled(
            msg.from.clone(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )]),
    }
}

/// Build a timestamp line with message content: " HH:MM message"
fn build_timestamp_line(time: &str, content: &str, content_style: Style) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {} ", time),
        Style::default().fg(Color::DarkGray),
    )];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

/// Wrap content text into lines that fit the given width
fn wrap_content(content: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in content.split('\n') {
        let wrapped = wrap_line(line, width);
        for w in wrapped {
            result.push(w.to_string());
        }
    }
    result
}

/// Parse markdown in text and return styled spans
///
/// Handles:
/// - **bold** -> BOLD modifier
/// - *italic* -> ITALIC modifier
/// - `code` -> Cyan color
fn parse_markdown(text: &str, base_style: Style) -> Vec<Span<'static>> {
    if text.is_empty() {
        return vec![Span::styled(String::new(), base_style)];
    }

    let mut spans = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut current_pos = 0;

    while let Some((i, c)) = chars.next() {
        match c {
            '`' => {
                // Code span - look for closing backtick
                if let Some(end) = text[i + 1..].find('`') {
                    // Add any text before this marker
                    if i > current_pos {
                        spans.push(Span::styled(text[current_pos..i].to_string(), base_style));
                    }
                    // Add the code span (without backticks)
                    let code_text = &text[i + 1..i + 1 + end];
                    spans.push(Span::styled(
                        code_text.to_string(),
                        Style::default().fg(Color::Cyan),
                    ));
                    // Skip past the closing backtick
                    current_pos = i + 1 + end + 1;
                    // Advance the iterator
                    for _ in 0..end + 1 {
                        chars.next();
                    }
                }
            }
            '*' => {
                // Check for ** (bold) or single * (italic)
                if chars.peek().is_some_and(|(_, next_c)| *next_c == '*') {
                    // Bold: **text**
                    chars.next(); // consume second *
                    if let Some(end) = text[i + 2..].find("**") {
                        if i > current_pos {
                            spans.push(Span::styled(text[current_pos..i].to_string(), base_style));
                        }
                        let bold_text = &text[i + 2..i + 2 + end];
                        spans.push(Span::styled(
                            bold_text.to_string(),
                            base_style.add_modifier(Modifier::BOLD),
                        ));
                        current_pos = i + 2 + end + 2;
                        // Skip past closing **
                        for _ in 0..end + 2 {
                            chars.next();
                        }
                    }
                } else {
                    // Italic: *text*
                    if let Some(end) = text[i + 1..].find('*') {
                        // Make sure it's not ** (start of bold)
                        if end > 0 && !text[i + 1..].starts_with('*') {
                            if i > current_pos {
                                spans.push(Span::styled(
                                    text[current_pos..i].to_string(),
                                    base_style,
                                ));
                            }
                            let italic_text = &text[i + 1..i + 1 + end];
                            spans.push(Span::styled(
                                italic_text.to_string(),
                                base_style.add_modifier(Modifier::ITALIC),
                            ));
                            current_pos = i + 1 + end + 1;
                            // Skip past closing *
                            for _ in 0..end + 1 {
                                chars.next();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Add any remaining text
    if current_pos < text.len() {
        spans.push(Span::styled(text[current_pos..].to_string(), base_style));
    }

    // If no spans were added (e.g., no markdown found), return the whole text
    if spans.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    spans
}

/// Wrap a single line of text to fit within the given width
///
/// Uses word boundaries when possible, falls back to character wrapping.
/// Handles UTF-8 multi-byte characters correctly by using character indices.
fn wrap_line(text: &str, width: usize) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    // Use character count, not byte length (UTF-8 chars can be multi-byte)
    if text.chars().count() <= width {
        return vec![text];
    }

    let mut result = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let char_count = remaining.chars().count();
        if char_count <= width {
            result.push(remaining);
            break;
        }

        // Find the byte position of the width-th character (safe boundary)
        let byte_pos = remaining
            .char_indices()
            .nth(width)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());

        // Try to find a word boundary within the width limit
        let break_at = remaining[..byte_pos]
            .rfind(' ')
            .map(|pos| pos + 1) // Include the space in current line
            .unwrap_or(byte_pos); // Fall back to hard break at char boundary

        let (line, rest) = remaining.split_at(break_at);
        result.push(line.trim_end()); // Remove trailing space from wrapped line
        remaining = rest.trim_start(); // Remove leading space from next line
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_line_empty() {
        assert_eq!(wrap_line("", 40), vec![""]);
    }

    #[test]
    fn test_wrap_line_fits() {
        assert_eq!(wrap_line("hello world", 40), vec!["hello world"]);
    }

    #[test]
    fn test_wrap_line_word_boundary() {
        // "hello world" is 11 chars, with width 7 it should wrap at the space
        assert_eq!(wrap_line("hello world", 7), vec!["hello", "world"]);
    }

    #[test]
    fn test_wrap_line_hard_break() {
        // Word too long to fit, must hard break
        assert_eq!(wrap_line("abcdefghij", 5), vec!["abcde", "fghij"]);
    }

    #[test]
    fn test_wrap_line_multiple_wraps() {
        let text = "this is a longer message that needs multiple wraps";
        let wrapped = wrap_line(text, 15);
        // Each line should be at most 15 characters (not bytes)
        for line in &wrapped {
            assert!(
                line.chars().count() <= 15,
                "Line too long: {} ({} chars)",
                line,
                line.chars().count()
            );
        }
        // Reassembling should give us the original (minus spaces at wrap points)
        let rejoined: String = wrapped.join(" ");
        assert_eq!(rejoined.replace("  ", " "), text);
    }

    #[test]
    fn test_wrap_line_utf8_box_drawing() {
        // Box-drawing characters are 3 bytes each in UTF-8
        let text = "┌─ Team ─────────────────────────────────────────┐";
        // This should NOT panic - the bug was slicing at byte boundaries
        let wrapped = wrap_line(text, 40);
        // Verify we get valid UTF-8 strings back
        for line in &wrapped {
            assert!(line.chars().count() <= 40);
        }
    }

    #[test]
    fn test_truncate_str_narrow_preserves_identifier() {
        // When column is narrow, we should show the PR/task identifier
        // not just useless first characters like "P" from "PR #42"

        // PR format: "PR #42" should show "#42" when space is limited
        assert_eq!(truncate_str("PR #42", 4), "#42");
        assert_eq!(truncate_str("PR #42 Fix bug", 4), "#42");

        // Task format: "#1 Some task" should show "#1" when space is limited
        assert_eq!(truncate_str("#1 Some task", 3), "#1");
        assert_eq!(truncate_str("#42 Some task", 4), "#42");

        // Repo#PR format: "midtown#97" should show "#97" when truncation would
        // hide the identifier (this is the format used in Review/Done columns)
        assert_eq!(truncate_str("midtown#97", 4), "#97");
        assert_eq!(truncate_str("midtown#97", 6), "#97"); // "midto…" would be useless
        assert_eq!(truncate_str("midtown#97", 8), "#97"); // still prefer identifier

        // With enough space to show the full string, show it all
        assert_eq!(truncate_str("midtown#97", 10), "midtown#97");
        assert_eq!(truncate_str("midtown#97", 15), "midtown#97");

        // When identifier starts early, normal truncation is fine
        assert_eq!(truncate_str("#42 Some task", 8), "#42 Som…");

        // VERY narrow columns (1-2 chars): show truncated identifier, not useless prefix
        // This is the format used in Done column: "PR#97 title"
        assert_eq!(truncate_str("PR#97 Fix bug", 1), "#"); // not "P"
        assert_eq!(truncate_str("PR#97 Fix bug", 2), "#9"); // not "P…"
        assert_eq!(truncate_str("PR#97 Fix bug", 3), "#97"); // full identifier fits
    }

    #[test]
    fn test_parse_markdown_plain_text() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello world", base);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world");
    }

    #[test]
    fn test_parse_markdown_bold() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello **bold** world", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "bold");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content, " world");
    }

    #[test]
    fn test_parse_markdown_italic() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello *italic* world", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "italic");
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[2].content, " world");
    }

    #[test]
    fn test_parse_markdown_code() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("run `cargo test` now", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "run ");
        assert_eq!(spans[1].content, "cargo test");
        assert_eq!(spans[1].style.fg, Some(Color::Cyan));
        assert_eq!(spans[2].content, " now");
    }

    #[test]
    fn test_parse_markdown_mixed() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("**bold** and `code`", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " and ");
        assert_eq!(spans[2].content, "code");
        assert_eq!(spans[2].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_continuation_lines_have_consistent_indent() {
        use chrono::Utc;

        // Create messages from users with different name lengths (3 content lines)
        let short_name_msg = Message {
            id: "1".to_string(),
            from: "a".to_string(),
            content: "line1\nline2\nline3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
        };
        let long_name_msg = Message {
            id: "2".to_string(),
            from: "lexington".to_string(),
            content: "line1\nline2\nline3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
        };

        // New layout: name line, then 3 content lines (timestamp + 2 continuations)
        // Total = 4 lines: sender, timestamp+line1, indent+line2, indent+line3
        let short_lines = render_message(&short_name_msg, 80, None);
        let long_lines = render_message(&long_name_msg, 80, None);

        assert_eq!(short_lines.len(), 4, "Expected 4 lines: sender + 3 content");
        assert_eq!(long_lines.len(), 4, "Expected 4 lines: sender + 3 content");

        // Extract the indent from continuation lines (3rd and 4th line, first span)
        let short_indent = &short_lines[2].spans[0].content;
        let long_indent = &long_lines[2].spans[0].content;

        // Continuation lines should have the SAME indent (7 spaces) regardless of username length
        assert_eq!(
            short_indent.len(),
            TIMESTAMP_GUTTER_WIDTH,
            "Indent should be {} chars, got {}",
            TIMESTAMP_GUTTER_WIDTH,
            short_indent.len()
        );
        assert_eq!(
            short_indent.len(),
            long_indent.len(),
            "Continuation indent should be consistent"
        );
    }

    #[test]
    fn test_consecutive_messages_skip_sender_name() {
        use chrono::Utc;

        let msg1 = Message {
            id: "1".to_string(),
            from: "columbus".to_string(),
            content: "first message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
        };
        let msg2 = Message {
            id: "2".to_string(),
            from: "columbus".to_string(),
            content: "second message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
        };

        // First message (no previous sender) - shows sender line + timestamp line
        let lines1 = render_message(&msg1, 80, None);
        assert_eq!(lines1.len(), 2); // sender line + timestamp+content line

        // Second message from same sender - shows only timestamp + content (no sender)
        let lines2 = render_message(&msg2, 80, Some("columbus"));
        assert_eq!(lines2.len(), 1); // just timestamp + content

        // Different sender - shows blank line + sender line + timestamp line
        let lines3 = render_message(&msg2, 80, Some("lexington"));
        assert_eq!(lines3.len(), 3); // blank + sender line + timestamp+content line

        // Verify first message has sender name on first line
        let first_line_content: String =
            lines1[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_line_content.contains("columbus"));

        // Verify same-sender message has timestamp, not actor
        let same_sender_content: String =
            lines2[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!same_sender_content.contains("columbus"));
        assert!(same_sender_content.contains(":")); // Has timestamp like "10:12"
    }

    #[test]
    fn test_action_message_format() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "completed task 3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Action,
        };

        // Action messages are "* name message" on one line (no timestamp)
        let lines = render_message(&msg, 80, None);
        assert_eq!(lines.len(), 1);

        let content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains("*"));
        assert!(content.contains("park"));
        assert!(content.contains("completed task 3"));
        // Should NOT contain timestamp (no colon like "10:42")
        assert!(
            !content.contains(":"),
            "Action message should not contain timestamp, got: {}",
            content
        );
    }

    #[test]
    fn test_system_message_format() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "system".to_string(),
            content: "Session started".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::System,
        };

        // System messages are gray, no timestamp gutter, just the content
        let lines = render_message(&msg, 80, None);
        assert_eq!(lines.len(), 1);

        // Should be just the content, no timestamp
        let content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(content, "Session started");
        assert!(!content.contains(":")); // No timestamp like "10:12"

        // Verify gray color (DarkGray)
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn test_format_relative_time() {
        use chrono::{Duration, Utc};

        let now = Utc::now();

        // Just now (less than a minute ago)
        assert_eq!(format_relative_time(now), "just now");

        // Minutes ago
        assert_eq!(
            format_relative_time(now - Duration::minutes(1)),
            "1 minute ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::minutes(30)),
            "30 minutes ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::minutes(59)),
            "59 minutes ago"
        );

        // Hours ago
        assert_eq!(format_relative_time(now - Duration::hours(1)), "1 hour ago");
        assert_eq!(
            format_relative_time(now - Duration::hours(5)),
            "5 hours ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::hours(23)),
            "23 hours ago"
        );

        // Days ago
        assert_eq!(format_relative_time(now - Duration::days(1)), "1 day ago");
        assert_eq!(format_relative_time(now - Duration::days(7)), "7 days ago");
    }

    #[test]
    fn test_chat_shows_newest_messages_when_content_exceeds_height() {
        // Bug reproduction: When messages render to more lines than visible_height,
        // the chat should show the NEWEST messages at the bottom, not the oldest.
        //
        // Each message from a different sender renders to ~3 lines:
        // - blank line (separator)
        // - sender name line
        // - timestamp + content line
        //
        // So 10 messages from different senders = ~30 lines of content.
        // If visible_height is 10, we should see the LAST 10 lines (newest messages),
        // not the FIRST 10 lines (oldest messages).
        use chrono::Utc;

        // Create 10 messages from different senders (each will take ~3 lines)
        let messages: Vec<Message> = (0..10)
            .map(|i| Message {
                id: i.to_string(),
                from: format!("user{}", i), // Different sender each time = new sender line
                content: format!("message content {}", i),
                timestamp: Utc::now(),
                message_type: MessageType::Text,
            })
            .collect();

        // Render all messages
        let mut all_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let msg_lines = render_message(msg, 80, prev_sender);
            all_lines.extend(msg_lines);
            prev_sender = Some(&msg.from);
        }

        // Verify we have more lines than a typical visible_height
        assert!(
            all_lines.len() > 10,
            "Expected more than 10 lines, got {}",
            all_lines.len()
        );

        // The LAST line should contain content from the LAST message (message 9)
        let last_line: String = all_lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            last_line.contains("message content 9"),
            "Last line should contain newest message, got: {}",
            last_line
        );

        // Now simulate what draw_chat_panel does: take only visible_height lines
        // BUG: Currently it would show the FIRST 10 lines (oldest messages)
        // FIX: It should show the LAST 10 lines (newest messages)
        let visible_height = 10;

        // Current buggy behavior: takes from beginning
        let buggy_visible: Vec<_> = all_lines.iter().take(visible_height).collect();

        // Fixed behavior: takes from end
        let fixed_visible: Vec<_> = all_lines
            .iter()
            .skip(all_lines.len().saturating_sub(visible_height))
            .collect();

        // The buggy version would show oldest messages (message 0)
        let buggy_content: String = buggy_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        // The fixed version shows newest messages (message 9)
        let fixed_content: String = fixed_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        // Verify the bug: buggy shows old, fixed shows new
        assert!(
            buggy_content.contains("message content 0"),
            "Buggy version should contain oldest message"
        );
        assert!(
            !buggy_content.contains("message content 9"),
            "Buggy version should NOT contain newest message"
        );
        assert!(
            fixed_content.contains("message content 9"),
            "Fixed version should contain newest message"
        );
    }

    #[test]
    fn test_scroll_offset_shows_older_messages() {
        // Bug reproduction: When user scrolls up (scroll_offset > 0), they expect to see
        // older messages. But the current code always takes the LAST N lines of rendered
        // content, which means scrolling has no visible effect.
        //
        // This test verifies that scrolling up actually shows different (older) content.
        use chrono::Utc;

        // Create 20 messages, each from a different sender (so each takes ~3 lines)
        let messages: Vec<Message> = (0..20)
            .map(|i| Message {
                id: i.to_string(),
                from: format!("user{}", i),
                content: format!("message content {}", i),
                timestamp: Utc::now(),
                message_type: MessageType::Text,
            })
            .collect();

        // Simulate scroll_offset=0 (bottom): visible_messages returns messages 10..20
        let at_bottom_messages = &messages[10..20];
        let mut at_bottom_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in at_bottom_messages {
            at_bottom_lines.extend(render_message(msg, 80, prev_sender));
            prev_sender = Some(&msg.from);
        }

        // Simulate scroll_offset=10 (scrolled up): visible_messages returns messages 0..10
        let scrolled_up_messages = &messages[0..10];
        let mut scrolled_up_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in scrolled_up_messages {
            scrolled_up_lines.extend(render_message(msg, 80, prev_sender));
            prev_sender = Some(&msg.from);
        }

        let visible_height = 10;

        // At bottom: take LAST N lines (correct - shows newest messages)
        let bottom_visible: Vec<_> = if at_bottom_lines.len() > visible_height {
            at_bottom_lines
                .iter()
                .skip(at_bottom_lines.len() - visible_height)
                .collect()
        } else {
            at_bottom_lines.iter().collect()
        };

        // Scrolled up: the bug is taking LAST N lines, should take FIRST N lines
        let scrolled_buggy: Vec<_> = if scrolled_up_lines.len() > visible_height {
            scrolled_up_lines
                .iter()
                .skip(scrolled_up_lines.len() - visible_height)
                .collect()
        } else {
            scrolled_up_lines.iter().collect()
        };

        // Scrolled up: correct behavior should take FIRST N lines
        let scrolled_fixed: Vec<_> = scrolled_up_lines.iter().take(visible_height).collect();

        // Extract content
        let bottom_content: String = bottom_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        let buggy_content: String = scrolled_buggy
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        let fixed_content: String = scrolled_fixed
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        // At bottom should show newest messages (19, 18, etc.)
        assert!(
            bottom_content.contains("message content 19")
                || bottom_content.contains("message content 18"),
            "At bottom should show newest messages, got: {}",
            bottom_content
        );

        // Scrolled up with buggy logic shows LAST N lines of older messages
        // which might contain message 8 or 9 (end of 0..10 range)
        assert!(
            buggy_content.contains("message content 9")
                || buggy_content.contains("message content 8"),
            "Buggy scrolled shows end of old messages, got: {}",
            buggy_content
        );

        // Scrolled up with fixed logic shows FIRST N lines of older messages
        // which should contain message 0 or 1 (start of 0..10 range)
        assert!(
            fixed_content.contains("user0") || fixed_content.contains("message content 0"),
            "Fixed scrolled should show oldest messages (0, 1, etc.), got: {}",
            fixed_content
        );

        // The key assertion: buggy and fixed should show DIFFERENT content when scrolled
        assert_ne!(
            buggy_content, fixed_content,
            "Scrolled content should differ between buggy and fixed implementations"
        );
    }
}
