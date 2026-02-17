//! Agent system prompt loading.
//!
//! Loads agent prompts from markdown files at runtime. The prompts are looked for in:
//! 1. The git repository root's `agents/` directory (for development)
//! 2. `~/.midtown/agents/` (for user customization)
//! 3. Embedded defaults (compiled into the binary as fallback)
//!
//! Additionally, a `MIDTOWN.md` file (similar to `CLAUDE.md`) is loaded from:
//! 1. The git repository root (project-level, takes precedence)
//! 2. `~/.midtown/MIDTOWN.md` (user-level)
//!
//! Custom prompts can also be appended via `LEAD.md` and `COWORKER.md` files:
//! 1. `~/.midtown/LEAD.md` / `~/.midtown/COWORKER.md` (global)
//! 2. `~/.midtown/projects/<repo>/LEAD.md` / `~/.midtown/projects/<repo>/COWORKER.md` (project-level)
//!
//! The coworker prompt uses `{name}` as a template variable that gets replaced
//! with the coworker's actual name at runtime.

use std::path::PathBuf;

use crate::config::Personality;

/// Embedded default for the Lead system prompt.
const DEFAULT_LEAD_PROMPT: &str = include_str!("../agents/lead.md");

/// Embedded default for the coworker system prompt template.
const DEFAULT_COWORKER_PROMPT: &str = include_str!("../agents/coworker.md");

/// Embedded default for common prompt content shared by both agents.
const DEFAULT_COMMON_PROMPT: &str = include_str!("../agents/common.md");

/// Embedded default for personality definitions.
const DEFAULT_PERSONALITIES: &str = include_str!("../agents/personalities.md");

/// Embedded default for the reviewer launch prompt template.
const DEFAULT_REVIEWER_PROMPT: &str = include_str!("../agents/reviewer.md");

/// Embedded default for the reviewer resume prompt template.
const DEFAULT_REVIEWER_RESUME_PROMPT: &str = include_str!("../agents/reviewer-resume.md");

/// Embedded default for the clusterer system prompt.
const DEFAULT_CLUSTERER_PROMPT: &str = include_str!("../agents/clusterer.md");

/// Embedded default for the channel lead system prompt template.
const DEFAULT_CHANNEL_LEAD_PROMPT: &str = include_str!("../agents/channel-lead.md");

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

/// Find the user's midtown home directory (~/.midtown/).
fn user_midtown_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".midtown"))
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

/// Load `MIDTOWN.md` from the project root or `~/.midtown/`.
///
/// Search order:
/// 1. `<git-repo-root>/MIDTOWN.md` (project-level)
/// 2. `~/.midtown/MIDTOWN.md` (user-level)
/// 3. Returns None if not found (optional file)
fn load_midtown_md_from_paths(
    project_root: Option<PathBuf>,
    midtown_home: Option<PathBuf>,
) -> Option<String> {
    // Try project root first
    if let Some(repo_root) = project_root {
        let path = repo_root.join("MIDTOWN.md");
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }

    // Try user midtown directory
    if let Some(midtown_dir) = midtown_home {
        let path = midtown_dir.join("MIDTOWN.md");
        if let Ok(content) = std::fs::read_to_string(&path) {
            return Some(content);
        }
    }

    None
}

fn load_midtown_md() -> Option<String> {
    load_midtown_md_from_paths(git_repo_root(), user_midtown_dir())
}

fn merge_common_with_midtown(common: String, midtown: Option<String>) -> String {
    match midtown {
        Some(midtown) => format!("{common}\n{midtown}"),
        None => common,
    }
}

/// Load the common prompt content shared by both agents.
///
/// Combines `agents/common.md` with `MIDTOWN.md` (if present).
fn common_prompt() -> String {
    let common = load_prompt_file("common.md").unwrap_or_else(|| DEFAULT_COMMON_PROMPT.to_string());
    merge_common_with_midtown(common, load_midtown_md())
}

/// Load custom prompt files from global and project-level locations.
///
/// This loads and concatenates content from:
/// 1. `~/.midtown/<filename>` (global)
/// 2. `~/.midtown/projects/<repo>/<filename>` (project-level)
///
/// Both files are optional. Content from both locations is concatenated
/// with newlines between them.
fn load_custom_prompt_files(filename: &str) -> String {
    let mut parts = Vec::new();

    // Load global custom prompt
    if let Some(home) = dirs::home_dir() {
        let global_path = home.join(".midtown").join(filename);
        if let Ok(content) = std::fs::read_to_string(&global_path) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }

    // Load project-level custom prompt
    if let Some(repo) = crate::paths::detect_repo_name() {
        let project_path = crate::paths::projects_dir_for_repo(&repo).join(filename);
        if let Ok(content) = std::fs::read_to_string(&project_path) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }

    if parts.is_empty() {
        String::new()
    } else {
        parts.join("\n\n")
    }
}

/// Extract a personality description for a given agent name and variant.
///
/// Parses `personalities.md` which uses `## name` and `### variant` headers.
/// Returns None if the name or variant is not found.
fn load_personality(name: &str, personality: Personality) -> Option<String> {
    let content =
        load_prompt_file("personalities.md").unwrap_or_else(|| DEFAULT_PERSONALITIES.to_string());
    let variant = personality.as_str();
    let name_lower = name.to_lowercase();

    // Find the section for this agent name (## name)
    let mut in_name_section = false;
    let mut in_variant_section = false;
    let mut lines = Vec::new();

    for line in content.lines() {
        if line.starts_with("## ") && !line.starts_with("### ") {
            let heading = line.trim_start_matches("## ").trim().to_lowercase();
            in_name_section = heading == name_lower;
            in_variant_section = false;
            continue;
        }

        if !in_name_section {
            continue;
        }

        if line.starts_with("### ") {
            let heading = line.trim_start_matches("### ").trim().to_lowercase();
            in_variant_section = heading == variant;
            continue;
        }

        if in_variant_section {
            lines.push(line);
        }
    }

    let text = lines.join("\n").trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Build the personality section to append to a system prompt.
fn personality_section(name: &str, personality: Personality) -> String {
    match load_personality(name, personality) {
        Some(desc) => {
            if personality == Personality::Normal {
                // Normal mode: strictly professional, no personality flair
                format!(
                    "\n\n## Personality\n\n\
                     Your personality variant is set to **normal**. Be strictly professional \
                     in all communication. Use direct, factual language with no flair or \
                     personality expression. Status messages should be plain: \"claimed task 5\", \
                     \"completed task 7\", \"idle\". Keep code itself clean and professional.\n\n{}",
                    desc
                )
            } else {
                // Fun/wild modes: encourage personality expression
                format!(
                    "\n\n## Personality\n\n\
                     Your personality variant is set to **{}**. Let this voice come through in your \
                     channel messages and GitHub comments (PR descriptions, review comments). \
                     Keep code itself clean and professional regardless.\n\n\
                     **Standard status messages are NOT exempt.** When you post claiming, developing, \
                     testing, completing, or idle updates to the channel, phrase them in your personality's \
                     voice. Don't fall back to generic, formulaic messages — every channel post is a \
                     chance to bring your character to life. Just make sure the required status keywords \
                     and task numbers are still present (see Workflow Phases).\n\n{}",
                    personality.as_str(),
                    desc
                )
            }
        }
        None => String::new(),
    }
}

/// Load the Lead agent's system prompt.
///
/// Returns the prompt from `agents/lead.md` if found, otherwise returns the
/// embedded default. Appends common prompt content shared with coworkers.
/// Custom content from `~/.midtown/LEAD.md` and
/// `~/.midtown/projects/<repo>/LEAD.md` is appended if present.
/// If a personality variant is configured, the matching personality description
/// is appended to give the Lead a unique voice.
pub fn lead_system_prompt() -> String {
    let lead = load_prompt_file("lead.md").unwrap_or_else(|| DEFAULT_LEAD_PROMPT.to_string());
    let common = common_prompt();
    let custom = load_custom_prompt_files("LEAD.md");
    let personality = crate::config::get_personality();

    let mut prompt = format!("{lead}\n{common}");
    if !custom.is_empty() {
        prompt = format!("{prompt}\n\n{custom}");
    }
    prompt.push_str(&personality_section("lead", personality));
    prompt.replace("{name}", "Lead")
}

/// Load the coworker agent's system prompt with name substitution.
///
/// Returns the prompt from `agents/coworker.md` if found, otherwise returns the
/// embedded default. Appends common prompt content. The `{name}` placeholder is
/// replaced with the coworker's actual name in both the coworker and common sections.
/// Custom content from `~/.midtown/COWORKER.md` and
/// `~/.midtown/projects/<repo>/COWORKER.md` is appended if present.
/// If a personality variant is configured, the matching personality description
/// is appended to give the coworker a unique voice.
pub fn coworker_system_prompt(name: &str) -> String {
    let template =
        load_prompt_file("coworker.md").unwrap_or_else(|| DEFAULT_COWORKER_PROMPT.to_string());
    let common = common_prompt();
    let custom = load_custom_prompt_files("COWORKER.md");
    let personality = crate::config::get_personality();

    let mut prompt = format!("{template}\n{common}");
    if !custom.is_empty() {
        prompt = format!("{prompt}\n\n{custom}");
    }
    prompt.push_str(&personality_section(name, personality));
    prompt.replace("{name}", name)
}

/// Load the reviewer agent's system prompt with name substitution.
///
/// Combines content from three sources:
/// - `agents/coworker.md` (base coworker behaviors)
/// - `agents/common.md` + `MIDTOWN.md` (shared foundations)
/// - `agents/reviewer.md` (reviewer-specific instructions)
///
/// This ensures reviewers follow reviewer.md instructions as part of their
/// behavioral identity, not just as a task description.
pub fn reviewer_system_prompt(name: &str) -> String {
    let coworker_template =
        load_prompt_file("coworker.md").unwrap_or_else(|| DEFAULT_COWORKER_PROMPT.to_string());
    let common = common_prompt();
    let reviewer =
        load_prompt_file("reviewer.md").unwrap_or_else(|| DEFAULT_REVIEWER_PROMPT.to_string());
    let custom = load_custom_prompt_files("COWORKER.md");
    let personality = crate::config::get_personality();

    // Merge: coworker + common + reviewer-specific instructions
    let mut prompt =
        format!("{coworker_template}\n{common}\n\n## Reviewer Instructions\n\n{reviewer}");
    if !custom.is_empty() {
        prompt = format!("{prompt}\n\n{custom}");
    }
    prompt.push_str(&personality_section(name, personality));
    prompt.replace("{name}", name)
}

/// Build the reviewer launch prompt for a given PR number.
///
/// This is just the task description (which PR to review), not the behavioral
/// instructions. The behavioral instructions are in `reviewer_system_prompt()`.
pub fn reviewer_launch_prompt(pr_number: u64) -> String {
    format!("Review PR #{pr_number} using /code-review:code-review {pr_number}")
}

/// Build the reviewer launch prompt for a given PR number (legacy function).
///
/// Loads `agents/reviewer.md` (or the embedded default) and replaces
/// `{pr_number}` with the actual PR number.
///
/// Note: This is the old approach where reviewer.md was passed as initial_prompt.
/// New code should use `reviewer_system_prompt()` for the system prompt and
/// `reviewer_launch_prompt()` for the task.
pub fn reviewer_prompt(pr_number: u64) -> String {
    let template =
        load_prompt_file("reviewer.md").unwrap_or_else(|| DEFAULT_REVIEWER_PROMPT.to_string());
    template.replace("{pr_number}", &pr_number.to_string())
}

/// Build the reviewer resume prompt for a given PR number.
///
/// Used when the daemon discovers a reviewer coworker still running after
/// a restart. Loads `agents/reviewer-resume.md` (or the embedded default)
/// and replaces `{pr_number}` with the actual PR number.
pub fn reviewer_resume_prompt(pr_number: u64) -> String {
    let template = load_prompt_file("reviewer-resume.md")
        .unwrap_or_else(|| DEFAULT_REVIEWER_RESUME_PROMPT.to_string());
    template.replace("{pr_number}", &pr_number.to_string())
}

/// Load the clusterer system prompt.
///
/// Returns the prompt from `agents/clusterer.md` if found, otherwise returns
/// the embedded default. This prompt guides the AI clusterer in organizing tasks
/// into topic channels based on code locality and thematic grouping.
///
/// The clusterer uses the haiku model to keep costs low while analyzing task
/// relationships and channel structure.
pub fn clusterer_system_prompt() -> String {
    load_prompt_file("clusterer.md").unwrap_or_else(|| DEFAULT_CLUSTERER_PROMPT.to_string())
}

/// Load the channel lead system prompt with channel name and domain context substitution.
///
/// Returns the prompt from `agents/channel-lead.md` if found, otherwise returns
/// the embedded default. The `{channel_name}` placeholder is replaced with the
/// actual channel name, and `{domain_context}` is replaced with daemon-injected
/// context (channel description, active tasks, recent PRs).
pub fn channel_lead_system_prompt(channel_name: &str, domain_context: &str) -> String {
    let template = load_prompt_file("channel-lead.md")
        .unwrap_or_else(|| DEFAULT_CHANNEL_LEAD_PROMPT.to_string());
    template
        .replace("{channel_name}", channel_name)
        .replace("{domain_context}", domain_context)
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
        assert!(prompt.contains("Your Task"));
        assert!(prompt.contains("Git Workflow"));
        assert!(prompt.contains("Coordination"));
        assert!(prompt.contains("Read the Channel"));
        assert!(prompt.contains("midtown channel read"));
        assert!(prompt.contains("[Midtown !"));
    }

    #[test]
    fn test_lead_prompt_contains_commands() {
        let prompt = lead_system_prompt();
        assert!(prompt.contains("midtown status"));
        assert!(prompt.contains("midtown coworker call-in"));
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
            prompt.contains("CRITICAL: NEVER use @mentions in GitHub"),
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
            prompt.contains("CRITICAL: NEVER use @mentions in GitHub"),
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

    #[test]
    fn test_load_midtown_md_from_paths_prefers_project_file() {
        let project = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();

        std::fs::write(project.path().join("MIDTOWN.md"), "project-level").unwrap();
        std::fs::write(user.path().join("MIDTOWN.md"), "user-level").unwrap();

        let loaded = load_midtown_md_from_paths(
            Some(project.path().to_path_buf()),
            Some(user.path().to_path_buf()),
        )
        .expect("MIDTOWN.md should load from one of the provided paths");

        assert_eq!(loaded, "project-level");
    }

    #[test]
    fn test_merge_common_with_midtown_appends_midtown_content() {
        let merged = merge_common_with_midtown(
            "base common prompt".to_string(),
            Some("project steer".into()),
        );
        assert_eq!(merged, "base common prompt\nproject steer");
    }

    #[test]
    fn test_load_midtown_md_returns_none_when_missing() {
        // MIDTOWN.md is optional - should return None when not present
        // (This test verifies the function doesn't panic)
        let result = load_midtown_md();
        // Result depends on environment - just verify it doesn't panic
        drop(result);
    }

    #[test]
    fn test_common_prompt_works_without_midtown_md() {
        // common_prompt should still return common.md content even without MIDTOWN.md
        let prompt = common_prompt();
        assert!(
            prompt.contains("GitHub Etiquette"),
            "Common prompt should contain base content even without MIDTOWN.md"
        );
    }

    #[test]
    fn test_load_custom_prompt_files_returns_empty_for_nonexistent() {
        // Should return empty string when no custom files exist
        let result = load_custom_prompt_files("NONEXISTENT_FILE_12345.md");
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_personality_normal() {
        let result = load_personality("york", Personality::Normal);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(
            text.contains("Professional"),
            "york normal should be professional"
        );
    }

    #[test]
    fn test_load_personality_fun() {
        let result = load_personality("york", Personality::Fun);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(
            text.contains("Upper East Side"),
            "york fun should reference Upper East Side"
        );
    }

    #[test]
    fn test_load_personality_wild() {
        let result = load_personality("york", Personality::Wild);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(
            text.contains("Yorkville"),
            "york wild should reference Yorkville neighborhood"
        );
    }

    #[test]
    fn test_load_personality_lead() {
        let result = load_personality("lead", Personality::Fun);
        assert!(result.is_some());
        let text = result.unwrap();
        assert!(!text.is_empty(), "lead fun should have content");
    }

    #[test]
    fn test_load_personality_unknown_name() {
        let result = load_personality("nonexistent_agent", Personality::Normal);
        assert!(result.is_none());
    }

    #[test]
    fn test_load_personality_case_insensitive() {
        let result = load_personality("York", Personality::Normal);
        assert!(
            result.is_some(),
            "Personality lookup should be case-insensitive"
        );
    }

    #[test]
    fn test_personality_section_builds_correctly() {
        let section = personality_section("broadway", Personality::Fun);
        assert!(section.contains("## Personality"));
        assert!(section.contains("**fun**"));
        assert!(section.contains("opening night"));
    }

    #[test]
    fn test_personality_section_empty_for_unknown() {
        let section = personality_section("nonexistent", Personality::Normal);
        assert!(section.is_empty());
    }

    #[test]
    fn test_all_coworker_names_have_personalities() {
        let names = &[
            "lexington",
            "park",
            "madison",
            "broadway",
            "amsterdam",
            "columbus",
            "riverside",
            "york",
            "pleasant",
            "vernon",
            "bleecker",
            "houston",
            "canal",
            "spring",
            "prince",
            "mercer",
        ];
        for name in names {
            for personality in &[Personality::Normal, Personality::Fun, Personality::Wild] {
                let result = load_personality(name, *personality);
                assert!(
                    result.is_some(),
                    "Missing personality for {} / {}",
                    name,
                    personality.as_str()
                );
            }
        }
    }

    #[test]
    fn test_lead_system_prompt_works_without_custom_files() {
        // Even without custom files, should return valid prompt with common content
        let prompt = lead_system_prompt();
        assert!(prompt.contains("Lead System Prompt"));
        assert!(
            prompt.contains("GitHub Etiquette"),
            "Lead prompt should include common content"
        );
    }

    #[test]
    fn test_coworker_system_prompt_works_without_custom_files() {
        // Even without custom files, should return valid prompt with common content
        let prompt = coworker_system_prompt("amsterdam");
        assert!(prompt.contains("**amsterdam**"));
        assert!(
            prompt.contains("GitHub Etiquette"),
            "Coworker prompt should include common content"
        );
    }

    #[test]
    fn test_reviewer_prompt_substitutes_pr_number() {
        let prompt = reviewer_prompt(42);
        assert!(prompt.contains("reviewing PR #42"));
        assert!(prompt.contains("/code-review:code-review 42"));
        assert!(prompt.contains("gh pr comment 42 --body"));
        assert!(prompt.contains("PR #42 repeats"));
        assert!(
            !prompt.contains("{pr_number}"),
            "Reviewer prompt should not contain unreplaced {{pr_number}} placeholders"
        );
    }

    #[test]
    fn test_reviewer_resume_prompt_substitutes_pr_number() {
        let prompt = reviewer_resume_prompt(99);
        assert!(prompt.contains("Resume reviewing PR #99"));
        assert!(prompt.contains("gh pr comment 99 --body"));
        assert!(prompt.contains("PR #99 repeats"));
        assert!(
            !prompt.contains("{pr_number}"),
            "Reviewer resume prompt should not contain unreplaced {{pr_number}} placeholders"
        );
    }

    #[test]
    fn test_reviewer_prompt_contains_required_sections() {
        let prompt = reviewer_prompt(1);
        assert!(
            prompt.contains("IMPORTANT"),
            "Reviewer prompt should contain IMPORTANT section"
        );
        assert!(
            prompt.contains("REFACTOR DETECTION"),
            "Reviewer prompt should contain REFACTOR DETECTION section"
        );
        assert!(
            prompt.contains("TASK DESCRIPTION VERIFICATION"),
            "Reviewer prompt should contain TASK DESCRIPTION VERIFICATION section"
        );
    }

    #[test]
    fn test_reviewer_resume_prompt_contains_task_verification() {
        let prompt = reviewer_resume_prompt(1);
        assert!(
            prompt.contains("TASK DESCRIPTION VERIFICATION"),
            "Reviewer resume prompt should contain TASK DESCRIPTION VERIFICATION section"
        );
        assert!(
            prompt.contains("/code-review:code-review"),
            "Reviewer resume prompt should contain explicit code-review skill invocation"
        );
    }

    #[test]
    fn test_reviewer_system_prompt_merges_all_sources() {
        // Bug: Reviewers weren't following instructions in reviewer.md because it was
        // passed as initial_prompt (just a task), not as part of the system prompt.
        //
        // Fix: reviewer_system_prompt() merges common + coworker + reviewer content
        // so reviewer instructions become part of the agent's identity/behavior.
        let prompt = reviewer_system_prompt("lexington");

        // Should contain content from common.md
        assert!(
            prompt.contains("GitHub Etiquette"),
            "Reviewer system prompt should include common.md content"
        );

        // Should contain content from coworker.md
        assert!(
            prompt.contains("Channel Usage"),
            "Reviewer system prompt should include coworker.md content"
        );

        // Should contain reviewer-specific instructions from reviewer.md
        assert!(
            prompt.contains("THRESHOLD OVERRIDE"),
            "Reviewer system prompt should include THRESHOLD OVERRIDE from reviewer.md"
        );
        assert!(
            prompt.contains("CHANNEL MESSAGE DISCIPLINE"),
            "Reviewer system prompt should include CHANNEL MESSAGE DISCIPLINE from reviewer.md"
        );
        assert!(
            prompt.contains("TEST SUGGESTIONS"),
            "Reviewer system prompt should include TEST SUGGESTIONS from reviewer.md"
        );

        // Should have name substituted
        assert!(
            prompt.contains("**lexington**"),
            "Reviewer system prompt should substitute {{name}} with actual name"
        );
        assert!(
            !prompt.contains("{name}"),
            "Reviewer system prompt should not contain unreplaced {{name}}"
        );
    }

    #[test]
    fn test_reviewer_prompts_include_frontmatter_requirement() {
        // Task !1068: The code-review skill doesn't include midtown frontmatter by default.
        // The reviewer prompts must explicitly instruct reviewers to prepend frontmatter.
        let system_prompt = reviewer_system_prompt("park");
        let resume_prompt = reviewer_resume_prompt(42);

        // The system prompt (which merges reviewer.md) should contain the frontmatter requirement
        assert!(
            system_prompt.contains("MIDTOWN FRONTMATTER REQUIREMENT"),
            "Reviewer system prompt should contain MIDTOWN FRONTMATTER REQUIREMENT section"
        );
        // After substitution, {name} becomes "park", so verify the complete frontmatter string
        assert!(
            system_prompt.contains("<!-- midtown: park -->"),
            "Reviewer system prompt should show the frontmatter format with substituted name"
        );
        assert!(
            system_prompt.contains("prepend the frontmatter"),
            "Reviewer system prompt should instruct to prepend frontmatter"
        );

        // Resume prompt should also have the frontmatter requirement
        assert!(
            resume_prompt.contains("MIDTOWN FRONTMATTER REQUIREMENT"),
            "Reviewer resume prompt should contain MIDTOWN FRONTMATTER REQUIREMENT section"
        );
    }

    #[test]
    fn test_clusterer_system_prompt_loads() {
        let prompt = clusterer_system_prompt();
        assert!(
            prompt.contains("AI Channel Clusterer"),
            "Clusterer prompt should contain title"
        );
        assert!(
            prompt.contains("Output Format"),
            "Clusterer prompt should describe output format"
        );
        assert!(
            prompt.contains("create_channels"),
            "Clusterer prompt should describe create_channels field"
        );
        assert!(
            prompt.contains("archive_channels"),
            "Clusterer prompt should describe archive_channels field"
        );
        assert!(
            prompt.contains("merge_channels"),
            "Clusterer prompt should describe merge_channels field"
        );
        assert!(
            prompt.contains("assign_tasks"),
            "Clusterer prompt should describe assign_tasks field"
        );
    }

    #[test]
    fn test_clusterer_prompt_contains_constraints() {
        let prompt = clusterer_system_prompt();
        assert!(
            prompt.contains("Code Locality"),
            "Clusterer prompt should contain Code Locality heuristics"
        );
        assert!(
            prompt.contains("Do NOT reassign in-flight tasks"),
            "Clusterer prompt should warn against reassigning in-progress tasks"
        );
        assert!(
            prompt.contains("kebab-case"),
            "Clusterer prompt should specify kebab-case for channel names"
        );
        assert!(
            prompt.contains("main channel"),
            "Clusterer prompt should explain main channel behavior"
        );
    }

    #[test]
    fn test_channel_lead_system_prompt_substitutes_channel_name() {
        let prompt = channel_lead_system_prompt("web-interface", "No context yet.");
        assert!(
            prompt.contains("#web-interface"),
            "Channel lead prompt should contain the channel name with # prefix"
        );
        assert!(
            prompt.contains("--channel web-interface"),
            "Channel lead prompt should show correct channel flag"
        );
        assert!(
            !prompt.contains("{channel_name}"),
            "Channel lead prompt should not contain unreplaced {{channel_name}} placeholders"
        );
    }

    #[test]
    fn test_channel_lead_system_prompt_substitutes_domain_context() {
        let context = "Active tasks: !42 Add WebSocket reconnect. Recent PRs: #99 merged.";
        let prompt = channel_lead_system_prompt("daemon-core", context);
        assert!(
            prompt.contains(context),
            "Channel lead prompt should inject domain context"
        );
        assert!(
            !prompt.contains("{domain_context}"),
            "Channel lead prompt should not contain unreplaced {{domain_context}} placeholder"
        );
    }

    #[test]
    fn test_channel_lead_system_prompt_contains_required_sections() {
        let prompt = channel_lead_system_prompt("tui", "No context.");
        assert!(
            prompt.contains("Identity & Role"),
            "Channel lead prompt should have Identity & Role section"
        );
        assert!(
            prompt.contains("Escalation Rules"),
            "Channel lead prompt should have Escalation Rules section"
        );
        assert!(
            prompt.contains("Living Documents"),
            "Channel lead prompt should have Living Documents section"
        );
        assert!(
            prompt.contains("read-only"),
            "Channel lead prompt should state read-only constraint"
        );
        assert!(
            prompt.contains("midtown channel post"),
            "Channel lead prompt should describe how to post"
        );
    }

    #[test]
    fn test_channel_lead_system_prompt_contains_escalation_to_lead() {
        let prompt = channel_lead_system_prompt("github-integration", "No context.");
        assert!(
            prompt.contains("@lead"),
            "Channel lead prompt should mention @lead for escalation"
        );
        assert!(
            prompt.contains("#midtown"),
            "Channel lead prompt should reference #midtown for cross-cutting escalations"
        );
    }

    #[test]
    fn test_coworker_prompt_prevents_orphaned_branches() {
        // Task !1213: Prevent coworkers from pushing orphaned branches without PRs
        // Coworkers should check for existing PRs before creating new branches
        let prompt = coworker_system_prompt("park");

        // Should warn to check for existing PRs before pushing
        assert!(
            prompt.contains("Before pushing"),
            "Coworker prompt should contain 'Before pushing' section"
        );
        assert!(
            prompt.contains("gh pr list --search"),
            "Coworker prompt should instruct to check for existing PRs by task number"
        );
        assert!(
            prompt.contains("force-push to the existing PR branch"),
            "Coworker prompt should instruct to force-push to existing PR branch"
        );
        assert!(
            prompt.contains("Never create a new branch or new PR"),
            "Coworker prompt should warn against creating duplicate PRs"
        );

        // Should instruct how to handle merged PRs when responding to feedback
        assert!(
            prompt.contains("First, check if the PR is still open"),
            "Coworker prompt should instruct to check PR state before addressing feedback"
        );
        assert!(
            prompt.contains("gh pr view <number> --json state"),
            "Coworker prompt should show command to check PR state"
        );
        assert!(
            prompt.contains("Delete the orphaned remote branch"),
            "Coworker prompt should instruct to clean up orphaned branches"
        );
        assert!(
            prompt.contains("git push origin --delete"),
            "Coworker prompt should show command to delete remote branches"
        );
    }
}
