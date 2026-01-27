//! UI rendering for the chat TUI

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use midtown::{Message, MessageType};

use super::app::App;

/// Fixed indent for continuation lines (7 = "HH:MM " time prefix)
/// Using a fixed indent keeps multi-line messages aligned consistently
/// regardless of sender name length.
const CONTINUATION_INDENT: usize = 7;

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

/// Draw the main UI
///
/// Note: The Team panel has been removed - coworker status is now shown
/// in tmux tab names instead, providing better visibility even when the
/// chat TUI is not in focus.
pub fn draw(f: &mut Frame, app: &mut App) {
    // Full width for chat panel - team status shown in tmux tabs instead
    draw_chat_panel(f, app, f.area());
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

    // Build lines for messages, splitting multi-line content
    let lines: Vec<Line> = visible
        .iter()
        .flat_map(|msg| render_message(msg, inner.width as usize))
        .collect();

    // No Wrap needed - we pre-split lines for better performance
    let paragraph = Paragraph::new(lines);

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Render a single message into one or more Lines
///
/// This handles:
/// - Multi-line content (explicit newlines in message)
/// - Long lines that need wrapping to fit the panel width
/// - Markdown formatting (**bold**, *italic*, `code`)
fn render_message(msg: &Message, width: usize) -> Vec<Line<'static>> {
    let time = msg.timestamp.format("%H:%M").to_string();
    let color = get_sender_color(&msg.from);

    // Calculate the prefix length for continuation line indentation
    // Format: "HH:MM <name> " or "HH:MM * name "
    let prefix_len = match msg.message_type {
        MessageType::Action => 6 + 2 + msg.from.len() + 1, // "HH:MM * name "
        MessageType::System => 6 + 9,                      // "HH:MM <system> "
        _ => 6 + 1 + msg.from.len() + 2,                   // "HH:MM <name> "
    };

    // Available width for content (account for prefix on first line)
    let content_width = width.saturating_sub(prefix_len);
    if content_width == 0 {
        return vec![]; // Panel too narrow
    }

    // Split content by explicit newlines, then wrap each line
    let content_lines: Vec<&str> = msg
        .content
        .split('\n')
        .flat_map(|line| wrap_line(line, content_width))
        .collect();

    let mut result = Vec::with_capacity(content_lines.len());

    for (i, content) in content_lines.into_iter().enumerate() {
        // Determine base style for content based on message type
        // For github/system senders, use DarkGray for both name and content
        let content_style = match msg.message_type {
            MessageType::Action => Style::default().fg(color),
            MessageType::System => Style::default().fg(Color::DarkGray),
            _ if msg.from == "github" => Style::default().fg(Color::DarkGray),
            _ => Style::default().fg(Color::White),
        };

        if i == 0 {
            // First line gets the full prefix
            result.push(build_first_line(msg, &time, color, content, content_style));
        } else {
            // Continuation lines get fixed indentation + markdown-parsed content
            let indent = " ".repeat(CONTINUATION_INDENT);
            let mut spans = vec![Span::raw(indent)];
            spans.extend(parse_markdown(content, content_style));
            result.push(Line::from(spans));
        }
    }

    result
}

/// Build the first line of a message with its prefix
fn build_first_line(
    msg: &Message,
    time: &str,
    color: Color,
    content: &str,
    content_style: Style,
) -> Line<'static> {
    let mut spans = match msg.message_type {
        MessageType::Action => {
            // IRC-style action: HH:MM * name action
            vec![
                Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                Span::styled("* ", Style::default().fg(color)),
                Span::styled(
                    format!("{} ", msg.from),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]
        }
        MessageType::System => {
            // System message: HH:MM <system> message
            vec![
                Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    String::from("<system> "),
                    Style::default().fg(Color::DarkGray),
                ),
            ]
        }
        _ => {
            // Regular message: HH:MM <name> message
            vec![
                Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                Span::styled(String::from("<"), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    msg.from.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(String::from("> "), Style::default().fg(Color::DarkGray)),
            ]
        }
    };

    // Add markdown-parsed content spans
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

        // Create messages from users with different name lengths
        let short_name_msg = Message {
            id: "1".to_string(),
            from: "a".to_string(),
            content: "line1\nline2".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
        };
        let long_name_msg = Message {
            id: "2".to_string(),
            from: "lexington".to_string(),
            content: "line1\nline2".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
        };

        let short_lines = render_message(&short_name_msg, 80);
        let long_lines = render_message(&long_name_msg, 80);

        // Both should have 2 lines
        assert_eq!(short_lines.len(), 2);
        assert_eq!(long_lines.len(), 2);

        // Extract the indent from continuation lines (second line, first span)
        let short_indent = &short_lines[1].spans[0].content;
        let long_indent = &long_lines[1].spans[0].content;

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
}
