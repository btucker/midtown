//! Thread panel: unified scrollable view with task cards, parent message, and thread replies.
//!
//! Layout: task cards (if any) → parent message → separator → thread replies
//! All content is in a single scrollable area above the input bar.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use ratatui_themes::ThemePalette;

use super::super::app::{App, FocusedPane, KanbanTask, TaskStatus};
use super::chat::calculate_input_bar_height;
use super::format_relative_time;
use super::messages::{apply_mention_highlights, render_content_lines, render_message};
use super::messages_mermaid::{render_header_content_segments, render_message_with_mermaid};
use crate::cli::chat::mermaid;

/// Draw the thread panel showing task cards, parent message, and thread replies.
pub fn draw_thread_panel(f: &mut Frame, app: &mut App, area: Rect) {
    if !app.is_thread_panel_open() {
        return;
    }

    let palette = app.theme.palette();
    let thread_input_height = calculate_input_bar_height(&app.thread_input_text, area.width);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),                      // Unified scrollable content area
            Constraint::Length(thread_input_height), // Thread input bar
        ])
        .split(area);

    draw_thread_content(f, app, chunks[0], palette);
    draw_thread_input(f, app, chunks[1]);
}

/// Render a compact task card as a sequence of `Line`s.
///
/// Format: `!{id}  {subject}` (bold) followed by metadata rows.
pub fn render_task_card<'a>(task: &KanbanTask, app: &App, _content_width: usize) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Task ID + subject on one line
    let (status_color, status_label) = match task.status {
        TaskStatus::Pending => (Color::Yellow, "pending"),
        TaskStatus::InProgress => (Color::Cyan, "in_progress"),
        TaskStatus::Completed => (Color::Green, "completed"),
    };

    lines.push(Line::from(vec![
        Span::styled(
            format!("!{}  ", task.id),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            task.subject.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Status + owner on one compact line
    let owner = task.owner.as_deref().unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled("  ", Style::default()),
        Span::styled(status_label, Style::default().fg(status_color)),
        Span::styled("  owner: ", Style::default().fg(Color::DarkGray)),
        Span::styled(owner.to_string(), Style::default().fg(Color::White)),
    ]));

    // PR number (if associated)
    let pr_number: Option<u64> = task.id.parse::<u64>().ok().and_then(|task_num| {
        app.prs
            .iter()
            .find(|pr| pr.task_id == Some(task_num))
            .map(|pr| pr.number)
    });
    if let Some(pr_num) = pr_number {
        lines.push(Line::from(vec![
            Span::styled("  pr: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("#{pr_num}"), Style::default().fg(Color::Magenta)),
        ]));
    }

    // Blocked by
    if !task.blocked_by.is_empty() {
        let blocked = task
            .blocked_by
            .iter()
            .map(|id| format!("!{id}"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(Line::from(vec![
            Span::styled("  blocked: ", Style::default().fg(Color::DarkGray)),
            Span::styled(blocked, Style::default().fg(Color::Red)),
        ]));
    }

    // Last modified time
    if let Some(modified) = task.modified_at {
        lines.push(Line::from(vec![
            Span::styled("  updated: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format_relative_time(modified),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    lines
}

/// Draw the unified thread content area (task cards + parent message + replies) in a single
/// scrollable Paragraph.
fn draw_thread_content(f: &mut Frame, app: &mut App, area: Rect, palette: ThemePalette) {
    let border_color = if app.focused_pane == FocusedPane::Thread {
        palette.accent
    } else {
        palette.muted
    };

    let block = Block::default()
        .title(" Thread ")
        .title_style(
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color));

    let inner = block.inner(area);
    let content_width = inner.width as usize;

    let mut all_lines: Vec<Line<'static>> = Vec::new();

    // ── Task cards ──────────────────────────────────────────────────────────────
    //
    // Collect tasks for this thread from two sources:
    // 1. thread_task_ids — explicitly set when opening via a task click
    // 2. find_tasks_for_thread — dynamic scan for tasks with matching message_id
    //
    // Deduplicate by task ID so a task doesn't appear twice.
    let mut seen_ids: Vec<String> = Vec::new();
    let mut task_cards: Vec<KanbanTask> = Vec::new();

    // Explicit task IDs first (from task click)
    for task_id in &app.thread_task_ids.clone() {
        if let Some(task) = app.tasks.iter().find(|t| &t.id == task_id)
            && !seen_ids.contains(&task.id)
        {
            seen_ids.push(task.id.clone());
            task_cards.push(task.clone());
        }
    }

    // Then tasks found via message_id scan (for threads opened via message click)
    if let Some(ref parent_id) = app.thread_parent_id.clone() {
        let extra: Vec<KanbanTask> = app
            .find_tasks_for_thread(parent_id)
            .into_iter()
            .filter(|t| !seen_ids.contains(&t.id))
            .cloned()
            .collect();
        for task in extra {
            seen_ids.push(task.id.clone());
            task_cards.push(task);
        }
    }

    for task in &task_cards {
        let card_lines = render_task_card(task, app, content_width);
        all_lines.extend(card_lines);
        // Blank line separator between cards
        all_lines.push(Line::from(""));
    }

    // ── Parent message ──────────────────────────────────────────────────────────
    if let Some(ref parent_id) = app.thread_parent_id.clone() {
        let use_light_theme = app.theme.palette().is_light();
        let content_style = Style::default().fg(palette.fg);

        if let Some(parent_msg) = app.messages.iter().find(|m| m.id == *parent_id).cloned() {
            // Sender header line
            all_lines.push(Line::from(Span::styled(
                parent_msg.from.clone(),
                Style::default()
                    .fg(palette.warning)
                    .add_modifier(Modifier::BOLD),
            )));

            // Message content (with mermaid/code block support)
            if content_width > 0 {
                let segments = mermaid::parse_content_segments(&parent_msg.content);
                let has_special = segments
                    .iter()
                    .any(|s| !matches!(s, mermaid::ContentSegment::Text(_)));

                let content_lines = if has_special {
                    apply_mention_highlights(render_header_content_segments(
                        &segments,
                        content_width,
                        content_style,
                        use_light_theme,
                    ))
                } else {
                    apply_mention_highlights(render_content_lines(
                        &parent_msg.content,
                        content_width,
                        content_style,
                    ))
                };
                all_lines.extend(content_lines);
            }
        } else if task_cards.is_empty() {
            // No parent message and no task cards — show placeholder
            all_lines.push(Line::from(Span::styled(
                "(parent message not in history)",
                Style::default().fg(palette.muted),
            )));
        }

        // Separator between parent message and replies
        let sep_width = content_width.min(60);
        all_lines.push(Line::from(""));
        all_lines.push(Line::from(Span::styled(
            "─".repeat(sep_width),
            Style::default().fg(palette.muted),
        )));
        all_lines.push(Line::from(""));
    }

    // ── Thread replies ──────────────────────────────────────────────────────────
    if app.thread_messages.is_empty() && app.thread_parent_id.is_some() {
        all_lines.push(Line::from(Span::styled(
            "No replies yet",
            Style::default().fg(palette.muted),
        )));
    } else {
        let current_tasks = app.current_tasks().clone();
        let user_display_name = app.user_display_name.clone();
        let use_light_theme = app.theme.palette().is_light();
        let lead_names: Vec<String> = std::iter::once(app.project_name.clone())
            .chain(app.channel_lead_names.iter().cloned())
            .collect();
        let thread_messages = app.thread_messages.clone();

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
                    content_width,
                    prev,
                    &current_tasks,
                    user_display_name.as_deref(),
                    &lead_names,
                    &app.mermaid_cache,
                    &mut all_lines,
                    &mut diagram_sources,
                    &mut mermaid_to_render,
                    use_light_theme,
                );
            } else {
                let msg_lines = render_message(
                    msg,
                    content_width,
                    prev,
                    &current_tasks,
                    user_display_name.as_deref(),
                    &lead_names,
                );
                all_lines.extend(msg_lines);
            }
        }

        for source in mermaid_to_render {
            app.mermaid_cache.get_or_render(&source);
        }
    }

    // ── Scroll and render ────────────────────────────────────────────────────────
    // scroll_offset=0 → bottom (newest content), higher → older content
    let visible_height = inner.height as usize;
    let total = all_lines.len();
    if total > visible_height {
        let max_offset = total - visible_height;
        let scroll = app.thread_scroll_offset.min(max_offset);
        let from = max_offset - scroll;
        all_lines = all_lines[from..from + visible_height].to_vec();
    }

    f.render_widget(block, area);
    f.render_widget(Paragraph::new(all_lines), inner);
}

#[path = "thread_tests.rs"]
#[cfg(test)]
mod thread_tests;

#[path = "task_panel_tests.rs"]
#[cfg(test)]
mod task_panel_tests;

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
