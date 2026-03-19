//! Unified persistent state for the midtown daemon.
//!
//! Consolidates what was previously spread across multiple JSON files
//! (github-state.json, reminders.json) into a single daemon-state.json.
//! Loaded once at startup, saved after any mutation.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ci_stats::CiCheckStats;
use crate::daemon::trackers::PrIssueType;
use crate::github_state::GitHubState;
use crate::reminders::ReminderState;
use crate::worktree_registry::WorktreeRegistry;

/// Per-profile usage state for pool-based profile selection.
///
/// Persisted in `DaemonPersistentState::profile_pool_state` keyed by profile email.
/// Used by the profile pool selector to skip limited profiles and pick LRU.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileState {
    /// Whether this profile is currently at a usage limit.
    pub is_usage_limited: bool,
    /// When the usage limit resets (if known).
    pub usage_limit_reset_at: Option<DateTime<Utc>>,
    /// When this profile was last used to spawn a coworker.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Summary of what a garbage collection pass cleaned up.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GcResult {
    /// Number of dead session records removed.
    pub sessions_removed: usize,
    /// Number of orphaned task metadata entries pruned.
    pub orphaned_tasks_pruned: usize,
}

impl GcResult {
    /// Returns true if any cleanup was performed.
    pub fn has_changes(&self) -> bool {
        self.sessions_removed + self.orphaned_tasks_pruned > 0
    }
}

/// A temporal record of a session working on a task.
///
/// Tracks the time span during which a specific session was assigned to a task.
/// A session record for the session-centric coworker model.
///
/// Keyed by `session_id` in `DaemonPersistentState::sessions`.
/// Tracks the full lifecycle of a headless coworker session — from spawn
/// through suspend/resume cycles to final shutdown. Keyed by session ID,
/// allowing names to be reassigned between sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Platform-agnostic session ID (opaque string from Claude Code).
    pub session_id: String,
    /// Task this session is working on (e.g., "1561").
    pub task_id: Option<String>,
    /// Stable name for this session (assigned at creation, never changes).
    pub name: String,
    /// Worktree path for this session.
    pub working_dir: String,
    /// Git branch the session is working on.
    pub branch: Option<String>,
    /// Associated PR number (set when coworker opens a PR).
    pub pr_number: Option<u64>,
    /// Initial prompt used to start the session (for restart/clear).
    pub initial_prompt: Option<String>,
    /// Agent type: "midtown-code-author", "midtown-code-reviewer", or "midtown-channel-lead".
    pub agent_type: String,
    /// Whether the session process is currently running.
    pub is_running: bool,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// Whether to resume this session on daemon restart.
    pub resume_on_startup: bool,
    /// Thread ID this session is bound to for automatic output tagging.
    ///
    /// When set (for forked topic sessions), all channel posts from this session
    /// are automatically tagged with this thread_parent_id so output appears in
    /// the correct thread without the session needing to pass `--thread` manually.
    #[serde(default)]
    pub bound_thread_id: Option<String>,
    /// Last time this session was active (event received or message sent).
    #[serde(default = "Utc::now")]
    pub last_active: DateTime<Utc>,
    /// Human-readable purpose (e.g., "task !5: Add auth endpoint").
    #[serde(default)]
    pub purpose: String,
    /// OS process ID for zombie detection and cleanup.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Channel name for channel-lead sessions and channel-routed tasks.
    #[serde(default)]
    pub channel: Option<String>,
    /// Auth provider (Claude, Codex, or Zai) — where the account lives.
    #[serde(default)]
    pub provider: Option<crate::auth::AuthProvider>,
    /// Platform (Claude Code or Codex CLI) — which agent tool binary.
    #[serde(default)]
    pub platform: Option<crate::platform::Platform>,
    /// Auth profile name (e.g., "ben@example.com") — account identity.
    #[serde(default)]
    pub profile: Option<String>,
    /// How many times this session has been restarted.
    #[serde(default)]
    pub restart_count: u32,
}

impl Default for SessionRecord {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            task_id: None,
            name: String::new(),
            working_dir: String::new(),
            branch: None,
            pr_number: None,
            initial_prompt: None,
            agent_type: "midtown-code-author".to_string(),
            is_running: false,
            created_at: Utc::now(),
            resume_on_startup: true,
            bound_thread_id: None,
            last_active: Utc::now(),
            purpose: String::new(),
            pid: None,
            channel: None,
            provider: None,
            platform: None,
            profile: None,
            restart_count: 0,
        }
    }
}

impl SessionRecord {
    /// Whether this session is a fork (channel-lead bound to a thread).
    ///
    /// Fork sessions are channel-lead sessions spawned for research/investigation
    /// in threads. They inherit task_id from the parent but should not be treated
    /// as PR owners or task dispatch targets. Regular dev coworkers also carry
    /// bound_thread_id (for thread routing) but ARE genuine task owners.
    pub fn is_fork_session(&self) -> bool {
        self.agent_type == "midtown-channel-lead" && self.bound_thread_id.is_some()
    }
}

/// All persistent daemon state in one struct.
///
/// Serialized to `~/.midtown/projects/<repo>/daemon-state.json`.
/// Contains GitHub PR state and one-shot reminders. Loaded at startup
/// and saved after every mutation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonPersistentState {
    /// GitHub PR reviewer assignments, review cache, pending spawns.
    #[serde(default)]
    pub github: GitHubState,

    /// One-shot condition-based reminders.
    #[serde(default)]
    pub reminders: ReminderState,

    /// CI check duration statistics for auto-retry of stale checks.
    #[serde(default)]
    pub ci_stats: CiCheckStats,

    /// Task-based worktree registry mapping tasks to worktrees by branch slug.
    /// Enables build cache reuse across coworker reassignment and automatic
    /// cleanup on PR merge.
    #[serde(default)]
    pub worktree_registry: WorktreeRegistry,

    /// Task-to-channel assignment mapping for message routing.
    /// Maps task ID → channel name. Used by the daemon to route coworker messages
    /// to the appropriate topic channel based on the task they're working on.
    /// Persists across daemon restarts so channel routing survives.
    #[serde(default)]
    pub task_channel: HashMap<String, String>,

    /// Task-to-model assignment mapping for coworker spawn.
    /// Maps task ID → model specification (e.g., "claude/opus", "claude/sonnet").
    /// Used by the daemon to launch coworkers with the requested model when spawning
    /// for a task. Stored separately from Claude Code's native task storage for
    /// compatibility. Persists across daemon restarts.
    #[serde(default)]
    pub task_model: HashMap<String, String>,

    /// Task-to-plan mapping for plan-driven execution.
    /// Maps task ID → absolute path to a plan file (e.g., "docs/plans/2026-02-13-feature.md").
    /// When a coworker is spawned for a task with a plan, the daemon reads the file
    /// and includes its content in the coworker's initial prompt. Stored separately
    /// from Claude Code's native task storage for compatibility.
    #[serde(default)]
    pub task_plan: HashMap<String, String>,

    /// Task-to-execution-skill mapping for plan-driven execution.
    /// Maps task ID → skill name (e.g., "subagent-driven-development", "executing-plans").
    /// When a coworker is spawned for a task with an execution skill, the daemon includes
    /// an explicit instruction to use that skill. Stored separately from Claude Code's
    /// native task storage for compatibility.
    #[serde(default)]
    pub task_execution_skill: HashMap<String, String>,

    /// Task-to-thread-ID mapping for thread routing.
    ///
    /// Maps task ID → thread_parent_id. Populated in two ways:
    /// 1. Explicitly via `--thread-id` on `midtown task create` (e.g., from fork sessions).
    /// 2. Auto-defaulted to the task's announcement message ID when no explicit
    ///    thread ID is provided, ensuring coworker posts route to the task thread.
    ///
    /// The daemon sets `bound_thread_id` on the spawned coworker's `SessionRecord`
    /// using this mapping, wiring the coworker's channel output into the correct thread.
    #[serde(default)]
    pub task_thread_id: HashMap<String, String>,

    /// Task-to-creation-message mapping for opening tasks as threads.
    ///
    /// Maps task ID → message ID (UUID of the "created task:" channel message).
    /// When a task is created, the daemon posts a notification message and stores
    /// its ID here. The TUI and web app use this to open a task as a thread,
    /// showing the task metadata as the header and allowing discussion replies.
    #[serde(default)]
    pub task_message_id: HashMap<String, String>,

    /// Task-to-parent mapping for UI grouping of related tasks.
    ///
    /// Maps child task ID → parent task ID. Parent-child is a UI grouping
    /// relationship for showing related tasks (e.g., a review task as a child
    /// of its implementation task). Child tasks can start while the parent is
    /// open — this is purely organizational, not a blocking dependency.
    #[serde(default)]
    pub task_parent: HashMap<String, String>,

    /// Task-to-agent-type mapping for specialized task dispatch.
    ///
    /// Maps task ID → agent type name (e.g., "midtown-code-reviewer").
    /// When set, the task dispatch system uses the specified agent definition
    /// instead of the default coworker agent. Used to route review tasks
    /// through the task dispatch system with the correct agent behavior.
    #[serde(default)]
    pub task_agent_type: HashMap<String, String>,

    /// Task-to-placeholder-comment-ID mapping for reviewer status comments.
    ///
    /// Maps task ID → GitHub comment ID. When a reviewer is spawned for a PR,
    /// a "Review in progress..." placeholder comment is posted. The comment ID
    /// is stored here so the reviewer can update it with the final review.
    #[serde(default)]
    pub task_placeholder_comment_id: HashMap<String, u64>,

    /// Task-to-restart-count mapping for reviewer backoff.
    ///
    /// Maps task ID → number of times the reviewer session has been restarted.
    /// Used to implement exponential backoff for stuck reviewers.
    #[serde(default)]
    pub task_restart_count: HashMap<String, u32>,

    /// Channel lead session IDs for resume-on-demand.
    ///
    /// Maps channel name → Claude Code session ID. One channel lead session
    /// per active (non-archived) topic channel. Spawned/resumed at daemon
    /// startup and when channels are created. Shut down when channels are archived.
    #[serde(default)]
    pub channel_lead_sessions: HashMap<String, String>,

    /// Session records for the session-centric coworker model.
    ///
    /// Maps session_id → SessionRecord. Primary store for coworker session state.
    #[serde(default)]
    pub sessions: HashMap<String, SessionRecord>,

    /// Per-profile usage and LRU state for pool-based profile selection.
    ///
    /// Maps profile email → ProfileState. Persists across daemon restarts
    /// so LRU ordering and usage-limit state survive restarts.
    #[serde(default)]
    pub profile_pool_state: HashMap<String, ProfileState>,

    /// Channel-to-workflow assignment mapping.
    ///
    /// Maps channel name → workflow name (e.g., "proj-auth" → "tdw").
    /// Workflows are named entities in `~/.midtown/projects/<project>/workflows/<name>/`.
    /// Multiple channels can share the same workflow. Channels without an entry
    /// use the daemon's default behavior (no workflow).
    #[serde(default)]
    pub channel_workflows: HashMap<String, String>,

    /// Channels operating in lead-driven mode.
    ///
    /// When a channel is in this set, the daemon relays workflow events as
    /// human-readable @mentions to the channel lead instead of executing its
    /// built-in state machine (auto-dispatch, reviewer spawning, PR nudges).
    #[serde(default)]
    pub lead_driven_channels: HashSet<String>,

    /// Workflow state, owned by the daemon.
    ///
    /// Maps channel name → per-channel state (JSON object). Nested keys
    /// within a channel's state can be set via the `key` parameter on
    /// `workflow.set_state` (e.g. `tasks.42.excluded`).
    ///
    /// Persisted as part of `daemon-state.json` for single-source-of-truth
    /// semantics and atomic updates.
    #[serde(default)]
    pub workflow_state: HashMap<String, serde_json::Value>,

    /// Permanent PR nudge entries that survive daemon restarts.
    ///
    /// Stores one-shot nudge records (e.g., ReviewComplete for user-authored PRs)
    /// so they aren't re-sent after a daemon restart. Restored into
    /// `PrIssueTracker::permanent` on startup.
    #[serde(default)]
    pub permanent_pr_nudges: Vec<(u64, PrIssueType)>,

    /// Temporal session history for tasks.
    /// Legacy field — kept for deserialization compat, not used.
    #[serde(default, rename = "task_session_spans", skip_serializing)]
    pub _task_session_spans: serde_json::Value,

    /// Task-to-PR-number mapping for reviewer tasks.
    /// Set at review task creation so PR lookups work before the reviewer session
    /// populates SessionRecord.pr_number.
    #[serde(default)]
    pub task_pr_number: HashMap<String, u64>,

    /// Write-through task index for fast lookups without directory scanning.
    ///
    /// Populated from `TaskStore::build_index()` on daemon startup.
    /// Updated after every `TaskStore::save()` call. Contains task status,
    /// parent, and agent_name for each task.
    #[serde(default)]
    pub task_index: HashMap<String, crate::task_store::TaskIndexEntry>,

    // ── Per-tick ephemeral state ──────────────────────────────────────────
    // Populated by `prepare_tick()` before each tick evaluation.
    // Not persisted to daemon-state.json.
    /// Process health for headless coworkers, keyed by name.
    #[serde(skip)]
    pub tick_process_health: HashMap<String, crate::daemon::snapshot::ProcessHealth>,

    /// Cached open PR data from last GitHub poll.
    #[serde(skip)]
    pub tick_open_prs: Vec<serde_json::Value>,

    /// Number of PRs needing review.
    #[serde(skip)]
    pub tick_prs_needing_review: usize,

    /// Merged PR numbers from last poll.
    #[serde(skip)]
    pub tick_merged_pr_numbers: HashSet<u64>,

    /// GitHub API rate limit state.
    #[serde(skip)]
    pub tick_rate_limit: crate::github_rate_limit::GitHubRateLimit,

    /// Freshly fetched rate limit (only during RateLimitCheckTick).
    #[serde(skip)]
    pub tick_fresh_rate_limit: Option<crate::github_rate_limit::GitHubRateLimit>,

    /// PR↔task index built from sessions + GitHub PR titles.
    #[serde(skip)]
    pub tick_pr_task_index: crate::daemon::snapshot::PrTaskIndex,

    /// Pre-evaluated cooldown states.
    #[serde(skip)]
    pub tick_orphan_spawn_cooldown_active: bool,
    #[serde(skip)]
    pub tick_session_dispatch_cooldown_active: bool,
    #[serde(skip)]
    pub tick_spawn_failure_cooldown_names: HashSet<String>,
    #[serde(skip)]
    pub tick_note_staleness_cooldown_channels: HashSet<String>,
    #[serde(skip)]
    pub tick_merge_rebase_nudge_cooldown_names: HashSet<String>,
    #[serde(skip)]
    pub tick_rebase_nudge_processed_prs: HashSet<u64>,
    #[serde(skip)]
    pub tick_rebase_regression_cooldown_names: HashSet<String>,
    #[serde(skip)]
    pub tick_lead_worktree_freshness_cooldown_channels: HashSet<String>,
    #[serde(skip)]
    pub tick_task_nudge_cooldown_ids: HashSet<String>,
    #[serde(skip)]
    pub tick_recently_recovered_session_ids: HashSet<String>,
    #[serde(skip)]
    pub tick_in_flight_task_spawns: HashSet<String>,

    /// Coworker start/stop times from DaemonState caches.
    #[serde(skip)]
    pub tick_coworker_start_times: HashMap<String, DateTime<Utc>>,
    #[serde(skip)]
    pub tick_coworker_stop_times: HashMap<String, DateTime<Utc>>,

    /// Attached coworkers with attach timestamp.
    #[serde(skip)]
    pub tick_attached_coworkers: HashMap<String, DateTime<Utc>>,

    /// Config constants from DaemonState.
    #[serde(skip)]
    pub tick_dir_key: String,
    #[serde(skip)]
    pub tick_project_name: String,
    #[serde(skip)]
    pub tick_default_channel: String,
    #[serde(skip)]
    pub tick_default_branch: String,
    #[serde(skip)]
    pub tick_repo_owner: Option<String>,
    #[serde(skip)]
    pub tick_max_in_progress_tasks: usize,
    #[serde(skip)]
    pub tick_lead_refresh_interval_secs: u64,
    #[serde(skip)]
    pub tick_now: DateTime<Utc>,

    /// Stale channel lead worktrees (behind origin/main).
    #[serde(skip)]
    pub tick_stale_lead_worktrees: HashSet<String>,

    /// Topic/fork sessions: thread_parent_id → session_id.
    #[serde(skip)]
    pub tick_topic_sessions: HashMap<String, String>,

    /// Session profile mapping: coworker name → auth profile email.
    #[serde(skip)]
    pub tick_session_profile_map: HashMap<String, String>,

    /// Pool profiles currently at usage limit.
    #[serde(skip)]
    pub tick_limited_pool_profiles: HashSet<String>,

    /// Channel messages for debugging context.
    #[serde(skip)]
    pub tick_channel_messages: Vec<crate::message::Message>,

    /// Daemon log tail for debugging context.
    #[serde(skip)]
    pub tick_daemon_logs: Vec<String>,

    /// Reviewer escalations already posted.
    #[serde(skip)]
    pub tick_reviewer_escalations_posted: HashSet<u64>,

    /// Orphaned PR lead nudges already sent.
    #[serde(skip)]
    pub tick_orphaned_pr_nudges_sent: HashSet<u64>,

    /// Archived channels.
    #[serde(skip)]
    pub tick_archived_channels: HashSet<String>,

    /// Stale channel notes.
    #[serde(skip)]
    pub tick_stale_channel_notes: HashMap<String, Vec<String>>,

    /// Active session names (lowercase) — running coworkers + alive headless sessions.
    #[serde(skip)]
    pub tick_active_session_names: HashSet<String>,

    /// Active coworker data from CoworkerManager.
    #[serde(skip)]
    pub tick_active_coworkers: Vec<crate::coworker::Coworker>,

    /// Running coworker data from CoworkerManager.
    #[serde(skip)]
    pub tick_running_coworkers: Vec<crate::coworker::Coworker>,

    /// Session name string (e.g., "midtown-projectname").
    #[serde(skip)]
    pub tick_session_name: String,
}

impl DaemonPersistentState {
    /// Load from the unified state file for a repository.
    ///
    /// If `daemon-state.json` doesn't exist, attempts migration from the
    /// legacy separate files (github-state.json, reminders.json). If those
    /// don't exist either, returns default state.
    pub fn load_for_repo(repo: &str) -> io::Result<Self> {
        let path = crate::paths::daemon_state_file_for_repo(repo);
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let mut state: Self = serde_json::from_str(&contents).map_err(|e| {
                    warn!("Failed to parse daemon-state.json: {}", e);
                    io::Error::new(ErrorKind::InvalidData, e)
                })?;
                // Rebuild reverse indexes that aren't serialized
                state.worktree_registry.rebuild_indexes();

                // Migrate legacy per-channel workflow-state.json files if
                // workflow_state is empty (upgrade from pre-migration installs).
                if state.workflow_state.is_empty() {
                    let (migrated, files_to_delete) = Self::migrate_workflow_state_files(repo);
                    if !migrated.is_empty() {
                        debug!(
                            "Migrated {} legacy workflow-state.json file(s) into existing daemon state",
                            migrated.len()
                        );
                        state.workflow_state = migrated;
                        // Persist the migrated state before deleting legacy files
                        if let Err(e) = state.save_for_repo(repo) {
                            warn!(
                                "Failed to save daemon state after workflow migration: {}",
                                e
                            );
                        } else {
                            for path in &files_to_delete {
                                let _ = fs::remove_file(path);
                                debug!("Removed legacy workflow-state.json: {}", path.display());
                            }
                        }
                    }
                }

                debug!(
                    "Loaded daemon state: {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings, {} task-model mappings, {} task-plan mappings, {} task-execution-skill mappings, {} task-thread-id mappings, {} task-message-id mappings, {} task-parent mappings, {} channel-lead sessions, {} profile-pool entries, {} channel-workflow assignments, {} workflow-state channels, {} lead-driven channels",
                    state.reminders.reminders.len(),
                    state.ci_stats.summary(),
                    state.worktree_registry.len(),
                    state.task_channel.len(),
                    state.task_model.len(),
                    state.task_plan.len(),
                    state.task_execution_skill.len(),
                    state.task_thread_id.len(),
                    state.task_message_id.len(),
                    state.task_parent.len(),
                    state.channel_lead_sessions.len(),
                    state.profile_pool_state.len(),
                    state.channel_workflows.len(),
                    state.workflow_state.len(),
                    state.lead_driven_channels.len()
                );
                Ok(state)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                debug!("daemon-state.json not found, attempting migration from legacy files");
                Self::migrate_from_legacy(repo)
            }
            Err(e) => Err(e),
        }
    }

    /// Save to the unified state file atomically (temp file + rename).
    pub fn save_for_repo(&self, repo: &str) -> io::Result<()> {
        let path = crate::paths::daemon_state_file_for_repo(repo);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, &contents)?;
        crate::paths::atomic_rename(&tmp_path, &path)?;
        debug!(
            "Saved daemon state: {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings, {} task-model mappings, {} task-plan mappings, {} task-execution-skill mappings, {} task-parent mappings, {} channel-lead sessions, {} profile-pool entries, {} channel-workflow assignments, {} workflow-state channels, {} lead-driven channels",
            self.reminders.reminders.len(),
            self.ci_stats.summary(),
            self.worktree_registry.len(),
            self.task_channel.len(),
            self.task_model.len(),
            self.task_plan.len(),
            self.task_execution_skill.len(),
            self.task_parent.len(),
            self.channel_lead_sessions.len(),
            self.profile_pool_state.len(),
            self.channel_workflows.len(),
            self.workflow_state.len(),
            self.lead_driven_channels.len()
        );
        Ok(())
    }

    /// Apply garbage collection mutations to this state in-place.
    ///
    /// Removes dead sessions and prunes orphaned task metadata map entries.
    /// Returns a summary of what was cleaned up.
    ///
    /// This is a pure mutation method — no I/O. The caller is responsible
    /// for saving state and logging after calling this.
    pub fn apply_gc(
        &mut self,
        dead_session_ids: &[String],
        orphaned_task_ids: &[String],
    ) -> GcResult {
        let mut result = GcResult::default();

        // 1. Remove dead sessions entirely
        for sid in dead_session_ids {
            if self.sessions.remove(sid).is_some() {
                result.sessions_removed += 1;
            }
        }

        // 2. Prune orphaned task metadata maps
        for task_id in orphaned_task_ids {
            self.task_channel.remove(task_id);
            self.task_model.remove(task_id);
            self.task_plan.remove(task_id);
            self.task_execution_skill.remove(task_id);
            self.task_thread_id.remove(task_id);
            self.task_message_id.remove(task_id);
            self.task_parent.remove(task_id);
            self.task_agent_type.remove(task_id);
            self.task_placeholder_comment_id.remove(task_id);
            self.task_restart_count.remove(task_id);
            self.task_pr_number.remove(task_id);
            result.orphaned_tasks_pruned += 1;
        }

        result
    }

    /// Returns the active reviewer session for a PR, if any.
    ///
    /// Checks both `task_pr_number` (set at task creation) and
    /// `SessionRecord.pr_number` (set when the session opens the PR).
    pub fn active_reviewer_for_pr(&self, pr_number: u64) -> Option<&SessionRecord> {
        self.sessions
            .values()
            .filter(|s| s.agent_type == "midtown-code-reviewer" && s.is_running)
            .find(|s| {
                s.pr_number == Some(pr_number)
                    || s.task_id
                        .as_ref()
                        .and_then(|tid| self.task_pr_number.get(tid))
                        == Some(&pr_number)
            })
    }

    /// Returns true if PR has an active reviewer session that is currently running.
    pub fn pr_has_active_reviewer(&self, pr_number: u64) -> bool {
        self.active_reviewer_for_pr(pr_number).is_some()
    }

    /// Returns all running reviewer sessions.
    pub fn active_reviewer_sessions(&self) -> Vec<&SessionRecord> {
        self.sessions
            .values()
            .filter(|s| s.agent_type == "midtown-code-reviewer" && s.is_running)
            .collect()
    }

    /// Look up the bound thread ID for a task from `task_thread_id`.
    ///
    /// Used by both the `SpawnForTask` effect path and `spawn_coworker()` to
    /// resolve a task's announcement thread so channel posts are auto-tagged.
    pub fn resolve_bound_thread_id(&self, task_id: Option<&str>) -> Option<String> {
        task_id.and_then(|tid| self.task_thread_id.get(tid).cloned())
    }

    /// Migrate from legacy separate files into the unified format.
    ///
    /// Loads github-state.json and reminders.json if they exist,
    /// combines them into a single DaemonPersistentState, saves as
    /// daemon-state.json, then removes the old files.
    fn migrate_from_legacy(repo: &str) -> io::Result<Self> {
        let github = crate::github_state::load_state_for_repo(repo).unwrap_or_else(|e| {
            if e.kind() != ErrorKind::NotFound {
                warn!(
                    "Failed to load legacy github-state.json during migration: {}",
                    e
                );
            }
            GitHubState::default()
        });

        let reminder_path = crate::paths::reminders_file_for_repo(repo);
        let reminders = ReminderState::load(&reminder_path).unwrap_or_else(|e| {
            if e.kind() != ErrorKind::NotFound {
                warn!(
                    "Failed to load legacy reminders.json during migration: {}",
                    e
                );
            }
            ReminderState::default()
        });

        // Migrate any existing per-channel workflow-state.json files.
        let (workflow_state, workflow_files_to_delete) = Self::migrate_workflow_state_files(repo);

        let state = Self {
            github,
            reminders,
            ci_stats: CiCheckStats::default(),
            worktree_registry: WorktreeRegistry::default(),
            task_channel: HashMap::new(),
            task_model: HashMap::new(),
            task_plan: HashMap::new(),
            task_execution_skill: HashMap::new(),
            task_thread_id: HashMap::new(),
            task_message_id: HashMap::new(),
            task_parent: HashMap::new(),
            task_agent_type: HashMap::new(),
            task_placeholder_comment_id: HashMap::new(),
            task_restart_count: HashMap::new(),
            channel_lead_sessions: HashMap::new(),
            sessions: HashMap::new(),
            profile_pool_state: HashMap::new(),
            channel_workflows: HashMap::new(),
            lead_driven_channels: HashSet::new(),
            workflow_state,
            permanent_pr_nudges: Vec::new(),
            _task_session_spans: serde_json::Value::Null,
            task_pr_number: HashMap::new(),
            task_index: HashMap::new(),
            ..Default::default()
        };

        // Save the unified file
        if let Err(e) = state.save_for_repo(repo) {
            warn!("Failed to save migrated daemon-state.json: {}", e);
            return Err(e);
        }

        // Clean up legacy files (best-effort, don't fail if removal fails).
        // Deletion is deferred until after save_for_repo succeeds to avoid
        // data loss if a crash occurs between reading and writing.
        let github_path = crate::paths::github_state_file_for_repo(repo);
        if github_path.exists() {
            let _ = fs::remove_file(&github_path);
            debug!("Removed legacy github-state.json after migration");
        }
        if reminder_path.exists() {
            let _ = fs::remove_file(&reminder_path);
            debug!("Removed legacy reminders.json after migration");
        }
        for path in &workflow_files_to_delete {
            let _ = fs::remove_file(path);
            debug!("Removed legacy workflow-state.json: {}", path.display());
        }

        Ok(state)
    }

    /// Update or insert a session record, marking existing stopped sessions as running.
    ///
    /// When resuming a stopped session, `entry().or_insert_with()` alone won't update
    /// `is_running` because the entry already exists. This method uses `and_modify` to
    /// mark existing sessions as running and refresh `name` before falling back
    /// to insert for new sessions.
    pub fn upsert_session_running(&mut self, session_id: String, new_record: SessionRecord) {
        let name = new_record.name.clone();
        self.sessions
            .entry(session_id)
            .and_modify(|r| {
                r.is_running = true;
                r.name = name;
            })
            .or_insert(new_record);
    }

    /// Clear reviewer assignment for a coworker and save state.
    ///
    /// Returns true if an assignment was cleared, false if the coworker had no assignment.
    /// This helper is used by both RPC handlers (coworker.break) and Effect handlers
    /// Used by both RPC handlers (coworker.break) and Effect handlers to avoid
    /// duplicating the cleanup logic.
    pub fn clear_reviewer_assignment(&mut self, reviewer_name: &str, repo: &str) -> bool {
        // Mark matching reviewer sessions as not running
        let session_ids: Vec<String> = self
            .sessions
            .values()
            .filter(|s| {
                s.name == reviewer_name && s.agent_type == "midtown-code-reviewer" && s.is_running
            })
            .map(|s| s.session_id.clone())
            .collect();
        for sid in &session_ids {
            if let Some(record) = self.sessions.get_mut(sid) {
                tracing::info!("Cleared reviewer session {} for {}", sid, reviewer_name);
                record.is_running = false;
                record.resume_on_startup = false;
            }
        }

        let cleared = !session_ids.is_empty();
        if cleared && let Err(e) = self.save_for_repo(repo) {
            tracing::warn!(
                "Failed to save persistent state after clearing reviewer assignment: {}",
                e
            );
        }
        cleared
    }

    /// Returns the set of active channel lead names (keys of `channel_lead_sessions`).
    pub fn channel_lead_names(&self) -> std::collections::HashSet<String> {
        self.channel_lead_sessions.keys().cloned().collect()
    }

    /// Find a session record by coworker name (exact match).
    pub fn session_by_name(&self, name: &str) -> Option<&SessionRecord> {
        self.sessions.values().find(|s| s.name == name)
    }

    /// Find a mutable session record by coworker name.
    pub fn session_by_name_mut(&mut self, name: &str) -> Option<&mut SessionRecord> {
        self.sessions.values_mut().find(|s| s.name == name)
    }

    /// Find a session record by task ID.
    pub fn session_by_task(&self, task_id: &str) -> Option<&SessionRecord> {
        self.sessions
            .values()
            .find(|s| s.task_id.as_deref() == Some(task_id))
    }

    /// All running reviewer sessions.
    pub fn running_reviewer_sessions(&self) -> Vec<&SessionRecord> {
        self.sessions
            .values()
            .filter(|s| s.agent_type == "midtown-code-reviewer" && s.is_running)
            .collect()
    }

    /// Name → task assignments derived from sessions.
    pub fn name_task_assignments(&self) -> HashMap<String, String> {
        self.sessions
            .values()
            .filter(|s| !s.name.is_empty())
            .filter_map(|s| {
                s.task_id
                    .as_ref()
                    .map(|tid| (s.name.to_lowercase(), tid.clone()))
            })
            .collect()
    }

    /// Migrate per-channel `workflow-state.json` files into in-memory state.
    ///
    /// Scans `~/.midtown/projects/<repo>/channels/*/workflow-state.json`,
    /// loads each file's content, and collects them into a HashMap keyed
    /// by channel name. Returns the migrated state and paths to legacy files
    /// that should be deleted after the combined state is persisted.
    fn migrate_workflow_state_files(
        repo: &str,
    ) -> (HashMap<String, serde_json::Value>, Vec<PathBuf>) {
        let channels_dir = crate::paths::projects_dir_for_repo(repo).join("channels");
        Self::migrate_workflow_state_from_dir(&channels_dir)
    }

    /// Core migration logic that scans a channels directory for legacy
    /// `workflow-state.json` files. Separated from `migrate_workflow_state_files`
    /// for testability (no dependency on global paths).
    ///
    /// Returns (migrated_state, legacy_files_to_delete). Callers must persist
    /// the combined state before deleting the legacy files to avoid data loss
    /// on crash.
    fn migrate_workflow_state_from_dir(
        channels_dir: &Path,
    ) -> (HashMap<String, serde_json::Value>, Vec<PathBuf>) {
        let mut workflow_state = HashMap::new();
        let mut files_to_delete = Vec::new();

        let entries = match fs::read_dir(channels_dir) {
            Ok(e) => e,
            Err(_) => return (workflow_state, files_to_delete), // No channels dir — nothing to migrate
        };

        for entry in entries.flatten() {
            if !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
                continue;
            }
            let channel_name = entry.file_name().to_string_lossy().to_string();
            let state_file = entry.path().join("workflow-state.json");

            if let Ok(content) = fs::read_to_string(&state_file) {
                match serde_json::from_str::<serde_json::Value>(&content) {
                    Ok(value) => {
                        debug!(
                            channel = %channel_name,
                            "Migrated workflow-state.json into daemon state"
                        );
                        workflow_state.insert(channel_name, value);
                        files_to_delete.push(state_file);
                    }
                    Err(e) => {
                        warn!(
                            channel = %channel_name,
                            "Failed to parse legacy workflow-state.json during migration: {}",
                            e
                        );
                    }
                }
            }
        }

        if !workflow_state.is_empty() {
            debug!(
                "Migrated {} channel workflow-state.json file(s) into daemon state",
                workflow_state.len()
            );
        }

        (workflow_state, files_to_delete)
    }
}

/// Derive a `pr_number → task_id` map from session records.
///
/// Only sessions that have both `pr_number` and `task_id` set are included.
/// Used as the primary source for PR↔task mapping in the task-centric model.
pub fn pr_to_task_map_from_sessions(
    sessions: &HashMap<String, SessionRecord>,
) -> HashMap<u64, String> {
    sessions
        .values()
        .filter_map(|s| {
            let pr = s.pr_number?;
            let task = s.task_id.as_ref()?;
            Some((pr, task.clone()))
        })
        .collect()
}

/// Derive a `task_id → pr_number` map from session records.
///
/// Only sessions that have both `pr_number` and `task_id` set are included.
/// Used as the primary source for task↔PR mapping in the task-centric model.
pub fn task_to_pr_map_from_sessions(
    sessions: &HashMap<String, SessionRecord>,
) -> HashMap<String, u64> {
    sessions
        .values()
        .filter_map(|s| {
            let pr = s.pr_number?;
            let task = s.task_id.as_ref()?;
            Some((task.clone(), pr))
        })
        .collect()
}

#[path = "state_tests.rs"]
#[cfg(test)]
mod tests;
