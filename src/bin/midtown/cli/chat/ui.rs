//! UI rendering for the chat TUI

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
};

use midtown::{Message, MessageType};

use super::app::App;

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
pub fn draw(f: &mut Frame, app: &mut App) {
    // Split into team panel (30%) and chat panel (70%)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(f.area());

    draw_team_panel(f, app, chunks[0]);
    draw_chat_panel(f, app, chunks[1]);
}

/// Draw the team panel showing coworkers and their status
fn draw_team_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Team ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);

    // Build list items for coworkers
    let items: Vec<ListItem> = app
        .coworkers
        .iter()
        .flat_map(|cw| {
            let color = get_sender_color(&cw.name);
            let name_line = Line::from(Span::styled(
                &cw.name,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ));

            let status_line = match &cw.last_action {
                Some(action) => {
                    // Truncate long actions
                    let display = if action.len() > 25 {
                        format!("  {}...", &action[..22])
                    } else {
                        format!("  {}", action)
                    };
                    Line::from(Span::styled(display, Style::default().fg(Color::DarkGray)))
                }
                None => Line::from(Span::styled(
                    "  (idle)",
                    Style::default().fg(Color::DarkGray),
                )),
            };

            vec![ListItem::new(name_line), ListItem::new(status_line)]
        })
        .collect();

    let list = List::new(items);

    f.render_widget(block, area);
    f.render_widget(list, inner);
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
fn render_message(msg: &Message, width: usize) -> Vec<Line<'_>> {
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
        if i == 0 {
            // First line gets the full prefix
            result.push(build_first_line(msg, &time, color, content));
        } else {
            // Continuation lines get indentation
            let indent = " ".repeat(prefix_len);
            let style = match msg.message_type {
                MessageType::Action => Style::default().fg(color),
                MessageType::System => Style::default().fg(Color::DarkGray),
                _ => Style::default().fg(Color::White),
            };
            result.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(content, style),
            ]));
        }
    }

    result
}

/// Build the first line of a message with its prefix
fn build_first_line<'a>(msg: &'a Message, time: &str, color: Color, content: &'a str) -> Line<'a> {
    match msg.message_type {
        MessageType::Action => {
            // IRC-style action: HH:MM * name action
            Line::from(vec![
                Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                Span::styled("* ", Style::default().fg(color)),
                Span::styled(
                    format!("{} ", msg.from),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(content, Style::default().fg(color)),
            ])
        }
        MessageType::System => {
            // System message: HH:MM <system> message
            Line::from(vec![
                Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                Span::styled("<system> ", Style::default().fg(Color::DarkGray)),
                Span::styled(content, Style::default().fg(Color::DarkGray)),
            ])
        }
        _ => {
            // Regular message: HH:MM <name> message
            Line::from(vec![
                Span::styled(format!("{} ", time), Style::default().fg(Color::DarkGray)),
                Span::styled("<", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &msg.from,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled("> ", Style::default().fg(Color::DarkGray)),
                Span::styled(content, Style::default().fg(Color::White)),
            ])
        }
    }
}

/// Wrap a single line of text to fit within the given width
///
/// Uses word boundaries when possible, falls back to character wrapping
fn wrap_line(text: &str, width: usize) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
    if text.len() <= width {
        return vec![text];
    }

    let mut result = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= width {
            result.push(remaining);
            break;
        }

        // Try to find a word boundary within the width limit
        let break_at = remaining[..width]
            .rfind(' ')
            .map(|pos| pos + 1) // Include the space in current line
            .unwrap_or(width); // Fall back to hard break

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
        // Each line should be at most 15 chars
        for line in &wrapped {
            assert!(line.len() <= 15, "Line too long: {}", line);
        }
        // Reassembling should give us the original (minus spaces at wrap points)
        let rejoined: String = wrapped.join(" ");
        assert_eq!(rejoined.replace("  ", " "), text);
    }
}
