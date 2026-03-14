//! Agent definition installation.
//!
//! Installs compiled-in agent definitions to `~/.claude/agents/` so Claude Code
//! can load them via the `--agent` flag (e.g., `claude --agent midtown-coworker`).

use std::fs;
use std::path::Path;

/// Compiled-in agent definitions: (filename, content).
pub const AGENT_DEFINITIONS: &[(&str, &str)] = &[
    (
        "midtown-coworker.md",
        include_str!("../../../../agents/definitions/midtown-coworker.md"),
    ),
    (
        "midtown-reviewer.md",
        include_str!("../../../../agents/definitions/midtown-reviewer.md"),
    ),
    (
        "midtown-lead.md",
        include_str!("../../../../agents/definitions/midtown-lead.md"),
    ),
    (
        "midtown-channel-lead.md",
        include_str!("../../../../agents/definitions/midtown-channel-lead.md"),
    ),
];

/// Install agent definitions to `~/.claude/agents/`.
///
/// If `force` is false, existing files are not overwritten (preserving user customizations).
/// If `force` is true, all files are overwritten with compiled-in defaults.
pub fn install_agent_definitions(force: bool) -> Result<(), String> {
    let agents_dir = dirs::home_dir()
        .ok_or("Cannot determine home directory")?
        .join(".claude")
        .join("agents");
    install_agent_definitions_to(&agents_dir, force)
}

/// Install agent definitions to a specific directory (testable).
pub fn install_agent_definitions_to(agents_dir: &Path, force: bool) -> Result<(), String> {
    fs::create_dir_all(agents_dir)
        .map_err(|e| format!("Failed to create {}: {}", agents_dir.display(), e))?;

    for (filename, content) in AGENT_DEFINITIONS {
        let path = agents_dir.join(filename);
        if !force && path.exists() {
            continue;
        }
        fs::write(&path, content)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;
    }

    Ok(())
}

/// Check which agent definitions differ from compiled-in versions.
///
/// Returns filenames that are missing or differ from the compiled-in defaults.
pub fn check_agent_definitions_outdated() -> Vec<String> {
    let agents_dir = match dirs::home_dir() {
        Some(home) => home.join(".claude").join("agents"),
        None => {
            return AGENT_DEFINITIONS
                .iter()
                .map(|(f, _)| f.to_string())
                .collect();
        }
    };
    check_agent_definitions_outdated_in(&agents_dir)
}

/// Check which agent definitions differ (testable with custom directory).
pub fn check_agent_definitions_outdated_in(agents_dir: &Path) -> Vec<String> {
    AGENT_DEFINITIONS
        .iter()
        .filter(|(filename, expected)| {
            let path = agents_dir.join(filename);
            match fs::read_to_string(&path) {
                Ok(content) => content != *expected,
                Err(_) => true, // missing file = outdated
            }
        })
        .map(|(filename, _)| filename.to_string())
        .collect()
}

#[path = "agents_install_tests.rs"]
#[cfg(test)]
mod tests;
