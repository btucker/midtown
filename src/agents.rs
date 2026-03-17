//! Agent system prompt loading — three-layer architecture.
//!
//! Prompts are assembled from three distinct layers:
//!
//! 1. **Agent definition (Layer 1)** — Role identity and behavioral instructions.
//!    Loaded from `.claude/agents/midtown-*.md` (Claude Code agent format with YAML
//!    frontmatter). Search order: project-level `.claude/agents/`, user-level
//!    `~/.claude/agents/`, then compiled-in fallback. For Claude Code sessions,
//!    Layer 1 is delivered via `--agent <name>`; for Codex, bundled into `--system-prompt`.
//!
//! 2. **Shared prompt (Layer 2)** — Operational rules shared across roles. `common.md` for
//!    all agents, plus `lead-common.md` for leads. Always uses compiled-in content.
//!
//! 3. **Runtime context (Layer 3)** — Template variable replacement (`{name}`, `{project_name}`,
//!    `{channel_lead}`, `{escalation_target}`, `{channel_name}`, `{domain_context}`,
//!    `{code_review_invocation}`) and runtime-only content injection (ops extras, AGENTS.md).
//!
//! The public functions (`coworker_system_prompt`, `main_lead_system_prompt`, etc.)
//! use `load_agent_definition_for_role()` for Layer 1, `shared_prompt_for_role()` for
//! Layer 2, and `build_runtime_context()` for Layer 3.

// ── Layer 2 compiled-in content ─────────────────────────────────

/// Embedded default for the shared lead coordination prompt.
const DEFAULT_LEAD_PROMPT: &str = include_str!("../agents/lead-common.md");

/// Embedded default for common prompt content shared by all agents.
const DEFAULT_COMMON_PROMPT: &str = include_str!("../agents/common.md");

// ── Layer 3 compiled-in content ─────────────────────────────────

/// Embedded default for the reviewer resume prompt template.
const DEFAULT_REVIEWER_RESUME_PROMPT: &str = include_str!("../agents/reviewer-resume.md");

/// Embedded default for the ops channel lead additional instructions.
///
/// Appended to the generic channel lead prompt when the channel is "ops".
const DEFAULT_OPS_CHANNEL_LEAD_PROMPT: &str = include_str!("../agents/ops-channel-lead.md");

// ── Layer 1 agent definition files (Claude Code agent format with YAML frontmatter) ──

/// Code author agent definition — role identity for coworkers that implement features.
const AGENT_DEF_CODE_AUTHOR: &str = include_str!("../agents/definitions/midtown-code-author.md");

/// Code reviewer agent definition — role identity for PR reviewers.
const AGENT_DEF_CODE_REVIEWER: &str =
    include_str!("../agents/definitions/midtown-code-reviewer.md");

/// Project lead agent definition — role identity for the human-facing lead.
const AGENT_DEF_PROJECT_LEAD: &str = include_str!("../agents/definitions/midtown-project-lead.md");

/// Channel lead agent definition — role identity for domain-specific channel leads.
const AGENT_DEF_CHANNEL_LEAD: &str = include_str!("../agents/definitions/midtown-channel-lead.md");

// ── Three-layer architecture ─────────────────────────────────────

/// Agent role classification for the three-layer prompt architecture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentRole {
    /// Code author — implements features, fixes bugs, opens PRs.
    Coworker,
    /// Code reviewer — reviews PRs for correctness and quality.
    Reviewer,
    /// Project lead — human-facing coordinator.
    ProjectLead,
    /// Channel lead — domain expert for a topic channel.
    ChannelLead,
}

/// Parameters for building runtime context (Layer 3).
///
/// All template variable replacement and runtime content injection
/// is driven by this struct.
#[derive(Default)]
pub struct RuntimeContext<'a> {
    /// The agent's display name (coworker name, channel name, or project name for leads).
    pub name: &'a str,
    /// The project name (e.g., "midtown").
    pub project_name: &'a str,
    /// Channel lead name to @mention for domain questions. Falls back to `project_name`.
    pub channel_lead: Option<&'a str>,
    /// Channel name for channel leads (e.g., "web-interface").
    pub channel_name: Option<&'a str>,
    /// Domain context injected at startup from channel notes.
    pub domain_context: Option<&'a str>,
    /// Who to @mention for review notes (reviewer only).
    pub escalation_target: Option<&'a str>,
    /// Optional AGENTS.md content for workflow facilitation.
    pub agents_md: Option<&'a str>,
    /// Platform for code review invocation formatting.
    pub platform: Option<crate::auth::AuthProvider>,
    /// PR number for code review invocation formatting.
    pub pr_number: Option<u64>,
}

/// Layer 1: Load the agent definition for a role.
///
/// Search order (via the `agent_definition` module):
/// 1. `.claude/agents/midtown-*.md` (project-level, CWD-relative)
/// 2. `~/.claude/agents/midtown-*.md` (user-level)
/// 3. Compiled-in fallback from `.claude/agents/midtown-*.md`
///
/// Returns the system prompt body (stripped of YAML frontmatter).
pub fn load_agent_definition_for_role(role: AgentRole) -> String {
    let (def_name, fallback) = match role {
        AgentRole::Coworker => ("midtown-code-author", AGENT_DEF_CODE_AUTHOR),
        AgentRole::Reviewer => ("midtown-code-reviewer", AGENT_DEF_CODE_REVIEWER),
        AgentRole::ProjectLead => ("midtown-project-lead", AGENT_DEF_PROJECT_LEAD),
        AgentRole::ChannelLead => ("midtown-channel-lead", AGENT_DEF_CHANNEL_LEAD),
    };

    // Try loading from filesystem first
    if let Ok(def) = crate::agent_definition::load_agent_definition(def_name) {
        return def.system_prompt;
    }

    // Fall back to compiled-in content, using agent_definition parser to strip frontmatter
    let dummy_path = std::path::Path::new("compiled-in");
    match crate::agent_definition::parse_agent_content(fallback, dummy_path) {
        Ok(def) => def.system_prompt,
        Err(_) => fallback.to_string(),
    }
}

/// Layer 2: Get the shared prompt content for a role.
///
/// - Coworker/Reviewer: `common.md` only
/// - ProjectLead/ChannelLead: `lead-common.md` + `common.md`
pub fn shared_prompt_for_role(role: AgentRole) -> String {
    let common = DEFAULT_COMMON_PROMPT.to_string();
    match role {
        AgentRole::Coworker | AgentRole::Reviewer => common,
        AgentRole::ProjectLead | AgentRole::ChannelLead => {
            let lead = DEFAULT_LEAD_PROMPT;
            format!("{lead}\n\n{common}")
        }
    }
}

/// Layer 3: Apply runtime context to an assembled prompt.
///
/// 1. Appends ops-specific instructions (if `channel_name == "ops"`)
/// 2. Performs template variable replacement (`{name}`, `{project_name}`, etc.)
///    on the combined prompt — including ops extras, so `{name}` IS replaced there
/// 3. Appends AGENTS.md content AFTER substitution — so literal `{name}`
///    in AGENTS.md is preserved verbatim
pub fn build_runtime_context(base_prompt: &str, ctx: &RuntimeContext) -> String {
    let channel_lead = ctx.channel_lead.unwrap_or(ctx.project_name);

    // Build the ops extra content if applicable
    let mut prompt = base_prompt.to_string();
    if ctx.channel_name == Some("ops") {
        prompt = format!("{prompt}\n\n{DEFAULT_OPS_CHANNEL_LEAD_PROMPT}");
    }

    // Apply template substitutions
    prompt = prompt
        .replace("{name}", ctx.name)
        .replace("{project_name}", ctx.project_name)
        .replace("{channel_lead}", channel_lead);

    // Channel-specific substitutions
    if let Some(channel_name) = ctx.channel_name {
        prompt = prompt.replace("{channel_name}", channel_name);
    }
    if let Some(domain_context) = ctx.domain_context {
        if prompt.contains("{domain_context}") {
            prompt = prompt.replace("{domain_context}", domain_context);
        } else if !domain_context.is_empty() {
            // Agent definition doesn't have the placeholder — append domain context
            prompt = format!("{prompt}\n\n## Domain Context\n\n{domain_context}");
        }
    }
    if let Some(escalation_target) = ctx.escalation_target {
        prompt = prompt.replace("{escalation_target}", escalation_target);
    }

    // Platform-specific code review invocation
    if let Some(platform) = ctx.platform {
        let invocation = code_review_invocation_for_platform(platform, ctx.pr_number);
        prompt = prompt.replace("{code_review_invocation}", &invocation);
    }

    // Append AGENTS.md AFTER template substitution to preserve literal placeholders
    if let Some(agents) = ctx.agents_md {
        let agents = agents.trim();
        if !agents.is_empty() {
            prompt = format!("{prompt}\n\n## Workflow Facilitation\n\n{agents}");
        }
    }

    prompt
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

/// Load the main Lead agent's system prompt.
///
/// Three-layer assembly:
/// - Layer 1: Agent definition from `midtown-project-lead.md`
/// - Layer 2: lead-common.md + common.md
/// - Layer 3: Runtime context with project_name substitutions
///
/// For the Project Lead, `{name}` = project_name (e.g., "midtown").
pub fn main_lead_system_prompt(project_name: &str) -> String {
    // Layer 1: Agent definition
    let layer1 = load_agent_definition_for_role(AgentRole::ProjectLead);
    // Layer 2: Shared prompt
    let layer2 = shared_prompt_for_role(AgentRole::ProjectLead);
    let prompt = format!("{layer1}\n\n{layer2}");
    // Layer 3: Runtime context
    build_runtime_context(
        &prompt,
        &RuntimeContext {
            name: project_name,
            project_name,
            channel_lead: Some(project_name),
            ..RuntimeContext::default()
        },
    )
}

/// Load the coworker agent's system prompt with name and project substitution.
///
/// Three-layer assembly:
/// - Layer 1: Agent definition from `midtown-code-author.md`
/// - Layer 2: common.md
/// - Layer 3: Runtime context with name, project_name, channel_lead substitutions
///
/// `channel_lead` is the name of the channel lead to @mention for domain questions.
/// Falls back to `project_name` when `None` (e.g., when no topic channel is assigned).
pub fn coworker_system_prompt(
    name: &str,
    project_name: &str,
    channel_lead: Option<&str>,
) -> String {
    // Layer 1: Agent definition
    let layer1 = load_agent_definition_for_role(AgentRole::Coworker);
    // Layer 2: Shared prompt
    let layer2 = shared_prompt_for_role(AgentRole::Coworker);
    let prompt = format!("{layer1}\n\n{layer2}");
    // Layer 3: Runtime context
    build_runtime_context(
        &prompt,
        &RuntimeContext {
            name,
            project_name,
            channel_lead,
            ..RuntimeContext::default()
        },
    )
}

/// Load the reviewer agent's system prompt with name and project substitution.
///
/// Three-layer assembly:
/// - Layer 1: Agent definition from `midtown-code-reviewer.md`
/// - Layer 2: common.md
/// - Layer 3: Runtime context with name, escalation_target, platform substitutions
pub fn reviewer_system_prompt(
    name: &str,
    project_name: &str,
    escalation_target: &str,
    platform: crate::auth::AuthProvider,
    pr_number: Option<u64>,
) -> String {
    // Layer 1: Agent definition (includes all reviewer-specific instructions)
    let layer1 = load_agent_definition_for_role(AgentRole::Reviewer);
    // Layer 2: Shared prompt (common.md — operational rules)
    let layer2 = shared_prompt_for_role(AgentRole::Reviewer);
    let prompt = format!("{layer1}\n\n{layer2}");
    // Layer 3: Runtime context
    build_runtime_context(
        &prompt,
        &RuntimeContext {
            name,
            project_name,
            channel_lead: Some(escalation_target),
            escalation_target: Some(escalation_target),
            platform: Some(platform),
            pr_number,
            ..RuntimeContext::default()
        },
    )
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
    escalation_target: Option<&str>,
) -> String {
    let invocation = code_review_invocation_for_platform(platform, Some(pr_number));

    let escalation_line = escalation_target
        .map(|target| format!("\n\nAddress review notes to @{target} in the channel."))
        .unwrap_or_default();

    if restart_count == 0 {
        format!("Review PR #{pr_number} — {invocation}{escalation_line}")
    } else {
        format!(
            "Review PR #{pr_number} — {invocation}\n\n\
             **NOTE (Restart #{restart_count})**: A previous reviewer started this review but \
             did not complete it. Check if there's an existing placeholder \
             comment on PR #{pr_number} (identified by `<!-- midtown task:... type:review-placeholder -->` \
             frontmatter) and update it with your final review results instead of \
             posting a new comment:\n\
             ```bash\n\
             # Find the existing placeholder comment ID:\n\
             COMMENT_ID=$(gh pr view {pr_number} --json comments --jq \
             '[.comments[] | select(.body | test(\"type:review-placeholder\"))] | last | .url' | grep -o '[0-9]*$')\n\
             # Then update it instead of posting new\n\
             ```\n\
             The review worktree (review-pr-{pr_number}) may already have the PR checked out.\
             {escalation_line}",
        )
    }
}

/// Build the reviewer resume prompt for a given PR number.
///
/// Used when the daemon discovers a reviewer coworker still running after
/// a restart. Replaces `{pr_number}` with the actual PR number.
pub fn reviewer_resume_prompt(pr_number: u64, platform: crate::auth::AuthProvider) -> String {
    let invocation = code_review_invocation_for_platform(platform, Some(pr_number));

    DEFAULT_REVIEWER_RESUME_PROMPT
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
         2. Wait for messages — do not post a greeting or startup announcement"
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
         2. Wait for messages — do not post a greeting or startup announcement"
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

/// Build the initial prompt for a coworker task assignment.
///
/// Used when a coworker is spawned to work on a task, either fresh or resuming.
/// When `is_resume` is true, includes context about the previous session being
/// interrupted and the worktree/branch being intact.
/// The `plan_section` parameter is a pre-built string from `build_plan_prompt_section()`
/// that may contain plan context and execution skill instructions (or be empty).
pub fn coworker_task_prompt(
    task_id: &str,
    subject: &str,
    plan_section: &str,
    is_resume: bool,
) -> String {
    let footer = task_footer(task_id);
    let resume_context = if is_resume {
        " Your previous session was interrupted but your worktree and branch are still intact. \
         Check your git status and get started!"
    } else {
        " Get started!"
    };
    format!(
        "You've been assigned task !{task_id}: {subject}.{resume_context}{plan_section}\n\n\
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
/// Three-layer assembly:
/// - Layer 1: Agent definition from `midtown-channel-lead.md`
/// - Layer 2: lead-common.md + common.md
/// - Layer 3: Runtime context with channel_name, domain_context, ops extras, AGENTS.md
///
/// For channel leads, `{name}` = channel_name.
///
/// `agents_md` is optional workflow facilitation content from the project's `AGENTS.md`.
pub fn channel_lead_system_prompt(
    channel_name: &str,
    domain_context: &str,
    project_name: &str,
    agents_md: Option<&str>,
) -> String {
    // Layer 1: Agent definition
    let layer1 = load_agent_definition_for_role(AgentRole::ChannelLead);
    // Layer 2: Shared prompt
    let layer2 = shared_prompt_for_role(AgentRole::ChannelLead);
    let prompt = format!("{layer1}\n\n{layer2}");
    // Layer 3: Runtime context (handles ops extras, template vars, AGENTS.md)
    build_runtime_context(
        &prompt,
        &RuntimeContext {
            name: channel_name,
            project_name,
            channel_lead: Some(channel_name), // channel lead IS the lead
            channel_name: Some(channel_name),
            domain_context: Some(domain_context),
            agents_md,
            ..RuntimeContext::default()
        },
    )
}

#[path = "agents_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "agents_definition_tests.rs"]
#[cfg(test)]
mod definition_tests;

#[path = "agents_three_layer_tests.rs"]
#[cfg(test)]
mod three_layer_tests;
