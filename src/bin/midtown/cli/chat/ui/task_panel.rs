//! Task detail panel: shows task metadata and description when a task is clicked in the board.

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::super::app::{App, TaskStatus};

/// Draw the task detail panel for the currently open task.
pub fn draw_task_panel(f: &mut Frame, app: &App, area: Rect) {
    let Some(ref task_id) = app.open_task_id else {
        return;
    };

    let Some(task) = app.tasks.iter().find(|t| &t.id == task_id) else {
        // Task not found — show a placeholder
        let block = Block::default()
            .title(format!(" !{task_id} "))
            .title_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(Span::styled(
                "Task not found",
                Style::default().fg(Color::DarkGray),
            )),
            inner,
        );
        return;
    };

    // Look up PR number for this task (match by task_id field on KanbanPr)
    let pr_number: Option<u64> = task.id.parse::<u64>().ok().and_then(|task_num| {
        app.prs
            .iter()
            .find(|pr| pr.task_id == Some(task_num))
            .map(|pr| pr.number)
    });

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header (task ID + subject)
            Constraint::Min(0),    // Body (metadata + description)
        ])
        .split(area);

    // --- Header ---
    let header_block = Block::default()
        .title(format!(" !{} ", task.id))
        .title_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let header_inner = header_block.inner(chunks[0]);
    let subject_text = task.subject.clone();
    let subject_para = Paragraph::new(Span::styled(
        subject_text,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
    .wrap(Wrap { trim: true });

    f.render_widget(header_block, chunks[0]);
    f.render_widget(subject_para, header_inner);

    // --- Body ---
    let body_block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let body_inner = body_block.inner(chunks[1]);
    let mut lines: Vec<Line> = Vec::new();

    // Status
    let (status_label, status_color) = match task.status {
        TaskStatus::Pending => ("pending", Color::Yellow),
        TaskStatus::InProgress => ("in_progress", Color::Cyan),
        TaskStatus::Completed => ("completed", Color::Green),
    };
    lines.push(Line::from(vec![
        Span::styled("Status:  ", Style::default().fg(Color::DarkGray)),
        Span::styled(status_label, Style::default().fg(status_color)),
    ]));

    // Owner
    let owner_text = task.owner.as_deref().unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled("Owner:   ", Style::default().fg(Color::DarkGray)),
        Span::styled(owner_text.to_string(), Style::default().fg(Color::White)),
    ]));

    // Channel
    if let Some(ref channel) = task.channel {
        lines.push(Line::from(vec![
            Span::styled("Channel: ", Style::default().fg(Color::DarkGray)),
            Span::styled(format!("#{channel}"), Style::default().fg(Color::Blue)),
        ]));
    }

    // PR
    if let Some(pr_num) = pr_number {
        lines.push(Line::from(vec![
            Span::styled("PR:      ", Style::default().fg(Color::DarkGray)),
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
            Span::styled("Blocked: ", Style::default().fg(Color::DarkGray)),
            Span::styled(blocked, Style::default().fg(Color::Red)),
        ]));
    }

    // Modified at
    if let Some(modified) = task.modified_at {
        use super::format_relative_time;
        lines.push(Line::from(vec![
            Span::styled("Updated: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format_relative_time(modified),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    }

    // Description (blank line separator + text)
    if let Some(ref desc) = task.description {
        lines.push(Line::from(""));
        // Word-wrap description manually to body_inner width
        let width = body_inner.width as usize;
        for raw_line in desc.lines() {
            if raw_line.is_empty() {
                lines.push(Line::from(""));
                continue;
            }
            // Simple word-wrap
            let mut current = String::new();
            for word in raw_line.split_whitespace() {
                if current.is_empty() {
                    current.push_str(word);
                } else if current.len() + 1 + word.len() <= width {
                    current.push(' ');
                    current.push_str(word);
                } else {
                    lines.push(Line::from(Span::styled(
                        current.clone(),
                        Style::default().fg(Color::Gray),
                    )));
                    current = word.to_string();
                }
            }
            if !current.is_empty() {
                lines.push(Line::from(Span::styled(
                    current,
                    Style::default().fg(Color::Gray),
                )));
            }
        }
    }

    // Scroll to show last lines if content overflows
    let visible_height = body_inner.height as usize;
    let para = if lines.len() > visible_height {
        let offset = lines.len() - visible_height;
        Paragraph::new(lines.split_off(offset))
    } else {
        Paragraph::new(lines)
    };

    f.render_widget(body_block, chunks[1]);
    f.render_widget(para, body_inner);
}
