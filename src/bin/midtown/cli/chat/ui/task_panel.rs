//! Task detail panel: shows full task info when a task is clicked in the board.

use chrono::Utc;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::super::app::App;

/// Draw the task detail panel for the currently open task.
pub fn draw_task_panel(f: &mut Frame, app: &App, area: Rect) {
    let Some(ref task_id) = app.open_task_id else {
        return;
    };

    let Some(task) = app.tasks.iter().find(|t| &t.id == task_id) else {
        return;
    };

    // Header (subject) + body (details)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Subject header
            Constraint::Min(3),    // Details body
        ])
        .split(area);

    // --- Header: subject ---
    let header_block = Block::default()
        .title(" Task ")
        .title_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = header_block.inner(chunks[0]);
    let max_len = inner.width as usize;
    let truncated: String = if task.subject.chars().count() > max_len {
        let taken: String = task
            .subject
            .chars()
            .take(max_len.saturating_sub(1))
            .collect();
        format!("{}\u{2026}", taken)
    } else {
        task.subject.clone()
    };
    let header_line = Line::from(Span::styled(
        truncated,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));

    f.render_widget(header_block, chunks[0]);
    f.render_widget(Paragraph::new(header_line), inner);

    // --- Body: details ---
    let body_block = Block::default()
        .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
        .border_style(Style::default().fg(Color::DarkGray));

    let body_inner = body_block.inner(chunks[1]);

    let mut lines: Vec<Line> = Vec::new();

    // Status
    let status_str = format!("{:?}", task.status);
    lines.push(Line::from(vec![
        Span::styled(
            "Status:  ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(status_str, Style::default().fg(Color::Yellow)),
    ]));

    // Owner
    let owner_str = task.owner.as_deref().unwrap_or("(unassigned)").to_string();
    lines.push(Line::from(vec![
        Span::styled(
            "Owner:   ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(owner_str, Style::default().fg(Color::Cyan)),
    ]));

    // Channel
    if let Some(ch) = &task.channel {
        lines.push(Line::from(vec![
            Span::styled(
                "Channel: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(ch.clone(), Style::default().fg(Color::Green)),
        ]));
    }

    // PR number
    if let Some(pr) = task.pr_number {
        lines.push(Line::from(vec![
            Span::styled(
                "PR:      ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("#{}", pr), Style::default().fg(Color::Blue)),
        ]));
    }

    // Blocked by
    if !task.blocked_by.is_empty() {
        let blocked_str = task.blocked_by.join(", ");
        lines.push(Line::from(vec![
            Span::styled(
                "Blocked: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(blocked_str, Style::default().fg(Color::Red)),
        ]));
    }

    // Modified at
    if let Some(modified_at) = task.modified_at {
        let now = Utc::now();
        let duration = now.signed_duration_since(modified_at);
        let age = if duration.num_days() > 0 {
            format!("{} days ago", duration.num_days())
        } else if duration.num_hours() > 0 {
            format!("{} hours ago", duration.num_hours())
        } else if duration.num_minutes() > 0 {
            format!("{} minutes ago", duration.num_minutes())
        } else {
            "just now".to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(
                "Updated: ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(age, Style::default().fg(Color::DarkGray)),
        ]));
    }

    // Description (may be multi-line — wrap at panel width)
    if let Some(desc) = &task.description {
        lines.push(Line::from("")); // blank separator
        let width = body_inner.width as usize;
        for raw_line in desc.lines() {
            // Word-wrap each description line
            if raw_line.is_empty() {
                lines.push(Line::from(""));
                continue;
            }
            let mut remaining = raw_line;
            loop {
                if remaining.chars().count() <= width {
                    lines.push(Line::from(Span::styled(
                        remaining.to_string(),
                        Style::default().fg(Color::White),
                    )));
                    break;
                }
                // Break at last space before width
                let cut = remaining
                    .char_indices()
                    .take(width)
                    .last()
                    .map(|(i, _)| i + 1)
                    .unwrap_or(remaining.len());
                let (chunk, rest) = remaining.split_at(cut);
                lines.push(Line::from(Span::styled(
                    chunk.to_string(),
                    Style::default().fg(Color::White),
                )));
                remaining = rest.trim_start_matches(' ');
                if remaining.is_empty() {
                    break;
                }
            }
        }
    }

    // Clip to visible height
    let visible = body_inner.height as usize;
    if lines.len() > visible {
        lines.truncate(visible);
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    f.render_widget(body_block, chunks[1]);
    f.render_widget(paragraph, body_inner);
}
