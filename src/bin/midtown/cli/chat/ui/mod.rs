//! UI rendering for the chat TUI
//!
//! Organized into focused submodules:
//! - `board`: Kanban task swimlanes and coworker status table
//! - `chat`: Message display, input bar, and autocomplete dropdown
//! - `highlight`: Syntax highlighting for fenced code blocks
//! - `messages`: Message rendering (sender headers, timestamps, content layout)
//! - `messages_mermaid`: Mermaid diagram rendering within messages
//! - `styles`: Shared color and sender classification helpers
//! - `text`: Markdown parsing and line wrapping utilities
//! - `thread`: Unified thread panel (task cards + parent message + replies)
//! - `usage`: Inline usage spans for the repo status bar

mod board;
mod chat;
mod highlight;
pub mod messages;
pub mod messages_mermaid;
pub mod styles;
pub mod text;
mod thread;
mod usage;

use chrono::{DateTime, Utc};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use ratatui_themes::ThemePalette;

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

/// Format a channel name for display.
///
/// DM channels (`dm-<name>`) are rendered as `@<name>` to match the web UI.
/// Regular channels keep the `#<name>` prefix.
pub(super) fn format_channel_display_name(channel_name: &str) -> String {
    if let Some(peer) = channel_name.strip_prefix("dm-") {
        format!("@{}", peer)
    } else {
        format!("#{}", channel_name)
    }
}

/// Maximum sidebar width in terminal columns (including left and right borders)
const MAX_SIDEBAR_WIDTH: u16 = 40;

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

    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(status_height), Constraint::Min(10)])
        .split(f.area());

    draw_repo_status_lines(f, app, vertical_chunks[0], &app.usage_data.clone());

    let sidebar_pct = app.sidebar_width_pct;
    let main_area = vertical_chunks[1];
    app.layout_width = main_area.width;
    app.main_area_y = main_area.y;
    app.main_area_bottom = main_area.y + main_area.height;
    // Cap sidebar at MAX_SIDEBAR_WIDTH columns; any remaining space goes to main content.
    // Use u32 arithmetic to avoid overflow on very wide terminals (u16 wraps at width > 1638).
    let sidebar_width =
        (main_area.width as u32 * sidebar_pct as u32 / 100).min(MAX_SIDEBAR_WIDTH as u32) as u16;
    // Sync stored percentage when the cap is active, so drag-resize starts from the
    // rendered divider position rather than the uncapped percentage (avoids dead zone).
    if main_area.width > 0 {
        let effective_pct = (sidebar_width as u32 * 100 / main_area.width as u32) as u16;
        if effective_pct < sidebar_pct {
            app.sidebar_width_pct = effective_pct.max(20);
        }
    }
    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(sidebar_width), Constraint::Min(0)])
        .split(main_area);

    // Track divider X position: the last column of the sidebar panel (its right border)
    app.divider_x = Some(horizontal_chunks[0].x + horizontal_chunks[0].width.saturating_sub(1));

    let (hyperlinks, tasks_area) = board::draw_board_panel(f, app, horizontal_chunks[0]);
    // Store tasks area for click detection (task_line_map line numbers are relative to this area)
    app.board_area = Some(tasks_area);

    // Track right panel geometry for thread resize calculations
    let right_area = horizontal_chunks[1];
    app.right_panel_x = right_area.x;
    app.right_panel_width = right_area.width;

    // When the thread panel is open, split chat+thread area dynamically.
    // thread_panel_pct controls the thread panel width (default 40%).
    if app.is_thread_panel_open() {
        let chat_pct = 100u16.saturating_sub(app.thread_panel_pct);
        let chat_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(chat_pct),
                Constraint::Percentage(app.thread_panel_pct),
            ])
            .split(right_area);
        // Record thread divider: left edge of thread panel = right edge of chat panel
        app.thread_divider_x = Some(chat_chunks[0].x + chat_chunks[0].width.saturating_sub(1));
        app.thread_panel_x = Some(chat_chunks[1].x);
        chat::draw_chat_panel(f, app, chat_chunks[0]);
        thread::draw_thread_panel(f, app, chat_chunks[1]);
    } else {
        app.thread_panel_x = None;
        app.thread_divider_x = None;
        chat::draw_chat_panel(f, app, right_area);
    }

    // Draw channel switcher overlay last so it appears on top
    if app.channel_switcher.show {
        chat::draw_channel_switcher_overlay(f, app, f.area());
    }

    // Draw search overlay on top of everything
    if app.search.show {
        chat::draw_search_overlay(f, app, f.area());
    }

    hyperlinks
}

/// Format relative time (e.g., "3 minutes ago", "2 hours ago", "1 day ago")
pub(super) fn format_relative_time(time: DateTime<Utc>) -> String {
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
///
/// Usage data is appended inline to the last (or only) status line.
fn draw_repo_status_lines(
    f: &mut Frame,
    app: &App,
    area: Rect,
    usage_data: &[midtown::usage::UsageData],
) {
    let palette = app.theme.palette();
    if app.repo_statuses.len() > 1 {
        let last_idx = app.repo_statuses.len() - 1;
        let lines: Vec<Line> = app
            .repo_statuses
            .iter()
            .enumerate()
            .map(|(i, (info, status))| {
                let u = if i == last_idx { usage_data } else { &[] };
                build_repo_status_line(&info.label, status, area.width, palette, u)
            })
            .collect();
        let paragraph = Paragraph::new(lines);
        f.render_widget(paragraph, area);
    } else {
        let line = build_repo_status_line(
            &app.repo_name,
            &app.repo_status,
            area.width,
            palette,
            usage_data,
        );
        let paragraph = Paragraph::new(line);
        f.render_widget(paragraph, area);
    }
}

/// Build a single repo status line with commit, CI, release info, and optional inline usage.
fn build_repo_status_line(
    repo_label: &str,
    status: &RepoStatus,
    width: u16,
    palette: ThemePalette,
    usage_data: &[midtown::usage::UsageData],
) -> Line<'static> {
    let bg = Color::Indexed(236);
    let mut spans = Vec::new();

    spans.push(Span::styled(
        format!(" {}  ", repo_label),
        Style::default().fg(palette.muted).bg(bg),
    ));

    if !status.commit_hash.is_empty() {
        spans.push(Span::styled(
            status.commit_hash.clone(),
            Style::default().fg(palette.accent).bg(bg),
        ));
        if let Some(commit_time) = status.commit_time {
            spans.push(Span::styled(
                format!("  {}  ", format_relative_time(commit_time)),
                Style::default().fg(palette.muted).bg(bg),
            ));
        } else {
            spans.push(Span::styled("  ", Style::default().bg(bg)));
        }
    }

    let (ci_char, ci_color) = match status.ci_status {
        CiStatus::Passed => ("●", palette.success),
        CiStatus::Failed => ("●", palette.error),
        CiStatus::Running => ("●", palette.warning),
        CiStatus::Unknown => ("○", palette.muted),
    };
    spans.push(Span::styled(
        ci_char.to_string(),
        Style::default().fg(ci_color).bg(bg),
    ));
    spans.push(Span::styled("  ", Style::default().bg(bg)));

    if let Some(tag) = &status.release_tag {
        spans.push(Span::styled(
            "Releases: ".to_string(),
            Style::default().fg(palette.muted).bg(bg),
        ));
        spans.push(Span::styled(
            tag.to_string(),
            Style::default().fg(palette.info).bg(bg),
        ));
        if let Some(release_time) = status.release_time {
            spans.push(Span::styled(
                format!("  {}", format_relative_time(release_time)),
                Style::default().fg(palette.muted).bg(bg),
            ));
        }
    }

    // Append inline usage percentages before the padding span
    spans.extend(usage::build_usage_inline_spans(usage_data));

    let content_len: usize = spans.iter().map(|s| s.content.len()).sum();
    if content_len < width as usize {
        spans.push(Span::styled(
            " ".repeat(width as usize - content_len),
            Style::default().bg(bg),
        ));
    }

    Line::from(spans)
}

#[path = "mod_tests.rs"]
#[cfg(test)]
mod tests;
