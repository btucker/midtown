//! Chat panel: message display, input bar, and autocomplete dropdown.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::cli::chat::mermaid;

use super::super::app::{App, FocusedPane, MessageRenderCache};
use super::messages::render_message;
use super::messages_mermaid::render_message_with_mermaid;
use super::text::wrap_content;

/// Draw the chat panel showing messages
pub fn draw_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let input_bar_height = calculate_input_bar_height(&app.input_text, area.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(input_bar_height)])
        .split(area);

    draw_chat_messages(f, app, chunks[0]);
    draw_input_bar(f, app, chunks[1]);

    if app.autocomplete.show {
        draw_autocomplete_dropdown(f, app, chunks[1]);
    }
}

/// Draw the chat messages area (top of chat panel)
fn draw_chat_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.selection_mode {
        format!(" #{} [SELECT] ", app.selected_channel)
    } else {
        format!(" #{} ", app.selected_channel)
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

    app.visible_height = inner.height as usize;
    app.clamp_scroll_offset();

    // Check if we can reuse cached rendered lines
    let cache_key = app.message_cache_key(inner.width);
    if let Some(ref cache) = app.message_render_cache
        && cache.cache_key == cache_key
    {
        let paragraph = Paragraph::new(cache.lines.clone());
        f.render_widget(block, area);
        f.render_widget(paragraph, inner);
        app.diagram_sources.clone_from(&cache.diagram_sources);
        return;
    }

    // Cache miss — full render
    let current_tasks = app.current_tasks().clone();
    let user_display_name = app.user_display_name.clone();
    let visible: Vec<midtown::Message> = app.visible_messages().to_vec();

    let mut lines: Vec<Line> = Vec::new();
    let prev_sender: Option<&str> = None;

    let mut mermaid_to_render: Vec<String> = Vec::new();

    app.diagram_sources.clear();

    for (idx, msg) in visible.iter().enumerate() {
        let segments = mermaid::parse_content_segments(&msg.content);
        let has_mermaid = segments
            .iter()
            .any(|s| matches!(s, mermaid::ContentSegment::Mermaid(_)));
        let prev = if idx > 0 {
            Some(visible[idx - 1].from.as_str())
        } else {
            prev_sender
        };

        if !has_mermaid {
            let msg_lines = render_message(
                msg,
                inner.width as usize,
                prev,
                &current_tasks,
                user_display_name.as_deref(),
            );
            lines.extend(msg_lines);
        } else {
            render_message_with_mermaid(
                msg,
                &segments,
                inner.width as usize,
                prev,
                &current_tasks,
                user_display_name.as_deref(),
                &app.mermaid_cache,
                &mut lines,
                &mut app.diagram_sources,
                &mut mermaid_to_render,
            );
        }
    }

    for source in mermaid_to_render {
        app.mermaid_cache.get_or_render(&source);
    }

    // Handle line truncation based on scroll position
    let total_lines = lines.len();
    let visible_lines = if total_lines > inner.height as usize {
        if app.is_at_max_scroll() {
            lines.truncate(inner.height as usize);
            lines
        } else {
            let truncation_offset = total_lines - inner.height as usize;
            lines.split_off(truncation_offset)
        }
    } else {
        lines
    };

    app.message_render_cache = Some(MessageRenderCache::new(
        visible_lines.clone(),
        app.diagram_sources.clone(),
        cache_key,
    ));

    let paragraph = Paragraph::new(visible_lines);

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Calculate the required height for the input bar based on wrapped text
///
/// Returns total height including borders (2) and content lines (1 minimum, 6 maximum).
fn calculate_input_bar_height(input_text: &str, area_width: u16) -> u16 {
    const PROMPT_WIDTH: usize = 3; // "› "
    const CURSOR_WIDTH: usize = 1; // "█"
    const MIN_CONTENT_LINES: u16 = 1;
    const MAX_CONTENT_LINES: u16 = 6;
    const BORDER_HEIGHT: u16 = 2;

    let available_width = area_width.saturating_sub(2) as usize;
    if available_width == 0 {
        return BORDER_HEIGHT + MIN_CONTENT_LINES;
    }

    let content_width = available_width.saturating_sub(PROMPT_WIDTH + CURSOR_WIDTH);
    if content_width == 0 {
        return BORDER_HEIGHT + MIN_CONTENT_LINES;
    }

    let line_count = if input_text.is_empty() {
        1
    } else {
        wrap_content(input_text, content_width).len()
    };

    let content_lines = (line_count as u16).clamp(MIN_CONTENT_LINES, MAX_CONTENT_LINES);
    BORDER_HEIGHT + content_lines
}

/// Draw the input bar at the bottom of the chat panel
fn draw_input_bar(f: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.focused_pane == FocusedPane::InputBar;
    let border_color = if is_focused {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    let prompt = "› ";
    let char_count = app.input_text.chars().count();
    let text_with_cursor = if is_focused && app.input_cursor == char_count {
        format!("{}{}█", prompt, app.input_text)
    } else if is_focused {
        let byte_idx = app
            .input_text
            .char_indices()
            .nth(app.input_cursor)
            .map(|(idx, _)| idx)
            .unwrap_or(app.input_text.len());
        let (before, after) = app.input_text.split_at(byte_idx);
        format!("{}{}█{}", prompt, before, after)
    } else {
        format!("{}{}", prompt, app.input_text)
    };

    let paragraph = Paragraph::new(text_with_cursor).wrap(Wrap { trim: false });

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Draw autocomplete dropdown above the input bar
fn draw_autocomplete_dropdown(f: &mut Frame, app: &App, input_area: Rect) {
    let items = &app.autocomplete.items;
    if items.is_empty() {
        return;
    }

    let item_count = items.len().min(8);
    let dropdown_height = (item_count * 2) as u16;
    let dropdown_width = 40u16.min(input_area.width.saturating_sub(4));

    let dropdown_y = input_area
        .y
        .saturating_sub(dropdown_height)
        .saturating_sub(1);
    let dropdown_x = input_area.x + 2;

    let dropdown_area = Rect {
        x: dropdown_x,
        y: dropdown_y,
        width: dropdown_width,
        height: dropdown_height,
    };

    let mut lines = Vec::new();
    for (i, item) in items.iter().enumerate().take(item_count) {
        let is_selected = i == app.autocomplete.selected_index;

        let value_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White).bg(Color::Black)
        };

        lines.push(Line::from(vec![Span::styled(
            format!(" {} ", item.value),
            value_style,
        )]));

        if let Some(ref desc) = item.description {
            let desc_text = if desc.len() > dropdown_width as usize - 4 {
                format!(" {}...", &desc[..dropdown_width as usize - 7])
            } else {
                format!(" {}", desc)
            };
            let desc_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray).bg(Color::Black)
            };
            lines.push(Line::from(vec![Span::styled(desc_text, desc_style)]));
        } else {
            lines.push(Line::from(Span::styled(
                "",
                Style::default().bg(Color::Black),
            )));
        }
    }

    // Clear the area first to ensure background is rendered properly
    f.render_widget(Clear, dropdown_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, dropdown_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_input_bar_height_empty_text() {
        let height = calculate_input_bar_height("", 80);
        assert_eq!(height, 3);
    }

    #[test]
    fn test_calculate_input_bar_height_short_text() {
        let height = calculate_input_bar_height("Hello", 80);
        assert_eq!(height, 3);
    }

    #[test]
    fn test_calculate_input_bar_height_wraps_long_text() {
        let long_text = "a".repeat(150);
        let height = calculate_input_bar_height(&long_text, 80);
        assert_eq!(
            height, 5,
            "150 chars should wrap to 3 lines: 3 + 2 borders = 5"
        );
    }

    #[test]
    fn test_calculate_input_bar_height_max_lines() {
        let very_long_text = "a".repeat(1000);
        let height = calculate_input_bar_height(&very_long_text, 80);
        assert_eq!(height, 8, "Max 6 content lines + 2 borders = 8");
    }

    #[test]
    fn test_calculate_input_bar_height_narrow_terminal() {
        let height = calculate_input_bar_height("Hello world", 10);
        assert!(height >= 3, "Minimum height should be 3");
        assert!(height <= 8, "Maximum height should be 8");
    }

    #[test]
    fn test_calculate_input_bar_height_zero_width() {
        let height = calculate_input_bar_height("test", 0);
        assert_eq!(height, 3, "Zero width should return minimum height");
    }

    #[test]
    fn test_calculate_input_bar_height_with_newlines() {
        let text = "Line 1\nLine 2\nLine 3";
        let height = calculate_input_bar_height(text, 80);
        assert_eq!(height, 5, "3 content lines + 2 border lines = 5");
    }
}
