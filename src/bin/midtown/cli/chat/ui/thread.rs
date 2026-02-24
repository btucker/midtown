//! Thread panel: parent message header, thread replies, and thread input bar.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use ratatui_themes::ThemePalette;

use super::super::app::{App, FocusedPane, TaskStatus};
use super::chat::calculate_input_bar_height;
use super::messages::{apply_mention_highlights, render_content_lines, render_message};
use super::messages_mermaid::{render_header_content_segments, render_message_with_mermaid};
use crate::cli::chat::mermaid;

/// Draw the thread panel showing a parent message's thread replies.
pub fn draw_thread_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(ref thread_parent_id) = app.thread_parent_id else {
        return;
    };

    let palette = app.theme.palette();

    // Pre-render parent message content upfront to get actual line count and avoid
    // borrow conflicts with &mut App. Content width is area width minus 2 (for borders).
    let content_width = area.width.saturating_sub(2) as usize;
    let content_style = Style::default().fg(palette.fg);

    // When the thread was opened via a task click, render task metadata in the header
    // instead of the raw "created task: ..." message content.
    let parent_msg_data: Option<(String, Vec<Line<'static>>)> =
        if let Some(ref task_id) = app.thread_task_id.clone() {
            build_task_thread_header(task_id, app, content_width)
        } else {
            let use_light_theme = app.theme.palette().is_light();
            app.messages
                .iter()
                .find(|m| m.id == *thread_parent_id)
                .map(|m| {
                    // Skip rendering content at zero width: render_content_lines would wrap each
                    // character to its own line, inflating header_height to the 12-line cap and
                    // crowding out thread replies in narrow terminals.
                    if content_width == 0 {
                        return (m.from.clone(), vec![]);
                    }
                    let segments = mermaid::parse_content_segments(&m.content);
                    let has_special = segments
                        .iter()
                        .any(|s| !matches!(s, mermaid::ContentSegment::Text(_)));
                    let rendered = if has_special {
                        let lines = render_header_content_segments(
                            &segments,
                            content_width,
                            content_style,
                            use_light_theme,
                        );
                        apply_mention_highlights(lines)
                    } else {
                        let lines = render_content_lines(&m.content, content_width, content_style);
                        apply_mention_highlights(lines)
                    };
                    (m.from.clone(), rendered)
                })
        };

    // Header height: 2 borders + 1 sender line + rendered content line count, capped at 12.
    let header_height = if let Some((_, ref lines)) = parent_msg_data {
        (lines.len() as u16 + 3).clamp(4, 12)
    } else {
        4
    };

    let thread_input_height = calculate_input_bar_height(&app.thread_input_text, area.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // Parent message header (dynamic)
            Constraint::Min(3),                // Thread replies
            Constraint::Length(thread_input_height), // Thread input bar (dynamic, 1–6 content lines + 2 borders)
        ])
        .split(area);

    draw_thread_header(f, parent_msg_data, chunks[0], palette);
    draw_thread_messages(f, app, chunks[1]);
    draw_thread_input(f, app, chunks[2]);
}

/// Build the thread header data for a task thread.
///
/// Returns `(label, content_lines)` suitable for `draw_thread_header`:
/// - label: "!{task_id}" used as the "sender" line (bold yellow)
/// - content_lines: task subject + metadata fields (status, owner, channel, blocked_by)
///
/// Returns `None` if the task ID is not found in the loaded task list.
fn build_task_thread_header(
    task_id: &str,
    app: &App,
    _content_width: usize,
) -> Option<(String, Vec<Line<'static>>)> {
    use ratatui::style::Color;

    let task = app.tasks.iter().find(|t| t.id == task_id)?;

    let label = format!("!{}", task.id);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Subject line (bold)
    lines.push(Line::from(Span::styled(
        task.subject.clone(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    )));

    // Status
    let (status_str, status_color) = match task.status {
        TaskStatus::Pending => ("pending", Color::Yellow),
        TaskStatus::InProgress => ("in_progress", Color::Cyan),
        TaskStatus::Completed => ("completed", Color::Green),
    };
    lines.push(Line::from(vec![
        Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
        Span::styled(status_str, Style::default().fg(status_color)),
    ]));

    // Owner
    let owner = task.owner.as_deref().unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled("Owner:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(owner.to_string(), Style::default().fg(Color::White)),
    ]));

    // Channel (only if present)
    if let Some(ref channel) = task.channel {
        lines.push(Line::from(vec![
            Span::styled("Channel:", Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" #{channel}"), Style::default().fg(Color::Blue)),
        ]));
    }

    // Blocked by (only if present)
    if !task.blocked_by.is_empty() {
        let blocked = task
            .blocked_by
            .iter()
            .map(|id| format!("!{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(vec![
            Span::styled("Blocked:", Style::default().fg(Color::DarkGray)),
            Span::styled(format!(" {blocked}"), Style::default().fg(Color::Red)),
        ]));
    }

    Some((label, lines))
}

/// Draw the thread header showing the full parent message with markdown formatting.
///
/// Takes pre-rendered content lines (from `render_content_lines` + `apply_mention_highlights`)
/// to match the markdown styling of regular chat messages. The sender name is shown
/// on its own bold-yellow line above the content, matching the main chat layout.
fn draw_thread_header(
    f: &mut Frame,
    parent_msg_data: Option<(String, Vec<Line<'static>>)>,
    area: Rect,
    palette: ThemePalette,
) {
    let block = Block::default()
        .title(" Thread ")
        .title_style(
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(palette.muted));

    let inner = block.inner(area);

    let lines: Vec<Line<'static>> = if let Some((from, content_lines)) = parent_msg_data {
        let mut lines = vec![Line::from(Span::styled(
            from,
            Style::default()
                .fg(palette.warning)
                .add_modifier(Modifier::BOLD),
        ))];
        lines.extend(content_lines);
        lines
    } else {
        vec![Line::from(Span::styled(
            "Thread (parent not found)",
            Style::default().fg(palette.muted),
        ))]
    };

    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);
}

/// Draw thread reply messages
fn draw_thread_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let palette = app.theme.palette();
    let border_color = if app.focused_pane == FocusedPane::Thread {
        palette.accent
    } else {
        palette.muted
    };

    let block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    if app.thread_messages.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            " No replies yet",
            Style::default().fg(palette.muted),
        )));
        f.render_widget(block, area);
        f.render_widget(empty, inner);
        return;
    }

    let current_tasks = app.current_tasks().clone();
    let user_display_name = app.user_display_name.clone();
    let use_light_theme = app.theme.palette().is_light();

    let lead_names: Vec<String> = std::iter::once(app.project_name.clone())
        .chain(app.channel_lead_names.iter().cloned())
        .collect();

    // Clone to avoid borrow conflicts between iterating thread_messages and
    // accessing app.mermaid_cache during render_message_with_mermaid.
    let thread_messages = app.thread_messages.clone();

    let mut lines: Vec<Line> = Vec::new();
    let mut mermaid_to_render: Vec<String> = Vec::new();
    let mut diagram_sources: Vec<String> = Vec::new();

    for (idx, msg) in thread_messages.iter().enumerate() {
        let prev = if idx > 0 {
            Some(thread_messages[idx - 1].from.as_str())
        } else {
            None
        };
        let segments = mermaid::parse_content_segments(&msg.content);
        let has_special = segments
            .iter()
            .any(|s| !matches!(s, mermaid::ContentSegment::Text(_)));

        if has_special {
            render_message_with_mermaid(
                msg,
                &segments,
                inner.width as usize,
                prev,
                &current_tasks,
                user_display_name.as_deref(),
                &lead_names,
                &app.mermaid_cache,
                &mut lines,
                &mut diagram_sources,
                &mut mermaid_to_render,
                use_light_theme,
            );
        } else {
            let msg_lines = render_message(
                msg,
                inner.width as usize,
                prev,
                &current_tasks,
                user_display_name.as_deref(),
                &lead_names,
            );
            lines.extend(msg_lines);
        }
    }

    for source in mermaid_to_render {
        app.mermaid_cache.get_or_render(&source);
    }

    // Show N lines based on scroll offset (0 = bottom/newest, higher = older)
    let visible_height = inner.height as usize;
    let total = lines.len();
    if total > visible_height {
        let max_offset = total - visible_height;
        let scroll = app.thread_scroll_offset.min(max_offset);
        let from = max_offset - scroll;
        lines = lines[from..from + visible_height].to_vec();
    }

    let paragraph = Paragraph::new(lines);
    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}

#[path = "thread_tests.rs"]
#[cfg(test)]
mod thread_tests;

/// Draw the thread input bar
fn draw_thread_input(f: &mut Frame, app: &mut App, area: Rect) {
    // Store for click-to-focus detection
    app.thread_input_area = Some(area);
    let is_focused = app.focused_pane == FocusedPane::Thread;
    let palette = app.theme.palette();
    let border_color = if is_focused {
        palette.accent
    } else {
        palette.muted
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    let prompt = "\u{21b3} "; // ↳
    let char_count = app.thread_input_text.chars().count();
    // `█` (FULL BLOCK) fills the entire cell with the foreground color — use palette.fg
    // (light in dark themes) so the block cursor is visible. The char cursor uses inverted
    // colors so the highlighted background (palette.fg) shows around the character glyph.
    let block_cursor_style = Style::default().fg(palette.fg).bg(palette.bg);
    let char_cursor_style = Style::default().fg(palette.bg).bg(palette.fg);
    let mut spans: Vec<Span> = vec![Span::raw(prompt)];
    if is_focused && app.thread_input_cursor == char_count {
        spans.push(Span::raw(app.thread_input_text.clone()));
        spans.push(Span::styled("\u{2588}", block_cursor_style)); // █
    } else if is_focused {
        let byte_idx = app
            .thread_input_text
            .char_indices()
            .nth(app.thread_input_cursor)
            .map(|(idx, _)| idx)
            .unwrap_or(app.thread_input_text.len());
        let (before, after_str) = app.thread_input_text.split_at(byte_idx);
        let cursor_char = after_str.chars().next().unwrap_or(' ');
        let rest = &after_str[cursor_char.len_utf8()..];
        spans.push(Span::raw(before.to_string()));
        spans.push(Span::styled(cursor_char.to_string(), char_cursor_style));
        spans.push(Span::raw(rest.to_string()));
    } else {
        spans.push(Span::raw(app.thread_input_text.clone()));
    }

    let paragraph = Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false });
    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}
