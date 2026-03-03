//! Agent system prompt loading.
//!
//! Loads agent prompts from markdown files at runtime. The prompts are looked for in:
//! 1. The git repository root's `agents/` directory (for development)
//! 2. `~/.midtown/agents/` (for user customization)
//! 3. Embedded defaults (compiled into the binary as fallback)
//!
//! Template variables:
//! - `{name}` — the agent's name (coworker name, channel name, or project name for Project Lead)
//! - `{project_name}` — the project name (e.g., "midtown")

use std::path::PathBuf;

/// Embedded default for the shared lead coordination prompt.
const DEFAULT_LEAD_PROMPT: &str = include_str!("../agents/lead.md");

/// Embedded default for the Project Lead overlay prompt.
const DEFAULT_PROJECT_LEAD_PROMPT: &str = include_str!("../agents/project-lead.md");

/// Embedded default for the coworker system prompt template.
const DEFAULT_COWORKER_PROMPT: &str = include_str!("../agents/coworker.md");

/// Embedded default for common prompt content shared by all agents.
const DEFAULT_COMMON_PROMPT: &str = include_str!("../agents/common.md");

/// Embedded default for the reviewer launch prompt template.
const DEFAULT_REVIEWER_PROMPT: &str = include_str!("../agents/reviewer.md");

/// Embedded default for the reviewer resume prompt template.
const DEFAULT_REVIEWER_RESUME_PROMPT: &str = include_str!("../agents/reviewer-resume.md");

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
fn user_agents_dir() -> PathBuf {
    crate::paths::midtown_base_dir().join("agents")
}

fn code_review_invocation_for_platform(
    platform: crate::auth::AuthProvider,
    pr_number: Option<u64>,
) -> String {
    let pr_suffix = pr_number
        .map(|n| format!("#{n}"))
        .unwrap_or_else(|| "<PR_NUMBER>".to_string());

    match platform {
        crate::auth::AuthProvider::Codex => {
            format!("use the code-review skill to review PR {pr_suffix}")
        }
        crate::auth::AuthProvider::Claude | crate::auth::AuthProvider::Zai => {
            format!(
                "run /code-review:code-review {}",
                pr_suffix.trim_start_matches('#')
            )
        }
    }
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
    let path = user_agents_dir().join(filename);
    if let Ok(content) = std::fs::read_to_string(&path) {
        return Some(content);
    }

    None
}

/// Load the common prompt content shared by all agents.
fn common_prompt() -> String {
    load_prompt_file("common.md").unwrap_or_else(|| DEFAULT_COMMON_PROMPT.to_string())
}

/// Load the main Lead agent's system prompt.
///
/// Assembly: project-lead.md + lead.md + common.md
/// For the Project Lead, `{name}` = project_name (e.g., "midtown").
pub fn main_lead_system_prompt(project_name: &str) -> String {
    let project_lead = load_prompt_file("project-lead.md")
        .unwrap_or_else(|| DEFAULT_PROJECT_LEAD_PROMPT.to_string());
    let lead = load_prompt_file("lead.md").unwrap_or_else(|| DEFAULT_LEAD_PROMPT.to_string());
    let common = common_prompt();
    format!("{project_lead}\n\n{lead}\n\n{common}")
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
pub fn reviewer_system_prompt(
    name: &str,
    project_name: &str,
    platform: crate::auth::AuthProvider,
    pr_number: Option<u64>,
) -> String {
    let coworker_template =
        load_prompt_file("coworker.md").unwrap_or_else(|| DEFAULT_COWORKER_PROMPT.to_string());
    let common = common_prompt();
    let reviewer =
        load_prompt_file("reviewer.md").unwrap_or_else(|| DEFAULT_REVIEWER_PROMPT.to_string());
    let invocation = code_review_invocation_for_platform(platform, pr_number);
    format!("{coworker_template}\n{common}\n\n## Reviewer Instructions\n\n{reviewer}")
        .replace("{name}", name)
        .replace("{project_name}", project_name)
        .replace("{code_review_invocation}", &invocation)
}

/// Build the reviewer launch prompt for a given PR number.
///
/// This is just the task description (which PR to review), not the behavioral
/// instructions. The behavioral instructions are in `reviewer_system_prompt()`.
///
/// For respawns (`restart_count > 0`), includes context about the previous attempt
/// and instructs the reviewer to update an existing placeholder comment rather than
/// posting a new one.
pub fn reviewer_launch_prompt(
    pr_number: u64,
    restart_count: u32,
    platform: crate::auth::AuthProvider,
) -> String {
    let invocation = code_review_invocation_for_platform(platform, Some(pr_number));

    if restart_count == 0 {
        format!("Review PR #{pr_number} — {invocation}")
    } else {
        format!(
            "Review PR #{pr_number} — {invocation}\n\n\
             **NOTE (Restart #{restart_count})**: A previous reviewer started this review but \
             did not complete it. Check if there's an existing \"Review in progress\" placeholder \
             comment on PR #{pr_number} and update it with your final review results instead of \
             posting a new comment:\n\
             ```bash\n\
             # Find the existing placeholder comment ID:\n\
             COMMENT_ID=$(gh pr view {pr_number} --json comments --jq \
             '[.comments[] | select(.body | test(\"Review in progress by\")) | \
             select(.body | test(\"midtown:\") | not)] | last | .url' | grep -o '[0-9]*$')\n\
             # Then update it instead of posting new\n\
             ```\n\
             The review worktree (review-pr-{pr_number}) may already have the PR checked out.",
        )
    }
}

/// Build the reviewer launch prompt for a given PR number (legacy function).
///
/// Loads `agents/reviewer.md` (or the embedded default) and replaces
/// `{pr_number}` with the actual PR number.
///
/// Note: This is the old approach where reviewer.md was passed as initial_prompt.
/// New code should use `reviewer_system_prompt()` for the system prompt and
/// `reviewer_launch_prompt()` for the task.
pub fn reviewer_prompt(pr_number: u64, platform: crate::auth::AuthProvider) -> String {
    let template =
        load_prompt_file("reviewer.md").unwrap_or_else(|| DEFAULT_REVIEWER_PROMPT.to_string());
    let invocation = code_review_invocation_for_platform(platform, Some(pr_number));

    template
        .replace("{pr_number}", &pr_number.to_string())
        .replace("{code_review_invocation}", &invocation)
}

/// Build the reviewer resume prompt for a given PR number.
///
/// Used when the daemon discovers a reviewer coworker still running after
/// a restart. Loads `agents/reviewer-resume.md` (or the embedded default)
/// and replaces `{pr_number}` with the actual PR number.
pub fn reviewer_resume_prompt(pr_number: u64, platform: crate::auth::AuthProvider) -> String {
    let template = load_prompt_file("reviewer-resume.md")
        .unwrap_or_else(|| DEFAULT_REVIEWER_RESUME_PROMPT.to_string());
    let invocation = code_review_invocation_for_platform(platform, Some(pr_number));

    template
        .replace("{pr_number}", &pr_number.to_string())
        .replace("{code_review_invocation}", &invocation)
}

/// Build the initial prompt for the Project Lead session.
///
/// Follows a standardized Role/Channel/Mission/First Actions structure
/// so all initial prompts are consistent and informative.
pub fn main_lead_initial_prompt(project_name: &str, main_channel: &str) -> String {
    format!(
        "## Role\nProject Lead for {project_name}\n\n\
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

/// Standard footer for task-related prompts and nudges.
///
/// Appended to every task prompt/nudge so agents know how to view the task
/// and how to reply in the correct thread.
pub fn task_footer(task_id: &str) -> String {
    format!(
        "Run `midtown task view {task_id}` for full details.\n\
         Reply with: `midtown channel post \"...\" --task {task_id}`"
    )
}

/// Build the initial prompt for a fresh coworker task assignment.
///
/// Used when a coworker is spawned fresh to work on a task.
/// The `plan_section` parameter is a pre-built string from `build_plan_prompt_section()`
/// that may contain plan context and execution skill instructions (or be empty).
pub fn coworker_task_prompt(task_id: &str, subject: &str, plan_section: &str) -> String {
    let footer = task_footer(task_id);
    format!(
        "You've been assigned task !{task_id}: {subject}. Get started!{plan_section}\n\n\
         {footer}"
    )
}

/// Build the initial prompt for a coworker claiming a task while already running.
///
/// Used when a running coworker is nudged to pick up a new task (e.g., grouped
/// tasks from the same PR or blockedBy chain).
pub fn coworker_claim_prompt(task_id: &str, subject: &str, plan_section: &str) -> String {
    let footer = task_footer(task_id);
    format!(
        "You've been assigned task !{task_id}: {subject}. \
         Run `midtown task claim {task_id}` to claim it, then get started!{plan_section}\n\n\
         {footer}"
    )
}

/// Build the initial prompt for recovering a coworker whose session was interrupted.
///
/// Used when a coworker's previous session died and needs to be resumed or
/// respawned. The worktree and branch from the previous run are intact.
pub fn coworker_recovery_prompt(task_id: &str, subject: &str, plan_section: &str) -> String {
    let footer = task_footer(task_id);
    format!(
        "You've been assigned task !{task_id}: {subject}. \
         Your previous session was interrupted but your worktree and branch are still intact. \
         Check your git status and get started!{plan_section}\n\n\
         {footer}"
    )
}

/// Build a nudge prompt for a coworker with a pending task.
///
/// Used when a coworker is idle and has a pending task to work on.
/// Unlike other prompts, this is a brief reminder rather than a full assignment.
pub fn coworker_nudge_prompt(task_id: &str, subject: &str) -> String {
    let footer = task_footer(task_id);
    format!(
        "You have pending task !{task_id}: {subject}. Get started!\n\n\
         {footer}"
    )
}

/// Load the channel lead system prompt with channel name and domain context substitution.
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

#[path = "agents_tests.rs"]
#[cfg(test)]
mod tests;
