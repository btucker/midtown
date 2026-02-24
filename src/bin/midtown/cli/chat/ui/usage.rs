//! Usage inline spans for the repo status bar.

use ratatui::{
    style::{Color, Style},
    text::Span,
};

use midtown::usage::UsageData;

/// Background color matching the repo status bar.
const STATUS_BAR_BG: Color = Color::Indexed(236);

/// Build inline usage spans for the repo status bar.
///
/// Returns an empty vec when `usage_data` is empty.
/// Single account: `  │  S:42% W:15%`
/// Multiple accounts: `  │  CLAUDE 42%/15%  ·  BEDROCK 30%/5%`
pub fn build_usage_inline_spans(usage_data: &[UsageData]) -> Vec<Span<'static>> {
    if usage_data.is_empty() {
        return vec![];
    }

    let bg = STATUS_BAR_BG;
    let mut spans = Vec::new();

    spans.push(Span::styled(
        "  │  ",
        Style::default().fg(Color::DarkGray).bg(bg),
    ));

    let multi = usage_data.len() > 1;

    for (i, usage) in usage_data.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(
                "  ·  ",
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        }

        // For multiple accounts, prefix with the provider name to distinguish them
        if multi {
            let label = usage.provider.as_str().to_uppercase();
            spans.push(Span::styled(
                format!("{label} "),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        }

        if usage.session_resets.is_some() || usage.week_resets.is_some() {
            let s_pct = usage.session_util.round() as u32;
            let w_pct = usage.week_util.round() as u32;
            let s_color = usage_color(usage.session_util);
            let w_color = usage_color(usage.week_util);

            if multi {
                // Compact slash-separated format for multiple accounts
                spans.push(Span::styled(
                    format!("{s_pct}%"),
                    Style::default().fg(s_color).bg(bg),
                ));
                spans.push(Span::styled(
                    "/",
                    Style::default().fg(Color::DarkGray).bg(bg),
                ));
                spans.push(Span::styled(
                    format!("{w_pct}%"),
                    Style::default().fg(w_color).bg(bg),
                ));
            } else {
                // Labeled format for single account: S:42% W:15%
                spans.push(Span::styled(
                    "S:".to_string(),
                    Style::default().fg(Color::DarkGray).bg(bg),
                ));
                spans.push(Span::styled(
                    format!("{s_pct}%"),
                    Style::default().fg(s_color).bg(bg),
                ));
                spans.push(Span::styled(
                    " W:".to_string(),
                    Style::default().fg(Color::DarkGray).bg(bg),
                ));
                spans.push(Span::styled(
                    format!("{w_pct}%"),
                    Style::default().fg(w_color).bg(bg),
                ));
            }
        } else {
            spans.push(Span::styled(
                "S:— W:—".to_string(),
                Style::default().fg(Color::DarkGray).bg(bg),
            ));
        }
    }

    spans
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

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use midtown::auth::AuthProvider;

    use super::*;

    fn make_usage(session_util: f64, week_util: f64, with_resets: bool) -> UsageData {
        let resets = if with_resets {
            Some(Utc::now() + chrono::Duration::hours(3))
        } else {
            None
        };
        UsageData {
            session_util,
            session_resets: resets,
            week_util,
            week_resets: resets,
            account_email: None,
            provider: AuthProvider::Claude,
            profile_name: "default".to_string(),
            cache_age_seconds: None,
            cache_stale: false,
        }
    }

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
    fn test_build_usage_inline_spans_empty() {
        let spans = build_usage_inline_spans(&[]);
        assert!(spans.is_empty());
    }

    #[test]
    fn test_build_usage_inline_spans_single_account() {
        let usage = make_usage(42.0, 15.0, true);
        let spans = build_usage_inline_spans(&[usage]);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("│"), "Should contain separator: {text}");
        assert!(text.contains("S:"), "Should contain S: label: {text}");
        assert!(text.contains("W:"), "Should contain W: label: {text}");
        assert!(text.contains("42%"), "Should contain session pct: {text}");
        assert!(text.contains("15%"), "Should contain weekly pct: {text}");
        // Single account should NOT have provider prefix
        assert!(
            !text.contains("CLAUDE"),
            "Single account should not show provider: {text}"
        );
    }

    #[test]
    fn test_build_usage_inline_spans_no_data() {
        let usage = make_usage(0.0, 0.0, false);
        let spans = build_usage_inline_spans(&[usage]);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("S:—"),
            "Should show em-dash when no resets: {text}"
        );
        assert!(
            text.contains("W:—"),
            "Should show em-dash when no resets: {text}"
        );
    }

    #[test]
    fn test_build_usage_inline_spans_multiple_accounts() {
        let mut u1 = make_usage(42.0, 15.0, true);
        u1.provider = AuthProvider::Claude;
        let mut u2 = make_usage(30.0, 5.0, true);
        u2.provider = AuthProvider::Codex;
        let spans = build_usage_inline_spans(&[u1, u2]);
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            text.contains("CLAUDE"),
            "Multiple accounts should show provider: {text}"
        );
        assert!(
            text.contains("CODEX"),
            "Multiple accounts should show provider: {text}"
        );
        assert!(
            text.contains("42%"),
            "Should contain first account session pct: {text}"
        );
        assert!(
            text.contains("30%"),
            "Should contain second account session pct: {text}"
        );
        assert!(
            text.contains("·"),
            "Multiple accounts should have dot separator: {text}"
        );
        // Multi-account format uses / not S:/W: labels
        assert!(
            !text.contains("S:"),
            "Multi-account should not have S: labels: {text}"
        );
    }

    #[test]
    fn test_build_usage_inline_spans_all_have_status_bar_bg() {
        let usage = make_usage(50.0, 25.0, true);
        let spans = build_usage_inline_spans(&[usage]);
        for span in &spans {
            assert_eq!(
                span.style.bg,
                Some(STATUS_BAR_BG),
                "All spans must carry the status bar background: {:?}",
                span.content
            );
        }
    }
}
