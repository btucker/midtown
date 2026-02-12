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
/// `is_dim_sender`.
pub fn is_system_like_sender(sender: &str) -> bool {
    matches!(sender.to_lowercase().as_str(), "daemon" | "system")
}

/// Check if a sender's message content should be rendered in DarkGray.
/// This includes system-like senders and github.
pub fn is_dim_sender(sender: &str) -> bool {
    matches!(
        sender.to_lowercase().as_str(),
        "daemon" | "github" | "system"
    )
}

/// Get color for a sender name
pub fn get_sender_color(name: &str) -> Color {
    match name.to_lowercase().as_str() {
        "lead" | "user" => Color::LightYellow,
        "daemon" | "github" | "system" => Color::DarkGray,
        _ => {
            // Check avenue colors
            for (avenue, color) in AVENUE_COLORS {
                if name.to_lowercase() == *avenue {
                    return *color;
                }
            }
            // Custom user display names get the same color as lead/user
            if midtown::config::get_user_display_name()
                .is_some_and(|dn| dn.eq_ignore_ascii_case(name))
            {
                return Color::LightYellow;
            }
            // Default for unknown names
            Color::White
        }
    }
}
