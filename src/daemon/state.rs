//! Unified persistent state for the midtown daemon.
//!
//! Consolidates what was previously spread across multiple JSON files
//! (github-state.json, reminders.json) into a single daemon-state.json.
//! Loaded once at startup, saved after any mutation.

use std::collections::HashMap;
use std::fs;
use std::io::{self, ErrorKind};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ci_stats::CiCheckStats;
use crate::github_state::GitHubState;
use crate::reminders::ReminderState;
use crate::worktree_registry::WorktreeRegistry;

/// Persisted info about a headless Claude Code session.
///
/// Stored in `DaemonPersistentState` to survive daemon restarts. The daemon
/// uses these session IDs to resume coworker sessions after restart, and to
/// support `midtown view` (which pauses headless execution and resumes it
/// in an interactive terminal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessSessionInfo {
    /// Claude Code session ID (used with `--resume <id>`).
    pub session_id: String,
    /// Last time this session was active (event received or message sent).
    pub last_active: DateTime<Utc>,
    /// Human-readable purpose (e.g., "task !5: Add auth endpoint", "reviewer for PR #42").
    pub purpose: String,
    /// OS process ID for zombie detection and cleanup.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Coworker type: "dev", "reviewer", or "channel-lead".
    #[serde(default)]
    pub coworker_type: Option<String>,
    /// Task ID if this is a dev coworker working on a task.
    #[serde(default)]
    pub task_id: Option<u64>,
    /// PR number if this is a reviewer coworker.
    #[serde(default)]
    pub pr_number: Option<u64>,
    /// Channel name if this is a channel-lead coworker.
    #[serde(default)]
    pub channel: Option<String>,
    /// Working directory (worktree path) for this session.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Provider (Claude or Codex) for this session.
    #[serde(default)]
    pub provider: Option<crate::auth::AuthProvider>,
    /// Auth profile name for this session (e.g., "ben@example.com").
    /// Used to restore the correct auth profile directory on daemon restart.
    #[serde(default)]
    pub profile: Option<String>,
    /// Whether this session should be auto-resumed when the daemon starts.
    ///
    /// Historical sessions remain persisted for manual attach/resume, but only
    /// sessions marked `true` are recovered automatically during startup.
    #[serde(default = "default_resume_on_startup")]
    pub resume_on_startup: bool,
    /// Full initial prompt sent to this session (for session clear/restart).
    #[serde(default)]
    pub initial_prompt: Option<String>,
}

fn default_resume_on_startup() -> bool {
    true
}

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
    /// Current name allocation (None if suspended/name released).
    pub current_name: Option<String>,
    /// Preferred name for next resume (the name it had last time).
    pub preferred_name: Option<String>,
    /// Worktree path for this session.
    pub working_dir: String,
    /// Git branch the session is working on.
    pub branch: Option<String>,
    /// Associated PR number (set when coworker opens a PR).
    pub pr_number: Option<u64>,
    /// Initial prompt used to start the session (for restart/clear).
    pub initial_prompt: Option<String>,
    /// Whether this is a reviewer session (ephemeral, never resumed after shutdown).
    pub is_reviewer: bool,
    /// Coworker type: "dev", "reviewer", or "channel-lead".
    pub coworker_type: String,
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
}

impl Default for SessionRecord {
    fn default() -> Self {
        Self {
            session_id: String::new(),
            task_id: None,
            current_name: None,
            preferred_name: None,
            working_dir: String::new(),
            branch: None,
            pr_number: None,
            initial_prompt: None,
            is_reviewer: false,
            coworker_type: "dev".to_string(),
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
        }
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

    /// Legacy headless sessions keyed by coworker name (read-only for migration).
    ///
    /// Superseded by `sessions` (session-ID-keyed `SessionRecord`). Kept for
    /// one release so older state files still deserialize. New data is written
    /// only to `sessions`; this field is never serialized.
    #[serde(default, skip_serializing)]
    pub headless_sessions: HashMap<String, HeadlessSessionInfo>,

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

    /// Task-to-thread-ID mapping for fork session routing.
    ///
    /// Maps task ID → thread_parent_id. When a fork session creates a task with
    /// `--thread-id`, coworker updates for that task are automatically tagged with
    /// this thread_parent_id so they appear in the fork session's thread. The daemon
    /// sets `bound_thread_id` on the spawned coworker's `SessionRecord` using this
    /// mapping, wiring the coworker's channel output into the correct thread.
    #[serde(default)]
    pub task_thread_id: HashMap<String, String>,

    /// Channel lead session IDs for resume-on-demand.
    ///
    /// Maps channel name → Claude Code session ID. One channel lead session
    /// per active (non-archived) topic channel. Spawned/resumed at daemon
    /// startup and when channels are created. Shut down when channels are archived.
    #[serde(default)]
    pub channel_lead_sessions: HashMap<String, String>,

    /// Session records for the session-centric coworker model.
    ///
    /// Maps session_id → SessionRecord. This is the primary store for coworker
    /// session state. Legacy `headless_sessions` entries are auto-migrated into
    /// this map at load time via `migrate_headless_to_sessions()`.
    #[serde(default)]
    pub sessions: HashMap<String, SessionRecord>,
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
                // Migrate legacy headless_sessions → sessions (one-time)
                state.migrate_headless_to_sessions();
                debug!(
                    "Loaded daemon state: {} PR reviewers, {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings, {} task-model mappings, {} task-plan mappings, {} task-execution-skill mappings, {} task-thread-id mappings, {} channel-lead sessions",
                    state.github.pr_reviewers.len(),
                    state.reminders.reminders.len(),
                    state.ci_stats.summary(),
                    state.worktree_registry.len(),
                    state.task_channel.len(),
                    state.task_model.len(),
                    state.task_plan.len(),
                    state.task_execution_skill.len(),
                    state.task_thread_id.len(),
                    state.channel_lead_sessions.len()
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
            "Saved daemon state: {} PR reviewers, {} reminders, CI stats: {}, {} worktree assignments, {} task-channel mappings, {} task-model mappings, {} task-plan mappings, {} task-execution-skill mappings, {} channel-lead sessions",
            self.github.pr_reviewers.len(),
            self.reminders.reminders.len(),
            self.ci_stats.summary(),
            self.worktree_registry.len(),
            self.task_channel.len(),
            self.task_model.len(),
            self.task_plan.len(),
            self.task_execution_skill.len(),
            self.channel_lead_sessions.len()
        );
        Ok(())
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

        let state = Self {
            github,
            reminders,
            ci_stats: CiCheckStats::default(),
            worktree_registry: WorktreeRegistry::default(),
            headless_sessions: HashMap::new(),
            task_channel: HashMap::new(),
            task_model: HashMap::new(),
            task_plan: HashMap::new(),
            task_execution_skill: HashMap::new(),
            task_thread_id: HashMap::new(),
            channel_lead_sessions: HashMap::new(),
            sessions: HashMap::new(),
        };

        // Save the unified file
        if let Err(e) = state.save_for_repo(repo) {
            warn!("Failed to save migrated daemon-state.json: {}", e);
            return Err(e);
        }

        // Clean up legacy files (best-effort, don't fail if removal fails)
        let github_path = crate::paths::github_state_file_for_repo(repo);
        if github_path.exists() {
            let _ = fs::remove_file(&github_path);
            debug!("Removed legacy github-state.json after migration");
        }
        if reminder_path.exists() {
            let _ = fs::remove_file(&reminder_path);
            debug!("Removed legacy reminders.json after migration");
        }

        Ok(state)
    }

    /// Update or insert a session record, marking existing stopped sessions as running.
    ///
    /// When resuming a stopped session, `entry().or_insert_with()` alone won't update
    /// `is_running` because the entry already exists. This method uses `and_modify` to
    /// mark existing sessions as running and refresh `current_name` before falling back
    /// to insert for new sessions.
    pub fn upsert_session_running(&mut self, session_id: String, new_record: SessionRecord) {
        let current_name = new_record.current_name.clone();
        self.sessions
            .entry(session_id)
            .and_modify(|r| {
                r.is_running = true;
                r.current_name = current_name;
            })
            .or_insert(new_record);
    }

    /// Migrate legacy `headless_sessions` entries into `sessions` (SessionRecord).
    ///
    /// Called once at load time. If `headless_sessions` has entries that don't already
    /// have a corresponding SessionRecord, creates one. This handles the case where
    /// an older daemon version wrote `headless_sessions` but not `sessions`.
    fn migrate_headless_to_sessions(&mut self) {
        if self.headless_sessions.is_empty() {
            return;
        }
        let mut migrated = 0usize;
        for (name, info) in &self.headless_sessions {
            if info.session_id.is_empty() {
                continue;
            }
            // Skip if a SessionRecord already exists for this session_id
            if self.sessions.contains_key(&info.session_id) {
                continue;
            }
            let record = SessionRecord {
                session_id: info.session_id.clone(),
                current_name: Some(name.clone()),
                preferred_name: Some(name.clone()),
                working_dir: info.working_dir.clone().unwrap_or_default(),
                coworker_type: info
                    .coworker_type
                    .clone()
                    .unwrap_or_else(|| "dev".to_string()),
                task_id: info.task_id.map(|id| id.to_string()),
                pr_number: info.pr_number,
                channel: info.channel.clone(),
                initial_prompt: info.initial_prompt.clone(),
                is_running: false, // not running yet — recovery will start them
                resume_on_startup: info.resume_on_startup,
                last_active: info.last_active,
                purpose: info.purpose.clone(),
                pid: info.pid,
                provider: info.provider,
                profile: info.profile.clone(),
                ..Default::default()
            };
            self.sessions.insert(info.session_id.clone(), record);
            migrated += 1;
        }
        if migrated > 0 {
            debug!(
                "Migrated {} headless_sessions entries to sessions (SessionRecord)",
                migrated
            );
        }
    }

    /// Clear reviewer assignment for a coworker and save state.
    ///
    /// Returns true if an assignment was cleared, false if the coworker had no assignment.
    /// This helper is used by both RPC handlers (coworker.break) and Effect handlers
    /// (ClearOrphanedReviewerAssignments) to avoid duplicating the cleanup logic.
    pub fn clear_reviewer_assignment(&mut self, reviewer_name: &str, repo: &str) -> bool {
        if let Some(assignment) = self.github.remove_assignment_by_reviewer(reviewer_name) {
            tracing::info!(
                "Cleared reviewer assignment for {} (was reviewing PR #{})",
                reviewer_name,
                assignment.pr_number
            );
            if let Err(e) = self.save_for_repo(repo) {
                tracing::warn!(
                    "Failed to save persistent state after clearing reviewer assignment: {}",
                    e
                );
            }
            true
        } else {
            false
        }
    }
}

#[path = "state_tests.rs"]
#[cfg(test)]
mod tests;
