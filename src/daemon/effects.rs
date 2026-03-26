use std::collections::HashSet;
use std::path::PathBuf;

use tracing::{debug, info, warn};

use super::DaemonState;
use super::constants::OPS_CHANNEL;
use super::trackers::PrIssueType;
use crate::message::Message;

/// Maximum tool activity entries per agent before oldest are evicted.
const MAX_TOOL_ITEMS_PER_AGENT: usize = 20;

/// Maximum length (in bytes) for a semantic header string before truncation.
const MAX_SEMANTIC_HEADER_BYTES: usize = 120;

/// Generate a human-readable header for a tool call (e.g. "$ git status", "read foo.rs").
fn semantic_header(name: &str, input: &serde_json::Value) -> String {
    let raw = match name {
        "Bash" => {
            let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            format!("$ {command}")
        }
        "Edit" | "Write" | "Read" => {
            let path = first_path_field(input);
            let verb = name.to_lowercase();
            format!("{verb} {path}")
        }
        "Glob" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            format!("glob {pattern}")
        }
        "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            format!("grep /{pattern}/")
        }
        "Task" | "Agent" => {
            let desc = input
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("task: {desc}")
        }
        "NotebookEdit" => {
            let path = first_path_field(input);
            format!("notebook edit {path}")
        }
        "WebFetch" => {
            let url_str = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
            format!("fetch {url_str}")
        }
        "WebSearch" => {
            let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");
            format!("search \"{query}\"")
        }
        "TodoWrite" => "todo: update".to_string(),
        "MultiEdit" => {
            let path = first_path_field(input);
            format!("multi-edit {path}")
        }
        _ => name.to_lowercase(),
    };

    if raw.len() > MAX_SEMANTIC_HEADER_BYTES {
        let boundary = raw.floor_char_boundary(MAX_SEMANTIC_HEADER_BYTES);
        format!("{}\u{2026}", &raw[..boundary])
    } else {
        raw
    }
}

/// Return the first path-like field found in the input object.
fn first_path_field(input: &serde_json::Value) -> &str {
    for key in &["file_path", "notebook_path", "path"] {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            return v;
        }
    }
    ""
}

async fn load_channel_lead_context(
    base_dir: PathBuf,
    channel_name: &str,
    project_root: PathBuf,
    dir_key: &str,
    workflow_name: Option<String>,
    workflows_dir: PathBuf,
    workflow_state_summary: Option<String>,
) -> (String, Option<String>) {
    let channel = channel_name.to_string();
    let channel_for_warn = channel.clone();
    let dk = dir_key.to_string();
    tokio::task::spawn_blocking(move || {
        let notes = crate::channel::load_channel_notes(&base_dir, &channel);
        let agents = crate::paths::agents_md_for_channel(&channel, &project_root, &dk);

        // Merge workflow AGENTS.md and state summary into agents_md
        let workflow_agents = workflow_name
            .as_deref()
            .and_then(|name| crate::paths::workflow_agents_md_content(&workflows_dir, name));
        let merged_agents = crate::paths::merge_workflow_agents_md(
            agents,
            workflow_agents.as_deref(),
            workflow_state_summary.as_deref(),
        );

        (notes, merged_agents)
    })
    .await
    .unwrap_or_else(|e| {
        warn!(
            "Channel lead discovery task failed for '{}': {}",
            channel_for_warn, e
        );
        (String::new(), None)
    })
}

/// Format a brief human-readable summary of workflow state for a channel.
///
/// The `workflow_state` JSON typically contains task phase information like:
/// ```json
/// {"tasks": {"42": {"phase": "observe"}, "43": {"phase": "study"}}}
/// ```
///
/// Produces a line-per-task summary. Falls back to raw JSON for unexpected shapes.
pub(super) fn format_workflow_state_summary(state: &serde_json::Value) -> String {
    if state.is_null() {
        return "No active workflow state.".to_string();
    }

    if let Some(tasks) = state.get("tasks").and_then(|t| t.as_object()) {
        if tasks.is_empty() {
            return "No active workflow state.".to_string();
        }
        let mut lines = Vec::new();
        let mut task_ids: Vec<&String> = tasks.keys().collect();
        task_ids.sort();
        for id in task_ids {
            if let Some(task_obj) = tasks.get(id).and_then(|v| v.as_object()) {
                let phase = task_obj
                    .get("phase")
                    .and_then(|p| p.as_str())
                    .unwrap_or("unknown");
                lines.push(format!("- Task !{id}: phase = {phase}"));
            } else {
                lines.push(format!("- Task !{id}: {}", tasks[id]));
            }
        }
        lines.join("\n")
    } else {
        // Unknown shape — dump compact JSON so the LLM can still make sense of it
        format!(
            "Raw state: {}",
            serde_json::to_string(state).unwrap_or_else(|_| "{}".to_string())
        )
    }
}

fn build_resume_handoff_prompt(
    name: &str,
    dir_key: &str,
    previous_session_id: &str,
    prior_prompt: Option<&str>,
    working_dir: Option<&std::path::Path>,
) -> String {
    let history_file = crate::paths::headless_output_file(dir_key, name);
    let worktree = working_dir
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|| "<current default worktree>".to_string());

    let mut prompt = format!(
        "Previous session resume for `{}` failed. Start a fresh continuation in the same worktree.\n\n",
        previous_session_id
    );
    prompt.push_str("Context sources:\n");
    prompt.push_str(&format!("- Worktree: {}\n", worktree));
    prompt.push_str(&format!(
        "- Prior history file: {}\n\n",
        history_file.display()
    ));

    if let Some(prior_prompt) = prior_prompt {
        prompt.push_str(prior_prompt);
        prompt.push('\n');
        prompt.push('\n');
    }

    prompt.push_str("Continue from the prior history, then resume work as if uninterrupted.");
    prompt
}

/// Build a DM separator `PostSystemMessage` for a newly spawned session.
///
/// Returns a `PostSystemMessage` effect targeting `dm-<name>` with a task header
/// (e.g., "─── Task !42: Fix auth bug ───") to visually delineate task boundaries.
pub(super) fn build_dm_separator_effect(
    name: &str,
    task_id: &str,
    task_subject: Option<&str>,
) -> Effect {
    let separator = match task_subject {
        Some(subject) => format!("─── Task !{}: {} ───", task_id, subject),
        None => format!("─── Task !{} ───", task_id),
    };
    Effect::PostSystemMessage {
        message: separator,
        channel: Some(format!("dm-{}", name)),
    }
}

/// PR-related context for TaskPrompt observability messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskPromptPrContext {
    pub pr_number: u64,
    pub issue_type: PrIssueType,
}

/// Extra data needed when spawning a reviewer coworker.
/// Passed in `SpawnForTask.reviewer` so the executor can create the session span
/// and post the placeholder PR comment after the real coworker name is known.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReviewerSpawnInfo {
    pub pr_number: u64,
    pub pr_comment_body: String,
    pub restart_count: u32,
    pub agent_type: String,
}

/// A side effect that the daemon should execute.
///
/// Pure evaluation functions return `Vec<Effect>` instead of performing side
/// effects inline. The `execute_effects` function is the single place where
/// effects are carried out. This separation makes the decision logic testable
/// without mocking async infrastructure.
#[derive(Debug)]
pub enum Effect {
    /// Spawn a coworker using a typed launch configuration.
    SpawnCoworker(crate::launch::LaunchConfig),
    /// Shut down a running coworker with a message.
    ShutdownCoworker { name: String, message: String },
    /// Shut down a coworker with conditional follow-up effects on success.
    ///
    /// On success, `on_success` effects are executed. On failure, nothing extra
    /// happens (the shutdown failure is logged). This allows RPC handlers to
    /// post channel messages and broadcast status updates only when shutdown succeeds.
    ShutdownCoworkerWithCallbacks {
        name: String,
        message: String,
        on_success: Vec<Effect>,
    },
    /// Resume a stopped headless coworker session.
    ///
    /// Uses `SessionManager::spawn` with `SessionMode::ResumeSession` to
    /// restart a coworker from a previously saved session ID.
    ResumeCoworker {
        name: String,
        session_id: String,
        config: crate::launch::LaunchConfig,
    },
    /// Send a nudge message to a coworker by name via stdin prompt injection.
    ///
    /// Unified nudge variant: delivers message via `send_message`, posts to
    /// the coworker's DM channel for observability, and records attribution.
    /// On success, `on_success` effects are executed (e.g., `RecordTaskAssignment`).
    NudgeCoworker {
        name: String,
        message: String,
        nudge_type: String,
        on_success: Vec<Effect>,
    },
    /// Post a message to the IRC-style channel (and broadcast to WebSocket clients).
    ///
    /// The executor resolves channel and thread in two phases:
    ///
    /// **Channel routing** (3-step):
    /// 1. If `channel` is explicitly provided, use that
    /// 2. Otherwise, extract task ID from message content (e.g., "!42") and route to that task's channel
    /// 3. Fall back to the default project channel if no task ID is found
    ///
    /// **Thread resolution**: If the sender has a `bound_thread_id` in their SessionRecord,
    /// the message is posted as a thread reply under the bound thread parent.
    /// DM channels (`dm-*`) skip thread resolution — messages are always top-level.
    /// This mirrors the RPC path in `rpc_channel.rs`.
    PostToChannel {
        sender: String,
        message: String,
        channel: Option<String>,
        /// Whether this message is auto-streamed output (vs. an explicit channel post).
        auto_output: bool,
        /// Override the default `MessageType::Text`. Used by nudge effects to
        /// mark DM messages as `MessageType::Nudge`.
        message_type: Option<crate::message::MessageType>,
        /// Specific nudge variant for client-side rendering (e.g. "task_assigned").
        /// Only meaningful when `message_type` is `Nudge`.
        nudge_type: Option<String>,
        /// Structured tool call data for channel messages (DM and topic channels).
        /// When present, the Message will carry raw tool blocks for client-side rendering.
        tool_data: Option<Vec<crate::message::ToolBlock>>,
        /// AI provider that produced this message (e.g., "claude", "codex").
        provider: Option<String>,
        /// The tool_use `id` from the first tool block. When set on a DM channel message,
        /// the executor registers `tool_use_id → message.id` in `dm_tool_threads` so
        /// sub-agent events referencing this ID can thread under it.
        tool_use_id: Option<String>,
        /// When set, the executor looks up `dm_tool_threads[parent_tool_use_id]` to
        /// resolve the thread parent message ID, posting this as a thread reply.
        parent_tool_use_id: Option<String>,
    },
    /// Post a system message to the channel (and broadcast to WebSocket clients).
    ///
    /// If `channel` is `Some`, the message is routed to that channel (e.g. "ops").
    /// If `None`, it goes to the default project channel.
    PostSystemMessage {
        message: String,
        channel: Option<String>,
    },
    /// Broadcast a coworker status update to WebSocket clients.
    BroadcastCoworkerUpdate {
        name: String,
        status: String,
        current_task: Option<String>,
        color: Option<String>,
        icon: Option<String>,
        avatar_badge: Option<String>,
    },
    /// Record a cooldown entry (category + key).
    RecordCooldown { category: String, key: String },
    /// Schedule a usage-limit nudge at a specific time.
    SetUsageLimitNudge { at: tokio::time::Instant },
    /// Clear the scheduled usage-limit nudge (after it fires).
    ClearUsageLimitNudge,
    /// Reset a task back to pending (e.g. when a coworker can't be respawned).
    ResetTaskToPending { task_id: String, dir_key: String },
    /// Clear a stale session record for a task.
    ///
    /// When spawn fails (e.g. missing worktree), the session→task link must be
    /// broken so dispatch doesn't retry the same dead session every tick.
    /// Clears `task_id` from the SessionRecord and removes the in-memory
    /// session record task_id entry.
    ClearSessionForTask { task_id: String },
    /// Clear persisted session IDs and session-record bindings for a coworker.
    ///
    /// Used for unrecoverable resume/session errors (e.g., stale Codex thread IDs).
    /// This prevents retry loops by ensuring the next spawn is fresh.
    ClearSavedSessionId { name: String },
    /// Clear the `working_dir` field from a session record.
    ///
    /// When a session's recorded `working_dir` no longer exists on disk (e.g.
    /// the worktree was cleaned up), dispatch falls back to a fresh worktree and
    /// emits this effect so the stale path is not retried on the next tick.
    ClearSessionWorkingDir { session_id: String },
    /// Spawn a coworker with conditional follow-up effects.
    ///
    /// On success, `on_success` effects are executed. On failure, `on_failure`
    /// effects are executed. This allows decision functions to express
    /// spawn-dependent branching as data without calling spawn inline.
    SpawnCoworkerWithCallbacks {
        config: crate::launch::LaunchConfig,
        on_success: Vec<Effect>,
        on_failure: Vec<Effect>,
    },
    /// Unified spawn effect for tasks.
    ///
    /// Allocates a coworker name (preferring `preferred_name` if available),
    /// writes task ownership + in_progress status to disk, then spawns.
    /// On success, inlines all bookkeeping using the real allocated name.
    /// On failure, resets the task to pending and records a spawn-failure cooldown.
    SpawnForTask {
        task_id: String,
        dir_key: String,
        preferred_name: Option<String>,
        config: Box<crate::launch::LaunchConfig>,
        worktree_id: String,
        success_message: String,
        failure_message: String,
        cooldown_category: String,
        /// Extra (category, key) cooldowns to record on success, beyond the main cooldown.
        extra_success_cooldowns: Vec<(String, String)>,
        /// Reviewer-specific extras. `None` for regular task spawns.
        reviewer: Option<ReviewerSpawnInfo>,
    },
    /// Mark reminders as fired and persist to disk.
    ///
    /// Defers the mutation from the decision phase to the effect executor,
    /// keeping `check_and_fire_reminders` pure.
    MarkRemindersFired {
        fired_ids: Vec<String>,
        dir_key: String,
    },
    /// Advance `last_evaluated_at` for all cron reminders to prevent window accumulation.
    /// Emitted every tick alongside any reminder effects.
    AdvanceCronEvalTimestamps {
        dir_key: String,
        now: chrono::DateTime<chrono::Utc>,
    },
    /// Record a PR issue nudge in the tracker (prevents repeated nudges).
    RecordPrNudge {
        pr_number: u64,
        issue_type: PrIssueType,
    },
    /// Record a permanent one-shot PR nudge that survives cleanup.
    /// Used for user-authored PR notifications that should fire exactly once.
    RecordPermanentPrNudge {
        pr_number: u64,
        issue_type: PrIssueType,
    },
    /// Record a task assignment: updates in-memory busy tracking, persistent
    /// session state (`sessions[].task_id`).
    ///
    /// Defers the mutation from the decision phase to the effect executor,
    /// keeping decision functions pure.
    RecordTaskAssignment { coworker: String, task_id: String },
    /// Record that a reviewer escalation warning has been posted for a PR.
    ///
    /// Prevents the escalation warning from firing every tick. The in-memory
    /// `reviewer_escalations_posted` set is checked via WorldSnapshot before
    /// emitting escalation effects.
    RecordReviewerEscalation { pr_number: u64 },
    /// Record that the lead has been nudged about an orphaned PR (reviewed + CI green,
    /// no active task). Prevents `reconcile_orphaned_prs` from nudging on every tick.
    RecordOrphanedPrLeadNudge { pr_number: u64 },
    /// Clear the orphaned PR lead nudge record for a PR that now has an active task.
    /// This allows the lead to be re-nudged if the task later completes without merging
    /// and the PR becomes orphaned again.
    ClearOrphanedPrLeadNudge { pr_number: u64 },
    /// Re-run a GitHub Actions workflow that appears to be stuck.
    ///
    /// Used when a CI check has been pending for > 4x its typical duration.
    ///
    /// Used when a CI check has been pending for > 4x its typical duration.
    RerunWorkflow {
        run_id: u64,
        check_name: String,
        pr_number: u64,
    },
    /// Update a GitHub PR issue comment (e.g., to mark an abandoned "Review in progress" placeholder).
    ///
    /// Uses `gh api --method PATCH /repos/{owner}/{repo}/issues/comments/{comment_id}`.
    /// The `repo_full_name` is the GitHub owner/repo string (e.g., "btucker/midtown").
    UpdatePrComment {
        comment_id: u64,
        repo_full_name: String,
        new_body: String,
    },
    /// Link a PR to its session record and worktree.
    ///
    /// When a coworker opens a PR, backfill `pr_number` and `branch` on the
    /// SessionRecord and link the PR to the worktree by branch name.
    LinkPrToSession {
        pr_number: u64,
        session_id: String,
        branch: String,
        author: String,
        title: String,
    },
    /// Mark a task as completed.
    ///
    /// Called when a PR is merged with `[Midtown !XX]` in the title (dispatch.rs).
    CompleteTask { task_id: String, dir_key: String },
    /// Clear a completed task ID from all dependent tasks' `blockedBy` arrays.
    ///
    /// Called after a task is completed to unblock dependent tasks.
    ClearBlockedBy {
        completed_task_id: String,
        dir_key: String,
    },
    /// Set the explicit PR association for a task.
    ///
    /// Called when a PR is opened with `[Midtown !XX]` in the title to link the task to the PR.
    SetTaskPr {
        task_id: String,
        pr_number: u64,
        dir_key: String,
    },

    /// Create a child review task for a PR that needs code review.
    ///
    /// Replaces the old direct-spawn reviewer flow. Creates a pending task with
    /// `agent_type=midtown-code-reviewer` and `parent=<implementation task>`.
    /// The task dispatch system picks it up on the next tick and spawns a
    /// reviewer session with the appropriate launch config.
    CreateReviewTask {
        pr_number: u64,
        parent_task_id: Option<String>,
        channel: Option<String>,
    },
    /// Send a push notification to the mobile PWA.
    ///
    /// Fire-and-forget: the push manager runs in a background task.
    /// `url` is a deep-link path (e.g. `/{project}?channel=web&msg=123`)
    /// that the PWA uses to navigate on notification click. `None` means
    /// no navigation — the app is focused without changing view.
    SendPushNotification {
        title: String,
        body: String,
        tag: String,
        url: Option<String>,
    },
    /// Clean up stale local branches that match task/review naming patterns
    /// and are already merged into the default branch.
    ///
    /// Catches branches left behind after worktree removal.
    CleanStaleBranches,
    /// Clean a coworker's target/ directory to reclaim disk space.
    ///
    /// Called when a coworker goes on break. Deletes the build artifacts
    /// (target/ dir) to free up 4-7GB per coworker. They'll rebuild when
    /// recalled. This prevents disk exhaustion from idle coworker builds.
    ///
    /// `working_dir` is the coworker's actual working directory.
    CleanWorktreeTarget { name: String, working_dir: PathBuf },
    /// Clean up a task-based worktree after its PR is merged.
    ///
    /// Looks up the worktree in the registry by PR number or branch name,
    /// removes it from the registry, and deletes the worktree directory.
    CleanupMergedWorktree { pr_number: u64, branch: String },
    /// Clean up a stale worktree after its task has been completed for too long.
    ///
    /// Removes the worktree from the registry and deletes the directory.
    /// This is the time-based cleanup path (N hours after completion),
    /// complementing the PR-merge cleanup path.
    CleanupStaleWorktree { worktree_id: String },
    /// Sweep orphaned worktree directories that exist on disk but are not in
    /// the registry and not in use by any active session.
    CleanupOrphanedWorktrees { retention_hours: u64 },
    /// Garbage-collect stale daemon persistent state in a single batch.
    ///
    /// Removes dead session records older than the retention period.
    /// Task metadata lives in TaskStore and is not pruned here.
    ///
    /// Runs during PollTickEvent alongside stale worktree cleanup.
    GarbageCollectState {
        /// Session IDs to remove entirely (dead + past retention).
        dead_session_ids: Vec<String>,
        /// Orphaned task IDs to remove from metadata maps.
        orphaned_task_ids: Vec<String>,
    },
    /// Ensure a task-based worktree exists at the specified path.
    ///
    /// Creates the worktree if it doesn't exist, or succeeds idempotently
    /// if it already exists. Must be executed BEFORE SpawnCoworker effects
    /// that depend on the worktree.
    ///
    /// This follows the effect-based architecture: worktree creation is a
    /// side effect that must go through the Effect pipeline, not happen
    /// inline in spawn_coworker().
    EnsureWorktree {
        worktree_id: String,
        path: std::path::PathBuf,
    },
    /// Bind a coworker to a worktree in the registry.
    ///
    /// Called when a coworker is assigned to work in an existing task-based
    /// worktree. Updates the registry's reverse indexes.
    BindCoworkerToWorktree {
        worktree_id: String,
        coworker: String,
    },
    /// Register a new task-based worktree assignment in the registry.
    ///
    /// Called during task dispatch when a new worktree is allocated for a task.
    RegisterWorktreeAssignment {
        assignment: crate::worktree_registry::WorktreeAssignment,
    },
    /// Update the GitHub API rate limit state in persistent storage.
    ///
    /// Called periodically (every 2 minutes) to track GraphQL and REST API quota
    /// consumption. Used by adaptive throttling to reduce PR polling frequency
    /// when quotas run low.
    UpdateRateLimit(crate::github_rate_limit::GitHubRateLimit),
    /// Create a new topic channel and assign initial tasks to it.
    ///
    /// Creates the channel JSONL file and posts a creation message to main channel.
    CreateChannel {
        name: String,
        initial_tasks: Vec<String>,
    },
    /// Archive a topic channel by marking it as archived.
    ///
    /// Archived channels stop receiving new messages but keep history readable.
    /// Cannot archive the project's main channel.
    ArchiveChannel { name: String },
    /// Merge one channel into another.
    ///
    /// Moves all messages from `from` channel into `into` channel, updates
    /// task-to-channel mappings, posts a merge notice, and archives the source channel.
    MergeChannels { from: String, into: String },
    /// Assign a task to a specific channel.
    ///
    /// Updates the task_channel mapping in daemon persistent state.
    AssignTaskChannel { task_id: String, channel: String },
    /// Clear a task's owner without changing its status.
    ///
    /// Used when a coworker opens a PR and goes idle — the task stays in_progress
    /// (linked to the PR via SessionRecord) but the coworker name is freed.
    UnassignTask { task_id: String, dir_key: String },
    /// Reset an abandoned task back to pending.
    ///
    /// Used when a PR is closed without merge — the associated task is reset
    /// so it can be picked up by another coworker.
    ResetAbandonedTask {
        task_id: String,
        pr_number: u64,
        dir_key: String,
    },
    /// Create a new task.
    ///
    /// Used by reconciliation logic to generate tasks for orphaned PRs or other
    /// conditions discovered during polling ticks.
    CreateTask {
        dir_key: String,
        subject: String,
        description: String,
        /// Optional PR number to associate with the task (for deduplication).
        pr: Option<u64>,
    },
    /// Save a channel lead session ID after a successful spawn.
    ///
    /// Called after `SpawnCoworker` succeeds for a channel lead. Persists the
    /// session ID to `DaemonPersistentState::channel_lead_sessions` so the
    /// daemon can resume it on next startup.
    SaveChannelLeadSession {
        channel_name: String,
        session_id: String,
    },
    /// Respawn a channel lead that died unexpectedly.
    ///
    /// Defers I/O (loading domain context, agents.md) to the
    /// effect executor, keeping `ensure_channel_leads_alive()` a pure decision
    /// function. The executor loads context via `load_channel_lead_context()`
    /// and spawns a fresh session.
    RespawnChannelLead { channel_name: String },
    /// Mark an auth profile as usage-limited in persistent `profile_pool_state`.
    ///
    /// Emitted by `check_for_usage_limits()` when a coworker with a pool-selected
    /// profile hits its usage limit. The profile is skipped for future spawns until
    /// a `ClearProfileLimit` effect fires (triggered by `maybe_nudge_usage_limit_expiry`).
    MarkProfileLimited {
        profile_email: String,
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// Clear usage-limited status for an auth profile in `profile_pool_state`.
    ///
    /// Emitted by `maybe_nudge_usage_limit_expiry()` when the usage limit reset
    /// timer fires for a coworker that was spawned from a pool profile. Allows
    /// the profile to be selected again for future coworker spawns.
    ClearProfileLimit { profile_email: String },

    /// Remove a coworker from the attached set when their interactive session
    /// appears to have ended without a proper `midtown agent detach`.
    ///
    /// The entry is cleared so `ensure_lead_alive()` sees the lead as detached
    /// and respawns the headless session on the next tick. The coworker stop
    /// time is NOT recorded here — we want an immediate respawn, not the usual
    /// 5-minute cooldown that follows a normal stop.
    AutoDetachCoworker { name: String },

    // ── Unified nudge effects (V2) ──────────────────────────────────────
    /// Nudge a channel lead (by channel name).
    /// Execution layer resolves channel → session, handles spawn-if-dead,
    /// resume-if-idle-shutdown, and dual-path routing for the project lead.
    NudgeChannelLead {
        channel_name: String,
        reason: super::wake_reason::WakeReason,
    },
    /// Nudge a session (by session ID).
    /// Resolves session_id → name via persistent state, sends nudge message.
    NudgeSession {
        session_id: String,
        reason: super::wake_reason::WakeReason,
    },

    // ── Session-centric effects ─────────────────────────────────────────
    /// Shut down a running session.
    ///
    /// Session-centric counterpart to `ShutdownCoworker`. Looks up the session's
    /// current name via persistent state and performs shutdown +
    /// cleanup through `cleanup_coworker_state`.
    ShutdownSession { session_id: String, reason: String },

    /// Record a session record in persistent state.
    ///
    /// Upserts the `SessionRecord` into `DaemonPersistentState::sessions` and
    /// updates the SessionRecord in persistent state.
    RecordSession {
        record: Box<crate::daemon::state::SessionRecord>,
    },

    /// Merge a PR using `gh pr merge --squash --auto`.
    ///
    /// Gated by the `pr.merge` RPC — coworkers must call `midtown pr merge`
    /// instead of running `gh pr merge` directly. The daemon verifies review
    /// completion, CI status, and addressed feedback before executing.
    MergePr { pr_number: u64, title: String },

    /// Enable GitHub auto-merge on a PR that is approved with all CI checks passing.
    ///
    /// Triggered automatically by PR polling when `is_auto_mergeable()` returns true.
    /// Unlike `MergePr` (which is RPC-gated), this fires proactively from the
    /// stuck-PR detection path in `pr.rs`.
    AutoMergePr { pr_number: u64, title: String },

    /// Post a "Review in progress" placeholder comment on a PR.
    ///
    /// Executed as an `on_success` callback after spawning a reviewer session.
    /// The daemon posts the comment (avoiding prompt-compliance issues with
    /// escaped `!` characters) and stores the comment ID on the
    /// task metadata for later update via `pr.review-post`.
    PostPrComment {
        pr_number: u64,
        reviewer_name: String,
        body: String,
    },
    /// Dispatch a workflow event to the Python plugin daemon.
    ///
    /// When plugins are configured, sends the event over the Unix socket to the
    /// long-running Python daemon. The daemon dispatches to all registered hooks
    /// and returns actions + a `default_prevented` flag.
    ///
    /// Returned actions are converted to `Effect` variants and executed. If no
    /// plugins are configured, this effect is a no-op.
    EmitWorkflowEvent(crate::workflow::WorkflowEvent),

    /// Post an insight extracted from a coworker's DM stream to the task's channel.
    ///
    /// The executor handles deduplication (via `insight_hashes`), resolves the
    /// coworker's task → channel + thread ID, posts the insight message, and
    /// nudges the channel lead.
    PostInsight { agent: String, insight: String },

    /// Deliver a prompt to a task's session (nudge if running, resume if stopped).
    /// This is the effect-pipeline equivalent of the `task.prompt` RPC call.
    /// Cooldown tracking (RecordPrNudge) is NOT included — callers emit it separately.
    TaskPrompt {
        task_id: String,
        message: String,
        /// Optional model override (e.g., "opus" for review feedback).
        model: Option<String>,
        /// PR context for observability logging. When set, the executor logs
        /// the delivery at INFO level on success, and posts to the ops channel
        /// on failure.
        pr_context: Option<TaskPromptPrContext>,
    },

    /// Create a new task session span (reviewer or dev session starting work).
    CreateTaskSessionSpan {
        task_id: String,
        agent_name: String,
        agent_type: String,
        session_id: String,
        /// For reviewer tasks: the PR number being reviewed.
        pr_number: Option<u64>,
        /// How many times this reviewer has been restarted for this PR.
        /// Persisted to `task_restart_count` for stuck reviewer backoff.
        restart_count: u32,
    },
    /// Close a task session span (session stopping work on a task).
    CloseTaskSessionSpan { session_id: String, task_id: String },
}

/// Extract task IDs that are currently claimed by spawn or nudge effects.
///
/// This is used by in-flight tracking and dual-path deduplication to avoid
/// generating multiple spawn/nudge effects for the same task in one or adjacent
/// ticks.
pub(crate) fn extract_claimed_task_ids_from_effects(effects: &[Effect]) -> HashSet<String> {
    let mut ids = HashSet::new();

    for effect in effects {
        match effect {
            // Task-based spawns.
            Effect::SpawnForTask { task_id, .. } => {
                ids.insert(task_id.clone());
            }

            // Resolved task IDs for callback-based success paths.
            Effect::NudgeCoworker { on_success, .. }
            | Effect::SpawnCoworkerWithCallbacks { on_success, .. } => {
                for sub_effect in on_success {
                    if let Effect::RecordTaskAssignment { task_id, .. } = sub_effect {
                        ids.insert(task_id.clone());
                    }
                }
            }

            // Task prompt claims the task's session (nudge or resume).
            Effect::TaskPrompt { task_id, .. } => {
                ids.insert(task_id.clone());
            }

            // Keep this for completeness and safety in tests/caller-defined
            // effects, even though production callsites generally use callback
            // forms above.
            Effect::RecordTaskAssignment { task_id, .. } => {
                ids.insert(task_id.clone());
            }

            _ => {}
        }
    }

    ids
}

/// Extract PR numbers from CreateReviewTask effects in this batch.
///
/// Used to prevent duplicate review task creation across ticks: if a
/// CreateReviewTask has already been emitted for a PR number, we should not
/// emit another one. Unlike task IDs (which are generated dynamically during
/// effect execution), PR numbers are known at effect creation time.
pub(crate) fn extract_review_pr_numbers_from_effects(effects: &[Effect]) -> HashSet<u64> {
    let mut prs = HashSet::new();
    for effect in effects {
        if let Effect::CreateReviewTask { pr_number, .. } = effect {
            prs.insert(*pr_number);
        }
    }
    prs
}

/// Extract task IDs that are being completed by effects in this batch.
///
/// Used to prevent orphan recovery from spawning a new session for a task
/// that is already being auto-closed in the same tick.
pub(crate) fn extract_completed_task_ids_from_effects(effects: &[Effect]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for effect in effects {
        if let Effect::CompleteTask { task_id, .. } = effect {
            ids.insert(task_id.clone());
        }
    }
    ids
}

impl Effect {
    /// Convenience: nudge a channel lead with a freeform message.
    ///
    /// Shorthand for `NudgeChannelLead` with `WakeReason::Nudge`. Use the full
    /// form when the wake reason carries structured data (e.g., `TaskCreated`,
    /// `UserMessage`, `InsightPosted`).
    pub fn nudge_channel_lead(channel_name: impl Into<String>, message: impl Into<String>) -> Self {
        Self::NudgeChannelLead {
            channel_name: channel_name.into(),
            reason: super::wake_reason::WakeReason::Nudge {
                message: message.into(),
            },
        }
    }

    /// Convenience: nudge a session with a freeform message.
    pub fn nudge_session(session_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::NudgeSession {
            session_id: session_id.into(),
            reason: super::wake_reason::WakeReason::Nudge {
                message: message.into(),
            },
        }
    }

    /// Convenience: nudge a coworker by name with a freeform message and optional callbacks.
    pub fn nudge_coworker(
        name: impl Into<String>,
        message: impl Into<String>,
        nudge_type: impl Into<String>,
        on_success: Vec<Effect>,
    ) -> Self {
        Self::NudgeCoworker {
            name: name.into(),
            message: message.into(),
            nudge_type: nudge_type.into(),
            on_success,
        }
    }

    /// Convenience: post a message to a channel with sensible defaults.
    ///
    /// Creates a `PostToChannel` with `auto_output: false` and all optional
    /// fields set to `None`. Use the full form when you need `auto_output`,
    /// `message_type`, `nudge_type`, `tool_data`, `provider`, or thread IDs.
    pub fn post_to_channel(
        sender: impl Into<String>,
        message: impl Into<String>,
        channel: Option<String>,
    ) -> Self {
        Self::PostToChannel {
            sender: sender.into(),
            message: message.into(),
            channel,
            auto_output: false,
            message_type: None,
            nudge_type: None,
            tool_data: None,
            provider: None,
            tool_use_id: None,
            parent_tool_use_id: None,
        }
    }

    /// Convenience: post a message to the ops channel as "midtown".
    ///
    /// Shorthand for `post_to_channel("midtown", message, Some("ops"))`.
    pub fn post_to_ops(message: impl Into<String>) -> Self {
        Self::post_to_channel(
            "midtown",
            message,
            Some(super::constants::OPS_CHANNEL.to_string()),
        )
    }
}

/// Returns true if a non-completed task already exists for the given PR number.
///
/// Used by the `CreateTask` handler in `execute_effects` to skip duplicate task
/// creation when multiple review comments arrive in quick succession.  The caller
/// must pass `continue` (not `return`) after this returns `true` so that remaining
/// effects in the batch are still processed.
pub(crate) fn create_task_duplicate_exists(tasks: &[crate::task_store::Task], pr_num: u64) -> bool {
    tasks
        .iter()
        .any(|t| t.pr == Some(pr_num) && t.status != crate::task_store::TaskStatus::Completed)
}

/// Deduplicate nudge effects targeting the same coworker within a single batch.
///
/// When multiple PR issue types (CI green, review complete, merge conflict)
/// each generate a nudge for the same coworker in one tick, only the first
/// nudge is kept. For `NudgeCoworker`, subsequent nudges' `on_success`
/// callbacks are merged into the first nudge's callbacks so state recording
/// (e.g., `RecordPrNudge`, `RecordTaskAssignment`) still happens.
///
/// Plain `NudgeSession` effects for already-nudged sessions are dropped entirely.
fn dedup_nudge_effects(effects: Vec<Effect>) -> Vec<Effect> {
    use std::collections::HashSet;

    let mut nudged_coworkers: HashSet<String> = HashSet::new();
    let mut nudged_sessions: HashSet<String> = HashSet::new();
    let mut nudged_channels: HashSet<String> = HashSet::new();
    let mut result: Vec<Effect> = Vec::with_capacity(effects.len());

    for effect in effects {
        match effect {
            Effect::NudgeChannelLead {
                ref channel_name, ..
            } => {
                if nudged_channels.contains(channel_name) {
                    debug!(
                        "Deduplicating NudgeChannelLead for '{}' (already nudged in this batch)",
                        channel_name
                    );
                    continue;
                }
                nudged_channels.insert(channel_name.clone());
                result.push(effect);
            }
            Effect::NudgeSession { ref session_id, .. } => {
                let key = session_id.clone();
                if nudged_sessions.contains(&key) {
                    debug!(
                        "Deduplicating NudgeSession for {} (already nudged in this batch)",
                        session_id
                    );
                    continue;
                }
                nudged_sessions.insert(key);
                result.push(effect);
            }
            Effect::NudgeCoworker {
                ref name,
                message,
                nudge_type,
                on_success,
            } => {
                let key = name.clone();
                if nudged_coworkers.contains(&key) {
                    debug!(
                        "Deduplicating NudgeCoworker for {} — \
                         executing on_success callbacks without re-nudging",
                        name
                    );
                    // Merge on_success into the existing nudge's callbacks.
                    let remaining =
                        merge_coworker_callbacks_into_existing(&mut result, &key, on_success);
                    if let Some(unmerged) = remaining {
                        // First nudge had no callbacks — promote as standalone effects.
                        result.extend(unmerged);
                    }
                    continue;
                }
                nudged_coworkers.insert(key);
                result.push(Effect::NudgeCoworker {
                    name: name.clone(),
                    message,
                    nudge_type,
                    on_success,
                });
            }
            _ => {
                result.push(effect);
            }
        }
    }

    result
}

/// Merge `on_success` callbacks into an existing `NudgeCoworker` effect
/// for the same coworker. Returns `None` if merged successfully, or `Some(callbacks)`
/// if no matching effect was found.
fn merge_coworker_callbacks_into_existing(
    effects: &mut [Effect],
    target_key: &str,
    additional_callbacks: Vec<Effect>,
) -> Option<Vec<Effect>> {
    for effect in effects.iter_mut() {
        if let Effect::NudgeCoworker {
            name, on_success, ..
        } = effect
            && name == target_key
        {
            on_success.extend(additional_callbacks);
            return None;
        }
    }
    Some(additional_callbacks)
}

fn should_resume_channel_lead_session(session_id: &str) -> bool {
    !session_id.is_empty()
}

/// Perform the core shutdown operations for a coworker.
///
/// Returns `Ok(())` if shutdown succeeds, `Err(())` if any step fails.
/// This helper is shared by `Effect::ShutdownCoworker` and
/// `Effect::ShutdownCoworkerWithCallbacks`.
async fn shutdown_coworker_impl(name: &str, message: &str, state: &DaemonState) -> Result<(), ()> {
    // Send goodbye message via headless stdin, then shut down the session
    if !message.is_empty()
        && let Err(e) = state.session_manager.send_message(name, message).await
    {
        warn!("Failed to send shutdown message to {}: {}", name, e);
    }
    if let Err(e) = state.session_manager.shutdown(name).await {
        warn!("Failed to shut down headless session {}: {}", name, e);
        return Err(());
    }
    info!(coworker = %name, "SHUTDOWN_COWORKER: headless session stopped");

    // Clean up all transient coworker state and release the worktree binding.
    // This preserves the previous shutdown behavior while avoiding duplicated
    // persistence-write paths.
    state.cleanup_dead_coworker_state(name).await;
    Ok(())
}

fn worktree_is_old_enough(path: &std::path::Path, min_age: std::time::Duration) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
        return false;
    };
    age >= min_age
}

fn is_worktree_active(
    worktree_path: &std::path::Path,
    active_workdirs: &std::collections::HashSet<std::path::PathBuf>,
) -> bool {
    active_workdirs
        .iter()
        .any(|dir| dir == worktree_path || dir.starts_with(worktree_path))
}

/// Core nudge delivery: send message + DM post + attribution tracking.
///
/// Used by `NudgeCoworker` effect executor, `handle_coworker_nudge()` RPC,
/// and `deliver_task_prompt()`. Returns follow-up effects (DM channel post)
/// on success, or an error string on failure.
pub(super) async fn deliver_coworker_nudge(
    state: &DaemonState,
    name: &str,
    message: &str,
    nudge_type: &str,
    sender: &str,
) -> Result<Vec<Effect>, String> {
    match state.session_manager.send_message(name, message).await {
        Ok(()) => {
            state.record_pending_nudge(name, message);

            // Post to DM channel for observability (skip fork sessions).
            let is_fork = {
                let ps = state.persistent_state.lock().await;
                ps.session_by_name(name)
                    .is_some_and(|s| s.is_fork_session())
            };
            let mut follow_up = Vec::new();
            if !is_fork {
                follow_up.push(Effect::PostToChannel {
                    sender: sender.to_owned(),
                    message: message.to_owned(),
                    channel: Some(format!("dm-{}", name)),
                    auto_output: false,
                    message_type: Some(crate::message::MessageType::Nudge),
                    nudge_type: Some(nudge_type.to_owned()),
                    tool_data: None,
                    provider: None,
                    tool_use_id: None,
                    parent_tool_use_id: None,
                });
            }
            Ok(follow_up)
        }
        Err(e) => {
            warn!("Failed to nudge coworker {}: {}", name, e);
            Err(format!("Failed to nudge coworker {}: {}", name, e))
        }
    }
}

/// Resolve a session ID to its coworker name and deliver a nudge message.
///
/// Shared implementation for `NudgeSession`.
/// Returns `None` on failure (name not found or send error). On success,
/// the nudge is recorded for attribution tracking and an optional
/// `PostToChannel` effect is returned for the caller to execute — posting
/// the nudge content to the coworker's DM channel for observability.
async fn send_session_nudge(
    state: &DaemonState,
    session_id: &str,
    reason: &super::wake_reason::WakeReason,
) -> Option<Vec<Effect>> {
    let name = {
        let ps = state.persistent_state.lock().await;
        ps.sessions.get(session_id).map(|s| s.name.clone())
    };
    let Some(name) = name else {
        warn!(
            "NudgeSession: no name found for session {} — cannot deliver",
            session_id
        );
        return None;
    };
    let msg = reason.to_nudge_message();
    match state.session_manager.send_message(&name, &msg).await {
        Ok(()) => {
            state.record_pending_nudge(&name, &msg);

            // Build a PostToChannel effect for the coworker's DM channel.
            // Only for non-fork sessions (sessions without a bound thread ID).
            // Fork sessions are ephemeral and don't have their own DM channels.
            // Skip DmFromUser — the user's message is already in the DM channel
            // (written by rpc_channel.rs before the nudge effect was created).
            let mut follow_up = Vec::new();
            let is_fork = {
                let ps = state.persistent_state.lock().await;
                ps.session_by_name(&name)
                    .is_some_and(|s| s.is_fork_session())
            };
            if !reason.already_in_dm_channel() && !is_fork {
                follow_up.push(Effect::PostToChannel {
                    sender: reason.sender().to_owned(),
                    message: msg,
                    channel: Some(format!("dm-{}", name)),
                    auto_output: false,
                    message_type: Some(crate::message::MessageType::Nudge),
                    nudge_type: Some(reason.nudge_type().to_owned()),
                    tool_data: None,
                    provider: None,
                    tool_use_id: None,
                    parent_tool_use_id: None,
                });
            }
            Some(follow_up)
        }
        Err(e) => {
            warn!("Failed to nudge session {}: {}", session_id, e);
            None
        }
    }
}

/// Clear stale task bindings from session records.
///
/// Removes `task_id` from matching session records so dispatch no longer
/// attempts to resume dead sessions for that task.
fn clear_task_binding_in_records(
    sessions: &mut std::collections::HashMap<String, crate::daemon::state::SessionRecord>,
    task_id: &str,
    expected_session_id: Option<&str>,
) -> usize {
    let mut cleared = 0usize;
    for record in sessions.values_mut() {
        if record.task_id.as_deref() != Some(task_id) {
            continue;
        }
        let expected_match = expected_session_id
            .map(|sid| sid == record.session_id)
            .unwrap_or(false);
        // Safe cleanup target:
        // - exact expected session, or
        // - non-running stale records still pointing at the task.
        if expected_match || !record.is_running {
            record.task_id = None;
            record.resume_on_startup = false;
            if expected_match {
                record.is_running = false;
            }
            cleared += 1;
        }
    }
    cleared
}

/// Clear task→session bindings from both in-memory maps and persistent session records.
async fn clear_stale_task_session_binding(
    state: &DaemonState,
    task_id: &str,
    expected_session_id: Option<&str>,
) -> usize {
    // clear_task_binding_in_records below handles session record cleanup
    // (clearing task_id, resume_on_startup, is_running with expected_session_id logic).
    let mut ps = state.persistent_state.lock().await;
    let cleared = clear_task_binding_in_records(&mut ps.sessions, task_id, expected_session_id);
    // Close open spans for the expected session (it is being marked stopped).
    if let Some(_sid) = expected_session_id {}
    if cleared > 0
        && let Err(e) = ps.save_for_repo(state.paths.dir_key())
    {
        warn!(
            "Failed to save state after clearing stale session binding for task !{}: {}",
            task_id, e
        );
    }
    cleared
}

/// Fire-and-forget worktree directory removal + ops channel notification.
///
/// Shared by `CleanupMergedWorktree` and `CleanupStaleWorktree` effect handlers.
/// The caller is responsible for removing the assignment from the registry first;
/// this function only handles the filesystem cleanup and ops message.
async fn cleanup_worktree_and_notify(
    state: &DaemonState,
    assignment: &crate::worktree_registry::WorktreeAssignment,
    context: &str,
) {
    let wt_mgr = state.coworkers.worktree_manager().clone();
    let wt_id = assignment.worktree_id.clone();
    let context_owned = context.to_string();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = wt_mgr.force_cleanup_task_worktree(&wt_id) {
            warn!("Failed to remove worktree {}: {}", wt_id, e);
        } else {
            info!("Cleaned up worktree {} ({})", wt_id, context_owned);
        }
    });
    let task_ref = assignment
        .task_id
        .as_ref()
        .map(|id| format!(" (task !{})", id))
        .unwrap_or_default();
    let mut msg = Message::system(format!(
        "🧹 Cleaned up worktree {} ({}){}",
        assignment.worktree_id, context, task_ref
    ));
    msg.channel = Some(OPS_CHANNEL.to_string());
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post worktree cleanup message: {}", e);
    }
}

/// Execute a list of effects against the daemon state.
///
/// This is the imperative shell — the only place where side effects happen.
/// Each effect variant maps to a call on `DaemonState` or its subsystems.
///
/// Before execution, nudge effects targeting the same coworker are deduplicated
/// to prevent rapid-fire nudges within a single tick (e.g., when CI green,
/// review complete, and merge conflict each independently nudge the same coworker).
///
/// Spawn effects (`SpawnForTask`, `SpawnCoworkerWithCallbacks`, `SpawnCoworker`,
/// `EnsureWorktree`) are parallelized using `tokio::spawn` to avoid sequential
/// blocking during startup when processing multiple pending tasks. Non-spawn effects
/// execute sequentially as before. This keeps the daemon responsive to RPC requests
/// during startup by avoiding long sequential pauses from worktree creation (1-5s each).
pub async fn execute_effects(effects: Vec<Effect>, state: &DaemonState) {
    let effects = dedup_nudge_effects(effects);
    for effect in effects {
        match effect {
            Effect::SpawnCoworker(mut config) => {
                // Resolve avatar_badge from agent definition if not already set
                if config.avatar_badge.is_none()
                    && let Ok(def) =
                        crate::agent_definition::load_agent_definition(&config.agent_type)
                {
                    config.avatar_badge = def.avatar_badge;
                }
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                    }
                }
            }
            Effect::ShutdownCoworker { name, message, .. } => {
                info!(
                    coworker = %name,
                    message_preview = %message.chars().take(50).collect::<String>(),
                    "SHUTDOWN_COWORKER: executing shutdown effect"
                );
                let _ = shutdown_coworker_impl(&name, &message, state).await;
            }
            Effect::ShutdownCoworkerWithCallbacks {
                name,
                message,
                on_success,
                ..
            } => {
                info!(
                    coworker = %name,
                    message_preview = %message.chars().take(50).collect::<String>(),
                    "SHUTDOWN_COWORKER_WITH_CALLBACKS: executing shutdown effect"
                );
                match shutdown_coworker_impl(&name, &message, state).await {
                    Ok(()) => {
                        info!(coworker = %name, "SHUTDOWN_COWORKER_WITH_CALLBACKS: executing on_success callbacks");
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(()) => {
                        warn!(coworker = %name, "SHUTDOWN_COWORKER_WITH_CALLBACKS: shutdown failed, skipping on_success callbacks");
                    }
                }
            }
            Effect::ResumeCoworker {
                name,
                session_id,
                mut config,
            } => {
                // Resume the saved session if we have a valid session_id,
                // otherwise spawn fresh (session_id may have been cleared
                // after a failed resume attempt).
                if session_id.is_empty() {
                    info!(
                        "No valid session_id for '{}', spawning fresh instead of resuming",
                        name
                    );
                    config.session_mode = crate::launch::SessionMode::Fresh;
                } else {
                    config.session_mode =
                        crate::launch::SessionMode::ResumeSession(session_id.clone());
                }

                match spawn_with_resume_fallback(state, state.paths.dir_key(), &mut config).await {
                    Ok((_, used_fallback)) => {
                        if used_fallback {
                            info!("Fell back to fresh resume handoff for coworker {}", name);
                        } else {
                            info!("Resumed coworker {} successfully", name);
                        }
                    }
                    Err(e) => {
                        warn!("Failed to resume coworker {}: {}", name, e);
                    }
                }
            }
            Effect::NudgeCoworker {
                name,
                message,
                nudge_type,
                on_success,
            } => {
                // Clear in-flight markers for task IDs claimed by this effect.
                let task_ids: Vec<String> = extract_claimed_task_ids_from_effects(&on_success)
                    .into_iter()
                    .collect();

                if let Ok(follow_up) =
                    deliver_coworker_nudge(state, &name, &message, &nudge_type, "system").await
                {
                    let mut all = on_success;
                    all.extend(follow_up);
                    Box::pin(execute_effects(all, state)).await;
                }
                // Clear in-flight markers regardless of success/failure
                for task_id in &task_ids {
                    state.clear_task_spawn_in_flight(task_id);
                }
            }
            Effect::PostToChannel {
                sender,
                message,
                channel,
                auto_output,
                message_type,
                nudge_type,
                tool_data,
                provider,
                tool_use_id,
                parent_tool_use_id,
            } => {
                let msg_type = message_type.unwrap_or(crate::message::MessageType::Text);

                // Resolve the target channel:
                // 1. Use explicit channel if provided
                // 2. Otherwise, try to extract task ID from message and look up its channel
                // 3. Fall back to default channel if no task mentioned
                let channel_name = if let Some(ch) = channel {
                    Some(ch)
                } else {
                    state.resolve_message_channel(&message).await
                };

                // Thread resolution: if the sender is a forked session with a bound
                // thread, auto-apply the thread_parent_id so auto-posted output appears
                // in the correct thread. Mirrors the RPC path in rpc_channel.rs.
                // Skip for DM channels — they don't have task announcement threads,
                // so messages should always be top-level (unless threaded via parent_tool_use_id).
                let is_dm_channel = channel_name
                    .as_ref()
                    .is_some_and(|ch| ch.starts_with("dm-"));

                // For DM channels: resolve parent_tool_use_id → thread_parent_id
                // via the dm_tool_threads lookup.
                let dm_thread_parent: Option<String> = if is_dm_channel {
                    parent_tool_use_id
                        .as_ref()
                        .and_then(|ptuid| state.dm_tool_threads.lock().unwrap().get(ptuid).cloned())
                } else {
                    None
                };

                let bound_thread: Option<String> = if dm_thread_parent.is_some() {
                    dm_thread_parent
                } else if is_dm_channel {
                    None
                } else {
                    let ps = state.persistent_state.lock().await;
                    ps.session_by_name(&sender)
                        .and_then(|s| s.bound_thread_id.clone())
                };

                let mut msg = if let Some(parent_id) = bound_thread {
                    let ch = channel_name
                        .unwrap_or_else(|| state.channel_router.default_channel_name().to_string());
                    Message::thread_reply(&ch, &sender, &message, parent_id, msg_type)
                } else if let Some(ch) = channel_name {
                    Message::for_channel(&ch, &sender, &message, msg_type)
                } else {
                    let ch = state.channel_router.default_channel_name().to_string();
                    Message::for_channel(&ch, &sender, &message, msg_type)
                };
                msg.auto_output = auto_output;
                msg.nudge_type = nudge_type;
                msg.tool_data = tool_data;
                msg.provider = provider;
                msg.tool_use_id = tool_use_id.clone();
                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                    warn!("Failed to post channel message: {}", e);
                }

                // Debug: log DM thread resolution results.
                if is_dm_channel && (tool_use_id.is_some() || parent_tool_use_id.is_some()) {
                    tracing::debug!(
                        tool_use_id = ?tool_use_id,
                        parent_tool_use_id = ?parent_tool_use_id,
                        resolved_thread_parent = ?msg.thread_parent_id,
                        msg_id = %msg.id,
                        "DM thread resolution"
                    );
                }

                // After posting: register tool_use_id → message.id for DM thread lookup.
                // This allows sub-agent events in later drain cycles to find this message
                // as their thread parent.
                if is_dm_channel {
                    if let Some(tuid) = tool_use_id {
                        state
                            .dm_tool_threads
                            .lock()
                            .unwrap()
                            .insert(tuid, msg.id.clone());
                    }
                    // Also register call_ids from all top-level tool blocks so that
                    // sub-agent events referencing any of them can thread correctly.
                    if let Some(ref blocks) = msg.tool_data {
                        for block in blocks {
                            if block.parent_tool_use_id.is_none()
                                && let Some(ref cid) = block.call_id
                            {
                                state
                                    .dm_tool_threads
                                    .lock()
                                    .unwrap()
                                    .insert(cid.clone(), msg.id.clone());
                            }
                        }
                    }
                }

                // Update tool_activity_headers from tool_data for TUI activity display.
                // When tool_data has blocks → generate semantic headers and append.
                // When no tool_data and sender is a real agent → clear (work phase done).
                // Skip system senders (midtown, user) and non-fork explicit-channel posts.
                let is_system_sender = matches!(sender.to_lowercase().as_str(), "midtown" | "user")
                    || sender.eq_ignore_ascii_case(&state.project_name);
                let has_fork_channel_binding = {
                    let ps = state.persistent_state.lock().await;
                    ps.session_by_name(&sender)
                        .is_some_and(|s| s.is_fork_session())
                };
                let has_explicit_channel = msg.channel.is_some();
                let skip = is_system_sender
                    || (has_explicit_channel && !has_fork_channel_binding && !is_dm_channel);

                if let Some(ref blocks) = msg.tool_data {
                    let agent_key = sender.to_lowercase();
                    let mut headers_map = state.tool_activity_headers.write().unwrap();
                    let entry = headers_map.entry(agent_key).or_default();
                    for block in blocks {
                        let header = semantic_header(&block.tool_name, &block.input);
                        let prefix = if block.output.is_some() {
                            if block.error {
                                "\u{2717}" // ✗
                            } else {
                                "\u{2713}" // ✓
                            }
                        } else {
                            "\u{203a}" // › (in-progress)
                        };
                        entry.push(format!("{prefix} {header}"));
                    }
                    // Cap to avoid unbounded growth.
                    if entry.len() > MAX_TOOL_ITEMS_PER_AGENT {
                        let drain_count = entry.len() - MAX_TOOL_ITEMS_PER_AGENT;
                        entry.drain(..drain_count);
                    }
                } else if !skip {
                    let mut headers_map = state.tool_activity_headers.write().unwrap();
                    headers_map.remove(&sender.to_lowercase());
                }
            }
            Effect::BroadcastCoworkerUpdate {
                name,
                status,
                current_task,
                color,
                icon,
                avatar_badge,
            } => {
                state.broadcast_coworker_update(
                    &name,
                    &status,
                    current_task.as_deref(),
                    color.as_deref(),
                    icon.as_deref(),
                    avatar_badge.as_deref(),
                );
            }
            Effect::RecordCooldown { category, key } => {
                let mut cooldowns = state.cooldowns.lock().unwrap();
                cooldowns.record(&category, &key);
            }
            Effect::SetUsageLimitNudge { at } => {
                let mut nudge_at = state.usage_limit_nudge_at.lock().await;
                *nudge_at = Some(at);
            }
            Effect::ClearUsageLimitNudge => {
                let mut nudge_at = state.usage_limit_nudge_at.lock().await;
                *nudge_at = None;
            }
            Effect::MarkProfileLimited {
                profile_email,
                reset_at,
            } => {
                let mut ps = state.persistent_state.lock().await;
                let entry = ps
                    .profile_pool_state
                    .entry(profile_email.clone())
                    .or_default();
                entry.is_usage_limited = true;
                entry.usage_limit_reset_at = reset_at;
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to persist MarkProfileLimited for {}: {}",
                        profile_email, e
                    );
                }
            }
            Effect::ClearProfileLimit { profile_email } => {
                let mut ps = state.persistent_state.lock().await;
                if let Some(entry) = ps.profile_pool_state.get_mut(&profile_email) {
                    entry.is_usage_limited = false;
                    entry.usage_limit_reset_at = None;
                }
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to persist ClearProfileLimit for {}: {}",
                        profile_email, e
                    );
                }
            }
            Effect::ResetTaskToPending {
                task_id,
                dir_key: _dir_key,
            } => {
                if let Err(e) = state.task_store.reset_task_to_pending(&task_id) {
                    warn!("Failed to reset task !{} to pending: {}", task_id, e);
                }
                // Clear task assignment tracking (task is no longer assigned)
                state.clear_task_assignment_by_task(&task_id).await;
            }
            Effect::ClearSessionForTask { task_id } => {
                let cleared = clear_stale_task_session_binding(state, &task_id, None).await;
                if cleared == 0 {
                    debug!(
                        "ClearSessionForTask: no stale session binding found for task !{}",
                        task_id
                    );
                }
            }
            Effect::ClearSavedSessionId { name } => {
                let mut ps = state.persistent_state.lock().await;
                // Gather candidate stale session IDs for this coworker from
                // persistent state and channel entries.
                let mapped_sid = ps
                    .session_by_name(&name)
                    .map(|s| s.session_id.clone())
                    .unwrap_or_default();
                let mut candidate_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                if !mapped_sid.is_empty() {
                    candidate_ids.insert(mapped_sid.clone());
                }
                // Also check ps.sessions for a record matching this name
                // (covers sessions that may have been persisted under a
                // different session_id).
                for record in ps.sessions.values() {
                    if record.name == name && !record.session_id.is_empty() {
                        candidate_ids.insert(record.session_id.clone());
                    }
                }
                if let Some(sid) = ps.channel_lead_sessions.get(&name)
                    && !sid.is_empty()
                {
                    candidate_ids.insert(sid.clone());
                }

                // Mark any SessionRecord currently allocated to this name as
                // stale so the session won't be auto-resumed under this name.
                let mut stale_ids_for_spans: Vec<String> = Vec::new();
                for record in ps.sessions.values_mut() {
                    if record.name == name && record.is_running {
                        info!(
                            "Clearing stale session record for '{}': {}",
                            name, record.session_id
                        );
                        record.is_running = false;
                        record.resume_on_startup = false;
                        stale_ids_for_spans.push(record.session_id.clone());
                    }
                }
                // Preserve the channel_lead_sessions key (insert empty string)
                // so ensure_channel_leads_alive knows this channel still needs
                // a lead and will emit RespawnChannelLead on the next tick.
                if let Some(stored_sid) = ps.channel_lead_sessions.get(name.as_str()) {
                    if !stored_sid.is_empty() {
                        info!(
                            "Clearing stale channel_lead_sessions ID for '{}': {}",
                            name, stored_sid
                        );
                    }
                    ps.channel_lead_sessions
                        .insert(name.to_string(), String::new());
                }

                // Clear task/session bindings for matching session records so dispatch
                // won't repeatedly attempt to resume stale IDs.
                let mut cleared_task_ids: Vec<String> = Vec::new();
                for record in ps.sessions.values_mut() {
                    let matches_id = candidate_ids.contains(&record.session_id);
                    let matches_running_name =
                        record.is_running && record.name.eq_ignore_ascii_case(&name);
                    if !(matches_id || matches_running_name) {
                        continue;
                    }
                    if let Some(task_id) = record.task_id.take() {
                        cleared_task_ids.push(task_id);
                    }
                    record.is_running = false;
                    record.resume_on_startup = false;
                    stale_ids_for_spans.push(record.session_id.clone());
                }

                // Close open spans for all sessions being marked stopped.
                for _sid in &stale_ids_for_spans {}

                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to save persistent state after clearing stale session ID for '{}': {}",
                        name, e
                    );
                }
                drop(ps);

                // Session records already cleared above (task_id.take()),
                // no separate clear_task_assignment_by_task needed.
            }
            Effect::ClearSessionWorkingDir { session_id } => {
                let mut ps = state.persistent_state.lock().await;
                if let Some(record) = ps.sessions.get_mut(&session_id) {
                    info!(
                        "ClearSessionWorkingDir: cleared stale working_dir '{}' from session {}",
                        record.working_dir, session_id
                    );
                    record.working_dir = String::new();
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!(
                            "Failed to save state after clearing working_dir for session {}: {}",
                            session_id, e
                        );
                    }
                } else {
                    debug!(
                        "ClearSessionWorkingDir: no session record found for {}",
                        session_id
                    );
                }
            }
            Effect::SpawnCoworkerWithCallbacks {
                mut config,
                on_success,
                on_failure,
            } => {
                // Resolve avatar_badge from agent definition if not already set
                if config.avatar_badge.is_none()
                    && let Ok(def) =
                        crate::agent_definition::load_agent_definition(&config.agent_type)
                {
                    config.avatar_badge = def.avatar_badge;
                }

                // DM separators are posted by the caller in on_success effects,
                // not here. For task-based spawns the separator is posted by
                // SpawnForTask; for reviewer spawns it is included directly
                // in the on_success vector (see pr.rs).
                //
                // Clear in-flight markers for task IDs claimed by this effect.
                let task_ids: Vec<String> = extract_claimed_task_ids_from_effects(&on_success)
                    .into_iter()
                    .collect();

                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                        // Recursively execute success follow-ups
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                        // Recursively execute failure follow-ups
                        Box::pin(execute_effects(on_failure, state)).await;
                    }
                }
                // Clear in-flight markers regardless of success/failure,
                // so these tasks can be retried on the next tick if needed.
                for task_id in &task_ids {
                    state.clear_task_spawn_in_flight(task_id);
                }
            }
            Effect::SpawnForTask {
                task_id,
                dir_key,
                preferred_name,
                mut config,
                worktree_id,
                success_message,
                failure_message,
                cooldown_category,
                extra_success_cooldowns,
                reviewer,
            } => {
                // Use the task's agent_name as the session name (names are
                // generated at task creation and never recycled).
                let Some(name) = preferred_name else {
                    warn!("SpawnForTask: no agent_name for task !{}", task_id);
                    state.clear_task_spawn_in_flight(&task_id);
                    continue;
                };

                config.name = name.clone();

                // Resolve avatar_badge from agent definition if not already set
                if config.avatar_badge.is_none()
                    && let Ok(def) =
                        crate::agent_definition::load_agent_definition(&config.agent_type)
                {
                    config.avatar_badge = def.avatar_badge;
                }

                // 3. Spawn via state.spawn_coworker
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("SpawnForTask: spawned {} for task !{}", name, task_id);

                        // Update session record's task_id
                        {
                            let mut ps = state.persistent_state.lock().await;
                            if let Some(record) = ps.session_by_name_mut(&name) {
                                record.task_id = Some(task_id.clone());
                            }
                            if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                warn!(
                                    "Failed to save persistent state after SpawnForTask task_id update: {}",
                                    e
                                );
                            }
                        }

                        // Set task owner on disk
                        if let Err(e) = state.task_store.set_agent_name(&task_id, &name) {
                            warn!(
                                "SpawnForTask: failed to set task !{} owner to {}: {}",
                                task_id, name, e
                            );
                        }

                        // Transition task from pending to in_progress
                        if let Err(e) = state.task_store.set_task_in_progress(&task_id) {
                            warn!(
                                "SpawnForTask: failed to set task !{} to in_progress: {}",
                                task_id, e
                            );
                        }

                        // Post DM separator
                        let task_subject = state.task_store.load(&task_id).ok().map(|t| t.subject);
                        let separator_effect = build_dm_separator_effect(
                            &name,
                            &task_id,
                            task_subject.as_deref().filter(|s| !s.is_empty()),
                        );
                        Box::pin(execute_effects(vec![separator_effect], state)).await;

                        // Bind worktree, broadcast status, post ops message, record main cooldown
                        let mut success_effects = vec![
                            Effect::BindCoworkerToWorktree {
                                worktree_id,
                                coworker: name.clone(),
                            },
                            {
                                let task_avatar = state
                                    .task_store
                                    .load(&task_id)
                                    .ok()
                                    .map(|t| (t.color, t.icon));
                                let session_badge = state
                                    .persistent_state
                                    .lock()
                                    .await
                                    .session_by_name(&name)
                                    .and_then(|s| s.avatar_badge.clone());
                                Effect::BroadcastCoworkerUpdate {
                                    name: name.clone(),
                                    status: "running".to_string(),
                                    current_task: None,
                                    color: task_avatar.as_ref().and_then(|(c, _)| c.clone()),
                                    icon: task_avatar.as_ref().and_then(|(_, i)| i.clone()),
                                    avatar_badge: session_badge,
                                }
                            },
                            Effect::post_to_ops(success_message),
                            Effect::RecordCooldown {
                                category: cooldown_category,
                                key: "global".to_string(),
                            },
                        ];
                        // Extra per-spawn cooldowns (e.g. session_recovered)
                        for (category, key) in extra_success_cooldowns {
                            success_effects.push(Effect::RecordCooldown { category, key });
                        }
                        Box::pin(execute_effects(success_effects, state)).await;

                        // Reviewer-specific extras (need real name + session_id)
                        if let Some(info) = reviewer {
                            let sid = {
                                let ps = state.persistent_state.lock().await;
                                ps.session_by_name(&name)
                                    .map(|s| s.session_id.clone())
                                    .unwrap_or_default()
                            };
                            Box::pin(execute_effects(
                                vec![
                                    Effect::CreateTaskSessionSpan {
                                        task_id: task_id.clone(),
                                        agent_name: name.clone(),
                                        agent_type: info.agent_type,
                                        session_id: sid,
                                        pr_number: Some(info.pr_number),
                                        restart_count: info.restart_count,
                                    },
                                    Effect::PostPrComment {
                                        pr_number: info.pr_number,
                                        reviewer_name: name.clone(),
                                        body: info.pr_comment_body,
                                    },
                                ],
                                state,
                            ))
                            .await;
                        }

                        // Clear in-flight marker after all success bookkeeping
                        state.clear_task_spawn_in_flight(&task_id);
                    }
                    Err(e) => {
                        warn!("SpawnForTask: failed to spawn for task !{}: {}", task_id, e);
                        // Clear in-flight marker on failure
                        state.clear_task_spawn_in_flight(&task_id);
                        // Inline failure bookkeeping with real coworker name
                        Box::pin(execute_effects(
                            vec![
                                Effect::RecordCooldown {
                                    category: "spawn_failure".to_string(),
                                    key: name.to_string(),
                                },
                                Effect::ResetTaskToPending {
                                    task_id: task_id.clone(),
                                    dir_key: dir_key.clone(),
                                },
                                Effect::post_to_ops(failure_message),
                            ],
                            state,
                        ))
                        .await;
                    }
                }
            }
            Effect::MarkRemindersFired { fired_ids, dir_key } => {
                let mut ps = state.persistent_state.lock().await;
                for reminder in &mut ps.reminders.reminders {
                    if fired_ids.contains(&reminder.id) {
                        reminder.fire_count += 1;
                    }
                }
                if let Err(e) = ps.save_for_repo(&dir_key) {
                    warn!(
                        "Failed to save daemon-state.json after firing reminders: {}",
                        e
                    );
                }
            }
            Effect::AdvanceCronEvalTimestamps { dir_key, now } => {
                let mut ps = state.persistent_state.lock().await;
                let mut any_updated = false;
                for reminder in &mut ps.reminders.reminders {
                    if matches!(
                        reminder.trigger,
                        crate::reminders::ReminderTrigger::CronUtc { .. }
                    ) {
                        reminder.last_evaluated_at = Some(now);
                        any_updated = true;
                    }
                }
                if any_updated && let Err(e) = ps.save_for_repo(&dir_key) {
                    warn!(
                        "Failed to save daemon-state.json after updating cron eval timestamps: {}",
                        e
                    );
                }
            }
            Effect::RecordPrNudge {
                pr_number,
                issue_type,
            } => {
                let mut tracker = state.pr_issue_tracker.lock().await;
                tracker.record_nudge(pr_number, issue_type);
            }
            Effect::RecordPermanentPrNudge {
                pr_number,
                issue_type,
            } => {
                let mut tracker = state.pr_issue_tracker.lock().await;
                tracker.record_permanent_nudge(pr_number, issue_type);
                // Sync to persistent state so it survives daemon restarts
                let mut ps = state.persistent_state.lock().await;
                ps.permanent_pr_nudges = tracker.permanent_nudges().iter().cloned().collect();
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to save persistent state after recording permanent PR nudge: {}",
                        e
                    );
                }
            }
            Effect::RecordReviewerEscalation { pr_number } => {
                let mut posted = state.reviewer_escalations_posted.lock().unwrap();
                posted.insert(pr_number);
                debug!("Recorded reviewer escalation for PR #{}", pr_number);
            }
            Effect::RecordOrphanedPrLeadNudge { pr_number } => {
                let mut sent = state.orphaned_pr_lead_nudges_sent.lock().unwrap();
                sent.insert(pr_number);
                debug!("Recorded orphaned PR lead nudge for PR #{}", pr_number);
            }
            Effect::ClearOrphanedPrLeadNudge { pr_number } => {
                let mut sent = state.orphaned_pr_lead_nudges_sent.lock().unwrap();
                sent.remove(&pr_number);
                debug!("Cleared orphaned PR lead nudge for PR #{}", pr_number);
            }
            Effect::RecordTaskAssignment { coworker, task_id } => {
                // Update sessions[].task_id — the single source of truth for
                // coworker→task mapping.
                let mut ps = state.persistent_state.lock().await;
                if let Some(record) = ps.session_by_name_mut(&coworker.to_lowercase()) {
                    record.task_id = Some(task_id.clone());
                }
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to save persistent state after RecordTaskAssignment: {}",
                        e
                    );
                }
            }
            Effect::CreateTaskSessionSpan {
                task_id,
                session_id,
                pr_number,
                restart_count,
                ..
            } => {
                // Update TaskStore with session_id, pr, and restart_count.
                if let Ok(mut store_task) = state.task_store.load(&task_id) {
                    store_task.session_id = Some(session_id.clone());
                    if let Some(pr) = pr_number {
                        store_task.pr = Some(pr);
                    }
                    if restart_count > 0 {
                        store_task.restart_count = restart_count;
                    }
                    if let Err(e) = state.task_store.save(&store_task) {
                        warn!("Failed to update TaskStore task {} session: {}", task_id, e);
                    }
                }
            }
            Effect::CloseTaskSessionSpan { .. } => {
                // No-op: spans removed, session lifecycle handled by is_running flag
            }
            Effect::PostSystemMessage { message, channel } => {
                // If the message contains @lead, nudge the lead directly so they
                // are interrupted even when the message is routed to a non-main
                // channel (e.g. "ops") that the chat monitor does not watch.
                if message.to_lowercase().contains("@lead") {
                    state.nudge_lead(&message).await;
                }
                // If the message contains @ops, nudge the ops channel lead.
                // Ops owns daemon operational alerts (stuck PRs, orphaned worktrees,
                // coworker health) and escalates to @lead when human judgment is required.
                if message.to_lowercase().contains("@ops") {
                    let nudge = Effect::nudge_channel_lead(OPS_CHANNEL, message.clone());
                    Box::pin(execute_effects(vec![nudge], state)).await;
                }
                let mut msg = Message::system(message);
                msg.channel = channel;
                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                    warn!("Failed to post system message: {}", e);
                }
            }
            Effect::RerunWorkflow {
                run_id,
                check_name,
                pr_number,
            } => {
                rerun_workflow(state, run_id, &check_name, pr_number).await;
            }
            Effect::UpdatePrComment {
                comment_id,
                repo_full_name,
                new_body,
            } => {
                let endpoint = format!("/repos/{}/issues/comments/{}", repo_full_name, comment_id);
                let output = tokio::process::Command::new("gh")
                    .args([
                        "api",
                        "--method",
                        "PATCH",
                        &endpoint,
                        "-f",
                        &format!("body={}", new_body),
                    ])
                    .output()
                    .await;
                match output {
                    Ok(out) if out.status.success() => {
                        info!(
                            "Updated placeholder comment {} on {}",
                            comment_id, repo_full_name
                        );
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        warn!("Failed to update comment {}: {}", comment_id, stderr.trim());
                    }
                    Err(e) => {
                        warn!(
                            "Failed to run gh api for comment update {}: {}",
                            comment_id, e
                        );
                    }
                }
            }
            Effect::LinkPrToSession {
                pr_number,
                session_id,
                branch,
                author,
                title: _,
            } => {
                let mut ps = state.persistent_state.lock().await;
                // Backfill pr_number on the SessionRecord (if it exists).
                if let Some(record) = ps.sessions.get_mut(&session_id)
                    && record.pr_number.is_none()
                {
                    record.pr_number = Some(pr_number);
                    debug!(
                        "Backfilled pr_number={} on SessionRecord {} (task={:?})",
                        pr_number, session_id, record.task_id
                    );
                }
                // Backfill SessionRecord.branch from PR head_ref (often None at spawn time).
                if let Some(record) = ps.sessions.get_mut(&session_id)
                    && record.branch.is_none()
                {
                    record.branch = Some(branch.clone());
                    debug!(
                        "Backfilled branch={} on SessionRecord {} (task={:?})",
                        branch, session_id, record.task_id
                    );
                }
                // Link the PR to the worktree by matching branch name.
                // Use get_by_branch instead of get_by_coworker because coworkers can have
                // multiple worktrees (one per task), and we need to match the exact branch.
                if let Some(assignment) = ps.worktree_registry.get_by_branch(&branch) {
                    let wt_id = assignment.worktree_id.clone();
                    ps.worktree_registry.set_pr_number(&wt_id, pr_number);
                    debug!(
                        "Linked PR #{} to worktree {} via branch {} (author: {})",
                        pr_number, wt_id, branch, author
                    );
                }
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!("Failed to persist PR→session link: {}", e);
                } else {
                    info!(
                        "Linked PR #{} to session {} (author={})",
                        pr_number, session_id, author
                    );
                }
            }
            Effect::CompleteTask { task_id, dir_key } => {
                if let Err(e) = state.task_store.complete_task(&task_id) {
                    warn!("Failed to complete task !{}: {}", task_id, e);
                } else {
                    info!("Auto-completed task !{}", task_id);
                    // Mark worktree as completed (for time-based cleanup)
                    {
                        let mut ps = state.persistent_state.lock().await;
                        if let Some(wt_id) = ps.worktree_registry.find_worktree_by_task(&task_id) {
                            ps.worktree_registry
                                .mark_completed(&wt_id, chrono::Utc::now());
                        }
                        if let Err(e) = ps.save_for_repo(&dir_key) {
                            warn!("Failed to save task completion state: {}", e);
                        }
                    }
                    // Clear task assignment tracking (coworker is now free)
                    state.clear_task_assignment_by_task(&task_id).await;
                }
            }
            Effect::ClearBlockedBy {
                completed_task_id,
                dir_key: _,
            } => {
                if let Err(e) = state.task_store.clear_blocked_by(&completed_task_id) {
                    warn!(
                        "Failed to clear blockedBy for task !{}: {}",
                        completed_task_id, e
                    );
                } else {
                    info!(
                        "Cleared blockedBy references to completed task !{}",
                        completed_task_id
                    );
                }
            }
            Effect::SetTaskPr {
                task_id,
                pr_number,
                dir_key: _,
            } => {
                if let Err(e) = state.task_store.update_task_fields(
                    &task_id,
                    None, // agent_name
                    None, // status
                    None, // description
                    None, // blocked_by
                    None, // channel
                    Some(pr_number),
                ) {
                    warn!("Failed to set PR association for task !{}: {}", task_id, e);
                } else {
                    info!(
                        "Set PR association for task !{}: PR #{}",
                        task_id, pr_number
                    );
                }
            }
            Effect::CreateReviewTask {
                pr_number,
                parent_task_id,
                channel,
            } => {
                let task_id = state.task_store.next_task_id().to_string();
                // Inherit parent thread for review tasks
                let parent_thread = parent_task_id
                    .as_ref()
                    .and_then(|pid| state.task_store.load(pid).ok())
                    .and_then(|t| t.thread_id.clone());
                let store_task = crate::task_store::Task {
                    id: task_id.clone(),
                    subject: format!("Review PR #{}", pr_number),
                    status: crate::task_store::TaskStatus::Pending,
                    description: Some(format!(
                        "Code review for PR #{}. Spawned automatically by the daemon.",
                        pr_number
                    )),
                    blocked_by: vec![],
                    channel: channel.clone(),
                    pr: Some(pr_number),
                    agent_name: String::new(), // Will be set by task dispatch
                    agent_type: "midtown-code-reviewer".to_string(),
                    session_id: None,
                    parent: parent_task_id.clone(),
                    message_id: None,
                    thread_id: parent_thread,
                    model: None,
                    plan: None,
                    placeholder_comment_id: None,
                    color: None,
                    icon: None,
                    restart_count: 0,
                    execution_skill: None,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                };
                match state.task_store.save(&store_task) {
                    Ok(()) => {
                        info!(
                            "Created review task !{} for PR #{} (parent: {:?})",
                            task_id, pr_number, parent_task_id
                        );
                    }
                    Err(e) => {
                        warn!("Failed to create review task for PR #{}: {}", pr_number, e);
                    }
                }
                // Clear the in-flight guard so the PR can be re-evaluated if needed.
                state.clear_review_pr_in_flight(pr_number);
            }
            Effect::SendPushNotification {
                title,
                body,
                tag,
                url,
            } => {
                state.send_push_notification(&title, &body, &tag, url.as_deref());
            }
            Effect::CleanStaleBranches => {
                // Fire-and-forget: branch cleanup runs git operations that can
                // take minutes with thousands of branches. Don't block other effects.
                let wt_manager = state.coworkers.worktree_manager().clone();
                tokio::spawn(async move {
                    let cleaned =
                        tokio::task::spawn_blocking(move || wt_manager.clean_stale_task_branches())
                            .await
                            .unwrap_or_default();
                    if !cleaned.is_empty() {
                        info!(
                            "Cleaned up {} stale task branch(es): {}",
                            cleaned.len(),
                            cleaned.join(", ")
                        );
                    }
                });
            }
            Effect::CleanWorktreeTarget { name, working_dir } => {
                let target_path = working_dir.join("target");

                if !target_path.exists() {
                    debug!(
                        "Target directory for {} doesn't exist at {}, skipping cleanup",
                        name,
                        target_path.display()
                    );
                    continue;
                }

                // Brief delay to let the coworker process finish terminating.
                // ShutdownCoworker sends a non-blocking kill signal, so the
                // process may still be writing to target/ when we get here.
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;

                let name_clone = name.clone();
                match tokio::task::spawn_blocking(move || {
                    match std::fs::remove_dir_all(&target_path) {
                        Ok(()) => {
                            info!(
                                "Cleaned target/ directory for {} to reclaim disk space",
                                name_clone
                            );
                        }
                        Err(e) => {
                            warn!(
                                "Failed to clean target/ directory for {}: {}",
                                name_clone, e
                            );
                        }
                    }
                })
                .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        warn!(
                            "spawn_blocking panicked during target/ cleanup for {}: {}",
                            name, e
                        );
                    }
                }
            }
            Effect::CleanupMergedWorktree { pr_number, branch } => {
                // Remove from registry
                let removed = {
                    let mut ps = state.persistent_state.lock().await;
                    let removed = ps.worktree_registry.cleanup_for_merged_pr(pr_number);
                    if removed.is_some()
                        && let Err(e) = ps.save_for_repo(state.paths.dir_key())
                    {
                        warn!("Failed to save daemon state after worktree cleanup: {}", e);
                    }
                    removed
                };
                if let Some(assignment) = removed {
                    cleanup_worktree_and_notify(
                        state,
                        &assignment,
                        &format!("after PR #{} merged", pr_number),
                    )
                    .await;
                } else {
                    debug!(
                        "No worktree registered for PR #{} (branch: {}), skipping cleanup",
                        pr_number, branch
                    );
                }
            }
            Effect::CleanupStaleWorktree { worktree_id } => {
                // Remove from registry
                let removed = {
                    let mut ps = state.persistent_state.lock().await;
                    let removed = ps.worktree_registry.remove_worktree(&worktree_id);
                    if removed.is_some()
                        && let Err(e) = ps.save_for_repo(state.paths.dir_key())
                    {
                        warn!(
                            "Failed to save daemon state after stale worktree cleanup: {}",
                            e
                        );
                    }
                    removed
                };
                if let Some(assignment) = removed {
                    cleanup_worktree_and_notify(state, &assignment, "retention period expired")
                        .await;
                } else {
                    debug!(
                        "Worktree {} not found in registry, skipping cleanup",
                        worktree_id
                    );
                }
            }
            Effect::CleanupOrphanedWorktrees { retention_hours } => {
                let min_age = std::time::Duration::from_secs(retention_hours.saturating_mul(3600));
                let (registered_ids, active_workdirs) = {
                    let ps = state.persistent_state.lock().await;
                    let registered_ids = ps
                        .worktree_registry
                        .all_assignments()
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>();
                    let mut active = std::collections::HashSet::new();
                    for coworker in state.coworkers.list() {
                        active.insert(std::path::PathBuf::from(coworker.working_dir));
                    }
                    for record in ps.sessions.values() {
                        if record.is_running && !record.working_dir.is_empty() {
                            active.insert(std::path::PathBuf::from(&record.working_dir));
                        }
                    }
                    (registered_ids, active)
                };

                let orphan_ids = state
                    .coworkers
                    .worktree_manager()
                    .find_orphaned_task_worktrees(&registered_ids);
                if orphan_ids.is_empty() {
                    continue;
                }

                let worktrees_base = state
                    .coworkers
                    .worktree_manager()
                    .task_worktrees_base()
                    .to_path_buf();
                for worktree_id in orphan_ids {
                    let worktree_path = worktrees_base.join(&worktree_id);
                    if is_worktree_active(&worktree_path, &active_workdirs) {
                        debug!(
                            "Skipping orphaned worktree {} because an active session is using it",
                            worktree_id
                        );
                        continue;
                    }
                    if !worktree_is_old_enough(&worktree_path, min_age) {
                        continue;
                    }

                    let wt_mgr = state.coworkers.worktree_manager().clone();
                    let wt_id = worktree_id.clone();
                    match tokio::task::spawn_blocking(move || {
                        wt_mgr.force_cleanup_task_worktree(&wt_id)
                    })
                    .await
                    {
                        Ok(Ok(())) => {
                            info!("Cleaned up orphaned worktree {}", worktree_id);
                            let mut msg = Message::system(format!(
                                "🧹 Cleaned up orphaned worktree {} (not in registry)",
                                worktree_id
                            ));
                            msg.channel = Some(OPS_CHANNEL.to_string());
                            if let Err(e) = state.send_and_broadcast_async(&msg).await {
                                warn!("Failed to post orphaned worktree cleanup message: {}", e);
                            }
                        }
                        Ok(Err(e)) => {
                            warn!("Failed to remove orphaned worktree {}: {}", worktree_id, e);
                        }
                        Err(e) => {
                            warn!(
                                "spawn_blocking panicked during orphaned worktree cleanup {}: {}",
                                worktree_id, e
                            );
                        }
                    }
                }
            }
            Effect::GarbageCollectState {
                dead_session_ids,
                orphaned_task_ids,
            } => {
                let result = {
                    let mut ps = state.persistent_state.lock().await;
                    let result = ps.apply_gc(&dead_session_ids, &orphaned_task_ids);

                    if result.has_changes()
                        && let Err(e) = ps.save_for_repo(state.paths.dir_key())
                    {
                        warn!("Failed to save daemon state after GC: {}", e);
                    }
                    result
                };

                if result.has_changes() {
                    // Sync CoworkerManager with alive sessions immediately.
                    // Without this, stale entries persist until the next
                    // SessionMonitorTick, causing ghost coworkers that block
                    // name allocation and spawn decisions.
                    let alive_names: std::collections::HashSet<String> = state
                        .session_manager
                        .list_alive_names()
                        .await
                        .into_iter()
                        .collect();
                    state.coworkers.retain_alive(&alive_names);

                    info!(
                        "State GC: removed {} dead sessions, \
                         pruned {} orphaned task entries",
                        result.sessions_removed, result.orphaned_tasks_pruned,
                    );

                    // Post to ops channel
                    let mut msg = crate::message::Message::system(format!(
                        "🧹 State GC: removed {} dead session(s), \
                         pruned {} orphaned task entries",
                        result.sessions_removed, result.orphaned_tasks_pruned,
                    ));
                    msg.channel = Some(OPS_CHANNEL.to_string());
                    if let Err(e) = state.send_and_broadcast_async(&msg).await {
                        warn!("Failed to post state GC message: {}", e);
                    }
                }
            }
            Effect::BindCoworkerToWorktree {
                worktree_id,
                coworker,
            } => {
                let mut ps = state.persistent_state.lock().await;

                // Collision guard: check if worktree is bound to a different ACTIVE coworker
                let needs_force_rebind = if let Some(assignment) =
                    ps.worktree_registry.get(&worktree_id)
                {
                    if let Some(ref current_coworker) = assignment.current_coworker {
                        if current_coworker != &coworker {
                            // Worktree is bound to a different coworker - check if they're active
                            if state.session_manager.is_alive(current_coworker).await {
                                warn!(
                                    "WORKTREE COLLISION BLOCKED: Refusing to bind {} to worktree {} - already bound to ACTIVE coworker {}",
                                    coworker, worktree_id, current_coworker
                                );
                                // Do NOT bind - this would crash both Claude Code sessions.
                                // Use `continue` (not `return`) so remaining effects in the
                                // batch are still processed.
                                continue;
                            } else {
                                debug!(
                                    "Worktree {} was bound to {} but they're not active - allowing rebind to {}",
                                    worktree_id, current_coworker, coworker
                                );
                                // Old coworker is dead - need force rebind
                                true
                            }
                        } else {
                            // Same coworker - idempotent, use normal bind
                            false
                        }
                    } else {
                        // No current coworker - use normal bind
                        false
                    }
                } else {
                    // Worktree doesn't exist in registry - will fail with normal bind
                    false
                };

                // Perform the bind operation
                let bind_result = if needs_force_rebind {
                    ps.worktree_registry
                        .force_rebind_coworker(&worktree_id, &coworker)
                } else {
                    ps.worktree_registry.bind_coworker(&worktree_id, &coworker)
                };

                if let Err(e) = bind_result {
                    warn!(
                        "Failed to bind {} to worktree {}: {}",
                        coworker, worktree_id, e
                    );
                } else {
                    debug!("Bound {} to worktree {}", coworker, worktree_id);
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!("Failed to save daemon state after binding coworker: {}", e);
                    }
                }
            }
            Effect::EnsureWorktree { worktree_id, path } => {
                if path.exists() {
                    debug!(
                        "Worktree {} already exists at {}, reusing",
                        worktree_id,
                        path.display()
                    );
                } else {
                    info!("Creating worktree {} at {}", worktree_id, path.display());
                    if let Err(e) = state
                        .coworkers
                        .worktree_manager()
                        .create_task_worktree(&worktree_id)
                    {
                        warn!("Failed to create worktree {}: {}", worktree_id, e);
                    }
                }
            }
            Effect::RegisterWorktreeAssignment { assignment } => {
                let mut ps = state.persistent_state.lock().await;
                let wt_id = assignment.worktree_id.clone();
                if let Err(e) = ps.worktree_registry.assign_worktree(assignment) {
                    warn!("Failed to register worktree assignment {}: {}", wt_id, e);
                } else {
                    debug!("Registered worktree assignment {}", wt_id);
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!(
                            "Failed to save daemon state after registering worktree: {}",
                            e
                        );
                    }
                }
            }
            Effect::UpdateRateLimit(rate_limit) => {
                let mut ps = state.persistent_state.lock().await;
                ps.github.rate_limit = rate_limit.clone();
                debug!("Updated GitHub rate limits: {}", rate_limit.summary());
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to save daemon state after updating rate limit: {}",
                        e
                    );
                }
            }
            Effect::CreateChannel {
                name,
                initial_tasks,
            } => {
                // Create the channel JSONL file
                let base_dir = state.paths.base_dir().to_path_buf();
                let already_exists = base_dir.join("channels").join(&name).exists();
                if let Err(e) = crate::channel::Channel::create(&base_dir, &name) {
                    warn!("Failed to create channel '{}': {}", name, e);
                } else {
                    info!("Created channel '{}'", name);
                    if !already_exists {
                        state.broadcast_web_update(crate::web::channel_list_changed(
                            "created", &name,
                        ));
                    }

                    // Post creation message to main channel
                    let msg = Message::text(
                        "midtown",
                        format!(
                            "📢 Created new topic channel '{}' with {} initial task(s)",
                            name,
                            initial_tasks.len()
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast_async(&msg).await {
                        warn!("Failed to post channel creation message: {}", e);
                    }

                    // Spawn a channel lead session for the new topic channel.
                    // Insert an empty placeholder first to guard against duplicate spawns
                    // (the double-spawn guard checks channel_lead_sessions.contains_key).
                    {
                        let mut ps = state.persistent_state.lock().await;
                        ps.channel_lead_sessions
                            .entry(name.clone())
                            .or_insert_with(String::new);
                        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                            warn!(
                                "Failed to save daemon state before spawning channel lead: {}",
                                e
                            );
                        }
                    }
                    let (wf_name, wf_state_summary, suppress_auto_output) = {
                        let ps = state.persistent_state.lock().await;
                        let wf = ps.channel_workflows.get(&name).cloned();
                        let wfs = ps
                            .workflow_state
                            .get(&name)
                            .map(format_workflow_state_summary);
                        let suppress = ps
                            .channel_settings
                            .get(&name)
                            .is_some_and(|s| !s.show_full_lead_output);
                        (wf, wfs, suppress)
                    };
                    let (domain_context, agents_md) = load_channel_lead_context(
                        base_dir.clone(),
                        &name,
                        state.all_repo_paths.first().cloned().unwrap_or_default(),
                        state.paths.dir_key(),
                        wf_name,
                        state.paths.workflows_dir(),
                        wf_state_summary,
                    )
                    .await;
                    let mut config = crate::launch::LaunchConfig::channel_lead(
                        &name,
                        state.paths.dir_key(),
                        crate::launch::SessionMode::Fresh,
                        domain_context,
                        agents_md,
                    );
                    config.cwd_subdir =
                        crate::paths::read_channel_directory(state.paths.dir_key(), &name);
                    config.suppress_auto_output = suppress_auto_output;
                    match state.spawn_coworker(&config).await {
                        Ok(session_id) => {
                            info!(
                                "Spawned channel lead for '{}' successfully (session={})",
                                name, session_id
                            );
                            // Update channel_lead_sessions with the real session_id
                            // immediately (spawn_coworker generated it upfront), eliminating
                            // the race window before the init StreamEvent arrives.
                            let mut ps = state.persistent_state.lock().await;
                            ps.channel_lead_sessions.insert(name.clone(), session_id);
                            if let Err(save_err) = ps.save_for_repo(state.paths.dir_key()) {
                                warn!(
                                    "Failed to save daemon state after channel lead spawn: {}",
                                    save_err
                                );
                            }
                        }
                        Err(e) => {
                            warn!("Failed to spawn channel lead for '{}': {}", name, e);
                            // Preserve the key with empty value so
                            // ensure_channel_leads_alive retries on the next tick.
                            let mut ps = state.persistent_state.lock().await;
                            ps.channel_lead_sessions.insert(name.clone(), String::new());
                            if let Err(save_err) = ps.save_for_repo(state.paths.dir_key()) {
                                warn!(
                                    "Failed to save daemon state after failed channel lead spawn: {}",
                                    save_err
                                );
                            }
                        }
                    }
                }
            }
            Effect::ArchiveChannel { name } => {
                // Archive the channel by using Channel::archive()
                let base_dir = state.paths.base_dir().to_path_buf();

                // Idempotency guard: check if the channel directory still exists before
                // archiving. This effect may fire repeatedly (once per TaskDispatchTick)
                // because completed tasks still reference the channel name. Without this
                // guard, Channel::new() would recreate the channel directory,
                // and archive() would then overwrite the real archived data — destroying
                // all channel history.
                let channel_dir = base_dir.join("channels").join(&name);
                if !channel_dir.exists() {
                    debug!("Channel '{}' already archived, skipping", name);
                    continue;
                }

                match crate::channel::Channel::new(&base_dir, &name) {
                    Ok(channel) => {
                        if let Err(e) = channel.archive(&state.project_name) {
                            warn!("Failed to archive channel '{}': {}", name, e);
                        } else {
                            info!("Archived channel '{}'", name);
                            state.broadcast_web_update(crate::web::channel_list_changed(
                                "archived", &name,
                            ));

                            // Post archive message to main channel
                            let msg = Message::text(
                                "midtown",
                                format!("📦 Archived channel '{}' (work complete)", name),
                            );
                            if let Err(e) = state.send_and_broadcast_async(&msg).await {
                                warn!("Failed to post archive message: {}", e);
                            }

                            // Gracefully shut down the channel lead session for this channel.
                            // The channel is archived so the lead is no longer needed.
                            let lead_session_name = crate::launch::channel_lead_session_name(&name);
                            let goodbye = format!(
                                "Channel '{}' has been archived. Your session is ending — \
                                 thank you for your service as domain expert for this channel.",
                                name
                            );
                            let _ =
                                shutdown_coworker_impl(&lead_session_name, &goodbye, state).await;
                            // Remove from channel_lead_sessions and mark session records
                            {
                                let mut ps = state.persistent_state.lock().await;
                                let removed_lead = ps.channel_lead_sessions.remove(&name).is_some();
                                // Mark any SessionRecord with this name as no longer running
                                let mut removed_session = false;
                                let mut stopped_ids: Vec<String> = Vec::new();
                                for record in ps.sessions.values_mut() {
                                    if record.name == lead_session_name {
                                        record.is_running = false;
                                        record.resume_on_startup = false;
                                        removed_session = true;
                                        stopped_ids.push(record.session_id.clone());
                                    }
                                }
                                for _sid in &stopped_ids {}
                                if removed_lead || removed_session {
                                    debug!(
                                        "Removed channel lead session for archived channel '{}'",
                                        name
                                    );
                                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                        warn!(
                                            "Failed to save daemon state after removing channel lead: {}",
                                            e
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to open channel '{}' for archiving: {}", name, e);
                    }
                }
            }
            Effect::MergeChannels { from, into } => {
                // Read all messages from source channel
                let base_dir = state.paths.base_dir().to_path_buf();
                let from_channel = match crate::channel::Channel::new(&base_dir, &from) {
                    Ok(ch) => ch,
                    Err(e) => {
                        warn!("Failed to open source channel '{}' for merge: {}", from, e);
                        continue;
                    }
                };

                // Append all messages to target channel
                let into_channel = match crate::channel::Channel::new(&base_dir, &into) {
                    Ok(ch) => ch,
                    Err(e) => {
                        warn!("Failed to open target channel '{}' for merge: {}", into, e);
                        continue;
                    }
                };

                // Read messages from source
                let messages = match from_channel.read_all_async().await {
                    Ok(msgs) => msgs,
                    Err(e) => {
                        warn!("Failed to read messages from channel '{}': {}", from, e);
                        continue;
                    }
                };

                // Write to target
                for msg in messages {
                    if let Err(e) = into_channel.send(&msg) {
                        warn!("Failed to send message to channel '{}': {}", into, e);
                    }
                }

                // Update TaskStore tasks that reference the merged channel
                for mut task in state.task_store.load_all() {
                    if task.channel.as_deref() == Some(&from) {
                        task.channel = Some(into.clone());
                        if let Err(e) = state.task_store.save(&task) {
                            warn!(
                                "Failed to update TaskStore task {} channel after merge: {}",
                                task.id, e
                            );
                        }
                    }
                }

                // Post merge notice to target channel
                let msg = Message::for_channel(
                    &into,
                    "midtown",
                    format!("🔗 Merged channel '{}' into this channel", from),
                    crate::message::MessageType::Text,
                );
                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                    warn!("Failed to post merge notice: {}", e);
                }

                // Archive the source channel using Channel::archive().
                // Idempotency guard: only archive if the active directory still exists.
                // Without this, a duplicate merge could recreate the channel via Channel::new()
                // and then archive() would overwrite the real archived data.
                let from_channel_dir = base_dir.join("channels").join(&from);
                if from_channel_dir.exists() {
                    if let Err(e) = from_channel.archive(&state.project_name) {
                        warn!(
                            "Failed to archive source channel '{}' after merge: {}",
                            from, e
                        );
                    } else {
                        info!(
                            "Merged channel '{}' into '{}' and archived source",
                            from, into
                        );
                        state.broadcast_web_update(crate::web::channel_list_changed(
                            "archived", &from,
                        ));
                    }
                } else {
                    debug!(
                        "Source channel '{}' already archived after merge, skipping",
                        from
                    );
                }
            }
            Effect::AssignTaskChannel { task_id, channel } => {
                debug!("Assigned task !{} to channel '{}'", task_id, channel);
                if let Ok(mut task) = state.task_store.load(&task_id) {
                    task.channel = Some(channel.clone());
                    if let Err(e) = state.task_store.save(&task) {
                        warn!("Failed to update task {} channel: {}", task_id, e);
                    }
                }
            }
            Effect::UnassignTask {
                task_id,
                dir_key: _dir_key,
            } => {
                if let Err(e) = state.task_store.unassign_task(&task_id) {
                    warn!("Failed to unassign task !{}: {}", task_id, e);
                } else {
                    info!(
                        "Unassigned task !{} (PR in review, freeing coworker name)",
                        task_id
                    );
                    state.clear_task_assignment_by_task(&task_id).await;
                }
            }
            Effect::ResetAbandonedTask {
                task_id,
                pr_number,
                dir_key: _,
            } => {
                if let Err(e) = state.task_store.reset_task_to_pending(&task_id) {
                    warn!(
                        "Failed to reset abandoned task !{} (PR #{} closed): {}",
                        task_id, pr_number, e
                    );
                } else {
                    info!(
                        "Reset task !{} to pending (PR #{} closed without merge)",
                        task_id, pr_number
                    );
                    state.clear_task_assignment_by_task(&task_id).await;
                    // Post to ops channel — PR closure is daemon operational info
                    let mut msg = crate::message::Message::system(format!(
                        "PR #{} closed without merge. Task !{} reset to pending.",
                        pr_number, task_id
                    ));
                    msg.channel = Some(OPS_CHANNEL.to_string());
                    if let Err(e) = state.send_and_broadcast_async(&msg).await {
                        warn!("Failed to post abandoned task message: {}", e);
                    }
                }
            }
            Effect::CreateTask {
                dir_key: _,
                subject,
                description,
                pr,
            } => {
                // If a PR number is associated, skip creation if a non-completed task
                // already exists for that PR. This prevents duplicate follow-up tasks
                // when multiple review comments arrive in quick succession (e.g., after
                // a daemon restart resets the in-memory cooldown).
                if let Some(pr_num) = pr {
                    let existing = state.task_store.load_all();
                    if create_task_duplicate_exists(&existing, pr_num) {
                        debug!(
                            "Skipping CreateTask for PR #{}: non-completed task already exists",
                            pr_num
                        );
                        continue;
                    }
                }

                let task_id = state.task_store.next_task_id().to_string();
                let store_task = crate::task_store::Task {
                    id: task_id.clone(),
                    subject: subject.clone(),
                    status: crate::task_store::TaskStatus::Pending,
                    description: Some(description.clone()),
                    blocked_by: vec![],
                    channel: None,
                    pr,
                    agent_name: String::new(),
                    agent_type: "midtown-code-author".to_string(),
                    session_id: None,
                    parent: None,
                    message_id: None,
                    thread_id: None,
                    model: None,
                    plan: None,
                    placeholder_comment_id: None,
                    color: None,
                    icon: None,
                    restart_count: 0,
                    execution_skill: None,
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                };
                match state.task_store.save(&store_task) {
                    Ok(()) => {
                        info!("Created task !{}: {}", task_id, subject);
                        let channel = state.default_channel_name();
                        let msg = crate::daemon::rpc_task::task_announcement_message(
                            channel, "lead", &subject, None,
                        );
                        let message_id = msg.id.clone();
                        match state.send_and_broadcast_async(&msg).await {
                            Ok(()) => {
                                // Update TaskStore with message/thread IDs
                                if let Ok(mut store_task) = state.task_store.load(&task_id) {
                                    store_task.message_id = Some(message_id.clone());
                                    if store_task.thread_id.is_none() {
                                        store_task.thread_id = Some(message_id.clone());
                                    }
                                    if let Err(e) = state.task_store.save(&store_task) {
                                        warn!(
                                            "Failed to update task {} message_id: {}",
                                            task_id, e
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                warn!("Failed to post task creation message: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Failed to create task '{}': {}", subject, e);
                    }
                }
            }
            Effect::SaveChannelLeadSession {
                channel_name,
                session_id,
            } => {
                let mut ps = state.persistent_state.lock().await;
                ps.channel_lead_sessions
                    .insert(channel_name.clone(), session_id.clone());
                debug!(
                    "Saved channel lead session for '{}': {}",
                    channel_name, session_id
                );
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to save daemon state after saving channel lead session: {}",
                        e
                    );
                }
            }
            Effect::RespawnChannelLead { channel_name } => {
                // Refresh the worktree to origin/<default_branch> BEFORE spawning.
                // This prevents crash loops caused by stale worktree contents —
                // if old code crashes the lead on startup, spawning into the same
                // stale worktree would repeat the crash indefinitely.
                let worktree_path =
                    crate::paths::worktrees_dir_for_repo(state.paths.dir_key()).join(&channel_name);
                refresh_channel_lead_worktree(&worktree_path, &state.default_branch).await;

                let base_dir = state.paths.base_dir().to_path_buf();
                let project_root = state.all_repo_paths.first().cloned().unwrap_or_default();
                let dir_key = state.paths.dir_key().to_string();
                let (wf_name, wf_state_summary, suppress_auto_output) = {
                    let ps = state.persistent_state.lock().await;
                    let wf = ps.channel_workflows.get(&channel_name).cloned();
                    let wfs = ps
                        .workflow_state
                        .get(&channel_name)
                        .map(format_workflow_state_summary);
                    let suppress = ps
                        .channel_settings
                        .get(&channel_name)
                        .is_some_and(|s| !s.show_full_lead_output);
                    (wf, wfs, suppress)
                };
                let channel_directory =
                    crate::paths::read_channel_directory(state.paths.dir_key(), &channel_name);
                let (domain_context, agents_md) = load_channel_lead_context(
                    base_dir,
                    &channel_name,
                    project_root,
                    &dir_key,
                    wf_name,
                    state.paths.workflows_dir(),
                    wf_state_summary,
                )
                .await;

                let mut config = crate::launch::LaunchConfig::channel_lead(
                    &channel_name,
                    state.paths.dir_key(),
                    crate::launch::SessionMode::Fresh,
                    &domain_context,
                    agents_md,
                );
                config.model = super::helpers::resolve_model_for_role(
                    state.paths.dir_key(),
                    config.auth_provider,
                    &config.agent_type,
                );
                config.cwd_subdir = channel_directory;
                config.suppress_auto_output = suppress_auto_output;

                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Respawned channel lead '{}' successfully", name);
                    }
                    Err(e) => {
                        warn!("Failed to respawn channel lead '{}': {}", name, e);
                    }
                }
            }
            // ── Session-centric effects ──────────────────────────────────
            Effect::ShutdownSession { session_id, reason } => {
                // Look up name from persistent state
                let name = {
                    let ps = state.persistent_state.lock().await;
                    ps.sessions.get(&session_id).map(|s| s.name.clone())
                };
                if let Some(name) = name {
                    info!(
                        "ShutdownSession: shutting down session {} (name: {}, reason: {})",
                        session_id, name, reason
                    );
                    // shutdown_coworker_impl → cleanup_coworker_state handles all
                    // cleanup: reverse maps and SessionRecord update in
                    // persistent state.
                    let _ = shutdown_coworker_impl(&name, &reason, state).await;

                    state.broadcast_coworker_update(&name, "stopped", None, None, None, None);
                } else {
                    // No name mapped — session may have already been partially
                    // cleaned up. Still mark SessionRecord as stopped
                    // so persistent state doesn't show a stale is_running=true.
                    warn!(
                        "ShutdownSession: no name found for session {} — marking record as stopped",
                        session_id
                    );
                    let mut ps = state.persistent_state.lock().await;
                    if let Some(record) = ps.sessions.get_mut(&session_id) {
                        record.is_running = false;
                    }
                    // Close any open task-session spans for the shutting-down session.
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!(
                            "Failed to save persistent state after ShutdownSession for {}: {}",
                            session_id, e
                        );
                    }
                }
            }

            // ── Unified nudge effects ─────────────────────────────────────
            Effect::NudgeChannelLead {
                channel_name,
                reason,
            } => {
                let default_channel = state.channel_router.default_channel_name();
                if channel_name == default_channel {
                    state.nudge_lead(&reason.to_nudge_message()).await;
                } else if let Some(agent_name) = channel_name.strip_prefix("dm-") {
                    // DM channel: resolve the agent type and use the appropriate
                    // resume mechanism (project lead, channel lead, fork, coworker).
                    let msg = reason.to_nudge_message();

                    // 1. Try to nudge the active session (covers all types)
                    let session_id = {
                        let ps = state.persistent_state.lock().await;
                        ps.session_by_name(agent_name)
                            .filter(|s| s.is_running)
                            .map(|s| s.session_id.clone())
                    };
                    let mut nudge_delivered = false;

                    if let Some(ref sid) = session_id
                        && state
                            .session_manager
                            .send_message_to_session_id(sid, &msg)
                            .await
                            .is_ok()
                    {
                        nudge_delivered = true;
                    }

                    if nudge_delivered {
                        continue;
                    }

                    // 2. No active session — determine agent type and fall back.
                    if agent_name == state.project_name {
                        // Project lead: use headed intercom fallback
                        state.nudge_lead(&msg).await;
                    } else if state
                        .persistent_state
                        .lock()
                        .await
                        .channel_lead_sessions
                        .contains_key(agent_name)
                    {
                        // Channel lead: re-emit NudgeChannelLead with the topic
                        // channel name (stripping dm- prefix) so the existing
                        // channel lead resume/spawn machinery handles it.
                        Box::pin(execute_effects(
                            vec![Effect::NudgeChannelLead {
                                channel_name: agent_name.to_string(),
                                reason,
                            }],
                            state,
                        ))
                        .await;
                    } else {
                        let is_fork = {
                            let ps = state.persistent_state.lock().await;
                            ps.session_by_name(agent_name)
                                .is_some_and(|s| s.is_fork_session())
                        };
                        if is_fork {
                            // Fork: dead forks stay dead — no auto-respawn.
                            warn!(
                                "NudgeChannelLead for DM '{}': fork '{}' is dead, not respawning",
                                channel_name, agent_name
                            );
                        } else {
                            // Coworker fallback: find a stored SessionRecord and resume
                            let stored_record = {
                                let ps = state.persistent_state.lock().await;
                                ps.sessions
                                    .iter()
                                    .find(|(_, r)| r.name == agent_name)
                                    .map(|(sid, r)| (sid.clone(), r.clone()))
                            };

                            if let Some((stored_session_id, record)) = stored_record {
                                let mut config = crate::launch::LaunchConfig::coworker(
                                    agent_name,
                                    state.paths.dir_key(),
                                    crate::launch::SessionMode::ResumeSession(
                                        stored_session_id.clone(),
                                    ),
                                    Some(msg),
                                    record.task_id.clone(),
                                );
                                config.working_dir = Some(record.working_dir.clone().into());

                                match spawn_with_resume_fallback(
                                    state,
                                    state.paths.dir_key(),
                                    &mut config,
                                )
                                .await
                                {
                                    Ok((new_session_id, _resumed)) => {
                                        info!(
                                            "Resumed coworker '{}' for DM nudge (session: {})",
                                            agent_name, new_session_id
                                        );
                                    }
                                    Err(e) => {
                                        warn!(
                                            "Failed to resume coworker '{}' for DM nudge: {}",
                                            agent_name, e
                                        );
                                    }
                                }
                            } else {
                                warn!(
                                    "NudgeChannelLead for DM '{}': no active session or stored record for '{}'",
                                    channel_name, agent_name
                                );
                            }
                        }
                    }
                } else {
                    let session_name = crate::launch::channel_lead_session_name(&channel_name);
                    let msg = reason.to_nudge_message();
                    let channel_lead_nudgeable =
                        state.session_manager.is_nudgeable(&session_name).await;
                    if !channel_lead_nudgeable {
                        info!(
                            "Channel lead '{}' is not nudgeable — skipping nudge, will attempt resume/respawn",
                            session_name
                        );
                    }
                    let session_id = {
                        let ps = state.persistent_state.lock().await;
                        ps.channel_lead_sessions.get(&channel_name).cloned()
                    };
                    let mut nudge_delivered = false;

                    // First, try to nudge the stored session_id for this channel lead.
                    // This avoids name collision bugs where a coworker shares the same
                    // name as the channel lead and would steal nudges.
                    if channel_lead_nudgeable
                        && let Some(stored_session_id) =
                            session_id.as_deref().filter(|id| !id.is_empty())
                    {
                        if let Err(e) = state
                            .session_manager
                            .send_message_to_session_id(stored_session_id, &msg)
                            .await
                        {
                            warn!(
                                "Failed to nudge channel lead '{}' using stored session_id '{}': {}",
                                channel_name, stored_session_id, e
                            );
                        } else {
                            nudge_delivered = true;
                        }
                    }

                    // If the stored mapping was missing or stale, use the active
                    // session currently attached to this lead name (if any), and
                    // sync the mapping from that. This keeps mappings fresh after
                    // Codex app-server reuse.
                    #[allow(clippy::collapsible_if)]
                    if channel_lead_nudgeable && !nudge_delivered {
                        if let Some(active_session_id) =
                            state.session_manager.get_session_id(&session_name).await
                        {
                            if let Err(e) = state
                                .session_manager
                                .send_message_to_session_id(&active_session_id, &msg)
                                .await
                            {
                                warn!(
                                    "Failed to nudge channel lead '{}' using active session_id '{}': {}",
                                    channel_name, active_session_id, e
                                );
                            } else {
                                nudge_delivered = true;
                                if !matches!(
                                    session_id.as_deref(),
                                    Some(stored) if stored == active_session_id
                                ) {
                                    let mut ps = state.persistent_state.lock().await;
                                    if let Some(stored_session_id) = session_id.as_deref() {
                                        if stored_session_id.is_empty() {
                                            warn!(
                                                "Refreshed empty channel lead session mapping for '{}' to {}",
                                                channel_name, active_session_id
                                            );
                                        } else {
                                            warn!(
                                                "Refreshed stale channel lead mapping for '{}' from {} to {}",
                                                channel_name, stored_session_id, active_session_id
                                            );
                                        }
                                    } else {
                                        info!(
                                            "Initialized channel lead mapping for '{}' to {}",
                                            channel_name, active_session_id
                                        );
                                    }

                                    ps.channel_lead_sessions
                                        .insert(channel_name.clone(), active_session_id.clone());
                                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                        warn!(
                                            "Failed to update channel lead session mapping for '{}': {}",
                                            channel_name, e
                                        );
                                    }
                                }
                            }
                        }
                    }

                    if nudge_delivered {
                        continue;
                    }

                    let can_resume_channel_lead = match session_id.as_deref() {
                        Some(id) if should_resume_channel_lead_session(id) => {
                            let ps = state.persistent_state.lock().await;
                            ps.sessions.contains_key(id)
                        }
                        _ => false,
                    };

                    let (wf_name, wf_state_summary) = {
                        let ps = state.persistent_state.lock().await;
                        let wf = ps.channel_workflows.get(&channel_name).cloned();
                        let wfs = ps
                            .workflow_state
                            .get(&channel_name)
                            .map(format_workflow_state_summary);
                        (wf, wfs)
                    };
                    let (domain_context, agents_md) = load_channel_lead_context(
                        state.paths.base_dir().to_path_buf(),
                        &channel_name,
                        state.all_repo_paths.first().cloned().unwrap_or_default(),
                        state.paths.dir_key(),
                        wf_name,
                        state.paths.workflows_dir(),
                        wf_state_summary,
                    )
                    .await;

                    // Refresh the worktree before spawning/resuming to prevent
                    // stale code from crashing the channel lead on startup.
                    let worktree_path = crate::paths::worktrees_dir_for_repo(state.paths.dir_key())
                        .join(&channel_name);
                    refresh_channel_lead_worktree(&worktree_path, &state.default_branch).await;

                    match (session_id.as_deref(), can_resume_channel_lead) {
                        (Some(id), true) => {
                            let mut config = crate::launch::LaunchConfig::channel_lead(
                                &channel_name,
                                state.paths.dir_key(),
                                crate::launch::SessionMode::ResumeSession(id.to_string()),
                                &domain_context,
                                agents_md.clone(),
                            );
                            config.initial_prompt = Some(reason.to_initial_prompt(&channel_name));
                            config.cwd_subdir = crate::paths::read_channel_directory(
                                state.paths.dir_key(),
                                &channel_name,
                            );

                            match spawn_with_resume_fallback(
                                state,
                                state.paths.dir_key(),
                                &mut config,
                            )
                            .await
                            {
                                Ok((resumed_session_id, _)) => {
                                    let active_session_id = state
                                        .session_manager
                                        .get_session_id(&session_name)
                                        .await
                                        .filter(|active_id| !active_id.is_empty())
                                        .unwrap_or_else(|| resumed_session_id.clone());

                                    {
                                        let mut ps = state.persistent_state.lock().await;
                                        ps.channel_lead_sessions.insert(
                                            channel_name.clone(),
                                            active_session_id.clone(),
                                        );
                                        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                            tracing::error!(
                                                "Failed to save state after channel lead resume/fallback: {}",
                                                e
                                            );
                                        }
                                    }

                                    if let Err(e) = state
                                        .session_manager
                                        .send_message_to_session_id(&active_session_id, &msg)
                                        .await
                                    {
                                        warn!(
                                            "Nudge after resume failed for '{}' — clearing stale mapping: {}",
                                            channel_name, e
                                        );
                                        let mut ps = state.persistent_state.lock().await;
                                        ps.channel_lead_sessions
                                            .insert(channel_name.clone(), String::new());
                                        if let Err(save_err) =
                                            ps.save_for_repo(state.paths.dir_key())
                                        {
                                            warn!(
                                                "Failed to clear stale channel lead session ID for '{}': {}",
                                                channel_name, save_err
                                            );
                                        }
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to resume channel lead '{}': {}",
                                        channel_name, e
                                    );
                                    {
                                        let mut ps = state.persistent_state.lock().await;
                                        ps.channel_lead_sessions
                                            .insert(channel_name.clone(), String::new());
                                        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                            warn!(
                                                "Failed to clear stale channel lead session ID for '{}': {}",
                                                channel_name, e
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            let mut config = crate::launch::LaunchConfig::channel_lead(
                                &channel_name,
                                state.paths.dir_key(),
                                crate::launch::SessionMode::Fresh,
                                &domain_context,
                                agents_md,
                            );
                            config.initial_prompt = Some(reason.to_initial_prompt(&channel_name));
                            config.cwd_subdir = crate::paths::read_channel_directory(
                                state.paths.dir_key(),
                                &channel_name,
                            );
                            // Insert empty placeholder before spawning to guard against
                            // duplicate NudgeChannelLead effects in the same batch.
                            {
                                let mut ps = state.persistent_state.lock().await;
                                ps.channel_lead_sessions
                                    .insert(channel_name.clone(), String::new());
                                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                    tracing::error!(
                                        "Failed to save state before spawning channel lead: {}",
                                        e
                                    );
                                }
                            }
                            match spawn_with_resume_fallback(
                                state,
                                state.paths.dir_key(),
                                &mut config,
                            )
                            .await
                            {
                                Ok((session_id, _)) => {
                                    // Update channel_lead_sessions with the real session_id
                                    // immediately (spawn_coworker generated it upfront),
                                    // eliminating the race window before init event arrives.
                                    let mut ps = state.persistent_state.lock().await;
                                    ps.channel_lead_sessions
                                        .insert(channel_name.clone(), session_id);
                                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                        tracing::error!(
                                            "Failed to save state after spawning channel lead: {}",
                                            e
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::error!(
                                        "Failed to spawn channel lead '{}': {}",
                                        channel_name,
                                        e
                                    );
                                    // Keep the empty placeholder in channel_lead_sessions
                                    // on failure. This allows a fresh spawn attempt on a
                                    // later nudge and preserves restart visibility.
                                }
                            }
                        }
                    }
                }
            }
            Effect::NudgeSession { session_id, reason } => {
                if let Some(follow_up) = send_session_nudge(state, &session_id, &reason).await {
                    Box::pin(execute_effects(follow_up, state)).await;
                }
            }

            Effect::RecordSession { record } => {
                let session_id = record.session_id.clone();

                if session_id.is_empty() {
                    warn!(
                        "RecordSession: skipping record with empty session_id (name: {})",
                        record.name
                    );
                } else {
                    // Persist session record
                    {
                        let mut ps = state.persistent_state.lock().await;
                        ps.sessions.insert(session_id.clone(), *record);
                        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                            warn!("Failed to save persistent state after RecordSession: {}", e);
                        }
                    }
                    info!("RecordSession: saved session {}", session_id);
                }
            }

            Effect::AutoDetachCoworker { name } => {
                // Clear from attached set — must happen before next tick so
                // ensure_lead_alive() sees the lead as detached and can respawn.
                // Do NOT record stop time: we want immediate respawn, not the
                // 5-minute LEAD_RESPAWN_COOLDOWN.
                {
                    let mut attached = state.attached_coworkers.lock().unwrap();
                    attached.remove(&name);
                }
                warn!(
                    "Auto-detached stale attached session for '{}' (no detach received within timeout)",
                    name
                );
                let is_channel_lead = {
                    let ps = state.persistent_state.lock().await;
                    ps.channel_lead_sessions.contains_key(name.as_str())
                };
                let suffix =
                    auto_detach_suffix_message(name.as_str(), &state.project_name, is_channel_lead);
                let mut msg = crate::message::Message::system(format!(
                    "⚠️ Auto-detached stale attached session for {} — interactive session ended without detach.{}",
                    name, suffix
                ));
                msg.channel = Some(OPS_CHANNEL.to_string());
                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                    warn!("Failed to post auto-detach message: {}", e);
                }
            }

            Effect::PostPrComment {
                pr_number,
                reviewer_name,
                body,
            } => {
                post_pr_comment(state, pr_number, &reviewer_name, &body).await;
            }

            Effect::MergePr { pr_number, title } => {
                auto_merge_pr(state, pr_number, &title).await;
            }

            Effect::AutoMergePr { pr_number, title } => {
                auto_merge_pr(state, pr_number, &title).await;
            }

            Effect::PostInsight { agent, insight } => {
                post_insight(state, &agent, &insight).await;
            }

            Effect::EmitWorkflowEvent(event) => {
                // Lead-driven mode: relay the event as a human-readable @mention
                // to the channel lead instead of dispatching to the Python plugin.
                let channel = event.channel().to_string();
                let is_lead_driven = {
                    let ps = state.persistent_state.lock().await;
                    ps.lead_driven_channels.contains(&channel)
                };

                if is_lead_driven {
                    if let Some(msg) = event.format_for_lead() {
                        let nudge_msg = format!("@{} {}", channel, msg);
                        Box::pin(execute_effects(
                            vec![
                                Effect::post_to_channel(
                                    "midtown",
                                    nudge_msg.clone(),
                                    Some(channel.clone()),
                                ),
                                Effect::nudge_channel_lead(channel, nudge_msg),
                            ],
                            state,
                        ))
                        .await;
                    }
                } else {
                    let _default_prevented = dispatch_workflow_event(state, event).await;
                    // When default_prevented is true, the plugin has taken full ownership
                    // of this event — compiled-in behavior is suppressed.
                }
            }

            Effect::TaskPrompt {
                task_id,
                message,
                model,
                pr_context,
            } => {
                let result = super::rpc_task::deliver_task_prompt(
                    &task_id,
                    &message,
                    "midtown", // daemon auto-pilot is always "midtown"
                    model.as_deref(),
                    state,
                )
                .await;

                // Clear in-flight marker so the task can be retried on future ticks.
                state.clear_task_spawn_in_flight(&task_id);

                match (&result, &pr_context) {
                    (Ok(_), Some(ctx)) => {
                        info!(
                            "TaskPrompt delivered for task !{} (PR #{}, {})",
                            task_id, ctx.pr_number, ctx.issue_type
                        );
                    }
                    (Err(e), _) => {
                        warn!("TaskPrompt failed for task !{}: {}", task_id, e);
                        // Post failure to ops channel
                        let fail_effect = Effect::PostToChannel {
                            sender: "midtown".to_string(),
                            message: format!(
                                "Failed to deliver prompt to task !{}: {}",
                                task_id, e
                            ),
                            channel: Some(OPS_CHANNEL.to_string()),
                            auto_output: false,
                            message_type: None,
                            nudge_type: None,
                            tool_data: None,
                            provider: None,
                            tool_use_id: None,
                            parent_tool_use_id: None,
                        };
                        // Execute inline via Box::pin (breaks async recursion cycle)
                        Box::pin(execute_effects(vec![fail_effect], state)).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Re-run a GitHub Actions workflow using `gh run rerun`.
///
/// Posts a channel message on success or failure.
async fn rerun_workflow(state: &DaemonState, run_id: u64, check_name: &str, pr_number: u64) {
    // Record cooldown before attempting (to prevent rapid retries on failure)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.ci_stats.record_rerun(run_id);
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!("Failed to save CI stats after recording rerun: {}", e);
        }
    }

    let output = match tokio::process::Command::new("gh")
        .args(["run", "rerun", &run_id.to_string()])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            warn!("Failed to run gh run rerun for workflow {}: {}", run_id, e);
            return;
        }
    };

    if output.status.success() {
        info!(
            "Re-ran workflow {} (check '{}') for PR #{}",
            run_id, check_name, pr_number
        );
        let msg = Message::for_channel(
            state.channel_router.default_channel_name(),
            "midtown",
            format!(
                "🔄 Re-running stale CI check '{}' on PR #{} (workflow {})",
                check_name, pr_number, run_id
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast_async(&msg).await {
            warn!("Failed to post workflow rerun message: {}", e);
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "gh run rerun failed for workflow {}: {}",
            run_id,
            stderr.trim()
        );
    }
}

/// Look up an existing placeholder comment ID for a PR using the 3-tier lookup:
///
/// 1. **Persistent state** — `task_placeholder_comment_id` (via active reviewer spans)
/// 2. **In-memory cache** — `reviewer_placeholder_cache` (TTL-based)
/// 3. **GitHub API fallback** — `pr_in_progress_placeholder_comment_id` via `spawn_blocking`
///
/// This reuses the same lookup infrastructure as `collect_world_snapshot` in
/// `snapshot.rs`, avoiding divergent detection criteria and pagination issues.
async fn lookup_existing_placeholder(state: &DaemonState, pr_number: u64) -> Option<u64> {
    // Tier 1: Check TaskStore for placeholder_comment_id on reviewer tasks.
    {
        let ps = state.persistent_state.lock().await;
        let task_id = ps
            .active_reviewer_sessions()
            .iter()
            .filter(|s| s.pr_number == Some(pr_number))
            .find_map(|s| s.task_id.clone());
        drop(ps);
        if let Some(tid) = task_id
            && let Ok(task) = state.task_store.load(&tid)
            && let Some(id) = task.placeholder_comment_id
        {
            return Some(id);
        }
    }

    // Tier 2: Check in-memory cache
    const PLACEHOLDER_CACHE_TTL_SECS: u64 = 120;
    let cached = {
        let cache = state.reviewer_placeholder_cache.lock().unwrap();
        cache.get(&pr_number).copied()
    };

    match cached {
        Some((id, checked_at)) if checked_at.elapsed().as_secs() < PLACEHOLDER_CACHE_TTL_SECS => {
            return id; // Use cached result within TTL
        }
        _ => {}
    }

    // Tier 3: Cache miss or expired — fetch from GitHub via spawn_blocking
    let id = tokio::task::spawn_blocking(move || {
        super::pr::pr_in_progress_placeholder_comment_id(pr_number)
    })
    .await
    .ok()
    .flatten();

    // Update cache with result
    {
        let mut cache = state.reviewer_placeholder_cache.lock().unwrap();
        cache.insert(pr_number, (id, std::time::Instant::now()));
    }

    id
}

/// Post a "Review in progress" placeholder comment on a PR.
///
/// Uses `gh api --method PATCH` to edit an existing placeholder or
/// `gh pr comment` to create a new one. Stores the comment ID on the
/// task metadata so the daemon can later update the placeholder
/// with the final review via `pr.review-post`.
///
/// When a placeholder comment already exists on the PR (from a previous
/// reviewer cycle that timed out), the existing comment is edited in-place
/// rather than creating a new one. This prevents placeholder accumulation.
async fn post_pr_comment(state: &DaemonState, pr_number: u64, reviewer_name: &str, body: &str) {
    let repo_path = state.all_repo_paths.first().cloned();

    // Check for an existing placeholder comment on the PR to reuse.
    // This prevents placeholder accumulation when reviewers are re-spawned after timeout.
    let existing_placeholder_id = lookup_existing_placeholder(state, pr_number).await;

    let comment_id = if let Some(existing_id) = existing_placeholder_id {
        // Edit the existing placeholder comment via gh api PATCH
        let repo_full_name = repo_path
            .as_deref()
            .map(|p| state.get_repo_full_name(p))
            .unwrap_or_default();

        if repo_full_name.is_empty() {
            warn!(
                "Cannot edit placeholder comment: repo_full_name is empty for PR #{}",
                pr_number
            );
            return;
        }

        let endpoint = format!("/repos/{}/issues/comments/{}", repo_full_name, existing_id);
        let output = tokio::process::Command::new("gh")
            .args([
                "api",
                "--method",
                "PATCH",
                &endpoint,
                "-f",
                &format!("body={}", body),
            ])
            .output()
            .await;

        match output {
            Ok(out) if out.status.success() => {
                info!(
                    "Edited existing placeholder comment {} on PR #{} for reviewer {}",
                    existing_id, pr_number, reviewer_name
                );
                Some(existing_id)
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                warn!(
                    "Failed to edit placeholder comment {} on PR #{}: {}",
                    existing_id,
                    pr_number,
                    stderr.trim()
                );
                return;
            }
            Err(e) => {
                warn!(
                    "Failed to edit placeholder comment {} on PR #{}: {}",
                    existing_id, pr_number, e
                );
                return;
            }
        }
    } else {
        // No existing placeholder — create a new comment
        let mut cmd = tokio::process::Command::new("gh");
        cmd.args(["pr", "comment", &pr_number.to_string(), "--body", body]);
        if let Some(ref path) = repo_path {
            cmd.current_dir(path);
        }

        let output = match cmd.output().await {
            Ok(output) => output,
            Err(e) => {
                warn!(
                    "Failed to post placeholder comment on PR #{}: {}",
                    pr_number, e
                );
                return;
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "gh pr comment failed for PR #{}: {}",
                pr_number,
                stderr.trim()
            );
            return;
        }

        // Parse comment ID from the URL in stdout (e.g., "https://github.com/.../issuecomment-12345")
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .trim()
            .rsplit('/')
            .next()
            .and_then(|segment| {
                // Handle both "issuecomment-12345" and bare "12345" formats
                segment
                    .strip_prefix("issuecomment-")
                    .or(Some(segment))
                    .and_then(|s| s.trim().parse::<u64>().ok())
            })
            .or_else(|| {
                // Fallback: find any trailing number in the URL
                stdout
                    .trim()
                    .rsplit(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
            })
    };

    if let Some(comment_id) = comment_id {
        info!(
            "Posted placeholder comment {} on PR #{} for reviewer {}",
            comment_id, pr_number, reviewer_name
        );

        // Store the comment ID in TaskStore.
        {
            let ps = state.persistent_state.lock().await;
            let task_ids: Vec<String> = ps
                .active_reviewer_sessions()
                .iter()
                .filter(|s| s.pr_number == Some(pr_number))
                .filter_map(|s| s.task_id.clone())
                .collect();
            drop(ps);
            // Write to TaskStore (primary)
            for tid in &task_ids {
                if let Ok(mut task) = state.task_store.load(tid) {
                    task.placeholder_comment_id = Some(comment_id);
                    if let Err(e) = state.task_store.save(&task) {
                        warn!(
                            "Failed to save placeholder comment ID on task {}: {}",
                            tid, e
                        );
                    }
                }
            }
        }

        // Populate the placeholder cache so snapshot doesn't need an API call
        {
            let mut cache = state.reviewer_placeholder_cache.lock().unwrap();
            cache.insert(pr_number, (Some(comment_id), std::time::Instant::now()));
        }
    } else {
        warn!(
            "Could not parse comment ID from gh output for PR #{}",
            pr_number
        );
    }
}

/// Auto-merge a PR using `gh pr merge --squash --auto`.
///
/// Posts a channel message on success or failure.
///
/// Invoked by two paths:
/// - `Effect::MergePr` — after the `pr.merge` RPC verifies all gates (reviewer, review, CI, feedback).
/// - `Effect::AutoMergePr` — proactively from the stuck-PR polling path in `pr.rs`, gated on
///   `is_auto_mergeable()` (approved + CI green) AND no active daemon-assigned reviewer.
async fn auto_merge_pr(state: &DaemonState, pr_number: u64, title: &str) {
    use super::helpers::truncate_str;

    // Use the first repo path for current_dir so `gh` can identify the target repo
    let repo_path = state.all_repo_paths.first().cloned();
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(["pr", "merge", &pr_number.to_string(), "--squash", "--auto"]);
    if let Some(ref path) = repo_path {
        cmd.current_dir(path);
    }
    let output = match cmd.output().await {
        Ok(output) => output,
        Err(e) => {
            warn!("Failed to run gh pr merge for PR #{}: {}", pr_number, e);
            return;
        }
    };

    if output.status.success() {
        info!("Auto-merge enabled for PR #{} ({})", pr_number, title);
        let msg = Message::for_channel(
            state.channel_router.default_channel_name(),
            "midtown",
            format!(
                "🤝 Auto-merge enabled for PR #{} ({}) — approved with all checks passing",
                pr_number,
                truncate_str(title, 40)
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast_async(&msg).await {
            warn!("Failed to post auto-merge message: {}", e);
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("gh pr merge failed for PR #{}: {}", pr_number, stderr);
        let msg = Message::for_channel(
            state.channel_router.default_channel_name(),
            "midtown",
            format!(
                "⚠️ Auto-merge failed for PR #{} ({}) — {}",
                pr_number,
                truncate_str(title, 40),
                truncate_str(stderr.trim(), 80)
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast_async(&msg).await {
            warn!("Failed to post auto-merge failure message: {}", e);
        }
    }
}

/// Dispatch a workflow event to the Python plugin daemon.
///
/// If the plugin daemon is running, sends the event over the Unix socket and
/// awaits a response containing plugin actions and a `default_prevented` flag.
/// Returned actions are converted to `Effect` variants and executed immediately.
///
/// Returns `true` if a plugin set `default_prevented`, meaning the daemon's
/// compiled-in behavior for this event should be suppressed. Returns `false`
/// if no plugins are configured, the daemon is unavailable, or no plugin
/// called `prevent_default()`.
///
/// If the daemon is not running or no plugins are configured, this is a no-op
/// (the daemon's compiled-in behavior runs unmodified).
///
/// On dispatch errors or daemon unavailability, a system message is posted to
/// the event's channel so failures are visible in the chat log.
async fn dispatch_workflow_event(
    state: &DaemonState,
    event: crate::workflow::WorkflowEvent,
) -> bool {
    let channel = event.channel().to_string();

    // Look up assigned workflow for this channel from persistent state.
    let workflow_name = {
        let ps = state.persistent_state.lock().await;
        ps.channel_workflows.get(&channel).cloned()
    };

    let Some(workflow_name) = workflow_name else {
        // No workflow assigned — daemon defaults run.
        return false;
    };

    // Ensure Python daemon is running.
    if !state.plugin_daemon.ensure_running().await {
        warn!(
            channel = %channel,
            workflow = %workflow_name,
            "dispatch_workflow_event: plugin daemon could not be started"
        );
        post_plugin_error(
            state,
            &channel,
            &format!(
                "Plugin daemon could not be started for workflow `{}` — event was not processed.",
                workflow_name
            ),
        )
        .await;
        return false;
    }

    // Build the request JSON matching the Python daemon's expected format.
    // The Python `_process_request` expects: {"type": "pr.opened", "event": {...}, ...}
    let event_json = match serde_json::to_value(&event) {
        Ok(val) => val,
        Err(e) => {
            warn!(
                "Failed to serialize WorkflowEvent for channel '{}': {}",
                channel, e
            );
            return false;
        }
    };

    let event_type = event_json
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let request = serde_json::json!({
        "type": event_type,
        "event": event_json,
        "task_id": event.task_id(),
        "channel_workflow": workflow_name,
    });
    let request_str = match serde_json::to_string(&request) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to serialize plugin dispatch request: {}", e);
            return false;
        }
    };

    let result = state.plugin_daemon.send_event(&request_str).await;

    let Some(dispatch_result) = result else {
        // Plugin daemon connection failed after ensure_running succeeded.
        // Post an error so the failure is visible.
        warn!(
            channel = %channel,
            event_type = %event_type,
            workflow = %workflow_name,
            "dispatch_workflow_event: plugin daemon unavailable, event dropped"
        );
        post_plugin_error(
            state,
            &channel,
            &format!(
                "Plugin daemon unavailable for event `{}` (workflow `{}`) — event was not processed.",
                event_type, workflow_name
            ),
        )
        .await;
        return false;
    };

    if !dispatch_result.ok {
        let error_msg = dispatch_result
            .error
            .unwrap_or_else(|| "unknown error".to_string());
        warn!(
            channel = %channel,
            event_type = %event_type,
            "dispatch_workflow_event: plugin dispatch error: {}",
            error_msg
        );
        post_plugin_error(state, &channel, &error_msg).await;
        return false;
    }

    debug!(
        channel = %channel,
        event_type = %event_type,
        action_count = dispatch_result.actions.len(),
        default_prevented = dispatch_result.default_prevented,
        "dispatch_workflow_event: received plugin response"
    );

    // Convert plugin actions to Effect variants and execute them.
    let effects = plugin_actions_to_effects(&dispatch_result.actions, state).await;
    if !effects.is_empty() {
        // Use Box::pin to execute effects recursively without growing the stack.
        Box::pin(execute_effects(effects, state)).await;
    }

    dispatch_result.default_prevented
}

/// Convert a list of plugin actions (from the Python daemon) to Effect variants.
///
/// Each `PluginAction` has a `method` (RPC method name like `"channel.post"`)
/// and `params` (JSON object with method-specific arguments). This maps them
/// to the corresponding `Effect` variants in the daemon's effect pipeline.
///
/// Unknown methods are logged and skipped.
async fn plugin_actions_to_effects(
    actions: &[super::plugin_daemon::PluginAction],
    state: &DaemonState,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for action in actions {
        match action.method.as_str() {
            "channel.post" => {
                let message = action
                    .params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if message.is_empty() {
                    debug!("plugin_actions_to_effects: channel.post with empty message, skipping");
                    continue;
                }
                let channel = action
                    .params
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                effects.push(Effect::PostSystemMessage { message, channel });
            }

            "coworker.nudge" => {
                let name = action
                    .params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let message = action
                    .params
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !name.is_empty() {
                    effects.push(Effect::nudge_session(
                        state.session_id_for_name(&name).await,
                        message,
                    ));
                }
            }

            "task.done" => {
                let task_id = action
                    .params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !task_id.is_empty() {
                    effects.push(Effect::CompleteTask {
                        task_id,
                        dir_key: state.paths.dir_key().to_string(),
                    });
                }
            }

            "pr.auto-merge" => {
                let pr_number = action
                    .params
                    .get("pr")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                if pr_number > 0 {
                    effects.push(Effect::AutoMergePr {
                        pr_number,
                        title: String::new(),
                    });
                }
            }

            "daemon.check-pending" => {
                // Trigger re-evaluation on the next tick — no dedicated effect needed.
                debug!("Plugin requested check-pending (will fire on next tick)");
            }

            other => {
                debug!(
                    method = other,
                    "plugin_actions_to_effects: unknown action method, skipping"
                );
            }
        }
    }

    effects
}

/// Post a plugin dispatch error to its channel as a system message.
async fn post_plugin_error(state: &DaemonState, channel: &str, detail: &str) {
    let mut msg = crate::message::Message::system(format!("⚠️ Plugin dispatch error: {}", detail));
    msg.channel = Some(channel.to_string());
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post plugin error message: {}", e);
    }
}

pub(crate) async fn spawn_with_resume_fallback(
    state: &DaemonState,
    dir_key: &str,
    config: &mut crate::launch::LaunchConfig,
) -> Result<(String, bool), String> {
    let resume_session_id = match &config.session_mode {
        crate::launch::SessionMode::ResumeSession(session_id) => Some(session_id.clone()),
        _ => None,
    };

    match state.spawn_coworker(config).await {
        Ok(session_id) => Ok((session_id, false)),
        Err(error) => {
            let Some(session_id) = resume_session_id else {
                return Err(error.to_string());
            };

            let prior_prompt = config
                .persisted_initial_prompt
                .clone()
                .or_else(|| config.initial_prompt.clone());

            if config.persisted_initial_prompt.is_none() {
                config.persisted_initial_prompt = prior_prompt.clone();
            }

            config.session_mode = crate::launch::SessionMode::Fresh;
            config.initial_prompt = Some(build_resume_handoff_prompt(
                &config.name,
                dir_key,
                &session_id,
                prior_prompt.as_deref(),
                config.working_dir.as_deref(),
            ));

            match state.spawn_coworker(config).await {
                Ok(session_id) => Ok((session_id, true)),
                Err(fallback_error) => {
                    Err(format!("{}; fallback failed: {}", error, fallback_error))
                }
            }
        }
    }
}

/// Choose the suffix for an auto-detach warning message.
///
/// The lead session (both canonical repo name and legacy "lead") gets a
/// respawn notice; channel leads get a channel-respawn notice; everyone
/// else gets a task-dispatch notice.
fn auto_detach_suffix_message(
    name: &str,
    project_name: &str,
    is_channel_lead: bool,
) -> &'static str {
    if super::helpers::is_project_lead(name, project_name) {
        " Headless session will respawn on the next tick."
    } else if is_channel_lead {
        " Channel lead session will be respawned for its channel."
    } else {
        " Session will be reassigned via normal task dispatch."
    }
}

/// Hash insight content for deduplication.
///
/// Normalizes whitespace and lowercases before hashing to catch near-duplicates.
fn hash_insight(insight: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let normalized: String = insight
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

/// Post an insight from a coworker's DM stream to the task's channel.
///
/// Handles deduplication (via `insight_hashes`), resolves the coworker's
/// task → channel + thread ID, posts the insight as a 💡 message, and
/// nudges the channel lead.
async fn post_insight(state: &DaemonState, agent: &str, insight: &str) {
    // Deduplicate via in-memory hash set.
    let hash = hash_insight(insight);
    {
        let mut hashes = state.insight_hashes.lock().unwrap();
        if !hashes.insert(hash) {
            debug!("post_insight: duplicate insight from {}, skipping", agent);
            return;
        }
    }

    // Suppress insights from channel leads (they auto-post all output).
    {
        let ps = state.persistent_state.lock().await;
        let is_channel_lead = ps
            .sessions
            .values()
            .any(|s| s.is_running && s.name == agent && s.agent_type == "midtown-channel-lead");
        if is_channel_lead {
            debug!(
                "post_insight: suppressing insight from channel lead {}, already auto-posted",
                agent
            );
            return;
        }
    }

    // Resolve channel and thread from the coworker's task binding.
    let (task_channel, task_thread_id): (Option<String>, Option<String>) = {
        let ps = state.persistent_state.lock().await;
        let task_id = ps
            .sessions
            .values()
            .find(|r| r.is_running && r.name == agent)
            .or_else(|| ps.sessions.values().find(|r| r.name == agent))
            .and_then(|r| r.task_id.as_deref());
        let (ch, thread) = if let Some(tid) = task_id {
            if let Ok(store_task) = state.task_store.load(tid) {
                (store_task.channel.clone(), store_task.thread_id.clone())
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };
        (ch, thread)
    };

    let channel_name: &str = task_channel
        .as_deref()
        .unwrap_or_else(|| state.channel_router.default_channel_name());

    // Only use the task thread if the final channel matches the task's channel.
    // When task_channel is None, the insight routes to the default channel —
    // which is also where the task announcement lives, so threading is correct.
    let resolved_thread_id = task_thread_id.filter(|_| {
        task_channel
            .as_ref()
            .is_none_or(|ch| ch.as_str() == channel_name)
    });

    let insight_content = format!("💡 {}", insight);
    let msg = if let Some(ref thread_id) = resolved_thread_id {
        Message::thread_reply(
            channel_name,
            agent,
            insight_content,
            thread_id,
            crate::message::MessageType::Text,
        )
    } else {
        Message::for_channel(
            channel_name,
            agent,
            insight_content,
            crate::message::MessageType::Text,
        )
    };
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("post_insight: failed to post to channel: {}", e);
        return;
    }

    info!(
        "post_insight: posted insight from {} to channel '{}'",
        agent, channel_name
    );

    // Nudge channel lead about the insight.
    let task_id = state.get_task_id_for_coworker(agent).await;
    let nudge_effect = Effect::NudgeChannelLead {
        channel_name: channel_name.to_string(),
        reason: super::wake_reason::WakeReason::InsightPosted {
            insight: insight.to_string(),
            agent: agent.to_string(),
            msg_id: msg.id.clone(),
            task_id,
            channel_name: channel_name.to_string(),
        },
    };
    Box::pin(execute_effects(vec![nudge_effect], state)).await;
}

/// Refresh a channel lead worktree to `origin/<default_branch>`.
///
/// Runs `git checkout --detach origin/<branch>` in the worktree directory.
/// The `git fetch` is already done by snapshot collection
/// (`collect_stale_channel_lead_worktrees`), so we only need the checkout.
///
/// This is called BEFORE spawning the channel lead to prevent crash loops
/// caused by stale worktree contents. If the refresh fails (e.g., worktree
/// doesn't exist yet), we log and continue — `create_detached_worktree` in
/// `prepare_spawn` will create a fresh one from `origin/<branch>` anyway.
async fn refresh_channel_lead_worktree(worktree_path: &std::path::Path, default_branch: &str) {
    if !worktree_path.exists() {
        debug!(
            "Channel lead worktree does not exist yet at {}, skipping refresh",
            worktree_path.display()
        );
        return;
    }

    let origin_ref = format!("origin/{}", default_branch);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new("git")
            .args(["checkout", "--detach", &origin_ref])
            .current_dir(worktree_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await;

    match result {
        Ok(Ok(output)) if output.status.success() => {
            info!(
                "Refreshed channel lead worktree at {} to {}",
                worktree_path.display(),
                origin_ref
            );
        }
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "Failed to refresh channel lead worktree at {} to {}: {}",
                worktree_path.display(),
                origin_ref,
                stderr.trim()
            );
        }
        Ok(Err(e)) => {
            warn!(
                "Failed to run git checkout in channel lead worktree at {}: {}",
                worktree_path.display(),
                e
            );
        }
        Err(_) => {
            warn!(
                "Timed out refreshing channel lead worktree at {}",
                worktree_path.display()
            );
        }
    }
}

#[path = "effects_tests.rs"]
#[cfg(test)]
mod tests;
