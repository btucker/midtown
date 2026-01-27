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

use super::app::App;

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

/// Fixed indent for message content (7 = "HH:MM  " time prefix + space)
/// Using a fixed indent keeps messages aligned consistently.
const MESSAGE_INDENT: usize = 7;

/// Avenue names mapped to colors (position-based assignment)
const AVENUE_COLORS: &[(&str, Color)] = &[
    ("lexington", Color::Cyan),
    ("park", Color::Green),
    ("madison", Color::Yellow),
    ("broadway", Color::Magenta),
    ("amsterdam", Color::Blue),
    ("columbus", Color::Red),
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

/// Draw the main UI
///
/// Note: The Team panel has been removed - coworker status is now shown
/// in tmux tab names instead, providing better visibility even when the
/// chat TUI is not in focus.
pub fn draw(f: &mut Frame, app: &mut App) {
    // Split into kanban (top) and chat (bottom) panels
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(KANBAN_HEIGHT), Constraint::Min(10)])
        .split(f.area());

    draw_kanban_panel(f, app, chunks[0]);
    draw_chat_panel(f, app, chunks[1]);
}

/// A kanban item that may span multiple lines
struct KanbanItem {
    /// Lines to display (1 for Backlog/Done, up to 2 for In Progress/Review)
    lines: Vec<String>,
    /// Optional URL for the first line (for clickable PR links)
    url: Option<String>,
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
        })
        .collect();
    draw_kanban_column(f, columns[0], "Backlog", Color::Blue, &backlog_items);

    // In Progress column (with owner and duration) - 2-line items
    let in_progress_items: Vec<KanbanItem> = in_progress
        .iter()
        .map(|t| {
            let line1 = format!("#{} {}", t.id, t.subject);
            let owner = t.owner.as_deref().unwrap_or("?");
            // TODO: Track when task became in_progress for accurate duration
            let line2 = format!("  └ {}", owner);
            KanbanItem {
                lines: vec![line1, line2],
                url: None,
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

    // Review column (open PRs with repo#XX format and duration) - 2-line items
    let review_items: Vec<KanbanItem> = app
        .prs
        .iter()
        .map(|pr| {
            let line1 = format!("{}#{} {}", app.repo_name, pr.number, pr.title);
            let duration = format_duration_minutes(pr.created_at);
            let line2 = format!("  └ {} {}", pr.author, duration);
            let url = format!("https://github.com/{}/pull/{}", app.repo_name, pr.number);
            KanbanItem {
                lines: vec![line1, line2],
                url: Some(url),
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
                lines: vec![format!("{}#{} {}", app.repo_name, pr.number, pr.title)],
                url: Some(url),
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

            // Only apply hyperlink to the first line of items that have URLs
            if line_idx == 0 && item.url.is_some() {
                let url = item.url.as_ref().unwrap();
                render_hyperlink_line(buffer, inner.x, y, &truncated, url, available_width);
            } else {
                // Render plain text
                for (i, ch) in truncated.chars().enumerate() {
                    if i < available_width {
                        buffer[(inner.x + i as u16, y)]
                            .set_char(ch)
                            .set_fg(Color::White);
                    }
                }
            }

            lines_used += 1;
        }
    }
}

/// Render a line with OSC 8 hyperlink escape sequences
///
/// Uses the OSC 8 format: \x1B]8;;{url}\x07{text}\x1B]8;;\x07
/// to create clickable hyperlinks in supported terminals.
fn render_hyperlink_line(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    text: &str,
    url: &str,
    max_width: usize,
) {
    // Render each character with hyperlink escape sequence
    // We use OSC 8 format: ESC ] 8 ; ; URL ST text ESC ] 8 ; ; ST
    // where ST (String Terminator) is BEL (\x07) or ESC \ (\x1B\\)
    for (i, ch) in text.chars().enumerate() {
        if i >= max_width {
            break;
        }
        // Create the hyperlink-wrapped character
        let hyperlink = format!("\x1B]8;;{}\x07{}\x1B]8;;\x07", url, ch);
        buffer[(x + i as u16, y)]
            .set_symbol(&hyperlink)
            .set_fg(Color::White);
    }
}

/// Truncate a string to fit within the given width, adding "..." if truncated
fn truncate_str(s: &str, max_width: usize) -> String {
    if s.chars().count() <= max_width {
        s.to_string()
    } else if max_width <= 3 {
        s.chars().take(max_width).collect()
    } else {
        let truncated: String = s.chars().take(max_width - 1).collect();
        format!("{}…", truncated)
    }
}

/// Draw the chat panel showing messages
fn draw_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let block = Block::default()
        .title(" #midtown ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

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

    // No Wrap needed - we pre-split lines for better performance
    let paragraph = Paragraph::new(lines);

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Render a single message into one or more Lines
///
/// Layout for new sender:
/// - Line 1: Actor name + first content line (on same line)
/// - Line 2: Timestamp + second content line (if multi-line)
/// - Line 3+: Indent + continuation lines
///
/// Layout for same sender:
/// - Line 1: Timestamp + first content line
/// - Line 2+: Indent + continuation lines
///
/// This handles:
/// - Multi-line content (explicit newlines in message)
/// - Long lines that need wrapping to fit the panel width
/// - Markdown formatting (**bold**, *italic*, `code`)
fn render_message(msg: &Message, width: usize, prev_sender: Option<&str>) -> Vec<Line<'static>> {
    let local_time = msg.timestamp.with_timezone(&Local);
    let time = local_time.format("%H:%M").to_string();
    let color = get_sender_color(&msg.from);

    // Determine if we need to show the sender name
    let show_sender = prev_sender.is_none_or(|prev| prev != msg.from);

    // Calculate available width for content
    // For new sender first line: width - actor prefix (varies by message type)
    // For timestamp lines: width - "HH:MM  " (MESSAGE_INDENT)
    let actor_prefix_width = get_actor_prefix_width(msg);
    let first_line_width = if show_sender {
        width.saturating_sub(actor_prefix_width)
    } else {
        width.saturating_sub(MESSAGE_INDENT)
    };
    let continuation_width = width.saturating_sub(MESSAGE_INDENT);

    if first_line_width == 0 || continuation_width == 0 {
        return vec![]; // Panel too narrow
    }

    // Split content by explicit newlines, then wrap each segment
    // First line may have different width than continuation lines
    let raw_lines: Vec<&str> = msg.content.split('\n').collect();

    let mut content_lines: Vec<String> = Vec::new();
    for (i, line) in raw_lines.iter().enumerate() {
        let wrap_width = if i == 0 && show_sender {
            first_line_width
        } else if content_lines.is_empty() {
            // This is still the first wrapped line (first raw line was empty or short)
            first_line_width
        } else {
            continuation_width
        };

        let wrapped = wrap_line(line, wrap_width);
        for (j, w) in wrapped.into_iter().enumerate() {
            // After first wrapped segment, use continuation width
            if i == 0 && j == 0 {
                content_lines.push(w.to_string());
            } else {
                // Re-wrap with continuation width if needed
                let rewrapped = wrap_line(w, continuation_width);
                for rw in rewrapped {
                    content_lines.push(rw.to_string());
                }
            }
        }
    }

    let mut result = Vec::new();

    // Determine base style for content based on message type
    let content_style = match msg.message_type {
        MessageType::Action => Style::default().fg(color),
        MessageType::System => Style::default().fg(Color::DarkGray),
        _ if msg.from == "github" => Style::default().fg(Color::DarkGray),
        _ => Style::default().fg(Color::White),
    };

    for (i, content) in content_lines.iter().enumerate() {
        if i == 0 {
            if show_sender {
                // New sender: actor + first content line on same line
                result.push(build_actor_content_line(msg, color, content, content_style));
            } else {
                // Same sender: timestamp + first content line
                result.push(build_timestamp_line(&time, content, content_style));
            }
        } else if i == 1 && show_sender {
            // New sender, second line: timestamp + content
            result.push(build_timestamp_line(&time, content, content_style));
        } else {
            // Continuation lines: just indent + content
            let indent = " ".repeat(MESSAGE_INDENT);
            let mut spans = vec![Span::raw(indent)];
            spans.extend(parse_markdown(content, content_style));
            result.push(Line::from(spans));
        }
    }

    result
}

/// Get the width of the actor prefix (for calculating content width)
fn get_actor_prefix_width(msg: &Message) -> usize {
    match msg.message_type {
        MessageType::Action => {
            // "* name  " = 2 + name.len() + 2
            2 + msg.from.len() + 2
        }
        MessageType::System => {
            // "<system>  " = 10
            10
        }
        _ => {
            // "<name>  " = 1 + name.len() + 1 + 2
            msg.from.len() + 4
        }
    }
}

/// Build a line with actor name and first content (on same line)
fn build_actor_content_line(
    msg: &Message,
    color: Color,
    content: &str,
    content_style: Style,
) -> Line<'static> {
    let mut spans = match msg.message_type {
        MessageType::Action => {
            vec![
                Span::styled("* ", Style::default().fg(color)),
                Span::styled(
                    msg.from.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
            ]
        }
        MessageType::System => {
            vec![
                Span::styled(
                    String::from("<system>"),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw("  "),
            ]
        }
        _ => {
            vec![
                Span::styled(String::from("<"), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    msg.from.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(String::from(">"), Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
            ]
        }
    };
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

/// Build a timestamp line with message content
fn build_timestamp_line(time: &str, content: &str, content_style: Style) -> Line<'static> {
    // Format: "HH:MM  content" (timestamp + 2 spaces + content)
    let mut spans = vec![Span::styled(
        format!("{}  ", time),
        Style::default().fg(Color::DarkGray),
    )];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
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

        // Create messages from users with different name lengths (3 lines to test continuation)
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

        // New layout: actor+content, timestamp+content, indent+content = 3 lines
        let short_lines = render_message(&short_name_msg, 80, None);
        let long_lines = render_message(&long_name_msg, 80, None);

        // Both should have 3 lines: actor+line1, timestamp+line2, indent+line3
        assert_eq!(short_lines.len(), 3);
        assert_eq!(long_lines.len(), 3);

        // Extract the indent from continuation lines (third line, first span)
        let short_indent = &short_lines[2].spans[0].content;
        let long_indent = &long_lines[2].spans[0].content;

        // Continuation lines should have the SAME indent regardless of username length
        assert_eq!(
            short_indent.len(),
            long_indent.len(),
            "Continuation indent should be consistent: short='{}' ({}), long='{}' ({})",
            short_indent,
            short_indent.len(),
            long_indent,
            long_indent.len()
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

        // First message (no previous sender) - shows actor + content on one line
        let lines1 = render_message(&msg1, 80, None);
        assert_eq!(lines1.len(), 1); // actor + content on same line

        // Second message from same sender - shows timestamp + content (no actor)
        let lines2 = render_message(&msg2, 80, Some("columbus"));
        assert_eq!(lines2.len(), 1); // timestamp + content

        // Different sender - shows actor + content on one line
        let lines3 = render_message(&msg2, 80, Some("lexington"));
        assert_eq!(lines3.len(), 1); // actor + content on same line

        // Verify first message has actor name
        let first_line_content: String =
            lines1[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_line_content.contains("columbus"));

        // Verify same-sender message has timestamp, not actor
        let same_sender_content: String =
            lines2[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!same_sender_content.contains("columbus"));
        assert!(same_sender_content.contains(":")); // Has timestamp like "10:12"
    }
}
