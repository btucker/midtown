//! Thread panel: parent message header, thread replies, and thread input bar.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::super::app::{App, FocusedPane};
use super::messages::render_message;

/// Draw the thread panel showing a parent message's thread replies.
pub fn draw_thread_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(ref thread_parent_id) = app.thread_parent_id else {
        return;
    };

    // Clone parent message data upfront to avoid borrow conflicts with &mut App
    let parent_msg_data: Option<(String, String)> = app
        .messages
        .iter()
        .find(|m| m.id == *thread_parent_id)
        .map(|m| (m.from.clone(), m.content.clone()));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Parent message header
            Constraint::Min(3),    // Thread replies
            Constraint::Length(3), // Thread input bar (1 content line + 2 border lines)
        ])
        .split(area);

    draw_thread_header(f, parent_msg_data.as_ref(), chunks[0]);
    draw_thread_messages(f, app, chunks[1]);
    draw_thread_input(f, app, chunks[2]);
}

/// Draw the thread header showing the parent message (truncated)
///
/// Takes pre-extracted (from, content) to avoid borrow conflicts.
fn draw_thread_header(f: &mut Frame, parent_msg_data: Option<&(String, String)>, area: Rect) {
    let block = Block::default()
        .title(" Thread ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);

    let line = if let Some((from, content)) = parent_msg_data {
        let max_len = inner.width as usize;
        // Use chars().take() for UTF-8 safe truncation
        let truncated: String = if content.chars().count() > max_len {
            let taken: String = content.chars().take(max_len.saturating_sub(1)).collect();
            format!("{}\u{2026}", taken) // ellipsis
        } else {
            content.clone()
        };
        Line::from(vec![
            Span::styled(
                format!("{}: ", from),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(truncated, Style::default().fg(Color::White)),
        ])
    } else {
        Line::from(Span::styled(
            "Thread (parent not found)",
            Style::default().fg(Color::DarkGray),
        ))
    };

    f.render_widget(block, area);
    f.render_widget(Paragraph::new(line), inner);
}

/// Draw thread reply messages
fn draw_thread_messages(f: &mut Frame, app: &mut App, area: Rect) {
    let border_color = if app.focused_pane == FocusedPane::Thread {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    if app.thread_messages.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            " No replies yet",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(block, area);
        f.render_widget(empty, inner);
        return;
    }

    let current_tasks = app.current_tasks().clone();
    let user_display_name = app.user_display_name.clone();

    let lead_names: Vec<String> = std::iter::once(app.project_name.clone())
        .chain(app.channel_lead_names.iter().cloned())
        .collect();

    let mut lines: Vec<Line> = Vec::new();
    for (idx, msg) in app.thread_messages.iter().enumerate() {
        let prev = if idx > 0 {
            Some(app.thread_messages[idx - 1].from.as_str())
        } else {
            None
        };
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

/// Draw the thread input bar
fn draw_thread_input(f: &mut Frame, app: &mut App, area: Rect) {
    // Store for click-to-focus detection
    app.thread_input_area = Some(area);
    let is_focused = app.focused_pane == FocusedPane::Thread;
    let border_color = if is_focused {
        Color::Yellow
    } else {
        Color::DarkGray
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);

    let prompt = "\u{21b3} "; // ↳
    let char_count = app.thread_input_text.chars().count();
    let cursor_style = Style::default().fg(Color::Black).bg(Color::Yellow);
    let mut spans: Vec<Span> = vec![Span::raw(prompt)];
    if is_focused && app.thread_input_cursor == char_count {
        spans.push(Span::raw(app.thread_input_text.clone()));
        spans.push(Span::styled("\u{2588}", cursor_style)); // █
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
        spans.push(Span::styled(cursor_char.to_string(), cursor_style));
        spans.push(Span::raw(rest.to_string()));
    } else {
        spans.push(Span::raw(app.thread_input_text.clone()));
    }

    let paragraph = Paragraph::new(Line::from(spans)).wrap(Wrap { trim: false });
    f.render_widget(block, area);
    f.render_widget(paragraph, inner);
}
