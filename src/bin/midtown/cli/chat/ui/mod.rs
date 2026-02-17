//! UI rendering for the chat TUI
//!
//! Organized into focused submodules:
//! - `board`: Kanban task swimlanes and coworker status table
//! - `chat`: Message display, input bar, and autocomplete dropdown
//! - `messages`: Message rendering (sender headers, timestamps, content layout)
//! - `messages_mermaid`: Mermaid diagram rendering within messages
//! - `styles`: Shared color and sender classification helpers
//! - `text`: Markdown parsing and line wrapping utilities
//! - `usage`: Usage progress bars (session + weekly utilization)

mod board;
mod chat;
pub mod messages;
pub mod messages_mermaid;
pub mod styles;
pub mod text;
mod usage;

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};

use super::app::{App, CiStatus, RepoStatus};

/// A hyperlink to be rendered after ratatui draws (using OSC 8 sequences)
#[derive(Debug, Clone, PartialEq)]
pub struct Hyperlink {
    /// Screen x coordinate
    pub x: u16,
    /// Screen y coordinate
    pub y: u16,
    /// Text to display (will be rewritten with OSC 8 wrapping)
    pub text: String,
    /// URL to link to
    pub url: String,
    /// Optional color for the first character (CI status dot)
    pub first_char_color: Option<Color>,
}

/// Gutter width for timestamp: " HH:MM " = 7 chars
const TIMESTAMP_GUTTER_WIDTH: usize = 7;

/// Calculate height for repo status lines (1 per repo, minimum 1)
fn repo_status_height(app: &App) -> u16 {
    let count = app.repo_statuses.len();
    if count > 1 { count as u16 } else { 1 }
}

/// Draw the main UI
///
/// Returns hyperlinks that should be rendered after ratatui draws.
/// These need to be written directly to the terminal using OSC 8 escape sequences,
/// bypassing ratatui's buffer system (which doesn't support hyperlinks).
pub fn draw(f: &mut Frame, app: &mut App) -> Vec<Hyperlink> {
    let status_height = repo_status_height(app);
    // Calculate height dynamically: 4 lines per account with data, 2 lines per account without
    let usage_height = if !app.usage_data.is_empty() {
        app.usage_data
            .iter()
            .map(|u| {
                if u.session_resets.is_some() || u.week_resets.is_some() {
                    4 // label + session + week + blank
                } else {
                    2 // label + "no data"
                }
            })
            .sum::<u16>()
            + 2 // border
    } else {
        0
    };

    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(status_height),
            Constraint::Min(10),
            Constraint::Length(usage_height),
        ])
        .split(f.area());

    draw_repo_status_lines(f, app, vertical_chunks[0]);

    let sidebar_pct = app.sidebar_width_pct;
    let main_area = vertical_chunks[1];
    app.layout_width = main_area.width;
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(sidebar_pct),
            Constraint::Percentage(100 - sidebar_pct),
        ])
        .split(main_area);

    // Track divider X position: the last column of the sidebar panel (its right border)
    app.divider_x = Some(horizontal_chunks[0].x + horizontal_chunks[0].width.saturating_sub(1));

    let (hyperlinks, tasks_area) = board::draw_board_panel(f, app, horizontal_chunks[0]);
    // Store tasks area for click detection (task_line_map line numbers are relative to this area)
    app.board_area = Some(tasks_area);
    chat::draw_chat_panel(f, app, horizontal_chunks[1]);

    if !app.usage_data.is_empty() {
        usage::draw_usage_bars(f, app, vertical_chunks[2]);
    }

    // Draw channel switcher overlay last so it appears on top
    if app.channel_switcher.show {
        chat::draw_channel_switcher_overlay(f, app, f.area());
    }

    hyperlinks
}

/// Format relative time (e.g., "3 minutes ago", "2 hours ago", "1 day ago")
fn format_relative_time(time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(time);
    let minutes = duration.num_minutes();
    let hours = duration.num_hours();
    let days = duration.num_days();

    if days > 0 {
        if days == 1 {
            "1 day ago".to_string()
        } else {
            format!("{} days ago", days)
        }
    } else if hours > 0 {
        if hours == 1 {
            "1 hour ago".to_string()
        } else {
            format!("{} hours ago", hours)
        }
    } else if minutes > 0 {
        if minutes == 1 {
            "1 minute ago".to_string()
        } else {
            format!("{} minutes ago", minutes)
        }
    } else {
        "just now".to_string()
    }
}

/// Draw stacked repo status lines (one per repo, or single line for single-repo)
fn draw_repo_status_lines(f: &mut Frame, app: &App, area: Rect) {
    if app.repo_statuses.len() > 1 {
        let lines: Vec<Line> = app
            .repo_statuses
            .iter()
            .map(|(info, status)| build_repo_status_line(&info.label, status, area.width))
            .collect();
        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, area);
    } else {
        let line = build_repo_status_line(&app.repo_name, &app.repo_status, area.width);
        let paragraph = Paragraph::new(line);
        f.render_widget(paragraph, area);
    }
}

/// Build a single repo status line with commit, CI, and release info
fn build_repo_status_line(repo_label: &str, status: &RepoStatus, width: u16) -> Line<'static> {
    let bg = Color::Indexed(236);
    let mut spans = Vec::new();

    spans.push(Span::styled(
        format!(" {}  ", repo_label),
        Style::default().fg(Color::DarkGray).bg(bg),
    ));

    if !status.commit_hash.is_empty() {
        spans.push(Span::styled(
            status.commit_hash.clone(),
            Style::default().fg(Color::Yellow).bg(bg),
        ));
        if let Some(commit_time) = status.commit_time {
            spans.push(Span::styled(
                format!("  {}  ", format_relative_time(commit_time)),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        } else {
            spans.push(Span::styled("  ", Style::default().bg(bg)));
        }
    }

    let (ci_char, ci_color) = match status.ci_status {
        CiStatus::Passed => ("●", Color::Rgb(0, 208, 80)),
        CiStatus::Failed => ("●", Color::Red),
        CiStatus::Running => ("●", Color::Yellow),
        CiStatus::Unknown => ("○", Color::DarkGray),
    };
    spans.push(Span::styled(
        ci_char.to_string(),
        Style::default().fg(ci_color).bg(bg),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bg)));

    if let Some(tag) = &status.release_tag {
        spans.push(Span::styled(
            "Releases: ".to_string(),
            Style::default().fg(Color::DarkGray).bg(bg),
        ));
        spans.push(Span::styled(
            tag.to_string(),
            Style::default().fg(Color::Cyan).bg(bg),
        ));
        if let Some(release_time) = status.release_time {
            spans.push(Span::styled(
                format!("  {}", format_relative_time(release_time)),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        }
    }

    let content_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if content_len < width as usize {
        spans.push(Span::styled(
            " ".repeat(width as usize - content_len),
            Style::default().bg(bg),
        ));
    }

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_relative_time() {
        use chrono::{Duration, Utc};

        let now = Utc::now();

        assert_eq!(format_relative_time(now), "just now");

        assert_eq!(
            format_relative_time(now - Duration::minutes(1)),
            "1 minute ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::minutes(30)),
            "30 minutes ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::minutes(59)),
            "59 minutes ago"
        );

        assert_eq!(format_relative_time(now - Duration::hours(1)), "1 hour ago");
        assert_eq!(
            format_relative_time(now - Duration::hours(5)),
            "5 hours ago"
        );
        assert_eq!(
            format_relative_time(now - Duration::hours(23)),
            "23 hours ago"
        );

        assert_eq!(format_relative_time(now - Duration::days(1)), "1 day ago");
        assert_eq!(format_relative_time(now - Duration::days(7)), "7 days ago");
    }
}
