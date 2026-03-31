#[path = "naming_tests.rs"]
#[cfg(test)]
mod tests;

const ADJECTIVES: &[&str] = &[
    "swift", "quiet", "bright", "deep", "wild", "bold", "calm", "keen", "warm", "cool", "dark",
    "fair", "glad", "pale", "soft", "vast", "wise", "pure", "rare", "true", "free", "fresh",
    "sharp", "vivid",
];

const NOUNS: &[&str] = &[
    "river", "storm", "grove", "ridge", "shore", "cliff", "brook", "flame", "frost", "ember",
    "cedar", "maple", "coral", "pearl", "opal", "amber", "hawk", "crane", "heron", "falcon",
    "otter", "raven", "finch", "wren",
];

const ICONS: &[&str] = &[
    "code",
    "terminal",
    "git-branch",
    "cpu",
    "database",
    "file-code",
    "wrench",
    "zap",
    "rocket",
    "compass",
    "anchor",
    "shield",
    "star",
    "heart",
    "diamond",
    "crown",
    "feather",
    "leaf",
];

const COLORS: &[&str] = &[
    "#4A90D9", "#D94A4A", "#4AD99A", "#D9A84A", "#9A4AD9", "#4AD9D9", "#D94A9A", "#7AD94A",
    "#D9D94A", "#4A7AD9", "#D96A4A", "#4AD96A",
];

/// Generate a creative two-word name not already in use.
pub fn generate_name(existing_names: &std::collections::HashSet<String>) -> String {
    for _ in 0..100 {
        let adj = ADJECTIVES[fastrand::usize(..ADJECTIVES.len())];
        let noun = NOUNS[fastrand::usize(..NOUNS.len())];
        let name = format!("{adj}-{noun}");
        if !existing_names.contains(&name) {
            return name;
        }
    }
    // Fallback with random suffix
    format!("agent-{}", fastrand::u32(1000..9999))
}

/// Pick a random Lucide icon.
pub fn random_icon() -> String {
    ICONS[fastrand::usize(..ICONS.len())].to_string()
}

/// Pick a random color.
pub fn random_color() -> String {
    COLORS[fastrand::usize(..COLORS.len())].to_string()
}
