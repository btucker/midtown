use std::collections::HashSet;
use std::path::PathBuf;

use tracing::{debug, info, warn};

/// Maximum number of tool call/result items retained per agent in `recent_tool_items`.
const MAX_TOOL_ITEMS_PER_AGENT: usize = 20;

use super::DaemonState;
use super::constants::OPS_CHANNEL;
use super::trackers::PrIssueType;
use crate::message::Message;

async fn load_channel_lead_context(
    base_dir: PathBuf,
    channel_name: &str,
    project_root: PathBuf,
    dir_key: &str,
) -> (String, Option<String>, Vec<(String, String)>) {
    let channel = channel_name.to_string();
    let channel_for_warn = channel.clone();
    let dk = dir_key.to_string();
    tokio::task::spawn_blocking(move || {
        let notes = crate::channel::load_channel_notes(&base_dir, &channel);
        let agents = crate::paths::agents_md_for_channel(&channel, &project_root, &dk);
        let plugin_dirs = crate::paths::discover_plugin_dirs(&project_root, &dk, Some(&channel));
        let skills = crate::paths::collect_skill_md_bodies(&plugin_dirs);
        (notes, agents, skills)
    })
    .await
    .unwrap_or_else(|e| {
        warn!(
            "Channel lead discovery task failed for '{}': {}",
            channel_for_warn, e
        );
        (String::new(), None, vec![])
    })
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
    /// Deliver a message to a coworker via the agent teams mailbox.
    ///
    /// Uses the filesystem-based inbox (`~/.claude/teams/{team}/inboxes/{name}.json`)
    /// for non-urgent messages like task assignments and PR feedback. The coworker
    /// polls its inbox between turns, so delivery is not immediate.
    DeliverMailboxMessage {
        name: String,
        message: String,
        summary: Option<String>,
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
    /// **Thread resolution**: If the sender has an entry in `fork_bound_threads`,
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
        /// Structured tool call data for DM channel messages.
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
    },
    /// Broadcast universal event items to WebSocket clients.
    ///
    /// Sends structured tool call data to connected web/TUI clients for
    /// real-time visualization of agent activity.
    ///
    /// `channel` is `None` for the main lead (displayed in the main channel)
    /// or `Some(channel_name)` for a channel lead (displayed only in that topic channel).
    ///
    /// `thread_parent_id` is set for fork-bound sessions whose tool calls should appear
    /// in the thread panel rather than the main channel activity strip.
    BroadcastUniversalItems {
        agent_name: String,
        channel: Option<String>,
        thread_parent_id: Option<String>,
        items: Vec<crate::universal_events::UniversalItem>,
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
    /// `task_to_session` entry.
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
    /// Spawn a coworker for a pending task.
    ///
    /// Records an in-memory task assignment for busy tracking and writes
    /// ownership + in_progress status directly to disk.
    AssignAndSpawn {
        task_id: String,
        owner: String,
        dir_key: String,
        config: crate::launch::LaunchConfig,
        on_success: Vec<Effect>,
        on_failure: Vec<Effect>,
    },
    /// Mark reminders as fired and persist to disk.
    ///
    /// Defers the mutation from the decision phase to the effect executor,
    /// keeping `check_and_fire_reminders` pure.
    MarkRemindersFired {
        fired_ids: Vec<String>,
        dir_key: String,
    },
    /// Record a PR issue nudge in the tracker (prevents repeated nudges).
    RecordPrNudge {
        pr_number: u64,
        issue_type: PrIssueType,
    },
    /// Record an in-memory task assignment for busy tracking.
    ///
    /// Defers the mutation from the decision phase to the effect executor,
    /// keeping decision functions pure.
    RecordTaskAssignment { coworker: String, task_id: String },
    /// Clear a saved PR break session after successful resume.
    ClearPrBreakSession { name: String },
    /// Assign a reviewer to a PR in github_state and persist.
    AssignReviewer {
        pr_number: u64,
        reviewer_name: String,
        source: crate::github_state::AssignmentSource,
        /// How many times this reviewer has been restarted for this PR.
        /// Passed through to `GitHubState` for stuck reviewer backoff tracking.
        restart_count: u32,
        /// Claude session ID for the reviewer, if known.
        /// Initially `None` for optimistic assignments (before spawn completes);
        /// backfilled by `backfill_reviewer_session_ids()` during subsequent poll ticks.
        reviewer_session_id: Option<String>,
    },
    /// Remove a reviewer assignment for a specific PR.
    ///
    /// Used when a reviewer spawn fails after the assignment was already recorded
    /// (optimistic assignment to prevent race conditions).
    RemoveReviewerAssignment { pr_number: u64 },
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
    /// Clear reviewer assignments for orphaned coworkers (sessions that ended unexpectedly).
    ClearOrphanedReviewerAssignments { orphaned_coworkers: Vec<String> },
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
    /// Store a PR author's session ID for potential handoff.
    ///
    /// When a coworker opens a PR, we store their session ID so any other
    /// coworker can later resume work on that PR with full context preserved.
    /// Also extracts and stores the task ID from the PR title.
    StorePrAuthorSession {
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
    /// Send a push notification to the mobile PWA.
    ///
    /// Fire-and-forget: the push manager runs in a background task.
    SendPushNotification {
        title: String,
        body: String,
        tag: String,
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
    /// Garbage-collect stale daemon persistent state in a single batch.
    ///
    /// Removes dead session records older than the retention period and prunes
    /// orphaned task metadata map entries (task_channel, task_model,
    /// task_plan, task_execution_skill, task_thread_id, task_message_id).
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
    /// (linked to the PR via PrAuthorSession) but the coworker name is freed.
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
    /// Defers I/O (loading domain context, agents.md, skill bodies) to the
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
    /// appears to have ended without a proper `midtown session detach`.
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
    /// Resolves session_id → name via session_to_name, sends nudge message.
    NudgeSession {
        session_id: String,
        reason: super::wake_reason::WakeReason,
    },
    /// Nudge a session with conditional follow-up effects on success.
    NudgeSessionWithCallbacks {
        session_id: String,
        reason: super::wake_reason::WakeReason,
        on_success: Vec<Effect>,
    },

    // ── Session-centric effects (new model) ─────────────────────────────
    /// Spawn a new session for a task. Allocates a name from the NamePool.
    ///
    /// This is the session-centric counterpart to `SpawnCoworker` / `AssignAndSpawn`.
    /// The key difference: `SpawnSession` allocates the name from the NamePool at
    /// execution time (not at decision time), keeping the decision functions pure.
    SpawnSession {
        session_id: String,
        task_id: String,
        working_dir: std::path::PathBuf,
        initial_prompt: String,
        preferred_name: Option<String>,
        is_reviewer: bool,
        resume: bool,
        config: Box<crate::launch::LaunchConfig>,
    },

    /// Shut down a running session. Releases the name back to the NamePool.
    ///
    /// Session-centric counterpart to `ShutdownCoworker`. Looks up the session's
    /// current name via `session_to_name` reverse map and performs shutdown +
    /// cleanup through `cleanup_coworker_state`.
    ShutdownSession { session_id: String, reason: String },

    /// Record a session record in persistent state.
    ///
    /// Upserts the `SessionRecord` into `DaemonPersistentState::sessions` and
    /// updates in-memory reverse maps (name_to_session, session_to_name, task_to_session).
    RecordSession {
        record: Box<crate::daemon::state::SessionRecord>,
    },

    /// Release a name back to the NamePool (session stopped, name no longer needed).
    ///
    /// Standalone effect for releasing a name without full shutdown. Used when
    /// a session is suspended (process stopped but session state preserved for later resume).
    ReleaseName { name: String },

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
    /// `PrReviewerAssignment` for later update via `pr.review-post`.
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

    /// Respawn a dead fork session bound to a thread.
    ///
    /// Spawns a fresh fork session (no parent resume) with the same thread binding,
    /// channel, and working directory as the original fork. Updates `topic_sessions`
    /// with the new session ID and creates a `SessionRecord` for the new fork.
    ///
    /// This is the fork counterpart to `SpawnCoworker` for task-based crash recovery.
    /// Fork sessions are thread-bound (not task-bound), so they need a separate
    /// respawn path that preserves the thread↔session binding.
    RespawnFork {
        fork_name: String,
        thread_parent_id: String,
        channel: Option<String>,
        working_dir: Option<String>,
        auth_provider: crate::auth::AuthProvider,
        is_channel_lead: bool,
        /// The original nudge message from when the fork was first created.
        /// When present, crash recovery resends this instead of generic framing,
        /// so the respawned fork retains context about what it was supposed to do.
        initial_prompt: Option<String>,
    },
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
            // Fresh and session-aware spawns.
            Effect::AssignAndSpawn { task_id, .. } => {
                ids.insert(task_id.clone());
            }
            Effect::SpawnSession { task_id, .. } => {
                ids.insert(task_id.clone());
            }

            // Resolved task IDs for callback-based success paths.
            Effect::NudgeSessionWithCallbacks { on_success, .. }
            | Effect::SpawnCoworkerWithCallbacks { on_success, .. } => {
                for sub_effect in on_success {
                    if let Effect::RecordTaskAssignment { task_id, .. } = sub_effect {
                        ids.insert(task_id.clone());
                    }
                }
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

    /// Convenience: nudge a session with callbacks and a freeform message.
    pub fn nudge_session_with_callbacks(
        session_id: impl Into<String>,
        message: impl Into<String>,
        on_success: Vec<Effect>,
    ) -> Self {
        Self::NudgeSessionWithCallbacks {
            session_id: session_id.into(),
            reason: super::wake_reason::WakeReason::Nudge {
                message: message.into(),
            },
            on_success,
        }
    }
}

/// Returns true if a non-completed task already exists for the given PR number.
///
/// Used by the `CreateTask` handler in `execute_effects` to skip duplicate task
/// creation when multiple review comments arrive in quick succession.  The caller
/// must pass `continue` (not `return`) after this returns `true` so that remaining
/// effects in the batch are still processed.
pub(crate) fn create_task_duplicate_exists(tasks: &[crate::tasks::Task], pr_num: u64) -> bool {
    tasks
        .iter()
        .any(|t| t.pr == Some(pr_num) && t.status != crate::tasks::TaskStatus::Completed)
}

/// Deduplicate nudge effects targeting the same session within a single batch.
///
/// When multiple PR issue types (CI green, review complete, merge conflict)
/// each generate a nudge for the same session in one tick, only the first
/// nudge is kept. For `NudgeSessionWithCallbacks`, subsequent nudges' `on_success`
/// callbacks are merged into the first nudge's callbacks so state recording
/// (e.g., `RecordPrNudge`, `RecordTaskAssignment`) still happens.
///
/// Plain `NudgeSession` effects for already-nudged sessions are dropped entirely.
fn dedup_nudge_effects(effects: Vec<Effect>) -> Vec<Effect> {
    use std::collections::HashSet;

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
            Effect::NudgeSessionWithCallbacks {
                ref session_id,
                reason,
                on_success,
            } => {
                let key = session_id.clone();
                if nudged_sessions.contains(&key) {
                    debug!(
                        "Deduplicating NudgeSessionWithCallbacks for {} — \
                         executing on_success callbacks without re-nudging",
                        session_id
                    );
                    // Merge on_success into the existing nudge's callbacks.
                    let remaining = merge_callbacks_into_existing(&mut result, &key, on_success);
                    if let Some(unmerged) = remaining {
                        // First nudge was a plain NudgeSession — promote the
                        // callbacks to standalone effects.
                        result.extend(unmerged);
                    }
                    continue;
                }
                nudged_sessions.insert(key);
                result.push(Effect::NudgeSessionWithCallbacks {
                    session_id: session_id.clone(),
                    reason,
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

/// Merge `on_success` callbacks into an existing `NudgeSessionWithCallbacks` effect
/// for the same session. Returns `None` if merged successfully, or `Some(callbacks)`
/// if no matching effect was found (e.g., first nudge was a plain `NudgeSession`).
fn merge_callbacks_into_existing(
    effects: &mut [Effect],
    target_key: &str,
    additional_callbacks: Vec<Effect>,
) -> Option<Vec<Effect>> {
    for effect in effects.iter_mut() {
        if let Effect::NudgeSessionWithCallbacks {
            session_id,
            on_success,
            ..
        } = effect
            && session_id == target_key
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

/// Resolve a session ID to its coworker name and deliver a nudge message.
///
/// Shared implementation for `NudgeSession` and `NudgeSessionWithCallbacks`.
/// Returns `None` on failure (name not found or send error). On success,
/// the nudge is recorded for attribution tracking and an optional
/// `PostToChannel` effect is returned for the caller to execute — posting
/// the nudge content to the coworker's DM channel for observability.
async fn send_session_nudge(
    state: &DaemonState,
    session_id: &str,
    reason: &super::wake_reason::WakeReason,
) -> Option<Vec<Effect>> {
    let name = state
        .session_to_name
        .lock()
        .unwrap()
        .get(session_id)
        .cloned();
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
            // Only for real coworkers (pool names like "lexington"), not fork sessions
            // ("lexington-web-push-a1b2") or other ephemeral sessions.
            // Skip DmFromUser — the user's message is already in the DM channel
            // (written by rpc_channel.rs before the nudge effect was created).
            let mut follow_up = Vec::new();
            if !reason.already_in_dm_channel() && crate::coworker::is_coworker_name(&name) {
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
    {
        let mut t2s = state.task_to_session.lock().unwrap();
        let should_remove = t2s
            .get(task_id)
            .map(|sid| expected_session_id.map(|exp| exp == sid).unwrap_or(true))
            .unwrap_or(false);
        if should_remove {
            t2s.remove(task_id);
        }
    }

    // Remove any in-memory assignment guard so task dispatch can recover cleanly.
    state.clear_task_assignment_by_task(task_id);

    let mut ps = state.persistent_state.lock().await;
    let cleared = clear_task_binding_in_records(&mut ps.sessions, task_id, expected_session_id);
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

/// Execute a list of effects against the daemon state.
///
/// This is the imperative shell — the only place where side effects happen.
/// Each effect variant maps to a call on `DaemonState` or its subsystems.
///
/// Before execution, nudge effects targeting the same coworker are deduplicated
/// to prevent rapid-fire nudges within a single tick (e.g., when CI green,
/// review complete, and merge conflict each independently nudge the same coworker).
///
/// Spawn effects (`AssignAndSpawn`, `SpawnCoworkerWithCallbacks`, `SpawnCoworker`,
/// `EnsureWorktree`) are parallelized using `tokio::spawn` to avoid sequential
/// blocking during startup when processing multiple pending tasks. Non-spawn effects
/// execute sequentially as before. This keeps the daemon responsive to RPC requests
/// during startup by avoiding long sequential pauses from worktree creation (1-5s each).
pub async fn execute_effects(effects: Vec<Effect>, state: &DaemonState) {
    let effects = dedup_nudge_effects(effects);
    for effect in effects {
        match effect {
            Effect::SpawnCoworker(config) => {
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
            Effect::DeliverMailboxMessage {
                name,
                message,
                summary,
            } => {
                let team_name = crate::mailbox::team_name_for_repo(&state.project_name);
                let mut msg = crate::mailbox::MailboxMessage::new(&message, "midtown")
                    .with_color("yellow".to_string());
                if let Some(s) = summary {
                    msg = msg.with_summary(s);
                }
                match crate::mailbox::write_to_inbox(&team_name, &name, msg) {
                    Ok(()) => {
                        debug!("Delivered mailbox message to {}", name);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to deliver mailbox message to {}: {} — falling back to headless stdin",
                            name, e
                        );
                        // Fallback: try headless send_message as a last resort
                        if let Err(nudge_err) =
                            state.session_manager.send_message(&name, &message).await
                        {
                            warn!(
                                "Fallback headless nudge also failed for {}: {}",
                                name, nudge_err
                            );
                        }
                    }
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
                let has_explicit_channel = channel.is_some();
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
                    state
                        .fork_bound_threads
                        .lock()
                        .unwrap()
                        .get(&sender)
                        .cloned()
                };

                let mut msg = if let Some(parent_id) = bound_thread {
                    let ch = channel_name
                        .unwrap_or_else(|| state.channel_router.default_channel_name().to_string());
                    Message::thread_reply(&ch, &sender, &message, parent_id, msg_type)
                } else if let Some(ch) = channel_name {
                    Message::for_channel(&ch, &sender, &message, msg_type)
                } else {
                    Message::new(&sender, &message, msg_type)
                };
                msg.auto_output = auto_output;
                msg.nudge_type = nudge_type;
                msg.tool_data = tool_data;
                msg.provider = provider;
                msg.tool_use_id = tool_use_id.clone();
                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                    warn!("Failed to post channel message: {}", e);
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

                // Clear tool activity for this agent when they post a channel message.
                // A channel post signals the end of a work phase — the activity strip should reset.
                // Skip system senders (midtown) and non-fork explicit-channel posts.
                // Forked sessions should still clear tool activity when they post to
                // their inherited thread channel.
                let is_system_sender = matches!(sender.to_lowercase().as_str(), "midtown" | "user")
                    || sender.eq_ignore_ascii_case(&state.project_name);
                let has_fork_channel_binding = state
                    .fork_bound_channels
                    .lock()
                    .unwrap()
                    .contains_key(&sender);
                let skip = is_system_sender || (has_explicit_channel && !has_fork_channel_binding);
                if !skip {
                    let mut tool_map = state.recent_tool_items.write().unwrap();
                    tool_map.remove(&sender.to_lowercase());
                }
            }
            Effect::BroadcastCoworkerUpdate {
                name,
                status,
                current_task,
            } => {
                state.broadcast_coworker_update(&name, &status, current_task.as_deref());
            }
            Effect::BroadcastUniversalItems {
                agent_name,
                channel,
                thread_parent_id,
                items,
            } => {
                // Store items in DaemonState for TUI RPC consumers (coworkers.status).
                {
                    let mut tool_map = state.recent_tool_items.write().unwrap();
                    let entry = tool_map.entry(agent_name.to_lowercase()).or_default();
                    entry.extend(items.iter().cloned());
                    // Cap to avoid unbounded growth.
                    if entry.len() > MAX_TOOL_ITEMS_PER_AGENT {
                        let drain_count = entry.len() - MAX_TOOL_ITEMS_PER_AGENT;
                        entry.drain(..drain_count);
                    }
                }
                // Also broadcast via WebSocket for web UI consumers.
                state.broadcast_web_update(crate::web::WebUpdate::UniversalItems(
                    crate::web::UniversalItemsData {
                        agent_name,
                        channel,
                        thread_parent_id,
                        items,
                    },
                ));
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
            Effect::ResetTaskToPending { task_id, dir_key } => {
                if let Err(e) = crate::tasks::reset_task_to_pending_for_repo(&task_id, &dir_key) {
                    warn!("Failed to reset task !{} to pending: {}", task_id, e);
                }
                // Clear task assignment tracking (task is no longer assigned)
                state.clear_task_assignment_by_task(&task_id);
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
                // Gather candidate stale session IDs for this coworker from
                // in-memory maps and persisted headless/channel entries.
                let mapped_sid = state
                    .name_to_session
                    .lock()
                    .unwrap()
                    .get(&name)
                    .cloned()
                    .unwrap_or_default();

                let mut ps = state.persistent_state.lock().await;
                let mut candidate_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                if !mapped_sid.is_empty() {
                    candidate_ids.insert(mapped_sid.clone());
                }
                // Also check ps.sessions for a record matching this name
                // (covers sessions that may have been persisted under a
                // different session_id than name_to_session currently maps).
                for record in ps.sessions.values() {
                    if record.current_name.as_deref().is_some_and(|n| n == name)
                        && !record.session_id.is_empty()
                    {
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
                for record in ps.sessions.values_mut() {
                    if record.current_name.as_deref().is_some_and(|n| n == name)
                        && record.is_running
                    {
                        info!(
                            "Clearing stale session record for '{}': {}",
                            name, record.session_id
                        );
                        record.is_running = false;
                        record.resume_on_startup = false;
                        record.current_name = None;
                    }
                }
                if let Some(stored_sid) = ps.channel_lead_sessions.remove(name.as_str()) {
                    if stored_sid.is_empty() {
                        info!(
                            "Removing stale empty channel_lead_sessions entry for '{}'",
                            name
                        );
                    } else {
                        info!(
                            "Removing stale channel_lead_sessions entry for '{}': {}",
                            name, stored_sid
                        );
                    }
                }

                // Clear task/session bindings for matching session records so dispatch
                // won't repeatedly attempt to resume stale IDs.
                let mut cleared_task_ids: Vec<String> = Vec::new();
                for record in ps.sessions.values_mut() {
                    let matches_id = candidate_ids.contains(&record.session_id);
                    let matches_running_name = record.is_running
                        && (record
                            .current_name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case(&name))
                            || record
                                .preferred_name
                                .as_deref()
                                .is_some_and(|n| n.eq_ignore_ascii_case(&name)));
                    if !(matches_id || matches_running_name) {
                        continue;
                    }
                    if let Some(task_id) = record.task_id.take() {
                        cleared_task_ids.push(task_id);
                    }
                    record.is_running = false;
                    record.resume_on_startup = false;
                    record.current_name = None;
                }

                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!(
                        "Failed to save persistent state after clearing stale session ID for '{}': {}",
                        name, e
                    );
                }
                drop(ps);

                if !cleared_task_ids.is_empty() {
                    let mut t2s = state.task_to_session.lock().unwrap();
                    for task_id in &cleared_task_ids {
                        t2s.remove(task_id);
                    }
                    drop(t2s);
                    for task_id in &cleared_task_ids {
                        state.clear_task_assignment_by_task(task_id);
                    }
                }
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
                config,
                on_success,
                on_failure,
            } => {
                // DM separators are posted by the caller in on_success effects,
                // not here. For task-based spawns the separator was posted by
                // AssignAndSpawn or SpawnSession; for reviewer spawns it is
                // included directly in the on_success vector (see pr.rs).
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
            Effect::AssignAndSpawn {
                task_id,
                owner,
                dir_key,
                config,
                on_success,
                on_failure,
            } => {
                // Spawn the coworker and set ownership + in_progress on disk.
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                        // Clear in-flight marker on success
                        state.clear_task_spawn_in_flight(&task_id);
                        // Record task assignment in-memory for busy tracking
                        state.record_task_assignment(&owner, &task_id);
                        // Update SessionRecord with task_id if the session is known.
                        let maybe_session_id =
                            state.name_to_session.lock().unwrap().get(&name).cloned();
                        if let Some(session_id) = maybe_session_id {
                            let mut ps = state.persistent_state.lock().await;
                            if let Some(record) = ps.sessions.get_mut(&session_id) {
                                record.task_id = Some(task_id.clone());
                            }
                            // Also update task_to_session reverse map.
                            state
                                .task_to_session
                                .lock()
                                .unwrap()
                                .insert(task_id.clone(), session_id);
                            if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                warn!(
                                    "Failed to save persistent state after AssignAndSpawn task_id update: {}",
                                    e
                                );
                            }
                        }
                        // Set task owner on disk so status and owner are consistent
                        if let Err(e) = crate::tasks::update_task_owner(&task_id, &owner) {
                            warn!(
                                "Failed to set task !{} owner to {} after spawn: {}",
                                task_id, owner, e
                            );
                        }
                        // Transition task from pending to in_progress now that the coworker is running
                        if let Err(e) =
                            crate::tasks::set_task_in_progress_for_repo(&task_id, &dir_key)
                        {
                            warn!(
                                "Failed to set task !{} to in_progress after spawn: {}",
                                task_id, e
                            );
                        }
                        // Post task divider to the coworker's DM channel
                        let task_subject = crate::tasks::read_tasks_for_repo(Some(&dir_key))
                            .into_iter()
                            .find(|t| t.id == task_id)
                            .map(|t| t.subject);
                        let separator_effect = build_dm_separator_effect(
                            &owner,
                            &task_id,
                            task_subject.as_deref().filter(|s| !s.is_empty()),
                        );
                        Box::pin(execute_effects(vec![separator_effect], state)).await;
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                        // Clear in-flight marker on failure (no disk rollback needed)
                        state.clear_task_spawn_in_flight(&task_id);
                        Box::pin(execute_effects(on_failure, state)).await;
                    }
                }
            }
            Effect::MarkRemindersFired { fired_ids, dir_key } => {
                let mut ps = state.persistent_state.lock().await;
                for reminder in &mut ps.reminders.reminders {
                    if fired_ids.contains(&reminder.id) {
                        reminder.fired = true;
                    }
                }
                if let Err(e) = ps.save_for_repo(&dir_key) {
                    warn!(
                        "Failed to save daemon-state.json after firing reminders: {}",
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
                state.record_task_assignment(&coworker, &task_id);
            }
            Effect::ClearPrBreakSession { name } => {
                let mut sessions = state.pr_break_sessions.write().unwrap();
                sessions.remove(&name);
                info!("Cleared PR break session for {}", name);
            }
            Effect::AssignReviewer {
                pr_number,
                reviewer_name,
                source,
                restart_count,
                reviewer_session_id,
            } => {
                let mut ps = state.persistent_state.lock().await;
                if restart_count > 0 {
                    ps.github.assign_reviewer_with_restart_count(
                        pr_number,
                        &reviewer_name,
                        source,
                        restart_count,
                    );
                } else {
                    ps.github.assign_reviewer(pr_number, &reviewer_name, source);
                }
                // Set the session ID if provided (assign_reviewer* methods don't take it yet)
                if let Some(sid) = reviewer_session_id
                    && let Some(assignment) = ps.github.pr_reviewers.get_mut(&pr_number)
                {
                    assignment.reviewer_session_id = Some(sid);
                }
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!("Failed to save daemon-state.json: {}", e);
                }
            }
            Effect::RemoveReviewerAssignment { pr_number } => {
                let mut ps = state.persistent_state.lock().await;
                if let Some(assignment) = ps.github.remove_assignment(pr_number) {
                    debug!(
                        "Removed reviewer assignment for PR #{} (was assigned to {})",
                        pr_number, assignment.reviewer
                    );
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!(
                            "Failed to save daemon-state.json after removing assignment: {}",
                            e
                        );
                    }
                } else {
                    debug!("No reviewer assignment to remove for PR #{}", pr_number);
                }
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
            Effect::ClearOrphanedReviewerAssignments { orphaned_coworkers } => {
                if orphaned_coworkers.is_empty() {
                    continue;
                }
                let mut ps = state.persistent_state.lock().await;
                for name in &orphaned_coworkers {
                    ps.clear_reviewer_assignment(name, state.paths.dir_key());
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
            Effect::StorePrAuthorSession {
                pr_number,
                session_id,
                branch,
                author,
                title,
            } => {
                let mut ps = state.persistent_state.lock().await;
                ps.github
                    .store_pr_author_session(pr_number, &session_id, &branch, &author, &title);
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
                    warn!("Failed to persist PR author session: {}", e);
                } else {
                    info!(
                        "Stored author session for PR #{}: session={}, author={}",
                        pr_number, session_id, author
                    );
                }
            }
            Effect::CompleteTask { task_id, dir_key } => {
                if let Err(e) = crate::tasks::complete_task_for_repo(&task_id, &dir_key) {
                    warn!("Failed to complete task !{}: {}", task_id, e);
                } else {
                    info!("Auto-completed task !{}", task_id);
                    // Mark worktree as completed (for time-based cleanup) and clean up pr_author_sessions
                    {
                        let mut ps = state.persistent_state.lock().await;
                        if let Some(wt_id) = ps.worktree_registry.find_worktree_by_task(&task_id) {
                            ps.worktree_registry
                                .mark_completed(&wt_id, chrono::Utc::now());
                        }
                        // Clean up pr_author_sessions for this task to prevent stale state
                        ps.github
                            .pr_author_sessions
                            .retain(|_, session| session.task_id.as_deref() != Some(&task_id));
                        // Save both mutations in a single write
                        if let Err(e) = ps.save_for_repo(&dir_key) {
                            warn!("Failed to save task completion state: {}", e);
                        }
                    }
                    // Clear task assignment tracking (coworker is now free)
                    state.clear_task_assignment_by_task(&task_id);
                }
            }
            Effect::ClearBlockedBy {
                completed_task_id,
                dir_key,
            } => {
                if let Err(e) =
                    crate::tasks::clear_blocked_by_for_repo(&completed_task_id, &dir_key)
                {
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
                dir_key,
            } => {
                if let Err(e) = crate::tasks::update_task_fields_for_repo(
                    &task_id,
                    &dir_key,
                    None, // owner
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
            Effect::SendPushNotification { title, body, tag } => {
                state.send_push_notification(&title, &body, &tag);
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
                // Remove from registry and clean up pr_author_sessions
                let removed = {
                    let mut ps = state.persistent_state.lock().await;
                    let removed = ps.worktree_registry.cleanup_for_merged_pr(pr_number);
                    // Also clean up pr_author_sessions for this PR (defense-in-depth)
                    let pr_session_removed = ps.github.pr_author_sessions.remove(&pr_number);
                    // Save if either worktree or pr_author_session was removed
                    if (removed.is_some() || pr_session_removed.is_some())
                        && let Err(e) = ps.save_for_repo(state.paths.dir_key())
                    {
                        warn!("Failed to save daemon state after worktree cleanup: {}", e);
                    }
                    removed
                };
                if let Some(assignment) = removed {
                    // Fire-and-forget: worktree directory removal runs git operations
                    // that can block for a long time with many branches. Registry is
                    // already updated above, so the state is consistent even if the
                    // filesystem cleanup is still running.
                    let wt_mgr = state.coworkers.worktree_manager().clone();
                    let wt_id = assignment.worktree_id.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = wt_mgr.force_cleanup_task_worktree(&wt_id) {
                            warn!("Failed to remove task worktree {}: {}", wt_id, e);
                        } else {
                            info!(
                                "Cleaned up task worktree {} (PR #{} merged)",
                                wt_id, pr_number
                            );
                        }
                    });
                    // Post to ops channel so the team sees what was cleaned up
                    let task_ref = assignment
                        .task_id
                        .map(|id| format!(" (task !{})", id))
                        .unwrap_or_default();
                    let mut msg = Message::system(format!(
                        "🧹 Cleaned up worktree {} after PR #{} merged{}",
                        assignment.worktree_id, pr_number, task_ref
                    ));
                    msg.channel = Some(OPS_CHANNEL.to_string());
                    if let Err(e) = state.send_and_broadcast_async(&msg).await {
                        warn!("Failed to post worktree cleanup message: {}", e);
                    }
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
                    // Fire-and-forget: directory removal runs slow git operations.
                    // Registry is already updated above.
                    let wt_mgr = state.coworkers.worktree_manager().clone();
                    let wt_id = assignment.worktree_id.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = wt_mgr.force_cleanup_task_worktree(&wt_id) {
                            warn!("Failed to remove stale worktree {}: {}", wt_id, e);
                        } else {
                            info!(
                                "Cleaned up stale worktree {} (retention period expired)",
                                wt_id
                            );
                        }
                    });
                    // Post to ops channel so the team sees what was cleaned up
                    let task_ref = assignment
                        .task_id
                        .map(|id| format!(" (task !{})", id))
                        .unwrap_or_default();
                    let mut msg = Message::system(format!(
                        "🧹 Cleaned up stale worktree {} (retention period expired){}",
                        assignment.worktree_id, task_ref
                    ));
                    msg.channel = Some(OPS_CHANNEL.to_string());
                    if let Err(e) = state.send_and_broadcast_async(&msg).await {
                        warn!("Failed to post worktree cleanup message: {}", e);
                    }
                } else {
                    debug!(
                        "Worktree {} not found in registry, skipping cleanup",
                        worktree_id
                    );
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
                    let (domain_context, agents_md, skill_bodies) = load_channel_lead_context(
                        base_dir.clone(),
                        &name,
                        state.all_repo_paths.first().cloned().unwrap_or_default(),
                        state.paths.dir_key(),
                    )
                    .await;
                    let config = crate::launch::LaunchConfig::channel_lead(
                        &name,
                        state.paths.dir_key(),
                        crate::launch::SessionMode::Fresh,
                        domain_context,
                        agents_md,
                        skill_bodies,
                    );
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
                            // Clean up the placeholder entry so it doesn't linger as dead state.
                            // Recovery will spawn a fresh session on the next daemon restart.
                            let mut ps = state.persistent_state.lock().await;
                            ps.channel_lead_sessions.remove(&name);
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
                                for record in ps.sessions.values_mut() {
                                    if record
                                        .current_name
                                        .as_deref()
                                        .is_some_and(|n| n == lead_session_name)
                                    {
                                        record.is_running = false;
                                        record.current_name = None;
                                        record.resume_on_startup = false;
                                        removed_session = true;
                                    }
                                }
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

                // Update task_channel mappings: any task assigned to `from` should now point to `into`
                {
                    let mut ps = state.persistent_state.lock().await;
                    for (_task_id, channel) in ps.task_channel.iter_mut() {
                        if channel == &from {
                            *channel = into.clone();
                        }
                    }
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!("Failed to save task_channel after merge: {}", e);
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
                // Update task_channel mapping in persistent state
                let mut ps = state.persistent_state.lock().await;
                ps.task_channel.insert(task_id.clone(), channel.clone());
                debug!("Assigned task !{} to channel '{}'", task_id, channel);
                if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                    warn!("Failed to save task_channel after assignment: {}", e);
                }
                // Also update the task file on disk so dispatch.rs reads the correct channel
                // (dispatch reads task.channel from the file, not from persistent state)
                if let Err(e) = crate::tasks::update_task_fields_for_repo(
                    &task_id,
                    state.paths.dir_key(),
                    None, // owner
                    None, // status
                    None, // description
                    None, // blocked_by
                    Some(&channel),
                    None, // pr
                ) {
                    warn!("Failed to update task file channel for !{}: {}", task_id, e);
                }
            }
            Effect::UnassignTask { task_id, dir_key } => {
                if let Err(e) = crate::tasks::unassign_task_for_repo(&task_id, &dir_key) {
                    warn!("Failed to unassign task !{}: {}", task_id, e);
                } else {
                    info!(
                        "Unassigned task !{} (PR in review, freeing coworker name)",
                        task_id
                    );
                    state.clear_task_assignment_by_task(&task_id);
                }
            }
            Effect::ResetAbandonedTask {
                task_id,
                pr_number,
                dir_key,
            } => {
                if let Err(e) = crate::tasks::reset_task_to_pending_for_repo(&task_id, &dir_key) {
                    warn!(
                        "Failed to reset abandoned task !{} (PR #{} closed): {}",
                        task_id, pr_number, e
                    );
                } else {
                    info!(
                        "Reset task !{} to pending (PR #{} closed without merge)",
                        task_id, pr_number
                    );
                    state.clear_task_assignment_by_task(&task_id);
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
                dir_key,
                subject,
                description,
                pr,
            } => {
                // If a PR number is associated, skip creation if a non-completed task
                // already exists for that PR. This prevents duplicate follow-up tasks
                // when multiple review comments arrive in quick succession (e.g., after
                // a daemon restart resets the in-memory cooldown).
                if let Some(pr_num) = pr {
                    let existing = crate::tasks::read_tasks_for_repo(Some(&dir_key));
                    if create_task_duplicate_exists(&existing, pr_num) {
                        debug!(
                            "Skipping CreateTask for PR #{}: non-completed task already exists",
                            pr_num
                        );
                        continue;
                    }
                }

                // Derive active_form from subject (simple present progressive form)
                let active_form = if subject.starts_with("Merge") {
                    subject.replace("Merge", "Merging")
                } else {
                    format!("Working on: {}", subject)
                };

                match crate::tasks::create_task_for_repo(
                    &subject,
                    &description,
                    &active_form,
                    "", // owner (empty = unassigned)
                    &dir_key,
                    None, // blocked_by
                    None, // channel
                    pr,   // pr (from effect)
                ) {
                    Ok(task_id) => {
                        info!("Created task !{}: {}", task_id, subject);
                        // Post channel notification attributed to the project lead.
                        // Use task_announcement_message for consistency with the RPC path,
                        // and capture the message ID for task-as-thread linking.
                        let channel = state.default_channel_name();
                        let msg = crate::daemon::rpc_task::task_announcement_message(
                            channel, "lead", &subject, None,
                        );
                        let message_id = msg.id.clone();
                        match state.send_and_broadcast_async(&msg).await {
                            Ok(()) => {
                                let mut ps = state.persistent_state.lock().await;
                                ps.task_message_id
                                    .insert(task_id.clone(), message_id.clone());
                                if !ps.task_thread_id.contains_key(&task_id) {
                                    ps.task_thread_id.insert(task_id.clone(), message_id);
                                }
                                if let Err(e) = ps.save_for_repo(&dir_key) {
                                    warn!("Failed to save task message_id mapping: {}", e);
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
                let base_dir = state.paths.base_dir().to_path_buf();
                let project_root = state.all_repo_paths.first().cloned().unwrap_or_default();
                let dir_key = state.paths.dir_key().to_string();
                let (domain_context, agents_md, skill_bodies) =
                    load_channel_lead_context(base_dir, &channel_name, project_root, &dir_key)
                        .await;

                let mut config = crate::launch::LaunchConfig::channel_lead(
                    &channel_name,
                    state.paths.dir_key(),
                    crate::launch::SessionMode::Fresh,
                    &domain_context,
                    agents_md,
                    skill_bodies,
                );
                config.model = super::helpers::resolve_model_for_role(
                    state.paths.dir_key(),
                    config.auth_provider,
                    &config.role,
                );

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
            Effect::SpawnSession {
                session_id,
                task_id,
                working_dir,
                initial_prompt,
                preferred_name,
                is_reviewer,
                resume,
                mut config,
            } => {
                let task_id = task_id.clone();
                // 1. Allocate name from NamePool
                let channel_lead_names = {
                    let ps = state.persistent_state.lock().await;
                    ps.channel_lead_names()
                };
                let name = {
                    let mut pool = state.name_pool.lock().unwrap();
                    pool.allocate_excluding(preferred_name.as_deref(), &channel_lead_names)
                };
                let Some(name) = name else {
                    warn!("No available names for SpawnSession {}", session_id);
                    state.clear_task_spawn_in_flight(&task_id);
                    continue;
                };

                // 2. Update config with allocated name
                config.name = name.clone();
                if !resume {
                    config.session_mode = crate::launch::SessionMode::Fresh;
                } else {
                    config.session_mode =
                        crate::launch::SessionMode::ResumeSession(session_id.clone());
                }
                config.working_dir = Some(working_dir.clone());
                config.initial_prompt = Some(initial_prompt.clone());

                // 2b. Clear any stale inbox messages left by a previous session
                // that held this name. Names are recycled from the NamePool, so
                // without this a new session would inherit unread messages meant
                // for its predecessor.
                {
                    let team_name = crate::mailbox::team_name_for_repo(&state.project_name);
                    if let Err(e) = crate::mailbox::clear_inbox(&team_name, &name) {
                        warn!("SpawnSession: failed to clear inbox for '{}': {}", name, e);
                    }
                }

                // 3. Spawn via state.spawn_coworker (handles worktree, register, session manager)
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!(
                            "SpawnSession: spawned session {} as {} for task !{}",
                            session_id, name, task_id
                        );

                        // 4. Update task_to_session (name↔session maps already set by spawn_coworker)
                        {
                            state
                                .task_to_session
                                .lock()
                                .unwrap()
                                .insert(task_id.clone(), session_id.clone());
                        }

                        // 5. Update SessionRecord in persistent state
                        {
                            let mut ps = state.persistent_state.lock().await;
                            // Mark any old session records with this name as not running
                            // and clear their current_name. Names are reused from the
                            // pool, so previous sessions for this name may still have
                            // is_running=true if they weren't properly cleaned up
                            // (e.g., after daemon restart). Clearing current_name
                            // prevents ambiguous lookups where multiple records share
                            // the same name (e.g., insight handler's find-by-name).
                            for record in ps.sessions.values_mut() {
                                if record.session_id != session_id
                                    && (record.preferred_name.as_deref() == Some(&name)
                                        || record.current_name.as_deref() == Some(&name))
                                {
                                    if record.is_running {
                                        record.is_running = false;
                                    }
                                    record.current_name = None;
                                }
                            }
                            // Look up task_thread_id so coworker posts route to the
                            // task's thread. This is set either explicitly via --thread-id
                            // or auto-defaulted to the task announcement message ID.
                            let bound_thread_id = ps.task_thread_id.get(&task_id).cloned();
                            // Populate in-memory cache so handle_channel_post can auto-tag
                            // the coworker's posts without touching persistent state.
                            if let Some(ref tid) = bound_thread_id {
                                state
                                    .fork_bound_threads
                                    .lock()
                                    .unwrap()
                                    .insert(name.clone(), tid.clone());
                            }
                            let record =
                                ps.sessions.entry(session_id.clone()).or_insert_with(|| {
                                    crate::daemon::state::SessionRecord {
                                        session_id: session_id.clone(),
                                        task_id: Some(task_id.clone()),
                                        current_name: Some(name.clone()),
                                        preferred_name: preferred_name
                                            .clone()
                                            .or_else(|| Some(name.clone())),
                                        working_dir: working_dir.to_string_lossy().to_string(),
                                        branch: None,
                                        pr_number: None,
                                        initial_prompt: Some(initial_prompt.clone()),
                                        is_reviewer,
                                        coworker_type: if is_reviewer {
                                            "reviewer".to_string()
                                        } else {
                                            "dev".to_string()
                                        },
                                        is_running: true,
                                        created_at: chrono::Utc::now(),
                                        resume_on_startup: !is_reviewer,
                                        bound_thread_id: bound_thread_id.clone(),
                                        last_active: chrono::Utc::now(),
                                        purpose: initial_prompt
                                            .chars()
                                            .take(120)
                                            .collect::<String>(),
                                        pid: None,
                                        channel: config.channel.clone(),
                                        provider: Some(config.auth_provider),
                                        platform: Some(crate::platform::Platform::from_provider(
                                            config.auth_provider,
                                        )),
                                        profile: None, // Resolved at spawn time, not available here
                                    }
                                });
                            record.current_name = Some(name.clone());
                            record.is_running = true;
                            // Update working_dir to the actual path used for this spawn.
                            // This clears any stale path that was overridden at dispatch time
                            // (e.g., when the recorded working_dir no longer existed on disk).
                            record.working_dir = working_dir.to_string_lossy().to_string();
                            if bound_thread_id.is_some() {
                                record.bound_thread_id = bound_thread_id;
                            }
                            if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                                warn!("Failed to save persistent state after SpawnSession: {}", e);
                            }
                        }

                        record_session_recovery_cooldown(&state.cooldowns, &session_id, resume);

                        // Post session separator to the coworker's DM channel.
                        // Post a DM separator so the user sees a task header in
                        // the coworker's DM channel (applies to all sessions
                        // including reviewers).
                        {
                            let task_subject =
                                crate::tasks::read_tasks_for_repo(Some(state.paths.dir_key()))
                                    .into_iter()
                                    .find(|t| t.id == task_id)
                                    .map(|t| t.subject);
                            let separator_effect =
                                build_dm_separator_effect(&name, &task_id, task_subject.as_deref());
                            Box::pin(execute_effects(vec![separator_effect], state)).await;
                        }

                        state.broadcast_coworker_update(&name, "running", None);
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        warn!("SpawnSession failed for {}: {}", session_id, err_msg);
                        if err_msg.contains("Specified working_dir does not exist") {
                            warn!(
                                "SpawnSession cleanup: clearing stale task/session binding for task !{} (session {}) after missing working_dir",
                                task_id, session_id
                            );
                            let cleared = clear_stale_task_session_binding(
                                state,
                                &task_id,
                                Some(&session_id),
                            )
                            .await;
                            if cleared == 0 {
                                debug!(
                                    "SpawnSession cleanup: no matching stale bindings found for task !{}",
                                    task_id
                                );
                            }
                            if let Err(reset_err) = crate::tasks::reset_task_to_pending_for_repo(
                                &task_id,
                                state.paths.dir_key(),
                            ) {
                                warn!(
                                    "SpawnSession cleanup: failed to reset task !{} to pending: {}",
                                    task_id, reset_err
                                );
                            }
                        }
                        // Release name back since spawn failed
                        {
                            let mut pool = state.name_pool.lock().unwrap();
                            pool.release(&name);
                        }
                    }
                }
                state.clear_task_spawn_in_flight(&task_id);
            }

            Effect::ShutdownSession { session_id, reason } => {
                // Look up name from session_to_name
                let name = state
                    .session_to_name
                    .lock()
                    .unwrap()
                    .get(&session_id)
                    .cloned();
                if let Some(name) = name {
                    info!(
                        "ShutdownSession: shutting down session {} (name: {}, reason: {})",
                        session_id, name, reason
                    );
                    // shutdown_coworker_impl → cleanup_coworker_state handles all
                    // cleanup: NamePool release, reverse maps, and SessionRecord
                    // update in persistent state.
                    let _ = shutdown_coworker_impl(&name, &reason, state).await;

                    state.broadcast_coworker_update(&name, "stopped", None);
                } else {
                    // No name mapped — session may have been suspended via ReleaseName
                    // or already partially cleaned up. Still mark SessionRecord as stopped
                    // so persistent state doesn't show a stale is_running=true.
                    warn!(
                        "ShutdownSession: no name found for session {} — marking record as stopped",
                        session_id
                    );
                    let mut ps = state.persistent_state.lock().await;
                    if let Some(record) = ps.sessions.get_mut(&session_id) {
                        record.is_running = false;
                    }
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
                } else {
                    let session_name = crate::launch::channel_lead_session_name(&channel_name);
                    let msg = reason.to_nudge_message();
                    let session_id = {
                        let ps = state.persistent_state.lock().await;
                        ps.channel_lead_sessions.get(&channel_name).cloned()
                    };
                    let mut nudge_delivered = false;

                    // First, try to nudge the stored session_id for this channel lead.
                    // This avoids name collision bugs where a coworker shares the same
                    // name as the channel lead and would steal nudges.
                    if let Some(stored_session_id) =
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
                    if !nudge_delivered {
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

                    let (domain_context, agents_md, skill_bodies) = load_channel_lead_context(
                        state.paths.base_dir().to_path_buf(),
                        &channel_name,
                        state.all_repo_paths.first().cloned().unwrap_or_default(),
                        state.paths.dir_key(),
                    )
                    .await;

                    match (session_id.as_deref(), can_resume_channel_lead) {
                        (Some(id), true) => {
                            let mut config = crate::launch::LaunchConfig::channel_lead(
                                &channel_name,
                                state.paths.dir_key(),
                                crate::launch::SessionMode::ResumeSession(id.to_string()),
                                &domain_context,
                                agents_md.clone(),
                                skill_bodies.clone(),
                            );
                            config.initial_prompt = Some(reason.to_initial_prompt(&channel_name));

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
                                skill_bodies,
                            );
                            config.initial_prompt = Some(reason.to_initial_prompt(&channel_name));
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
            Effect::NudgeSessionWithCallbacks {
                session_id,
                reason,
                on_success,
            } => {
                // Clear in-flight markers for task IDs claimed by this effect.
                let task_ids: Vec<String> = extract_claimed_task_ids_from_effects(&on_success)
                    .into_iter()
                    .collect();

                if let Some(follow_up) = send_session_nudge(state, &session_id, &reason).await {
                    let mut all = on_success;
                    all.extend(follow_up);
                    Box::pin(execute_effects(all, state)).await;
                }
                // Clear in-flight markers regardless of success/failure
                for task_id in &task_ids {
                    state.clear_task_spawn_in_flight(task_id);
                }
            }

            Effect::RecordSession { record } => {
                let session_id = record.session_id.clone();

                // Update in-memory reverse maps
                if let Some(ref name) = record.current_name {
                    state
                        .name_to_session
                        .lock()
                        .unwrap()
                        .insert(name.clone(), session_id.clone());
                    state
                        .session_to_name
                        .lock()
                        .unwrap()
                        .insert(session_id.clone(), name.clone());
                }
                if let Some(ref task_id) = record.task_id {
                    state
                        .task_to_session
                        .lock()
                        .unwrap()
                        .insert(task_id.clone(), session_id.clone());
                }

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

            Effect::ReleaseName { name } => {
                {
                    let mut pool = state.name_pool.lock().unwrap();
                    pool.release(&name);
                }
                // Clean up reverse maps
                let session_id = state.name_to_session.lock().unwrap().remove(&name);
                if let Some(session_id) = session_id {
                    state.session_to_name.lock().unwrap().remove(&session_id);
                }
                info!("ReleaseName: released '{}' back to NamePool", name);
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
                let _default_prevented = dispatch_workflow_event(state, event).await;
                // When default_prevented is true, the plugin has taken full ownership
                // of this event — compiled-in behavior is suppressed. Currently pr.rs
                // already skips inline effects when plugins are configured, so this
                // flag confirms the plugin's intent. Future: use this to conditionally
                // re-emit compiled-in fallback effects when default_prevented is false.
            }

            Effect::RespawnFork {
                fork_name,
                thread_parent_id,
                channel,
                working_dir,
                auth_provider,
                is_channel_lead,
                initial_prompt,
            } => {
                respawn_fork(
                    state,
                    &fork_name,
                    &thread_parent_id,
                    channel.as_deref(),
                    working_dir.as_deref(),
                    auth_provider,
                    is_channel_lead,
                    initial_prompt.as_deref(),
                )
                .await;
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

/// Post a "Review in progress" placeholder comment on a PR via `gh pr comment`.
///
/// Parses the comment ID from the stdout URL and stores it on the
/// `PrReviewerAssignment` so the daemon can later update the placeholder
/// with the final review via `pr.review-post`.
async fn post_pr_comment(state: &DaemonState, pr_number: u64, reviewer_name: &str, body: &str) {
    let repo_path = state.all_repo_paths.first().cloned();
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
    let comment_id = stdout
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
        });

    if let Some(comment_id) = comment_id {
        info!(
            "Posted placeholder comment {} on PR #{} for reviewer {}",
            comment_id, pr_number, reviewer_name
        );

        // Store the comment ID on the reviewer assignment.
        // Serialize under the lock, then write to disk after releasing it
        // to avoid blocking the tokio runtime with file I/O.
        let serialized = {
            let mut ps = state.persistent_state.lock().await;
            if let Some(assignment) = ps.github.pr_reviewers.get_mut(&pr_number) {
                assignment.placeholder_comment_id = Some(comment_id);
            }
            serde_json::to_string_pretty(&*ps).ok()
        };
        if let Some(json) = serialized {
            let path = state.paths.daemon_state_file();
            if let Err(e) = tokio::task::spawn_blocking(move || {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let tmp_path = path.with_extension("json.tmp");
                std::fs::write(&tmp_path, &json)?;
                crate::paths::atomic_rename(&tmp_path, &path)
            })
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e)))
            {
                warn!(
                    "Failed to save daemon-state.json after storing placeholder comment ID: {}",
                    e
                );
            }
        }

        // Populate the placeholder cache so snapshot doesn't need an API call
        {
            let mut cache = state.reviewer_placeholder_cache.lock().unwrap();
            cache.insert(pr_number, (Some(comment_id), std::time::Instant::now()));
        }
    } else {
        warn!(
            "Could not parse comment ID from gh pr comment output: {}",
            stdout.trim()
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

    // Discover channel-specific plugin directories and merge with existing dirs.
    // merge_plugin_dirs is a no-op if the merged set hasn't changed.
    let project_root = state.all_repo_paths.first().cloned().unwrap_or_default();
    let channel_dirs =
        crate::paths::discover_plugin_dirs(&project_root, state.paths.dir_key(), Some(&channel));
    if !channel_dirs.is_empty() {
        state.plugin_daemon.merge_plugin_dirs(channel_dirs).await;
        // merge_plugin_dirs kills the running daemon when new dirs are added.
        // Restart it before dispatching so the event isn't dropped.
        state.plugin_daemon.ensure_running().await;
    }

    if !state.plugin_daemon.has_plugins() {
        // No plugins configured — silent no-op.
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
        // Plugin daemon not running or connection failed. When has_plugins()
        // returned true, pr.rs took the script-authoritative path and skipped
        // compiled-in inline effects. Post an error so the failure is visible.
        warn!(
            channel = %channel,
            event_type = %event_type,
            "dispatch_workflow_event: plugin daemon unavailable, event dropped"
        );
        post_plugin_error(
            state,
            &channel,
            &format!(
                "Plugin daemon unavailable for event `{}` — event was not processed. \
                 Compiled-in behavior was skipped because plugins are configured.",
                event_type
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
    let effects = plugin_actions_to_effects(&dispatch_result.actions, state);
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
fn plugin_actions_to_effects(
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
                        state.session_id_for_name(&name),
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

/// Record the `session_recovered` cooldown for resume spawns to prevent rapid retries.
fn record_session_recovery_cooldown(
    cooldowns: &std::sync::Mutex<crate::rules::CooldownTracker>,
    session_id: &str,
    resume: bool,
) {
    if !resume {
        return;
    }
    let mut guard = cooldowns.lock().unwrap();
    guard.record("session_recovered", session_id);
}

async fn spawn_with_resume_fallback(
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

/// Respawn a dead fork session bound to a thread.
///
/// Builds a fresh fork HeadlessConfig (no parent resume), spawns it via
/// SessionManager, and re-establishes the topic_sessions and reverse-map
/// bindings so the thread continues routing to the new fork.
#[allow(clippy::too_many_arguments)]
async fn respawn_fork(
    state: &DaemonState,
    fork_name: &str,
    thread_parent_id: &str,
    channel: Option<&str>,
    working_dir: Option<&str>,
    auth_provider: crate::auth::AuthProvider,
    is_channel_lead: bool,
    initial_prompt: Option<&str>,
) {
    // Build a fork config. We pass an empty calling_session_id and override
    // resume_session_id to None — crash recovery spawns fresh, not from parent.
    // Use name_override (not fork_name_hint) to reuse the exact original name,
    // keeping cooldown keys stable and HeadlessConfig identity consistent.
    let (name, mut headless_config) = super::rpc_session::build_fork_config(
        thread_parent_id,
        "",   // no calling session (crash recovery)
        None, // no caller name
        None, // no hint — we use name_override instead
        channel,
        working_dir,
        auth_provider,
        is_channel_lead,
        state.paths.dir_key(),
        Some(fork_name), // reuse exact original fork name
    );
    headless_config.resume_session_id = None; // Fresh session, don't resume from parent

    // Spawn the fork
    let fork_session_id = match state
        .session_manager
        .spawn_fork(&name, headless_config)
        .await
    {
        Ok(sid) => sid,
        Err(e) => {
            warn!("Failed to respawn fork {}: {}", fork_name, e);
            return;
        }
    };

    // Update topic_sessions with the new session ID
    {
        let mut topic = state.topic_sessions.lock().unwrap();
        topic.insert(thread_parent_id.to_string(), fork_session_id.clone());
    }

    // Create SessionRecord for the new fork
    {
        let mut ps = state.persistent_state.lock().await;
        // Clear current_name/preferred_name on any old session records that
        // still claim this name. Same cleanup as SpawnSession (PR #1819) —
        // prevents ambiguous find-by-name lookups when multiple records share
        // the same name. Must check both fields because rpc_auth.rs matches
        // sessions by either preferred_name or current_name.
        for record in ps.sessions.values_mut() {
            if record.session_id != fork_session_id
                && (record.preferred_name.as_deref() == Some(&name)
                    || record.current_name.as_deref() == Some(&name))
            {
                record.is_running = false;
                record.current_name = None;
                record.preferred_name = None;
            }
        }
        ps.sessions.insert(
            fork_session_id.clone(),
            crate::daemon::state::SessionRecord {
                session_id: fork_session_id.clone(),
                task_id: None,
                current_name: Some(name.clone()),
                preferred_name: Some(name.clone()),
                working_dir: working_dir.unwrap_or_default().to_string(),
                branch: None,
                pr_number: None,
                initial_prompt: initial_prompt.map(String::from),
                is_reviewer: false,
                coworker_type: if is_channel_lead {
                    "channel-lead".to_string()
                } else {
                    "dev".to_string()
                },
                is_running: true,
                created_at: chrono::Utc::now(),
                resume_on_startup: false,
                bound_thread_id: Some(thread_parent_id.to_string()),
                last_active: chrono::Utc::now(),
                purpose: format!(
                    "respawned fork in thread {} (crash recovery)",
                    thread_parent_id
                ),
                pid: None,
                channel: channel.map(String::from),
                provider: Some(auth_provider),
                platform: Some(crate::platform::Platform::from_provider(auth_provider)),
                profile: None,
            },
        );
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!("Failed to persist session record for respawned fork: {}", e);
        }
    }

    // Populate in-memory reverse maps
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert(name.clone(), fork_session_id.clone());
    state
        .session_to_name
        .lock()
        .unwrap()
        .insert(fork_session_id.clone(), name.clone());

    // Cache the bound thread mapping for the output binding hot path
    state
        .fork_bound_threads
        .lock()
        .unwrap()
        .insert(name.clone(), thread_parent_id.to_string());
    if let Some(ch) = channel {
        state
            .fork_bound_channels
            .lock()
            .unwrap()
            .insert(name.clone(), ch.to_string());
    }

    info!(
        "Respawned fork {} → thread={}, new_session={}",
        name, thread_parent_id, fork_session_id
    );

    // Send an initial nudge so the fork has a message to act on.
    // Without this, the fork session sits idle forever with no initial prompt
    // (same issue as the original fork creation path — see rpc_session.rs).
    //
    // Priority: preserved initial_prompt (from the original fork) > generic framing.
    // This ensures crash-recovered forks retain context about their original task
    // instead of getting confused by generic "crash recovery" framing.
    let nudge_message = if let Some(prompt) = initial_prompt {
        Some(format!(
            "{}\n\n(This is a crash recovery session — the previous fork for this thread exited unexpectedly.)",
            prompt
        ))
    } else if is_channel_lead {
        channel
            .map(|ch| {
                format!(
                    "{}\n\n(This is a crash recovery session — the previous fork for this thread exited unexpectedly.)",
                    super::rpc_channel::fork_initial_framing(ch)
                )
            })
    } else {
        Some(
            "This is a crash recovery session — the previous fork for this thread exited unexpectedly. \
             Please read the thread context and continue where the previous session left off.".to_string()
        )
    };
    if let Some(message) = nudge_message {
        let reason = crate::daemon::wake_reason::WakeReason::Nudge { message };
        if let Some(follow_up) = send_session_nudge(state, &fork_session_id, &reason).await {
            Box::pin(execute_effects(follow_up, state)).await;
        }
    }

    // Broadcast ThreadOwnership to web clients so the "Dedicated session"
    // indicator reappears after crash recovery (cleanup_coworker_state
    // already broadcast has_dedicated_session: false when the fork died).
    if let Some(ch) = channel {
        // Resolve the parent channel lead's name for the web UI display
        let parent_lead = {
            let ps = state.persistent_state.lock().await;
            ps.channel_lead_sessions
                .get(ch)
                .and_then(|lead_sid| state.session_to_name.lock().unwrap().get(lead_sid).cloned())
        };
        state.broadcast_web_update(crate::web::WebUpdate::ThreadOwnership(
            crate::web::ThreadOwnershipData {
                thread_parent_id: thread_parent_id.to_string(),
                channel: ch.to_string(),
                has_dedicated_session: true,
                owner: Some(name.clone()),
                parent_lead,
            },
        ));
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
        let is_channel_lead = ps.sessions.values().any(|s| {
            s.is_running
                && s.current_name.as_deref() == Some(agent)
                && s.coworker_type == "channel-lead"
        });
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
            .find(|r| r.is_running && r.current_name.as_deref() == Some(agent))
            .or_else(|| {
                ps.sessions
                    .values()
                    .find(|r| r.current_name.as_deref() == Some(agent))
            })
            .and_then(|r| r.task_id.as_deref());
        let ch = task_id.and_then(|tid| ps.task_channel.get(tid).cloned());
        let thread = task_id.and_then(|tid| ps.task_thread_id.get(tid).cloned());
        (ch, thread)
    };

    let channel_name: &str = task_channel
        .as_deref()
        .unwrap_or_else(|| state.channel_router.default_channel_name());

    // Only use the task thread if the final channel matches the task's channel.
    let resolved_thread_id =
        task_thread_id.filter(|_| task_channel.as_deref() == Some(channel_name));

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
    let task_id = state.get_task_id_for_coworker(agent);
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

#[path = "effects_tests.rs"]
#[cfg(test)]
mod tests;
