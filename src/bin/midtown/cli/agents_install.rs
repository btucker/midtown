//! Agent definition installation.
//!
//! Installs compiled-in agent definition files to Claude Code agent directories
//! so that Claude Code can load them via the `--agent` flag. Definitions are
//! installed to every auth profile's `agents/` subdir (so profile-scoped sessions
//! can find them) plus `~/.claude/agents/` as a fallback for non-midtown sessions.
//!
//! Called from `midtown start` (first-run install) and `midtown update` (upgrade install).

use std::fs;
use std::path::{Path, PathBuf};

/// A compiled-in agent definition file.
#[derive(Debug)]
pub struct AgentDefinition {
    pub filename: &'static str,
    pub content: &'static str,
}

/// All agent definitions compiled into the binary.
pub static AGENT_DEFINITIONS: &[AgentDefinition] = &[
    AgentDefinition {
        filename: "midtown-code-author.md",
        content: include_str!("../../../../agents/definitions/midtown-code-author.md"),
    },
    AgentDefinition {
        filename: "midtown-code-reviewer.md",
        content: include_str!("../../../../agents/definitions/midtown-code-reviewer.md"),
    },
    AgentDefinition {
        filename: "midtown-project-lead.md",
        content: include_str!("../../../../agents/definitions/midtown-project-lead.md"),
    },
    AgentDefinition {
        filename: "midtown-channel-lead.md",
        content: include_str!("../../../../agents/definitions/midtown-channel-lead.md"),
    },
];

/// Install agent definitions to the given directory.
///
/// Returns the list of definitions that were written.
/// - Without `force`: only writes files that don't already exist.
/// - With `force`: overwrites all files regardless.
pub fn install_agent_definitions(
    agents_dir: &Path,
    force: bool,
) -> Result<Vec<&'static AgentDefinition>, String> {
    fs::create_dir_all(agents_dir).map_err(|e| {
        format!(
            "Failed to create agents directory {}: {e}",
            agents_dir.display()
        )
    })?;

    let mut installed = Vec::new();

    for def in AGENT_DEFINITIONS {
        let path = agents_dir.join(def.filename);
        if !force && path.exists() {
            continue;
        }
        fs::write(&path, def.content)
            .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
        installed.push(def);
    }

    Ok(installed)
}

/// Check which installed agent definitions differ from the compiled-in versions.
///
/// Returns definitions that are either missing or have different content.
pub fn check_agent_definitions_outdated(agents_dir: &Path) -> Vec<&'static AgentDefinition> {
    AGENT_DEFINITIONS
        .iter()
        .filter(|def| {
            let path = agents_dir.join(def.filename);
            match fs::read_to_string(&path) {
                Ok(content) => content != def.content,
                Err(_) => true, // missing file counts as outdated
            }
        })
        .collect()
}

/// Return the default Claude Code agents directory (`~/.claude/agents/`).
pub fn claude_agents_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("agents")
}

/// Return all Claude agents directories: one per auth profile plus the global fallback.
///
/// Each profile at `~/.midtown/auth/<profile>/claude/` gets an `agents/` subdir so
/// sessions launched with `CLAUDE_CONFIG_DIR` pointing there can find the definitions.
/// The global `~/.claude/agents/` is included as a fallback for sessions running
/// outside midtown.
pub fn all_claude_agents_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Ok(profiles) = midtown::auth::list_profiles_for(midtown::auth::AuthProvider::Claude) {
        for profile in profiles {
            dirs.push(
                midtown::auth::profile_dir_for(midtown::auth::AuthProvider::Claude, &profile)
                    .join("agents"),
            );
        }
    }

    // Fallback: ~/.claude/agents/ for non-midtown sessions
    let fallback = claude_agents_dir();
    if !dirs.contains(&fallback) {
        dirs.push(fallback);
    }

    dirs
}

#[path = "agents_install_tests.rs"]
#[cfg(test)]
mod tests;
