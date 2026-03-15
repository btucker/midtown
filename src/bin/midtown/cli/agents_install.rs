//! Agent definition installation.
//!
//! Installs compiled-in agent definition files to `~/.midtown/platforms/claude/agents/`
//! so that Claude Code can load them via the `--agent` flag. Each auth profile
//! symlinks `agents/` to this shared directory (via `CLAUDE_SHARED_SYMLINK_ENTRIES`),
//! so all profile-scoped sessions can find the definitions.
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
        content: include_str!("../../../../.claude/agents/midtown-code-author.md"),
    },
    AgentDefinition {
        filename: "midtown-code-reviewer.md",
        content: include_str!("../../../../.claude/agents/midtown-code-reviewer.md"),
    },
    AgentDefinition {
        filename: "midtown-project-lead.md",
        content: include_str!("../../../../.claude/agents/midtown-project-lead.md"),
    },
    AgentDefinition {
        filename: "midtown-channel-lead.md",
        content: include_str!("../../../../.claude/agents/midtown-channel-lead.md"),
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

/// Return the shared Claude Code agents directory (`~/.midtown/platforms/claude/agents/`).
///
/// Agent definitions are installed here once. Each auth profile symlinks its own
/// `agents/` entry to this shared directory, so definitions are visible to all
/// profile-scoped sessions without per-profile installation.
pub fn claude_agents_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("platforms")
        .join("claude")
        .join("agents")
}

#[path = "agents_install_tests.rs"]
#[cfg(test)]
mod tests;
