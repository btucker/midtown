//! Agent system prompt loading.
//!
//! Loads agent prompts from markdown files at runtime. The prompts are looked for in:
//! 1. The git repository root's `agents/` directory (for development)
//! 2. `~/.midtown/agents/` (for user customization)
//! 3. Embedded defaults (compiled into the binary as fallback)
//!
//! The coworker prompt uses `{name}` as a template variable that gets replaced
//! with the coworker's actual name at runtime.

use std::path::PathBuf;

/// Embedded default for the Lead system prompt.
const DEFAULT_LEAD_PROMPT: &str = include_str!("../agents/lead.md");

/// Embedded default for the coworker system prompt template.
const DEFAULT_COWORKER_PROMPT: &str = include_str!("../agents/coworker.md");

/// Embedded default for common prompt content shared by both agents.
const DEFAULT_COMMON_PROMPT: &str = include_str!("../agents/common.md");

/// Find the git repository root directory.
fn git_repo_root() -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout);
    Some(PathBuf::from(path.trim()))
}

/// Find the user's midtown config directory.
fn user_agents_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".midtown").join("agents"))
}

/// Load a prompt file, trying multiple locations.
///
/// Search order:
/// 1. `<git-repo-root>/agents/<filename>`
/// 2. `~/.midtown/agents/<filename>`
/// 3. Returns None (caller should use embedded default)
fn load_prompt_file(filename: &str) -> Option<String> {
    // Try git repo root first
    if let Some(repo_root) = git_repo_root() {
        let path = repo_root.join("agents").join(filename);
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }

    // Try user config directory
    if let Some(user_dir) = user_agents_dir() {
        let path = user_dir.join(filename);
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }

    None
}

/// Load the common prompt content shared by both agents.
fn common_prompt() -> String {
    load_prompt_file("common.md").unwrap_or_else(|| DEFAULT_COMMON_PROMPT.to_string())
}

/// Load the Lead agent's system prompt.
///
/// Returns the prompt from `agents/lead.md` if found, otherwise returns the
/// embedded default. Appends common prompt content shared with coworkers.
pub fn lead_system_prompt() -> String {
    let lead = load_prompt_file("lead.md").unwrap_or_else(|| DEFAULT_LEAD_PROMPT.to_string());
    let common = common_prompt().replace("{name}", "Lead");
    format!("{lead}\n{common}")
}

/// Load the coworker agent's system prompt with name substitution.
///
/// Returns the prompt from `agents/coworker.md` if found, otherwise returns the
/// embedded default. Appends common prompt content. The `{name}` placeholder is
/// replaced with the coworker's actual name in both the coworker and common sections.
pub fn coworker_system_prompt(name: &str) -> String {
    let template =
        load_prompt_file("coworker.md").unwrap_or_else(|| DEFAULT_COWORKER_PROMPT.to_string());
    let common = common_prompt();
    format!("{template}\n{common}").replace("{name}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lead_system_prompt_loads() {
        let prompt = lead_system_prompt();
        assert!(prompt.contains("Lead System Prompt"));
        assert!(prompt.contains("midtown"));
    }

    #[test]
    fn test_coworker_system_prompt_substitutes_name() {
        let prompt = coworker_system_prompt("lexington");
        assert!(prompt.contains("**lexington**"));
        assert!(prompt.contains("git checkout -b lexington/"));
        assert!(!prompt.contains("{name}"));
    }

    #[test]
    fn test_coworker_system_prompt_contains_required_sections() {
        let prompt = coworker_system_prompt("park");
        assert!(prompt.contains("Channel Usage"));
        assert!(prompt.contains("Task Workflow"));
        assert!(prompt.contains("Git Workflow"));
        assert!(prompt.contains("Coordination"));
    }

    #[test]
    fn test_lead_prompt_contains_commands() {
        let prompt = lead_system_prompt();
        assert!(prompt.contains("midtown status"));
        assert!(prompt.contains("midtown coworker spawn"));
        assert!(prompt.contains("midtown channel"));
    }

    #[test]
    fn test_lead_prompt_contains_delegation_section() {
        let prompt = lead_system_prompt();
        assert!(prompt.contains("Delegation First"));
        assert!(prompt.contains("COORDINATOR"));
    }

    #[test]
    fn test_common_prompt_included_in_lead() {
        let prompt = lead_system_prompt();
        assert!(
            prompt.contains("DO NOT use @mentions in GitHub"),
            "Lead prompt should include GitHub @mentions rule from common.md"
        );
        assert!(
            prompt.contains("GitHub Etiquette"),
            "Lead prompt should include GitHub Etiquette section from common.md"
        );
        assert!(
            prompt.contains("Insights"),
            "Lead prompt should include Insights section from common.md"
        );
    }

    #[test]
    fn test_common_prompt_included_in_coworker() {
        let prompt = coworker_system_prompt("park");
        assert!(
            prompt.contains("DO NOT use @mentions in GitHub"),
            "Coworker prompt should include GitHub @mentions rule from common.md"
        );
        assert!(
            prompt.contains("GitHub Etiquette"),
            "Coworker prompt should include GitHub Etiquette section from common.md"
        );
        assert!(
            prompt.contains("Insights"),
            "Coworker prompt should include Insights section from common.md"
        );
    }

    #[test]
    fn test_common_prompt_name_substitution_in_lead() {
        let prompt = lead_system_prompt();
        assert!(
            prompt.contains("<!-- midtown: Lead -->"),
            "Lead prompt should have {{name}} replaced with 'Lead' in common content"
        );
        assert!(
            !prompt.contains("{name}"),
            "Lead prompt should not contain unreplaced {{name}} placeholders"
        );
    }

    #[test]
    fn test_common_prompt_name_substitution_in_coworker() {
        let prompt = coworker_system_prompt("broadway");
        assert!(
            prompt.contains("<!-- midtown: broadway -->"),
            "Coworker prompt should have {{name}} replaced in common content"
        );
    }
}
