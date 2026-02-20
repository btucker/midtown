//! Agent system prompt loading.
//!
//! Loads agent prompts from markdown files at runtime. The prompts are looked for in:
//! 1. The git repository root's `agents/` directory (for development)
//! 2. `~/.midtown/agents/` (for user customization)
//! 3. Embedded defaults (compiled into the binary as fallback)
//!
//! Template variables:
//! - `{name}` — the agent's name (coworker name, channel name, or project name for main lead)
//! - `{project_name}` — the project name (e.g., "midtown")

use std::path::PathBuf;

/// Embedded default for the shared lead coordination prompt.
const DEFAULT_LEAD_PROMPT: &str = include_str!("../agents/lead.md");

/// Embedded default for the main lead overlay prompt.
const DEFAULT_MAIN_LEAD_PROMPT: &str = include_str!("../agents/main-lead.md");

/// Embedded default for the coworker system prompt template.
const DEFAULT_COWORKER_PROMPT: &str = include_str!("../agents/coworker.md");

/// Embedded default for common prompt content shared by all agents.
const DEFAULT_COMMON_PROMPT: &str = include_str!("../agents/common.md");

/// Embedded default for the reviewer launch prompt template.
const DEFAULT_REVIEWER_PROMPT: &str = include_str!("../agents/reviewer.md");

/// Embedded default for the reviewer resume prompt template.
const DEFAULT_REVIEWER_RESUME_PROMPT: &str = include_str!("../agents/reviewer-resume.md");

/// Embedded default for the clusterer system prompt.
const DEFAULT_CLUSTERER_PROMPT: &str = include_str!("../agents/clusterer.md");

/// Embedded default for the channel lead system prompt template.
const DEFAULT_CHANNEL_LEAD_PROMPT: &str = include_str!("../agents/channel-lead.md");

/// Embedded default for the ops channel lead additional instructions.
///
/// Appended to the generic channel lead prompt when the channel is "ops".
const DEFAULT_OPS_CHANNEL_LEAD_PROMPT: &str = include_str!("../agents/ops-channel-lead.md");

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

/// Load the common prompt content shared by all agents.
fn common_prompt() -> String {
    load_prompt_file("common.md").unwrap_or_else(|| DEFAULT_COMMON_PROMPT.to_string())
}

/// Load the main Lead agent's system prompt.
///
/// Assembly: main-lead.md + lead.md + common.md
/// For the main lead, `{name}` = project_name (e.g., "midtown").
pub fn main_lead_system_prompt(project_name: &str) -> String {
    let main_lead =
        load_prompt_file("main-lead.md").unwrap_or_else(|| DEFAULT_MAIN_LEAD_PROMPT.to_string());
    let lead = load_prompt_file("lead.md").unwrap_or_else(|| DEFAULT_LEAD_PROMPT.to_string());
    let common = common_prompt();
    format!("{main_lead}\n\n{lead}\n\n{common}")
        .replace("{name}", project_name)
        .replace("{project_name}", project_name)
}

/// Load the coworker agent's system prompt with name and project substitution.
///
/// Assembly: coworker.md + common.md
pub fn coworker_system_prompt(name: &str, project_name: &str) -> String {
    let template =
        load_prompt_file("coworker.md").unwrap_or_else(|| DEFAULT_COWORKER_PROMPT.to_string());
    let common = common_prompt();
    format!("{template}\n{common}")
        .replace("{name}", name)
        .replace("{project_name}", project_name)
}

/// Load the reviewer agent's system prompt with name and project substitution.
///
/// Assembly: coworker.md + common.md + reviewer.md
pub fn reviewer_system_prompt(name: &str, project_name: &str) -> String {
    let coworker_template =
        load_prompt_file("coworker.md").unwrap_or_else(|| DEFAULT_COWORKER_PROMPT.to_string());
    let common = common_prompt();
    let reviewer =
        load_prompt_file("reviewer.md").unwrap_or_else(|| DEFAULT_REVIEWER_PROMPT.to_string());
    format!("{coworker_template}\n{common}\n\n## Reviewer Instructions\n\n{reviewer}")
        .replace("{name}", name)
        .replace("{project_name}", project_name)
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

/// Build the initial prompt for the main lead session.
///
/// Follows a standardized Role/Channel/Mission/First Actions structure
/// so all initial prompts are consistent and informative.
pub fn main_lead_initial_prompt(project_name: &str, main_channel: &str) -> String {
    format!(
        "## Role\nMain lead for {project_name}\n\n\
         ## Channel\n#{main_channel}\n\n\
         ## Mission\nCoordinate the team, triage incoming work, delegate to coworkers.\n\n\
         ## First Actions\n\
         1. Read the channel for context\n\
         2. Post to the channel that you're online and ready"
    )
}

/// Build the initial prompt for a channel lead session.
pub fn channel_lead_initial_prompt(channel_name: &str) -> String {
    format!(
        "## Role\nChannel lead for #{channel_name}\n\n\
         ## Channel\n#{channel_name}\n\n\
         ## Mission\nDomain expert for this channel. Track active work, brainstorm, surface issues proactively.\n\n\
         ## First Actions\n\
         1. Read recent messages in #{channel_name} for context\n\
         2. Introduce yourself as the domain expert for this channel"
    )
}

/// Build the initial prompt for a fresh coworker task assignment.
///
/// Used when a coworker is spawned fresh to work on a task.
/// The `plan_section` parameter is a pre-built string from `build_plan_prompt_section()`
/// that may contain plan context and execution skill instructions (or be empty).
pub fn coworker_task_prompt(task_id: &str, subject: &str, plan_section: &str) -> String {
    format!(
        "You've been assigned task !{task_id}: {subject}. Get started!{plan_section}\n\n\
         Run `midtown task view {task_id}` for full details."
    )
}

/// Build the initial prompt for a coworker claiming a task while already running.
///
/// Used when a running coworker is nudged to pick up a new task (e.g., grouped
/// tasks from the same PR or blockedBy chain).
pub fn coworker_claim_prompt(task_id: &str, subject: &str, plan_section: &str) -> String {
    format!(
        "You've been assigned task !{task_id}: {subject}. \
         Run `midtown task claim {task_id}` to claim it, then get started!{plan_section}\n\n\
         Run `midtown task view {task_id}` for full details."
    )
}

/// Build the initial prompt for recovering a coworker whose session was interrupted.
///
/// Used when a coworker's previous session died and needs to be resumed or
/// respawned. The worktree and branch from the previous run are intact.
pub fn coworker_recovery_prompt(task_id: &str, subject: &str, plan_section: &str) -> String {
    format!(
        "You've been assigned task !{task_id}: {subject}. \
         Your previous session was interrupted but your worktree and branch are still intact. \
         Check your git status and get started!{plan_section}\n\n\
         Run `midtown task view {task_id}` for full details."
    )
}

/// Build a nudge prompt for a coworker with a pending task.
///
/// Used when a coworker is idle and has a pending task to work on.
/// Unlike other prompts, this is a brief reminder rather than a full assignment.
pub fn coworker_nudge_prompt(task_id: &str, subject: &str) -> String {
    format!(
        "You have pending task !{task_id}: {subject}. Get started!\n\n\
         Run `midtown task view {task_id}` for full details."
    )
}

/// Load the clusterer system prompt.
///
/// Returns the prompt from `agents/clusterer.md` if found, otherwise returns
/// the embedded default. This prompt guides the AI clusterer in organizing tasks
/// into topic channels based on code locality and thematic grouping.
pub fn clusterer_system_prompt() -> String {
    load_prompt_file("clusterer.md").unwrap_or_else(|| DEFAULT_CLUSTERER_PROMPT.to_string())
}

/// Load the channel lead system prompt with channel name, domain context, and project name substitution.
///
/// Assembly: channel-lead.md + lead.md + common.md (+ ops-channel-lead.md for "ops" channel)
/// For channel leads, `{name}` = channel_name.
pub fn channel_lead_system_prompt(
    channel_name: &str,
    domain_context: &str,
    project_name: &str,
) -> String {
    let template = load_prompt_file("channel-lead.md")
        .unwrap_or_else(|| DEFAULT_CHANNEL_LEAD_PROMPT.to_string());
    let lead = load_prompt_file("lead.md").unwrap_or_else(|| DEFAULT_LEAD_PROMPT.to_string());
    let common = common_prompt();

    let mut prompt = format!("{template}\n\n{lead}\n\n{common}");

    // Append ops-specific instructions for the ops channel
    if channel_name == "ops" {
        let ops_extra = load_prompt_file("ops-channel-lead.md")
            .unwrap_or_else(|| DEFAULT_OPS_CHANNEL_LEAD_PROMPT.to_string());
        prompt = format!("{prompt}\n\n{ops_extra}");
    }

    prompt
        .replace("{channel_name}", channel_name)
        .replace("{domain_context}", domain_context)
        .replace("{project_name}", project_name)
        .replace("{name}", channel_name) // channel lead's {name} = channel name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_main_lead_system_prompt_loads() {
        let prompt = main_lead_system_prompt("midtown");
        assert!(prompt.contains("Lead Coordination"));
        assert!(prompt.contains("midtown"));
    }

    #[test]
    fn test_coworker_system_prompt_substitutes_name() {
        let prompt = coworker_system_prompt("lexington", "midtown");
        assert!(prompt.contains("**lexington**"));
        assert!(prompt.contains("git checkout -b lexington/"));
        assert!(!prompt.contains("{name}"));
    }

    #[test]
    fn test_coworker_system_prompt_contains_required_sections() {
        let prompt = coworker_system_prompt("park", "midtown");
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
        let prompt = main_lead_system_prompt("midtown");
        assert!(prompt.contains("midtown status"));
        assert!(prompt.contains("midtown coworker call-in"));
        assert!(prompt.contains("midtown channel"));
    }

    #[test]
    fn test_lead_prompt_contains_delegation_section() {
        let prompt = main_lead_system_prompt("midtown");
        assert!(prompt.contains("Delegation Mindset"));
        assert!(prompt.contains("coordinator"));
    }

    #[test]
    fn test_common_prompt_included_in_lead() {
        let prompt = main_lead_system_prompt("midtown");
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
        let prompt = coworker_system_prompt("park", "midtown");
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
        let prompt = main_lead_system_prompt("midtown");
        assert!(
            prompt.contains("<!-- midtown: midtown -->"),
            "Lead prompt should have {{name}} replaced with project_name in common content"
        );
        assert!(
            !prompt.contains("{name}"),
            "Lead prompt should not contain unreplaced {{name}} placeholders"
        );
    }

    #[test]
    fn test_common_prompt_name_substitution_in_coworker() {
        let prompt = coworker_system_prompt("broadway", "midtown");
        assert!(
            prompt.contains("<!-- midtown: broadway -->"),
            "Coworker prompt should have {{name}} replaced in common content"
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
        let prompt = reviewer_system_prompt("lexington", "midtown");

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
        let system_prompt = reviewer_system_prompt("park", "midtown");
        let resume_prompt = reviewer_resume_prompt(42);

        assert!(
            system_prompt.contains("MIDTOWN FRONTMATTER REQUIREMENT"),
            "Reviewer system prompt should contain MIDTOWN FRONTMATTER REQUIREMENT section"
        );
        assert!(
            system_prompt.contains("<!-- midtown: park -->"),
            "Reviewer system prompt should show the frontmatter format with substituted name"
        );
        assert!(
            system_prompt.contains("prepend the frontmatter"),
            "Reviewer system prompt should instruct to prepend frontmatter"
        );

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
        let prompt = channel_lead_system_prompt("web-interface", "No context yet.", "midtown");
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
        let prompt = channel_lead_system_prompt("daemon-core", context, "midtown");
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
        let prompt = channel_lead_system_prompt("tui", "No context.", "midtown");
        assert!(
            prompt.contains("Identity"),
            "Channel lead prompt should have Identity section"
        );
        assert!(
            prompt.contains("Escalation"),
            "Channel lead prompt should have Escalation section"
        );
        assert!(
            prompt.contains("Living Documents"),
            "Channel lead prompt should have Living Documents section"
        );
        assert!(
            prompt.contains("midtown channel post"),
            "Channel lead prompt should describe how to post"
        );
    }

    #[test]
    fn test_channel_lead_system_prompt_contains_escalation_to_lead() {
        let prompt = channel_lead_system_prompt("github-integration", "No context.", "midtown");
        assert!(
            prompt.contains("@midtown"),
            "Channel lead prompt should mention @midtown for escalation"
        );
    }

    #[test]
    fn test_coworker_prompt_prevents_orphaned_branches() {
        let prompt = coworker_system_prompt("park", "midtown");

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

    #[test]
    fn test_main_lead_initial_prompt_structure() {
        let prompt = main_lead_initial_prompt("midtown", "main");
        assert!(prompt.contains("## Role"));
        assert!(prompt.contains("Main lead for midtown"));
        assert!(prompt.contains("## Channel"));
        assert!(prompt.contains("#main"));
        assert!(prompt.contains("## Mission"));
        assert!(prompt.contains("## First Actions"));
    }

    #[test]
    fn test_channel_lead_initial_prompt_structure() {
        let prompt = channel_lead_initial_prompt("web-interface");
        assert!(prompt.contains("## Role"));
        assert!(prompt.contains("Channel lead for #web-interface"));
        assert!(prompt.contains("## Channel"));
        assert!(prompt.contains("#web-interface"));
        assert!(prompt.contains("## Mission"));
        assert!(prompt.contains("## First Actions"));
    }

    #[test]
    fn test_coworker_task_prompt_contains_id_and_subject() {
        let prompt = coworker_task_prompt("42", "Fix login bug", "");
        assert!(prompt.contains("task !42"));
        assert!(prompt.contains("Fix login bug"));
        assert!(prompt.contains("Get started!"));
        assert!(prompt.contains("midtown task view 42"));
    }

    #[test]
    fn test_coworker_task_prompt_includes_plan_section() {
        let plan = "\n\n## Plan Context\nSome plan details here.";
        let prompt = coworker_task_prompt("42", "Fix login bug", plan);
        assert!(prompt.contains("## Plan Context"));
        assert!(prompt.contains("Some plan details here."));
    }

    #[test]
    fn test_coworker_claim_prompt_includes_claim_command() {
        let prompt = coworker_claim_prompt("42", "Fix login bug", "");
        assert!(prompt.contains("midtown task claim 42"));
        assert!(prompt.contains("task !42"));
    }

    #[test]
    fn test_coworker_recovery_prompt_mentions_intact_worktree() {
        let prompt = coworker_recovery_prompt("42", "Fix login bug", "");
        assert!(prompt.contains("worktree and branch are still intact"));
        assert!(prompt.contains("git status"));
        assert!(prompt.contains("task !42"));
    }

    #[test]
    fn test_coworker_nudge_prompt_is_brief() {
        let prompt = coworker_nudge_prompt("42", "Fix login bug");
        assert!(prompt.contains("pending task !42"));
        assert!(prompt.contains("Fix login bug"));
        assert!(prompt.contains("Get started!"));
    }
}
