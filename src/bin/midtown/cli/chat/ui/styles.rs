//! Shared color and sender classification helpers.

use ratatui::style::Color;

/// Avenue names mapped to colors (position-based assignment)
const AVENUE_COLORS: &[(&str, Color)] = &[
    ("lexington", Color::Cyan),
    ("park", Color::Green),
    ("madison", Color::LightRed),
    ("broadway", Color::Magenta),
    ("amsterdam", Color::Blue),
    ("columbus", Color::Red),
    ("riverside", Color::LightCyan),
    ("york", Color::LightGreen),
    ("pleasant", Color::LightMagenta),
    ("vernon", Color::LightBlue),
    // Overflow names
    ("bleecker", Color::Indexed(208)), // orange
    ("houston", Color::Indexed(213)),  // pink
    ("canal", Color::Indexed(117)),    // light blue
    ("spring", Color::Indexed(156)),   // light green
    ("prince", Color::Indexed(183)),   // lavender
    ("mercer", Color::Indexed(216)),   // salmon
];

/// Check if a sender is a "system-like" sender that should be grouped together
/// (daemon, system) without blank lines between consecutive messages.
///
/// Note: "github" was previously included here but is now treated like a regular
/// sender for spacing purposes, so github messages get blank line separation
/// matching coworker messages. GitHub content is still styled DarkGray via
/// `get_sender_color_with_leads`.
pub fn is_system_like_sender(sender: &str) -> bool {
    matches!(sender.to_lowercase().as_str(), "daemon" | "system")
}

/// Get color for a sender name.
///
/// Pass `channel_lead_names` to give channel-specific leads (e.g. a lead
/// posting from topic channel "auth") the same LightYellow treatment as the
/// main lead. Pass an empty slice when no channel lead context is available.
pub fn get_sender_color_with_leads(name: &str, channel_lead_names: &[String]) -> Color {
    match name.to_lowercase().as_str() {
        "lead" => Color::LightYellow,
        "user" => Color::White,
        "daemon" | "github" | "system" => Color::DarkGray,
        _ => {
            // Check avenue colors
            for (avenue, color) in AVENUE_COLORS {
                if name.to_lowercase() == *avenue {
                    return *color;
                }
            }
            // Custom user display names get the same color as the main lead
            if midtown::config::get_user_display_name()
                .is_some_and(|dn| dn.eq_ignore_ascii_case(name))
            {
                return Color::LightYellow;
            }
            // Channel leads post with from = channel name (e.g. "auth", "tui")
            if channel_lead_names
                .iter()
                .any(|lead| lead.eq_ignore_ascii_case(name))
            {
                return Color::LightYellow;
            }
            // Default for unknown names
            Color::White
        }
    }
}
