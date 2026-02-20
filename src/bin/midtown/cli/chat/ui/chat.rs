//! Chat panel: message display, input bar, and autocomplete dropdown.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::cli::chat::mermaid;

use std::collections::HashMap;

use super::super::app::{
    App, CHANNEL_LEAD_THINKING_TIMEOUT, FocusedPane, MessageRenderCache, PendingQuestion,
};
use super::messages::{build_reply_indicator_line, render_message};
use super::messages_mermaid::render_message_with_mermaid;
use super::text::wrap_content;

/// Draw the chat panel showing messages
pub fn draw_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let input_bar_height = calculate_input_bar_height(&app.input_text, area.width);

    // Reserve space for pending questions banner when questions are present.
    // Each question takes 2 lines: the question itself and the answer hint.
    let questions_height = pending_questions_height(&app.pending_questions);

    let indicator_height = lead_indicator_height(app);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(questions_height),
            Constraint::Min(5),
            Constraint::Length(indicator_height),
            Constraint::Length(input_bar_height),
        ])
        .split(area);

    // Store input area for click detection
    app.input_area = Some(chunks[3]);

    // Draw pending questions banner (collapsed to 0 height when empty)
    if questions_height > 0 {
        let questions = app.pending_questions.clone();
        draw_pending_questions(f, &questions, chunks[0]);
    }
    draw_chat_messages(f, app, chunks[1]);
    draw_lead_indicator(f, app, chunks[2]);
    draw_input_bar(f, app, chunks[3]);

    if app.autocomplete.show {
        draw_autocomplete_dropdown(f, app, chunks[3], indicator_height);
    }
}

/// Compute the number of lines the lead indicator area should occupy (1–3).
///
/// Always returns at least 1 to maintain a stable status area — the indicator
/// never collapses to zero, preventing messages from jumping when activity starts.
/// Returns 1 when idle (dim placeholder), optimistic thinking, or only one entry.
fn lead_indicator_height(app: &App) -> u16 {
    let agent_key = if app.selected_channel == "main" || app.selected_channel == "midtown" {
        "lead"
    } else {
        app.selected_channel.as_str()
    };
    let entries_len = app.visible_tool_entries(agent_key).len();
    if entries_len > 0 {
        entries_len as u16
    } else {
        1 // Always reserve at least 1 line (dim placeholder or optimistic spinner)
    }
}

/// Draw the lead working indicator area between chat messages and the input bar.
///
/// Shows up to 3 tool entries in chronological order (oldest at top, newest at bottom).
/// The last line (newest entry) includes a yellow braille spinner and the agent name in yellow.
/// Completed (✓/✗) entries age out after 30 seconds, collapsing the area to 0.
/// Descriptions are not explicitly truncated — ratatui clips at the terminal edge naturally.
fn draw_lead_indicator(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 {
        return;
    }

    let agent_key = if app.selected_channel == "main" || app.selected_channel == "midtown" {
        "lead"
    } else {
        app.selected_channel.as_str()
    };

    let entries = app.visible_tool_entries(agent_key);

    // Check for optimistic thinking state (show spinner even before tool activity arrives)
    let channel_thinking = app
        .channel_lead_thinking
        .get(agent_key)
        .map(|t| t.elapsed() < CHANNEL_LEAD_THINKING_TIMEOUT)
        .unwrap_or(false);

    if entries.is_empty() {
        if channel_thinking {
            // Show just the spinner with agent name, no tool entries
            let spinner = app.spinner_char();
            let line = Line::from(vec![
                Span::styled(format!(" {} ", spinner), Style::default().fg(Color::Yellow)),
                Span::styled(
                    agent_key.to_string(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]);
            f.render_widget(Paragraph::new(vec![line]), area);
        } else {
            // Idle: render a dim placeholder to keep the status area stable.
            // This prevents messages from jumping when activity starts/stops.
            let line = Line::from(vec![Span::styled(
                format!("   {agent_key}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            )]);
            f.render_widget(Paragraph::new(vec![line]), area);
        }
        return;
    }

    // Show spinner when any entry is still in-progress.
    // We don't require lead_working because tool entries can be in-progress even when
    // lead_working is false (stale RPC data), and the spinner should still animate.
    let has_in_progress = entries.iter().any(|e| e.header.starts_with('›'));
    let show_spinner = has_in_progress;
    // Width of " ⠋ lead " prefix: 1 space + 1 spinner + 1 space + agent_name + 1 space
    let prefix_width = 3 + agent_key.chars().count() + 1;

    let mut lines: Vec<Line<'static>> = Vec::new();
    let n = entries.len();

    // Display oldest first (reverse of visible_tool_entries which returns newest first).
    // Agent name goes on the last line (newest entry = bottom), like a standard CLI.
    for (i, entry) in entries.iter().rev().enumerate() {
        let mut chars = entry.header.chars();
        let prefix_char = chars.next().unwrap_or('›');
        let description = chars.as_str().trim_start().to_string();

        let prefix_color = match prefix_char {
            '\u{2713}' => Color::Green, // ✓
            '\u{2717}' => Color::Red,   // ✗
            _ => Color::DarkGray,       // ›
        };
        let text_color = if prefix_char == '\u{2717}' {
            Color::Red
        } else {
            Color::DarkGray
        };

        let is_last = i == n - 1; // last rendered = newest entry = agent name line
        if is_last {
            // Last line (newest): " ⠋ lead › Read foo.rs" — spinner and agent name in yellow
            let spinner = if show_spinner {
                app.spinner_char()
            } else {
                " "
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", spinner), Style::default().fg(Color::Yellow)),
                Span::styled(
                    format!("{agent_key} "),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(prefix_char.to_string(), Style::default().fg(prefix_color)),
                Span::styled(format!(" {description}"), Style::default().fg(text_color)),
            ]));
        } else {
            // Older entries: indented to align with the agent-name line's description.
            let indent = " ".repeat(prefix_width);
            lines.push(Line::from(vec![
                Span::raw(indent),
                Span::styled(prefix_char.to_string(), Style::default().fg(prefix_color)),
                Span::styled(format!(" {description}"), Style::default().fg(text_color)),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines), area);
}

/// Compute the number of lines needed to display pending questions.
///
/// Returns 0 when there are no questions (no space reserved).
/// Each question occupies 2 lines: the question line and the answer hint.
fn pending_questions_height(questions: &[PendingQuestion]) -> u16 {
    if questions.is_empty() {
        0
    } else {
        // 2 lines per question (question + hint)
        (questions.len() as u16) * 2
    }
}

/// Draw a banner showing pending questions from coworkers.
///
/// Each question is shown as:
///   [coworker_name asks]: question text
///   (answer with: midtown coworker nudge --to <name> --message "your answer")
fn draw_pending_questions(f: &mut Frame, questions: &[PendingQuestion], area: Rect) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for q in questions {
        // Question line: bold yellow "[name asks]: question"
        lines.push(Line::from(vec![
            Span::styled(
                format!("[{} asks]: ", q.coworker_name),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(q.question.clone(), Style::default().fg(Color::Yellow)),
        ]));
        // Hint line: dim gray answer instruction
        lines.push(Line::from(vec![Span::styled(
            format!(
                "  (answer: midtown coworker nudge --to {} --message \"...\")",
                q.coworker_name
            ),
            Style::default().fg(Color::DarkGray),
        )]));
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);
}

/// Draw the chat messages area (top of chat panel)
fn draw_chat_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let title = if app.selection_mode {
        format!(" #{} [SELECT] ", app.selected_channel)
    } else {
        format!(" #{} ", app.selected_channel)
    };
    let is_focused = app.focused_pane == FocusedPane::Chat;
    let border_color = if app.selection_mode || is_focused {
        Color::Yellow
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    // Store chat messages area for click detection.
    app.chat_messages_area = Some(area);
    let inner = block.inner(area);

    let msg_height = inner.height as usize;
    app.visible_height = msg_height;
    app.clamp_scroll_offset();

    // Check if we can reuse cached rendered lines
    let cache_key = app.message_cache_key(inner.width, inner.height);
    if let Some(ref cache) = app.message_render_cache
        && cache.cache_key == cache_key
    {
        let mut lines = cache.lines.clone();
        lines.truncate(msg_height);
        let paragraph = Paragraph::new(lines);
        f.render_widget(block, area);
        f.render_widget(paragraph, inner);
        app.diagram_sources.clone_from(&cache.diagram_sources);
        return;
    }

    // Cache miss — full render
    let current_tasks = app.current_tasks().clone();
    let user_display_name = app.user_display_name.clone();
    // Only show top-level messages in the main channel — thread replies belong in the thread panel.
    let visible: Vec<midtown::Message> = app
        .visible_messages()
        .iter()
        .filter(|m| m.thread_parent_id.is_none())
        .cloned()
        .collect();

    // Compute reply counts for thread indicators
    // Maps parent message ID -> (reply count, last replier name)
    let thread_reply_counts: HashMap<String, (usize, Option<String>)> = {
        let mut counts: HashMap<String, (usize, Option<String>)> = HashMap::new();
        for msg in app.messages.iter() {
            if let Some(ref parent_id) = msg.thread_parent_id {
                let entry = counts.entry(parent_id.clone()).or_insert((0, None));
                entry.0 += 1;
                entry.1 = Some(msg.from.clone());
            }
        }
        counts
    };

    let mut lines: Vec<Line> = Vec::new();
    let mut reply_line_map: HashMap<usize, String> = HashMap::new();
    let prev_sender: Option<&str> = None;

    let mut mermaid_to_render: Vec<String> = Vec::new();

    app.diagram_sources.clear();

    for (idx, msg) in visible.iter().enumerate() {
        let segments = mermaid::parse_content_segments(&msg.content);
        let has_special = segments
            .iter()
            .any(|s| !matches!(s, mermaid::ContentSegment::Text(_)));
        let prev = if idx > 0 {
            Some(visible[idx - 1].from.as_str())
        } else {
            prev_sender
        };

        if !has_special {
            let msg_lines = render_message(
                msg,
                inner.width as usize,
                prev,
                &current_tasks,
                user_display_name.as_deref(),
                &app.channel_lead_names,
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
                &app.channel_lead_names,
                &app.mermaid_cache,
                &mut lines,
                &mut app.diagram_sources,
                &mut mermaid_to_render,
            );
        }

        // Add reply indicator if this message has thread replies
        if let Some((count, last_from)) = thread_reply_counts.get(&msg.id) {
            reply_line_map.insert(lines.len(), msg.id.clone());
            lines.push(build_reply_indicator_line(*count, last_from.as_deref()));
        }
    }

    for source in mermaid_to_render {
        app.mermaid_cache.get_or_render(&source);
    }

    // Handle line truncation based on scroll position
    let total_lines = lines.len();
    let visible_start = if total_lines > msg_height && !app.is_at_max_scroll() {
        total_lines - msg_height
    } else {
        0
    };
    let visible_lines = if total_lines > msg_height {
        if app.is_at_max_scroll() {
            lines.truncate(msg_height);
            lines
        } else {
            let truncation_offset = total_lines - msg_height;
            lines.split_off(truncation_offset)
        }
    } else {
        lines
    };
    let visible_len = visible_lines.len();

    // Rebuild click map for visible reply-indicator lines.
    app.thread_reply_line_map = reply_line_map
        .into_iter()
        .filter_map(|(line_idx, parent_id)| {
            if line_idx >= visible_start && line_idx < visible_start + visible_len {
                Some(((line_idx - visible_start) as u16, parent_id))
            } else {
                None
            }
        })
        .collect();

    // Feed rendered overflow back to app so max_scroll() can unblock scrolling
    // on channels where few messages render to more lines than the display area.
    app.rendered_overflow = total_lines.saturating_sub(msg_height);

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
        format!("{}█", app.input_text)
    } else if is_focused {
        let byte_idx = app
            .input_text
            .char_indices()
            .nth(app.input_cursor)
            .map(|(idx, _)| idx)
            .unwrap_or(app.input_text.len());
        let (before, after) = app.input_text.split_at(byte_idx);
        format!("{}█{}", before, after)
    } else {
        app.input_text.clone()
    };

    // Build the line with optional pending image indicator
    let mut spans: Vec<Span> = vec![Span::raw(prompt)];
    if let Some(ref img) = app.pending_image {
        let format_name = img
            .media_type
            .split('/')
            .nth(1)
            .unwrap_or(&img.media_type)
            .to_uppercase();
        let label = format!("[📎 image: {}] ", format_name);
        spans.push(Span::styled(
            label,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        ));
    }
    spans.push(Span::raw(text_with_cursor));

    let paragraph = Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false });

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Draw autocomplete dropdown above the input bar
fn draw_autocomplete_dropdown(f: &mut Frame, app: &App, input_area: Rect, indicator_height: u16) {
    let items = &app.autocomplete.items;
    if items.is_empty() {
        return;
    }

    let is_thread = app.autocomplete.trigger_type == Some('/');
    let item_count = items.len().min(8);
    let dropdown_height = (item_count * 2) as u16;
    let max_width = if is_thread { 60u16 } else { 40u16 };
    let dropdown_width = max_width.min(input_area.width.saturating_sub(4));

    // Position above input bar, accounting for the dynamic indicator area height.
    let dropdown_y = input_area
        .y
        .saturating_sub(dropdown_height)
        .saturating_sub(indicator_height);
    let dropdown_x = input_area.x + 2;

    let dropdown_area = Rect {
        x: dropdown_x,
        y: dropdown_y,
        width: dropdown_width,
        height: dropdown_height,
    };

    let is_thread_autocomplete = app.autocomplete.trigger_type == Some('/');

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

        if is_thread_autocomplete {
            // For /thread autocomplete, show the description (sender: content) as main line
            let display = item.description.as_deref().unwrap_or(&item.value);
            let display_text = if display.len() > dropdown_width as usize - 4 {
                format!(
                    " {}...",
                    &display[..display.floor_char_boundary(dropdown_width as usize - 7)]
                )
            } else {
                format!(" {}", display)
            };
            lines.push(Line::from(vec![Span::styled(display_text, value_style)]));
            // Second line: empty (keeps 2-row-per-item layout consistent)
            lines.push(Line::from(Span::styled(
                "",
                Style::default().bg(if is_selected {
                    Color::Yellow
                } else {
                    Color::Black
                }),
            )));
        } else {
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

/// Draw the channel switcher overlay (Ctrl+K quick switcher)
pub fn draw_channel_switcher_overlay(f: &mut Frame, app: &App, area: Rect) {
    if !app.channel_switcher.show {
        return;
    }

    // Calculate centered popup size
    let popup_width = 50u16.min(area.width.saturating_sub(4));
    let max_visible_items = 10;
    let item_count = app
        .channel_switcher
        .filtered_channels
        .len()
        .min(max_visible_items);
    // 1 line for input + 1 separator + N channel lines + 2 borders
    let popup_height = (3 + item_count as u16).min(area.height.saturating_sub(4));

    // Center the popup
    let popup_x = (area.width.saturating_sub(popup_width)) / 2;
    let popup_y = (area.height.saturating_sub(popup_height)) / 2;

    let popup_area = Rect {
        x: popup_x,
        y: popup_y,
        width: popup_width,
        height: popup_height,
    };

    // Clear the area first to ensure background is rendered properly
    f.render_widget(Clear, popup_area);

    // Build the content
    let mut lines = Vec::new();

    // Input line with prompt
    let input_line = format!("🔍 {}", app.channel_switcher.input);
    lines.push(Line::from(Span::styled(
        input_line,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));

    // Separator line
    lines.push(Line::from(Span::styled(
        "─".repeat(popup_width.saturating_sub(2) as usize),
        Style::default().fg(Color::DarkGray),
    )));

    // Channel list
    if app.channel_switcher.filtered_channels.is_empty() {
        lines.push(Line::from(Span::styled(
            " No matching channels",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        // Calculate scrolling offset to keep selected item visible
        let total_channels = app.channel_switcher.filtered_channels.len();
        let selected = app.channel_switcher.selected_index;

        // Scroll window to keep selection visible
        let offset = if selected < max_visible_items / 2 {
            // Near start - show from beginning
            0
        } else if selected >= total_channels.saturating_sub(max_visible_items / 2) {
            // Near end - show last N items
            total_channels.saturating_sub(max_visible_items)
        } else {
            // Middle - center selection in window
            selected.saturating_sub(max_visible_items / 2)
        };

        for (i, channel) in app
            .channel_switcher
            .filtered_channels
            .iter()
            .enumerate()
            .skip(offset)
            .take(max_visible_items)
        {
            let is_selected = i == app.channel_switcher.selected_index;

            // Format: "#channel-name (N)" where N is unread count if > 0
            let unread_suffix = if channel.unread_count > 0 {
                format!(" ({})", channel.unread_count)
            } else {
                String::new()
            };
            let channel_text = format!(" #{}{}", channel.name, unread_suffix);

            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            lines.push(Line::from(Span::styled(channel_text, style)));
        }
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Quick Channel Switcher (Ctrl+K) ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, popup_area);
}

#[cfg(test)]
mod tests {
    use super::super::super::app::tests::test_app;
    use super::*;

    // --- lead_indicator_height tests ---

    #[test]
    fn test_lead_indicator_height_empty() {
        // Even with no tool entries, the stable status area always reserves 1 line.
        let app = test_app();
        assert_eq!(lead_indicator_height(&app), 1);
    }

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

#[path = "chat_tests.rs"]
#[cfg(test)]
mod chat_tests;
