use super::*;

/// Mirrors the sidebar width calculation from `draw()`.
fn compute_sidebar_width(terminal_width: u16, sidebar_pct: u16) -> u16 {
    (terminal_width as u32 * sidebar_pct as u32 / 100).min(MAX_SIDEBAR_WIDTH as u32) as u16
}

#[test]
fn test_sidebar_width_capped_at_max() {
    // 120-column terminal at 40% → 48 columns, capped to 40
    assert_eq!(compute_sidebar_width(120, 40), 40);
    // 100-column terminal at 40% → exactly 40
    assert_eq!(compute_sidebar_width(100, 40), 40);
    // 80-column terminal at 40% → 32, under cap
    assert_eq!(compute_sidebar_width(80, 40), 32);
}

#[test]
fn test_sidebar_width_no_u16_overflow_on_wide_terminal() {
    // 2000 columns at 40% = 800, capped to 40 — should not panic or wrap
    assert_eq!(compute_sidebar_width(2000, 40), 40);
    // 5000 columns at 60% = 3000, capped to 40
    assert_eq!(compute_sidebar_width(5000, 60), 40);
}

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

#[test]
fn test_format_channel_display_name_regular() {
    assert_eq!(format_channel_display_name("midtown"), "#midtown");
    assert_eq!(format_channel_display_name("tui"), "#tui");
    assert_eq!(format_channel_display_name("ops"), "#ops");
}

#[test]
fn test_format_channel_display_name_dm() {
    assert_eq!(format_channel_display_name("dm-park"), "@park");
    assert_eq!(format_channel_display_name("dm-vernon"), "@vernon");
    assert_eq!(format_channel_display_name("dm-riverside"), "@riverside");
}

#[test]
fn test_format_channel_display_name_dm_edge_cases() {
    // Channel that contains "dm-" but doesn't start with it
    assert_eq!(
        format_channel_display_name("my-dm-channel"),
        "#my-dm-channel"
    );
    // Bare "dm-" prefix with empty peer name
    assert_eq!(format_channel_display_name("dm-"), "@");
}
