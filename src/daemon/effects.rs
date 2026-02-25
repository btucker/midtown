use std::path::PathBuf;

use tracing::{debug, info, warn};

/// Maximum number of tool call/result items retained per agent in `recent_tool_items`.
const MAX_TOOL_ITEMS_PER_AGENT: usize = 20;

use super::DaemonState;
use super::constants::OPS_CHANNEL;
use super::trackers::PrIssueType;
use crate::message::Message;

fn build_resume_handoff_prompt(
    name: &str,
    repo_name: &str,
    previous_session_id: &str,
    prior_prompt: Option<&str>,
    working_dir: Option<&std::path::Path>,
) -> String {
    let history_file = crate::paths::headless_output_file(repo_name, name);
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
    /// Channel routing follows a 3-step resolution:
    /// 1. If `channel` is explicitly provided, use that
    /// 2. Otherwise, extract task ID from message content (e.g., "!42") and route to that task's channel
    /// 3. Fall back to the default "midtown" channel if no task ID is found
    PostToChannel {
        sender: String,
        message: String,
        channel: Option<String>,
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
    BroadcastUniversalItems {
        agent_name: String,
        channel: Option<String>,
        items: Vec<crate::universal_events::UniversalItem>,
    },
    /// Record a cooldown entry (category + key).
    RecordCooldown { category: String, key: String },
    /// Schedule a usage-limit nudge at a specific time.
    SetUsageLimitNudge { at: tokio::time::Instant },
    /// Clear the scheduled usage-limit nudge (after it fires).
    ClearUsageLimitNudge,
    /// Reset a task back to pending (e.g. when a coworker can't be respawned).
    ResetTaskToPending { task_id: String, repo_name: String },
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
        #[allow(dead_code)]
        repo_name: String,
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
        repo_name: String,
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
    CompleteTask { task_id: String, repo_name: String },
    /// Clear a completed task ID from all dependent tasks' `blockedBy` arrays.
    ///
    /// Called after a task is completed to unblock dependent tasks.
    ClearBlockedBy {
        completed_task_id: String,
        repo_name: String,
    },
    /// Set the explicit PR association for a task.
    ///
    /// Called when a PR is opened with `[Midtown !XX]` in the title to link the task to the PR.
    SetTaskPr {
        task_id: String,
        pr_number: u64,
        repo_name: String,
    },
    /// Force-delete orphaned worktrees whose PRs were merged (squash-merge).
    ///
    /// These worktrees appear to have "unmerged commits" because the squash-merge
    /// changed commit SHAs, but the work is already in main. Safe to force-remove.
    ForceCleanupWorktrees { names: Vec<String> },
    /// Send a push notification to the mobile PWA.
    ///
    /// Fire-and-forget: the push manager runs in a background task.
    SendPushNotification {
        title: String,
        body: String,
        tag: String,
    },
    /// Clean up stale local branches that match coworker naming patterns
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
    /// `working_dir` is the coworker's actual working directory (resolved
    /// from the coworker record at decision time), not the legacy
    /// `worktree_path()` which only covers coworker-named worktrees.
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
    /// Cannot archive the main "midtown" channel.
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
    UnassignTask { task_id: String, repo_name: String },
    /// Reset an abandoned task back to pending.
    ///
    /// Used when a PR is closed without merge — the associated task is reset
    /// so it can be picked up by another coworker.
    ResetAbandonedTask {
        task_id: String,
        pr_number: u64,
        repo_name: String,
    },
    /// Create a new task.
    ///
    /// Used by reconciliation logic to generate tasks for orphaned PRs or other
    /// conditions discovered during polling ticks.
    CreateTask {
        repo_name: String,
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

    /// Invoke the channel's workflow script with a domain event.
    ///
    /// When a workflow script exists for the event's channel (resolved via
    /// `paths::workflow_script_for_channel`), the daemon spawns it as
    /// `uv run <script> --event <json> --state <state_file> --socket <socket>`.
    /// If no script is found, this effect is a no-op.
    ///
    /// On non-zero exit or spawn failure, the script's stderr is posted to the
    /// channel as a system message so failures are visible in the chat log.
    EmitWorkflowEvent(crate::workflow::WorkflowEvent),
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

    // Clean up all transient coworker state (shared with session death path).
    // This releases the name back to NamePool, cleans up session reverse maps
    // (name_to_session, session_to_name, task_to_session), and marks the
    // SessionRecord as stopped in persistent state.
    state.cleanup_coworker_state(name).await;

    // Unbind from worktree registry (worktree persists for build cache reuse)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.worktree_registry.unbind_coworker(name);
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            warn!(
                "Failed to save daemon state after unbinding coworker: {}",
                e
            );
        }
    }
    Ok(())
}

/// Resolve a session ID to its coworker name and deliver a nudge message.
///
/// Shared implementation for `NudgeSession` and `NudgeSessionWithCallbacks`.
/// Returns `true` on successful delivery, `false` on failure (name not found
/// or send error). On success, the nudge is recorded for attribution tracking.
async fn send_session_nudge(
    state: &DaemonState,
    session_id: &str,
    reason: &super::wake_reason::WakeReason,
) -> bool {
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
        return false;
    };
    let msg = reason.to_nudge_message();
    match state.session_manager.send_message(&name, &msg).await {
        Ok(()) => {
            state.record_pending_nudge(&name, &msg);
            true
        }
        Err(e) => {
            warn!("Failed to nudge session {}: {}", session_id, e);
            false
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
        && let Err(e) = ps.save_for_repo(&state.repo_name)
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

                match spawn_with_resume_fallback(state, &state.repo_name, &mut config).await {
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
                let team_name = crate::mailbox::team_name_for_repo(&state.repo_name);
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
            } => {
                let has_explicit_channel = channel.is_some();

                // Resolve the target channel:
                // 1. Use explicit channel if provided
                // 2. Otherwise, try to extract task ID from message and look up its channel
                // 3. Fall back to default channel if no task mentioned
                let channel_name = if let Some(ch) = channel {
                    Some(ch)
                } else {
                    state.resolve_message_channel(&message).await
                };

                let msg = if let Some(ch) = channel_name {
                    Message::for_channel(&ch, &sender, &message, crate::message::MessageType::Text)
                } else {
                    Message::text(&sender, &message)
                };
                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                    warn!("Failed to post channel message: {}", e);
                }

                // Clear tool activity for this agent when they post a channel message.
                // A channel post signals the end of a work phase — the activity strip should reset.
                // Skip system senders (midtown) and non-fork explicit-channel posts.
                // Forked sessions should still clear tool activity when they post to
                // their inherited thread channel.
                let is_system_sender = matches!(sender.to_lowercase().as_str(), "midtown" | "user")
                    || sender.eq_ignore_ascii_case(&state.repo_name);
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
                items,
            } => {
                // Store items in DaemonState for TUI RPC consumers (kanban.data).
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
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!(
                        "Failed to persist ClearProfileLimit for {}: {}",
                        profile_email, e
                    );
                }
            }
            Effect::ResetTaskToPending { task_id, repo_name } => {
                if let Err(e) = crate::tasks::reset_task_to_pending_for_repo(&task_id, &repo_name) {
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

                if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                // Extract task IDs from on_success RecordTaskAssignment effects
                // to clear their in-flight markers after the spawn completes.
                let task_ids: Vec<String> = on_success
                    .iter()
                    .filter_map(|e| {
                        if let Effect::RecordTaskAssignment { task_id, .. } = e {
                            Some(task_id.clone())
                        } else {
                            None
                        }
                    })
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
                repo_name,
                config,
                on_success,
                on_failure,
            } => {
                // Spawn the coworker and set ownership + in_progress on disk.
                // The coworker also claims the task via `midtown task claim` after starting,
                // which writes ownership directly via the daemon.
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
                            if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                            crate::tasks::set_task_in_progress_for_repo(&task_id, &repo_name)
                        {
                            warn!(
                                "Failed to set task !{} to in_progress after spawn: {}",
                                task_id, e
                            );
                        }
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
            Effect::MarkRemindersFired {
                fired_ids,
                repo_name,
            } => {
                let mut ps = state.persistent_state.lock().await;
                for reminder in &mut ps.reminders.reminders {
                    if fired_ids.contains(&reminder.id) {
                        reminder.fired = true;
                    }
                }
                if let Err(e) = ps.save_for_repo(&repo_name) {
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
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                    ps.clear_reviewer_assignment(name, &state.repo_name);
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
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!("Failed to persist PR author session: {}", e);
                } else {
                    info!(
                        "Stored author session for PR #{}: session={}, author={}",
                        pr_number, session_id, author
                    );
                }
            }
            Effect::CompleteTask { task_id, repo_name } => {
                if let Err(e) = crate::tasks::complete_task_for_repo(&task_id, &repo_name) {
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
                        if let Err(e) = ps.save_for_repo(&repo_name) {
                            warn!("Failed to save task completion state: {}", e);
                        }
                    }
                    // Clear task assignment tracking (coworker is now free)
                    state.clear_task_assignment_by_task(&task_id);
                }
            }
            Effect::ClearBlockedBy {
                completed_task_id,
                repo_name,
            } => {
                if let Err(e) =
                    crate::tasks::clear_blocked_by_for_repo(&completed_task_id, &repo_name)
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
                repo_name,
            } => {
                if let Err(e) = crate::tasks::update_task_fields_for_repo(
                    &task_id,
                    &repo_name,
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
            Effect::ForceCleanupWorktrees { names } => {
                if names.is_empty() {
                    continue;
                }
                let coworkers = state.coworkers.clone();

                // Filter out names where headless sessions are still alive.
                // Check session_manager before entering spawn_blocking since is_alive is async.
                let mut to_cleanup = Vec::new();
                for name in names {
                    // Guard: check SessionManager directly to avoid a race where
                    // the headless session exists but hasn't yet registered in
                    // the daemon's coworkers map.
                    if state.session_manager.is_alive(&name).await {
                        warn!(
                            "Skipping cleanup of worktree for {} — headless session still running",
                            name
                        );
                        continue;
                    }
                    // Double-check: skip if coworker is registered in the manager
                    if coworkers.get(&name).is_some() {
                        warn!(
                            "Skipping cleanup of worktree for {} — coworker still registered",
                            name
                        );
                        continue;
                    }
                    to_cleanup.push(name);
                }

                if to_cleanup.is_empty() {
                    continue;
                }

                tokio::task::spawn_blocking(move || {
                    for name in to_cleanup {
                        match coworkers.force_cleanup_worktree(&name) {
                            Ok(()) => {
                                info!("Cleaned up orphaned worktree for {} (PR was merged)", name);
                            }
                            Err(e) => {
                                warn!("Failed to cleanup merged-PR worktree for {}: {}", name, e);
                            }
                        }
                    }
                })
                .await
                .ok();
            }
            Effect::SendPushNotification { title, body, tag } => {
                state.send_push_notification(&title, &body, &tag);
            }
            Effect::CleanStaleBranches => {
                let coworkers = state.coworkers.clone();
                let cleaned =
                    tokio::task::spawn_blocking(move || coworkers.clean_stale_coworker_branches())
                        .await
                        .unwrap_or_default();
                if !cleaned.is_empty() {
                    info!(
                        "Cleaned up {} stale coworker branch(es): {}",
                        cleaned.len(),
                        cleaned.join(", ")
                    );
                }
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
                        && let Err(e) = ps.save_for_repo(&state.repo_name)
                    {
                        warn!("Failed to save daemon state after worktree cleanup: {}", e);
                    }
                    removed
                };
                if let Some(assignment) = removed {
                    // Remove the worktree directory using the primary worktree manager
                    let wt_mgr = state.coworkers.worktree_manager().clone();
                    let wt_id = assignment.worktree_id.clone();
                    let task_id = assignment.task_id.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = wt_mgr.force_cleanup_task_worktree(&wt_id) {
                            warn!("Failed to remove task worktree {}: {}", wt_id, e);
                        } else {
                            info!(
                                "Cleaned up task worktree {} (PR #{} merged)",
                                wt_id, pr_number
                            );
                        }
                    })
                    .await
                    .ok();
                    // Post to ops channel so the team sees what was cleaned up
                    let task_ref = task_id
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
                        && let Err(e) = ps.save_for_repo(&state.repo_name)
                    {
                        warn!(
                            "Failed to save daemon state after stale worktree cleanup: {}",
                            e
                        );
                    }
                    removed
                };
                if let Some(assignment) = removed {
                    // Remove the worktree directory
                    let wt_mgr = state.coworkers.worktree_manager().clone();
                    let wt_id = assignment.worktree_id.clone();
                    let task_id = assignment.task_id.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = wt_mgr.force_cleanup_task_worktree(&wt_id) {
                            warn!("Failed to remove stale worktree {}: {}", wt_id, e);
                        } else {
                            info!(
                                "Cleaned up stale worktree {} (retention period expired)",
                                wt_id
                            );
                        }
                    })
                    .await
                    .ok();
                    // Post to ops channel so the team sees what was cleaned up
                    let task_ref = task_id
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
                                // Do NOT bind - this would crash both Claude Code sessions
                                return;
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
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                let base_dir = crate::paths::projects_dir_for_repo(&state.repo_name);
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
                        if let Err(e) = ps.save_for_repo(&state.repo_name) {
                            warn!(
                                "Failed to save daemon state before spawning channel lead: {}",
                                e
                            );
                        }
                    }
                    let config = crate::launch::LaunchConfig::channel_lead(
                        &name,
                        &state.repo_name,
                        crate::launch::SessionMode::Fresh,
                        "", // domain_context: empty at creation, accumulates via session
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
                            if let Err(save_err) = ps.save_for_repo(&state.repo_name) {
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
                            if let Err(save_err) = ps.save_for_repo(&state.repo_name) {
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
                let base_dir = crate::paths::projects_dir_for_repo(&state.repo_name);

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
                        if let Err(e) = channel.archive() {
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
                                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                let base_dir = crate::paths::projects_dir_for_repo(&state.repo_name);
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
                let messages = match from_channel.read_all() {
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
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                    if let Err(e) = from_channel.archive() {
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
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!("Failed to save task_channel after assignment: {}", e);
                }
                // Also update the task file on disk so dispatch.rs reads the correct channel
                // (dispatch reads task.channel from the file, not from persistent state)
                if let Err(e) = crate::tasks::update_task_fields_for_repo(
                    &task_id,
                    &state.repo_name,
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
            Effect::UnassignTask { task_id, repo_name } => {
                if let Err(e) = crate::tasks::unassign_task_for_repo(&task_id, &repo_name) {
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
                repo_name,
            } => {
                if let Err(e) = crate::tasks::reset_task_to_pending_for_repo(&task_id, &repo_name) {
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
                repo_name,
                subject,
                description,
                pr,
            } => {
                // If a PR number is associated, skip creation if a non-completed task
                // already exists for that PR. This prevents duplicate follow-up tasks
                // when multiple review comments arrive in quick succession (e.g., after
                // a daemon restart resets the in-memory cooldown).
                if let Some(pr_num) = pr {
                    let existing = crate::tasks::read_tasks_for_repo(Some(&repo_name));
                    let already_has_task = existing.iter().any(|t| {
                        t.pr == Some(pr_num) && t.status != crate::tasks::TaskStatus::Completed
                    });
                    if already_has_task {
                        debug!(
                            "Skipping CreateTask for PR #{}: non-completed task already exists",
                            pr_num
                        );
                        return;
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
                    &repo_name,
                    None, // blocked_by
                    None, // channel
                    pr,   // pr (from effect)
                ) {
                    Ok(task_id) => {
                        info!("Created task !{}: {}", task_id, subject);
                        // Post channel notification
                        let msg =
                            crate::message::Message::system(format!("created task: {}", subject));
                        if let Err(e) = state.send_and_broadcast_async(&msg).await {
                            warn!("Failed to post task creation message: {}", e);
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
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!(
                        "Failed to save daemon state after saving channel lead session: {}",
                        e
                    );
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
                // 1. Allocate name from NamePool
                let channel_lead_names: std::collections::HashSet<String> = {
                    let ps = state.persistent_state.lock().await;
                    ps.channel_lead_sessions.keys().cloned().collect()
                };
                let name = {
                    let mut pool = state.name_pool.lock().unwrap();
                    pool.allocate_excluding(preferred_name.as_deref(), &channel_lead_names)
                };
                let Some(name) = name else {
                    warn!("No available names for SpawnSession {}", session_id);
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
                    let team_name = crate::mailbox::team_name_for_repo(&state.repo_name);
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
                            // Mark any old session records with this name as not running.
                            // Names are reused from the pool, so previous sessions for
                            // this name may still have is_running=true if they weren't
                            // properly cleaned up (e.g., after daemon restart).
                            for record in ps.sessions.values_mut() {
                                if record.session_id != session_id
                                    && record.is_running
                                    && (record.preferred_name.as_deref() == Some(&name)
                                        || record.current_name.as_deref() == Some(&name))
                                {
                                    record.is_running = false;
                                }
                            }
                            // Look up task_thread_id so coworker posts route to the
                            // fork session's thread (if the task was created with --thread-id).
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
                            if let Err(e) = ps.save_for_repo(&state.repo_name) {
                                warn!("Failed to save persistent state after SpawnSession: {}", e);
                            }
                        }

                        record_session_recovery_cooldown(&state.cooldowns, &session_id, resume);

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
                                &state.repo_name,
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
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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

                    match (session_id.as_deref(), can_resume_channel_lead) {
                        (Some(id), true) => {
                            let mut config = crate::launch::LaunchConfig::channel_lead(
                                &channel_name,
                                &state.repo_name,
                                crate::launch::SessionMode::ResumeSession(id.to_string()),
                                "",
                            );
                            config.initial_prompt = Some(reason.to_initial_prompt(&channel_name));

                            match spawn_with_resume_fallback(state, &state.repo_name, &mut config)
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
                                        if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                                        if let Err(save_err) = ps.save_for_repo(&state.repo_name) {
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
                                        if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                                &state.repo_name,
                                crate::launch::SessionMode::Fresh,
                                "",
                            );
                            config.initial_prompt = Some(reason.to_initial_prompt(&channel_name));
                            // Insert empty placeholder before spawning to guard against
                            // duplicate NudgeChannelLead effects in the same batch.
                            {
                                let mut ps = state.persistent_state.lock().await;
                                ps.channel_lead_sessions
                                    .insert(channel_name.clone(), String::new());
                                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                                    tracing::error!(
                                        "Failed to save state before spawning channel lead: {}",
                                        e
                                    );
                                }
                            }
                            match spawn_with_resume_fallback(state, &state.repo_name, &mut config)
                                .await
                            {
                                Ok((session_id, _)) => {
                                    // Update channel_lead_sessions with the real session_id
                                    // immediately (spawn_coworker generated it upfront),
                                    // eliminating the race window before init event arrives.
                                    let mut ps = state.persistent_state.lock().await;
                                    ps.channel_lead_sessions
                                        .insert(channel_name.clone(), session_id);
                                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                send_session_nudge(state, &session_id, &reason).await;
            }
            Effect::NudgeSessionWithCallbacks {
                session_id,
                reason,
                on_success,
            } => {
                // Extract task IDs from on_success RecordTaskAssignment effects
                // to clear their in-flight markers after the nudge completes.
                let task_ids: Vec<String> = on_success
                    .iter()
                    .filter_map(|e| {
                        if let Effect::RecordTaskAssignment { task_id, .. } = e {
                            Some(task_id.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                if send_session_nudge(state, &session_id, &reason).await {
                    Box::pin(execute_effects(on_success, state)).await;
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
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
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
                let suffix = if name.eq_ignore_ascii_case(&state.repo_name) {
                    " Headless session will respawn on the next tick."
                } else if is_channel_lead {
                    " Channel lead session will be respawned for its channel."
                } else {
                    " Session will be reassigned via normal task dispatch."
                };
                let mut msg = crate::message::Message::system(format!(
                    "⚠️ Auto-detached stale attached session for {} — interactive session ended without detach.{}",
                    name, suffix
                ));
                msg.channel = Some(OPS_CHANNEL.to_string());
                if let Err(e) = state.send_and_broadcast_async(&msg).await {
                    warn!("Failed to post auto-detach message: {}", e);
                }
            }

            Effect::EmitWorkflowEvent(event) => {
                invoke_workflow_script(state, event).await;
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
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
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

/// Auto-merge a PR using `gh pr merge --squash`.
///
/// Posts a channel message on success or failure.
/// TODO: Wire up to Effect::AutoMergePr when auto-merge logic is complete.
#[allow(dead_code)]
async fn auto_merge_pr(state: &DaemonState, pr_number: u64, title: &str) {
    use super::helpers::truncate_str;

    let output = match tokio::process::Command::new("gh")
        .args(["pr", "merge", &pr_number.to_string(), "--squash", "--auto"])
        .output()
        .await
    {
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

/// Invoke the channel's workflow script for a `WorkflowEvent`.
///
/// Resolves the script path using the 4-level priority order in `paths.rs`, then
/// spawns `uv run <script> --event <json> --state <state_file> --socket <socket>`
/// with a 30-second timeout. If no script is found, returns immediately (no-op).
///
/// On non-zero exit or I/O failure the script's stderr is posted to the event's
/// channel as a system message so workflow errors surface in the chat log.
async fn invoke_workflow_script(state: &DaemonState, event: crate::workflow::WorkflowEvent) {
    let channel = event.channel().to_string();

    // Resolve the workflow script path (4-level priority order).
    let project_root = state.all_repo_paths.first().cloned().unwrap_or_default();
    let script_path =
        crate::paths::workflow_script_for_channel(&channel, &project_root, &state.repo_name);
    let Some(script) = script_path else {
        // No workflow script configured for this channel — silent no-op.
        return;
    };

    // Serialize the event to JSON for the --event flag.
    let event_json = match serde_json::to_string(&event) {
        Ok(json) => json,
        Err(e) => {
            warn!(
                "Failed to serialize WorkflowEvent for channel '{}': {}",
                channel, e
            );
            return;
        }
    };

    let state_file = crate::paths::workflow_state_file(&channel, &state.repo_name);
    let socket = &state.socket_path;

    debug!(
        channel = %channel,
        script = %script.display(),
        "invoke_workflow_script: spawning uv run"
    );

    let child = tokio::process::Command::new("uv")
        .args([
            "run",
            script.to_str().unwrap_or_default(),
            "--event",
            &event_json,
            "--state",
            state_file.to_str().unwrap_or_default(),
            "--socket",
            socket.to_str().unwrap_or_default(),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        // Kill the subprocess if the future is dropped (e.g. 30s timeout fires).
        // Without this, timed-out processes survive and can accumulate as orphans.
        .kill_on_drop(true)
        .spawn();

    let child = match child {
        Ok(c) => c,
        Err(e) => {
            warn!(
                channel = %channel,
                script = %script.display(),
                "invoke_workflow_script: failed to spawn uv: {}",
                e
            );
            post_workflow_error(state, &channel, &script, &format!("spawn error: {e}")).await;
            return;
        }
    };

    let result =
        tokio::time::timeout(std::time::Duration::from_secs(30), child.wait_with_output()).await;

    match result {
        Err(_timeout) => {
            // wait_with_output consumed `child`; the future was dropped by the timeout,
            // `kill_on_drop(true)` ensures the subprocess is killed when the
            // future is dropped here.
            warn!(
                channel = %channel,
                script = %script.display(),
                "invoke_workflow_script: timed out after 30s"
            );
            post_workflow_error(
                state,
                &channel,
                &script,
                "workflow script timed out after 30s",
            )
            .await;
        }
        Ok(Err(e)) => {
            warn!(
                channel = %channel,
                script = %script.display(),
                "invoke_workflow_script: wait_with_output error: {}",
                e
            );
            post_workflow_error(state, &channel, &script, &format!("I/O error: {e}")).await;
        }
        Ok(Ok(output)) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                warn!(
                    channel = %channel,
                    script = %script.display(),
                    exit = ?output.status,
                    "invoke_workflow_script: non-zero exit"
                );
                let detail = if stderr.is_empty() {
                    format!("exited with {}", output.status)
                } else {
                    format!("exited with {}: {}", output.status, stderr)
                };
                post_workflow_error(state, &channel, &script, &detail).await;
            } else {
                debug!(
                    channel = %channel,
                    script = %script.display(),
                    "invoke_workflow_script: completed successfully"
                );
            }
        }
    }
}

/// Post a workflow script error to its channel as a system message.
async fn post_workflow_error(
    state: &DaemonState,
    channel: &str,
    script: &std::path::Path,
    detail: &str,
) {
    let mut msg = crate::message::Message::system(format!(
        "⚠️ Workflow script error ({}): {}",
        script.file_name().unwrap_or_default().to_string_lossy(),
        detail
    ));
    msg.channel = Some(channel.to_string());
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post workflow error message: {}", e);
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
    repo_name: &str,
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
                repo_name,
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

#[path = "effects_tests.rs"]
#[cfg(test)]
mod tests;
