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

use super::super::app::{App, FocusedPane, MessageRenderCache, PendingQuestion};
use super::messages::{build_reply_indicator_line, render_message};
use super::messages_mermaid::render_message_with_mermaid;
use super::text::wrap_content;

/// Draw the chat panel showing messages
pub fn draw_chat_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let input_bar_height = calculate_input_bar_height(&app.input_text, area.width);

    // Reserve space for pending questions banner when questions are present.
    // Each question takes 2 lines: the question itself and the answer hint.
    let questions_height = pending_questions_height(&app.pending_questions);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(questions_height),
            Constraint::Min(5),
            Constraint::Length(1),
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
        draw_autocomplete_dropdown(f, app, chunks[3]);
    }
}

/// Draw the lead working indicator line between chat messages and input bar.
///
/// Shows a braille spinner with "lead..." when the headless lead session is
/// actively working. Always reserves the space to prevent layout jitter.
fn draw_lead_indicator(f: &mut Frame, app: &mut App, area: Rect) {
    let line = if app.lead_working {
        let spinner = app.spinner_char();
        Line::from(vec![Span::styled(
            format!(" {} lead...", spinner),
            Style::default().fg(Color::DarkGray),
        )])
    } else {
        Line::from("")
    };
    let paragraph = Paragraph::new(line);
    f.render_widget(paragraph, area);
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

    let inner = block.inner(area);

    // Reserve space for tool activity before setting visible_height so that
    // max_scroll() and visible_messages() use the actual message display area.
    let activity_lines = count_tool_activity_lines(&app.tool_activity);
    let msg_height = (inner.height as usize).saturating_sub(activity_lines);
    app.visible_height = msg_height;
    app.clamp_scroll_offset();

    // Check if we can reuse cached rendered lines
    let cache_key = app.message_cache_key(inner.width);
    if let Some(ref cache) = app.message_render_cache
        && cache.cache_key == cache_key
    {
        let mut lines = cache.lines.clone();
        lines.truncate(msg_height);
        append_tool_activity_lines(&app.tool_activity, &mut lines, &app.channel_lead_names);
        let paragraph = Paragraph::new(lines);
        f.render_widget(block, area);
        f.render_widget(paragraph, inner);
        app.diagram_sources.clone_from(&cache.diagram_sources);
        return;
    }

    // Cache miss — full render
    let current_tasks = app.current_tasks().clone();
    let user_display_name = app.user_display_name.clone();
    let visible: Vec<midtown::Message> = app.visible_messages().to_vec();

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
            lines.push(build_reply_indicator_line(*count, last_from.as_deref()));
        }
    }

    for source in mermaid_to_render {
        app.mermaid_cache.get_or_render(&source);
    }

    // Handle line truncation based on scroll position
    let total_lines = lines.len();
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

    app.message_render_cache = Some(MessageRenderCache::new(
        visible_lines.clone(),
        app.diagram_sources.clone(),
        cache_key,
    ));

    // Append live tool activity (not cached — changes independently of messages).
    let mut final_lines = visible_lines;
    append_tool_activity_lines(
        &app.tool_activity,
        &mut final_lines,
        &app.channel_lead_names,
    );

    let paragraph = Paragraph::new(final_lines);

    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

/// Count how many lines tool activity will occupy (for reserving space).
fn count_tool_activity_lines(
    tool_activity: &std::collections::HashMap<String, Vec<String>>,
) -> usize {
    let mut count = 0;
    for headers in tool_activity.values() {
        if headers.is_empty() {
            continue;
        }
        count += 1; // agent name header
        count += 1; // 1 most recent tool call line
    }
    if count > 0 {
        count += 1; // blank separator before activity strip
    }
    count
}

/// Append per-agent tool call activity lines to the message display.
///
/// Renders a compact activity strip for each agent with recent tool calls,
/// shown below the message history. Skips agents with no activity.
fn append_tool_activity_lines(
    tool_activity: &std::collections::HashMap<String, Vec<String>>,
    lines: &mut Vec<Line<'static>>,
    channel_lead_names: &[String],
) {
    use super::styles::get_sender_color_with_leads;

    // Only render if at least one agent has non-empty headers
    let has_activity = tool_activity.values().any(|h| !h.is_empty());
    if !has_activity {
        return;
    }

    // Blank separator before activity strips
    lines.push(Line::from(""));

    // Sort agents for deterministic ordering
    let mut agents: Vec<&String> = tool_activity.keys().collect();
    agents.sort();

    for agent in agents {
        let headers = &tool_activity[agent];
        if headers.is_empty() {
            continue;
        }

        let color = get_sender_color_with_leads(agent, channel_lead_names);

        // Agent name header: "amsterdam working…"
        lines.push(Line::from(vec![
            Span::styled(
                agent.clone(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" working\u{2026}", Style::default().fg(Color::DarkGray)),
        ]));

        // Show the most recent tool call (last item, most recent last).
        // Each header starts with a prefix char (✓/✗/›) followed by a space and the description.
        let start = headers.len().saturating_sub(1);
        for header in &headers[start..] {
            // Split prefix character from the rest of the description.
            // Format is "<prefix_char> <description>" where prefix is a single Unicode char.
            let mut chars = header.chars();
            let prefix = chars.next().unwrap_or('›');
            let description: String = chars.collect::<String>().trim_start().to_string();

            let prefix_color = match prefix {
                '\u{2713}' => Color::Green, // ✓
                '\u{2717}' => Color::Red,   // ✗
                _ => Color::DarkGray,       // › or anything else
            };
            let text_color = match prefix {
                '\u{2717}' => Color::Red,
                _ => Color::DarkGray,
            };

            lines.push(Line::from(vec![
                Span::styled("  ", Style::default()),
                Span::styled(prefix.to_string(), Style::default().fg(prefix_color)),
                Span::styled(format!(" {description}"), Style::default().fg(text_color)),
            ]));
        }
    }
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
fn draw_autocomplete_dropdown(f: &mut Frame, app: &App, input_area: Rect) {
    let items = &app.autocomplete.items;
    if items.is_empty() {
        return;
    }

    let is_thread = app.autocomplete.trigger_type == Some('/');
    let item_count = items.len().min(8);
    let dropdown_height = (item_count * 2) as u16;
    let max_width = if is_thread { 60u16 } else { 40u16 };
    let dropdown_width = max_width.min(input_area.width.saturating_sub(4));

    // saturating_sub(2): skip past the 1-row lead indicator + 1-row original spacing
    let dropdown_y = input_area
        .y
        .saturating_sub(dropdown_height)
        .saturating_sub(2);
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
    use std::collections::HashMap;

    use ratatui::style::Color;

    use super::*;

    // --- append_tool_activity_lines tests ---

    fn make_activity(agent: &str, headers: Vec<&str>) -> HashMap<String, Vec<String>> {
        let mut map = HashMap::new();
        map.insert(
            agent.to_string(),
            headers.iter().map(|s| s.to_string()).collect(),
        );
        map
    }

    #[test]
    fn test_append_tool_activity_lines_empty_map() {
        let activity: HashMap<String, Vec<String>> = HashMap::new();
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_tool_activity_lines(&activity, &mut lines, &[]);
        assert!(
            lines.is_empty(),
            "No lines should be emitted for empty activity"
        );
    }

    #[test]
    fn test_append_tool_activity_lines_empty_headers() {
        let activity = make_activity("amsterdam", vec![]);
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_tool_activity_lines(&activity, &mut lines, &[]);
        assert!(lines.is_empty(), "No lines for agent with empty headers");
    }

    #[test]
    fn test_append_tool_activity_lines_in_progress_prefix_color() {
        let activity = make_activity("amsterdam", vec!["› Read foo.rs"]);
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_tool_activity_lines(&activity, &mut lines, &[]);

        // Lines: blank separator, agent header, tool call line
        assert_eq!(lines.len(), 3);
        let call_line = &lines[2];
        // Three spans: "  ", "›", " Read foo.rs"
        assert_eq!(call_line.spans.len(), 3);
        // Prefix span should be DarkGray for in-progress
        assert_eq!(
            call_line.spans[1].style.fg,
            Some(Color::DarkGray),
            "In-progress prefix should be DarkGray"
        );
        assert_eq!(
            call_line.spans[2].style.fg,
            Some(Color::DarkGray),
            "In-progress text should be DarkGray"
        );
    }

    #[test]
    fn test_append_tool_activity_lines_success_prefix_color() {
        let activity = make_activity("amsterdam", vec!["✓ Read foo.rs"]);
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_tool_activity_lines(&activity, &mut lines, &[]);

        let call_line = &lines[2];
        assert_eq!(
            call_line.spans[1].style.fg,
            Some(Color::Green),
            "Success prefix should be Green"
        );
        assert_eq!(
            call_line.spans[2].style.fg,
            Some(Color::DarkGray),
            "Success text should be DarkGray"
        );
    }

    #[test]
    fn test_append_tool_activity_lines_error_prefix_color() {
        let activity = make_activity("amsterdam", vec!["✗ Run tests"]);
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_tool_activity_lines(&activity, &mut lines, &[]);

        let call_line = &lines[2];
        assert_eq!(
            call_line.spans[1].style.fg,
            Some(Color::Red),
            "Error prefix should be Red"
        );
        assert_eq!(
            call_line.spans[2].style.fg,
            Some(Color::Red),
            "Error text should be Red"
        );
    }

    #[test]
    fn test_append_tool_activity_lines_channel_lead_color() {
        // A channel lead ("auth") posting tool activity must display with LightYellow,
        // matching their message color in the chat — not the default Color::White for
        // unknown senders.
        let activity = make_activity("auth", vec!["› Read config.toml"]);
        let channel_lead_names: Vec<String> = vec!["auth".to_string()];
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_tool_activity_lines(&activity, &mut lines, &channel_lead_names);

        // Lines: blank separator, agent header, tool call line
        assert_eq!(lines.len(), 3);
        let header_line = &lines[1];

        // The agent name span (first span) should be LightYellow for channel leads
        assert_eq!(
            header_line.spans[0].style.fg,
            Some(Color::LightYellow),
            "Channel lead 'auth' in tool activity strip must be LightYellow, not White"
        );
    }

    #[test]
    fn test_append_tool_activity_lines_shows_only_most_recent() {
        // 5 headers — should show only the last 1 (most recent)
        let activity = make_activity(
            "amsterdam",
            vec!["✓ call1", "✓ call2", "✓ call3", "✓ call4", "› call5"],
        );
        let mut lines: Vec<Line<'static>> = Vec::new();
        append_tool_activity_lines(&activity, &mut lines, &[]);

        // blank sep + agent header + 1 call line
        assert_eq!(lines.len(), 3, "Should emit blank + agent + 1 call line");

        // The single call line should be for "call5" (most recent)
        let last = &lines[2];
        let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("call5"),
            "Only line should be call5 (most recent), got: {text}"
        );
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
