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
    /// Avatar color override (CSS color string, e.g., "#ff5f5f").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Lucide icon name for avatar (e.g., "shield", "database").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
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
            color: None,
            icon: None,
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

    /// Active session IDs — running coworkers + alive headless sessions.
    #[serde(skip)]
    pub tick_active_session_ids: HashSet<String>,

    /// Whether the in-progress task count has reached the configured limit.
    #[serde(skip)]
    pub tick_is_at_task_limit: bool,

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

    // ── Health-check tick fields ──────────────────────────────────────────
    // Populated by `prepare_tick()` for health.rs decision functions.
    /// Whether a usage-limit nudge is already scheduled.
    #[serde(skip)]
    pub tick_usage_limit_nudge_scheduled: bool,

    /// The scheduled usage-limit nudge time (if any).
    #[serde(skip)]
    pub tick_usage_limit_nudge_at: Option<tokio::time::Instant>,

    /// Reviewer name → assigned PR number (from all reviewer sessions + task_pr_number).
    #[serde(skip)]
    pub tick_reviewer_pr_assignments: HashMap<String, u64>,

    /// PR number → restart count for reviewer backoff.
    #[serde(skip)]
    pub tick_reviewer_restart_counts: HashMap<u64, u32>,

    /// Placeholder comment IDs for PRs with an unupdated "Review in progress" comment.
    #[serde(skip)]
    pub tick_reviewer_in_progress_comment_ids: HashMap<u64, u64>,

    /// Name → session ID mapping (lowercase name → session_id).
    #[serde(skip)]
    pub tick_name_session_map: HashMap<String, String>,

    // ── Dispatch tick fields ────────────────────────────────────────────
    // Populated by `prepare_tick()` for dispatch.rs decision functions.
    /// Stale working-dir sessions (worktree path no longer exists on disk).
    #[serde(skip)]
    pub tick_stale_working_dir_sessions: HashSet<String>,

    /// PR-protected task IDs (tasks that should not be orphan-recovered).
    #[serde(skip)]
    pub tick_pr_protected_tasks: HashSet<String>,

    /// Busy coworker names (lowercase) — coworkers with in-progress tasks.
    #[serde(skip)]
    pub tick_busy_coworkers: HashSet<String>,

    /// Active reviewer names (lowercase).
    #[serde(skip)]
    pub tick_active_reviewers: HashSet<String>,

    /// Pending tasks with owners — (task_id, subject, owner).
    #[serde(skip)]
    pub tick_pending_tasks_with_owners: Vec<(String, String, String)>,

    /// In-progress tasks — (task_id, subject, owner).
    #[serde(skip)]
    pub tick_in_progress_tasks: Vec<(String, String, String)>,

    /// Blocks map: task_id → list of task IDs that it blocks.
    #[serde(skip)]
    pub tick_blocks_map: HashMap<String, Vec<String>>,

    /// Session task map: task_id → session_id.
    #[serde(skip)]
    pub tick_session_task_map: HashMap<String, String>,

    /// PR number → branch name for merged PRs (from worktree registry).
    #[serde(skip)]
    pub tick_merged_pr_branches: HashMap<u64, String>,
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
                    "Loaded daemon state: {} reminders, CI stats: {}, {} worktree assignments, {} channel-lead sessions, {} profile-pool entries, {} channel-workflow assignments, {} workflow-state channels, {} lead-driven channels",
                    state.reminders.reminders.len(),
                    state.ci_stats.summary(),
                    state.worktree_registry.len(),
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
            "Saved daemon state: {} reminders, CI stats: {}, {} worktree assignments, {} channel-lead sessions, {} profile-pool entries, {} channel-workflow assignments, {} workflow-state channels, {} lead-driven channels",
            self.reminders.reminders.len(),
            self.ci_stats.summary(),
            self.worktree_registry.len(),
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

        // 2. Count orphaned tasks (metadata lives in TaskStore now, no maps to prune)
        result.orphaned_tasks_pruned = orphaned_task_ids.len();

        result
    }

    /// Returns the active reviewer session for a PR, if any.
    pub fn active_reviewer_for_pr(&self, pr_number: u64) -> Option<&SessionRecord> {
        self.sessions
            .values()
            .filter(|s| s.agent_type == "midtown-code-reviewer" && s.is_running)
            .find(|s| s.pr_number == Some(pr_number))
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

    /// Returns all reviewer sessions (running or stopped).
    /// Used by snapshot to include dead reviewers for respawn detection.
    pub fn all_reviewer_sessions(&self) -> Vec<&SessionRecord> {
        self.sessions
            .values()
            .filter(|s| s.agent_type == "midtown-code-reviewer")
            .collect()
    }

    /// Insert a session record for a task.
    ///
    /// Creates a `SessionRecord` with the given parameters and inserts it into
    /// `self.sessions`. The caller should set `pr_number` on the returned record
    /// if needed (loaded from TaskStore).
    pub fn insert_session_for_task(
        &mut self,
        task_id: &str,
        agent_name: &str,
        agent_type: &str,
        session_id: &str,
    ) {
        let sid = if session_id.is_empty() {
            format!("sess-{}", task_id)
        } else {
            session_id.to_string()
        };
        self.sessions.insert(
            sid.clone(),
            SessionRecord {
                session_id: sid,
                name: agent_name.to_string(),
                agent_type: agent_type.to_string(),
                task_id: Some(task_id.to_string()),
                is_running: true,
                ..Default::default()
            },
        );
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
            channel_lead_sessions: HashMap::new(),
            sessions: HashMap::new(),
            profile_pool_state: HashMap::new(),
            channel_workflows: HashMap::new(),
            lead_driven_channels: HashSet::new(),
            workflow_state,
            permanent_pr_nudges: Vec::new(),
            _task_session_spans: serde_json::Value::Null,
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
        if session_id.is_empty() {
            tracing::warn!(
                "upsert_session_running: refusing to insert record with empty session_id (name: {})",
                new_record.name
            );
            return;
        }
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

    /// Find the best session record bound to a given thread.
    ///
    /// Prefers running sessions over stopped ones. Returns `None` if no session
    /// is bound to this thread. This replaces the old `topic_sessions` in-memory
    /// map — SessionRecord is the single source of truth.
    pub fn session_by_thread(&self, thread_id: &str) -> Option<&SessionRecord> {
        let mut best: Option<&SessionRecord> = None;
        for s in self.sessions.values() {
            if s.bound_thread_id.as_deref() == Some(thread_id) {
                if s.is_running {
                    return Some(s);
                }
                if best.is_none() {
                    best = Some(s);
                }
            }
        }
        best
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

    /// Look up the topic channel for a PR via tick_pr_task_index and a tasks slice.
    pub fn channel_for_pr(
        &self,
        pr_number: u64,
        tasks: &[crate::task_store::Task],
    ) -> Option<String> {
        let task_id = self.tick_pr_task_index.task_for_pr(pr_number)?;
        tasks
            .iter()
            .find(|t| t.id == task_id)
            .and_then(|t| t.channel.clone())
    }

    /// Look up the topic channel for a PR, falling back to project name.
    pub fn channel_for_pr_or_default(
        &self,
        pr_number: u64,
        tasks: &[crate::task_store::Task],
    ) -> String {
        self.channel_for_pr(pr_number, tasks)
            .unwrap_or_else(|| self.tick_project_name.clone())
    }

    /// Derive usage-limited coworkers from tick_process_health.
    pub fn usage_limited_coworkers(&self) -> std::collections::HashSet<String> {
        self.tick_process_health
            .iter()
            .filter(|(_, h)| h.has_usage_limit)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Derive auth-error coworkers from tick_process_health.
    pub fn auth_error_coworkers(&self) -> std::collections::HashSet<String> {
        self.tick_process_health
            .iter()
            .filter(|(_, h)| h.has_auth_error)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Derive API-error coworkers from tick_process_health.
    pub fn api_error_coworkers(&self) -> std::collections::HashSet<String> {
        self.tick_process_health
            .iter()
            .filter(|(_, h)| h.has_api_error)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Derive tool-name-conflict coworkers from tick_process_health.
    pub fn tool_name_conflict_coworkers(&self) -> std::collections::HashSet<String> {
        self.tick_process_health
            .iter()
            .filter(|(_, h)| h.has_tool_name_conflict)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Get coworker names that have sessions with open PRs.
    pub fn sessions_with_open_prs(&self) -> std::collections::HashSet<String> {
        let open_pr_numbers: std::collections::HashSet<u64> = self
            .tick_open_prs
            .iter()
            .filter_map(|pr| pr["number"].as_u64())
            .collect();

        self.sessions
            .values()
            .filter(|s| s.pr_number.is_some_and(|pr| open_pr_numbers.contains(&pr)))
            .filter_map(|s| {
                if s.name.is_empty() {
                    None
                } else {
                    Some(s.name.clone())
                }
            })
            .collect()
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

    /// Find a session record by task ID using the pre-built tick_session_task_map.
    pub fn find_session_for_task(&self, task_id: &str) -> Option<&SessionRecord> {
        let session_id = self.tick_session_task_map.get(task_id)?;
        self.sessions.get(session_id)
    }

    /// Check whether a worktree is bound to a different ACTIVE coworker.
    pub fn worktree_collision(&self, worktree_id: &str, intended_coworker: &str) -> Option<String> {
        let assignment = self.worktree_registry.get(worktree_id)?;
        let bound_coworker = assignment.current_coworker.as_deref()?;

        if bound_coworker.eq_ignore_ascii_case(intended_coworker) {
            return None;
        }

        let bound_lower = bound_coworker.to_lowercase();
        if self.tick_active_session_names.contains(&bound_lower) {
            return Some(bound_coworker.to_string());
        }

        None
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
