//! Usage progress bars (session + weekly utilization).

use chrono::{DateTime, Local, Utc};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::super::app::App;

/// Draw the usage progress bars (session + weekly utilization).
///
/// Renders two compact lines showing utilization percentage as progress bars
/// with color thresholds: green <60%, yellow 60-80%, red >80%.
pub fn draw_usage_bars(f: &mut Frame, app: &App, area: Rect) {
    let usage = match &app.usage_data {
        Some(data) => data,
        None => return,
    };

    let title = match &usage.account_email {
        Some(email) => format!(" Usage ({}) ", email),
        None => " Usage ".to_string(),
    };

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    if inner.height < 2 {
        return;
    }

    let session_line = render_usage_line(
        "Session",
        usage.session_util,
        usage.session_resets.as_ref(),
        true,
    );
    let week_line = render_usage_line(
        "Week   ",
        usage.week_util,
        usage.week_resets.as_ref(),
        false,
    );

    let lines = vec![session_line, week_line];
    let paragraph = Paragraph::new(lines);
    f.render_widget(paragraph, inner);
}

/// Render a single usage progress bar line.
///
/// Format: `Label ████████░░░░░░░░░░ XX%  ~Xh remaining  ↻ reset_time`
fn render_usage_line(
    label: &str,
    utilization: f64,
    resets_at: Option<&DateTime<Utc>>,
    is_session: bool,
) -> Line<'static> {
    let color = usage_color(utilization);
    let pct = utilization.round() as u32;

    let bar_width: usize = 20;
    let filled = ((utilization / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_filled: String = "█".repeat(filled);
    let bar_empty: String = "░".repeat(empty);

    let (estimate_text, reset_text) = match resets_at {
        Some(r) => (
            estimate_time_to_full(utilization, r, is_session),
            format_reset_time(r, is_session),
        ),
        None => ("—".to_string(), "—".to_string()),
    };

    Line::from(vec![
        Span::styled(format!(" {label} "), Style::default().fg(Color::DarkGray)),
        Span::styled(bar_filled, Style::default().fg(color)),
        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
        Span::styled(format!(" {:>3}%", pct), Style::default().fg(color)),
        Span::styled(
            format!("  {estimate_text}"),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("  ↻ {reset_text}"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// Choose bar color based on utilization threshold.
fn usage_color(utilization: f64) -> Color {
    if utilization >= 80.0 {
        Color::Red
    } else if utilization >= 60.0 {
        Color::Yellow
    } else {
        Color::Green
    }
}

/// Estimate time until utilization reaches 100% based on current consumption rate.
///
/// Uses the known window duration (5h session, 7d weekly) and current utilization
/// to extrapolate when usage will hit the limit. Returns "—" if rate is zero
/// (no consumption) or utilization is already at/above 100%.
fn estimate_time_to_full(utilization: f64, resets_at: &DateTime<Utc>, is_session: bool) -> String {
    if utilization <= 0.0 || utilization >= 100.0 {
        return "—".to_string();
    }

    let now = Utc::now();
    let time_until_reset = resets_at.signed_duration_since(now);
    let secs_until_reset = time_until_reset.num_seconds();

    if secs_until_reset <= 0 {
        return "—".to_string();
    }

    // Total window duration in seconds
    let window_secs: f64 = if is_session {
        5.0 * 3600.0 // 5 hours
    } else {
        7.0 * 24.0 * 3600.0 // 7 days
    };

    // Elapsed time in this window = total_window - time_remaining
    let elapsed_secs = window_secs - secs_until_reset as f64;
    if elapsed_secs <= 0.0 {
        return "—".to_string();
    }

    // Rate = utilization percentage per second
    let rate = utilization / elapsed_secs;
    // Time to reach 100% from current utilization
    let remaining_pct = 100.0 - utilization;
    let secs_to_full = remaining_pct / rate;

    format_duration_estimate(secs_to_full)
}

/// Format a duration in seconds as a human-readable estimate string.
fn format_duration_estimate(secs: f64) -> String {
    let minutes = (secs / 60.0).round() as i64;
    if minutes < 1 {
        "~<1m left".to_string()
    } else if minutes < 60 {
        format!("~{minutes}m left")
    } else {
        let hours = minutes / 60;
        let remaining_mins = minutes % 60;
        if remaining_mins == 0 {
            format!("~{hours}h left")
        } else {
            format!("~{hours}h{remaining_mins}m left")
        }
    }
}

/// Format reset time for display.
///
/// Session: "H:MMam/pm" (e.g., "4:59pm")
/// Weekly: "Mon DD" (e.g., "Feb 11")
/// Returns "now" if the reset time is in the past.
fn format_reset_time(resets_at: &DateTime<Utc>, is_session: bool) -> String {
    if *resets_at <= Utc::now() {
        return "now".to_string();
    }
    let local = resets_at.with_timezone(&Local);
    if is_session {
        local.format("%-I:%M%P").to_string()
    } else {
        local.format("%b %-d").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_color_green() {
        assert_eq!(usage_color(0.0), Color::Green);
        assert_eq!(usage_color(59.9), Color::Green);
    }

    #[test]
    fn test_usage_color_yellow() {
        assert_eq!(usage_color(60.0), Color::Yellow);
        assert_eq!(usage_color(79.9), Color::Yellow);
    }

    #[test]
    fn test_usage_color_red() {
        assert_eq!(usage_color(80.0), Color::Red);
        assert_eq!(usage_color(100.0), Color::Red);
    }

    #[test]
    fn test_render_usage_line_produces_spans() {
        let resets_at = Utc::now() + chrono::Duration::hours(3);
        let line = render_usage_line("Session", 50.0, Some(&resets_at), true);
        assert_eq!(line.spans.len(), 6);
    }

    #[test]
    fn test_render_usage_line_bar_proportions() {
        let resets_at = Utc::now();
        let line = render_usage_line("Test   ", 50.0, Some(&resets_at), true);
        let filled_content = &line.spans[1].content;
        let empty_content = &line.spans[2].content;
        assert_eq!(filled_content.chars().count(), 10);
        assert_eq!(empty_content.chars().count(), 10);
    }

    #[test]
    fn test_render_usage_line_zero_percent() {
        let resets_at = Utc::now();
        let line = render_usage_line("Test   ", 0.0, Some(&resets_at), true);
        let filled_content = &line.spans[1].content;
        let empty_content = &line.spans[2].content;
        assert_eq!(filled_content.chars().count(), 0);
        assert_eq!(empty_content.chars().count(), 20);
    }

    #[test]
    fn test_render_usage_line_full_percent() {
        let resets_at = Utc::now();
        let line = render_usage_line("Test   ", 100.0, Some(&resets_at), true);
        let filled_content = &line.spans[1].content;
        let empty_content = &line.spans[2].content;
        assert_eq!(filled_content.chars().count(), 20);
        assert_eq!(empty_content.chars().count(), 0);
    }

    #[test]
    fn test_render_usage_line_none_resets_at() {
        let line = render_usage_line("Session", 0.0, None, true);
        assert_eq!(line.spans.len(), 6);
        let estimate = &line.spans[4].content;
        let reset = &line.spans[5].content;
        assert!(
            estimate.contains('—'),
            "Estimate should contain em-dash when resets_at is None: {:?}",
            estimate
        );
        assert!(
            reset.contains('—'),
            "Reset should contain em-dash when resets_at is None: {:?}",
            reset
        );
    }

    #[test]
    fn test_format_reset_time_past_returns_now() {
        let past = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(format_reset_time(&past, true), "now");
        assert_eq!(format_reset_time(&past, false), "now");
    }

    #[test]
    fn test_format_reset_time_future_returns_formatted() {
        let future = Utc::now() + chrono::Duration::hours(2);
        let result = format_reset_time(&future, true);
        assert_ne!(result, "now");
        assert!(
            result.contains(':'),
            "Session format should contain colon: {}",
            result
        );

        let result = format_reset_time(&future, false);
        assert_ne!(result, "now");
    }

    #[test]
    fn test_estimate_time_to_full_zero_utilization() {
        let resets_at = Utc::now() + chrono::Duration::hours(3);
        assert_eq!(estimate_time_to_full(0.0, &resets_at, true), "—");
    }

    #[test]
    fn test_estimate_time_to_full_already_full() {
        let resets_at = Utc::now() + chrono::Duration::hours(3);
        assert_eq!(estimate_time_to_full(100.0, &resets_at, true), "—");
    }

    #[test]
    fn test_estimate_time_to_full_past_reset() {
        let resets_at = Utc::now() - chrono::Duration::hours(1);
        assert_eq!(estimate_time_to_full(50.0, &resets_at, true), "—");
    }

    #[test]
    fn test_estimate_time_to_full_session_midpoint() {
        let resets_at = Utc::now() + chrono::Duration::minutes(150);
        let result = estimate_time_to_full(50.0, &resets_at, true);
        assert!(result.contains("left"), "Expected 'left' in: {result}");
        assert!(result.starts_with('~'), "Expected '~' prefix in: {result}");
    }

    #[test]
    fn test_estimate_time_to_full_high_utilization() {
        let resets_at = Utc::now() + chrono::Duration::minutes(30);
        let result = estimate_time_to_full(90.0, &resets_at, true);
        assert!(result.contains("left"), "Expected 'left' in: {result}");
    }

    #[test]
    fn test_format_duration_estimate_minutes() {
        assert_eq!(format_duration_estimate(1800.0), "~30m left");
    }

    #[test]
    fn test_format_duration_estimate_hours() {
        assert_eq!(format_duration_estimate(7200.0), "~2h left");
    }

    #[test]
    fn test_format_duration_estimate_hours_and_minutes() {
        assert_eq!(format_duration_estimate(5400.0), "~1h30m left");
    }

    #[test]
    fn test_format_duration_estimate_less_than_one_minute() {
        assert_eq!(format_duration_estimate(10.0), "~<1m left");
    }
}
