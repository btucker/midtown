//! Unified persistent state for the midtown daemon.
//!
//! Loaded from `~/.midtown/projects/<repo>/daemon-state.json`.
//! Contains GitHub PR state, session records, workflow assignments, and more.
//! Read by the web API for status and workflow endpoints.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::ci_stats::CiCheckStats;
use crate::github_state::GitHubState;
use crate::reminders::ReminderState;
use crate::worktree_registry::WorktreeRegistry;

// ---------------------------------------------------------------------------
// PrIssueType (inlined from former daemon::trackers)
// ---------------------------------------------------------------------------

/// Types of actionable PR issues
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrIssueType {
    /// PR has merge conflicts
    MergeConflict,
    /// CI checks failed
    CiFailed,
    /// Review requested changes
    ChangesRequested,
    /// PR is approved and ready to merge
    Approved,
    /// PR has review comments from non-owners
    ReviewComment,
    /// PR review is complete (Claude review posted), author should act
    ReviewComplete,
    /// PR has all CI checks passing and has review feedback to address
    GreenWithFeedback,
}

impl std::fmt::Display for PrIssueType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PrIssueType::MergeConflict => write!(f, "merge conflict"),
            PrIssueType::CiFailed => write!(f, "CI failed"),
            PrIssueType::ChangesRequested => write!(f, "changes requested"),
            PrIssueType::Approved => write!(f, "approved"),
            PrIssueType::ReviewComment => write!(f, "review comment"),
            PrIssueType::ReviewComplete => write!(f, "review complete"),
            PrIssueType::GreenWithFeedback => write!(f, "CI green with feedback"),
        }
    }
}

// ---------------------------------------------------------------------------
// Session record types
// ---------------------------------------------------------------------------

/// Per-profile usage state for pool-based profile selection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileState {
    pub is_usage_limited: bool,
    pub usage_limit_reset_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Summary of what a garbage collection pass cleaned up.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct GcResult {
    pub sessions_removed: usize,
    pub orphaned_tasks_pruned: usize,
}

impl GcResult {
    pub fn has_changes(&self) -> bool {
        self.sessions_removed + self.orphaned_tasks_pruned > 0
    }
}

/// A session record for the session-centric coworker model.
///
/// Keyed by `session_id` in `DaemonPersistentState::sessions`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub task_id: Option<String>,
    pub name: String,
    pub working_dir: String,
    pub branch: Option<String>,
    #[serde(default, skip_serializing)]
    pub pr_number: Option<u64>,
    pub initial_prompt: Option<String>,
    pub agent_type: String,
    pub is_running: bool,
    pub created_at: DateTime<Utc>,
    pub resume_on_startup: bool,
    #[serde(default)]
    pub bound_thread_id: Option<String>,
    #[serde(default = "Utc::now")]
    pub last_active: DateTime<Utc>,
    #[serde(default)]
    pub purpose: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub provider: Option<crate::auth::AuthProvider>,
    #[serde(default)]
    pub platform: Option<crate::platform::Platform>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub restart_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_badge: Option<String>,
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
            avatar_badge: None,
        }
    }
}

impl SessionRecord {
    pub fn is_fork_session(&self) -> bool {
        self.agent_type == "midtown-channel-lead" && self.bound_thread_id.is_some()
    }

    pub fn is_reviewer(&self) -> bool {
        self.agent_type == "midtown-code-reviewer"
    }

    pub fn is_active_reviewer(&self) -> bool {
        self.is_reviewer() && self.is_running
    }

    pub fn is_active_fork(&self) -> bool {
        self.is_fork_session() && self.is_running
    }
}

/// Per-channel settings controlling daemon behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSettings {
    #[serde(default = "default_true")]
    pub show_full_lead_output: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self {
            show_full_lead_output: true,
        }
    }
}

/// Per-user read state for threads and channels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadState {
    #[serde(default)]
    pub threads: HashMap<String, String>,
    #[serde(default)]
    pub channels: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// DaemonPersistentState — persisted fields only (no tick_ ephemeral state)
// ---------------------------------------------------------------------------

/// All persistent daemon state in one struct.
///
/// Serialized to `~/.midtown/projects/<repo>/daemon-state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonPersistentState {
    #[serde(default)]
    pub github: GitHubState,

    #[serde(default)]
    pub reminders: ReminderState,

    #[serde(default)]
    pub ci_stats: CiCheckStats,

    #[serde(default)]
    pub worktree_registry: WorktreeRegistry,

    #[serde(default)]
    pub channel_lead_sessions: HashMap<String, String>,

    #[serde(default)]
    pub sessions: HashMap<String, SessionRecord>,

    #[serde(default)]
    pub profile_pool_state: HashMap<String, ProfileState>,

    #[serde(default)]
    pub channel_workflows: HashMap<String, String>,

    #[serde(default)]
    pub lead_driven_channels: HashSet<String>,

    #[serde(default)]
    pub channel_settings: HashMap<String, ChannelSettings>,

    #[serde(default)]
    pub workflow_state: HashMap<String, serde_json::Value>,

    #[serde(default)]
    pub read_state: HashMap<String, ReadState>,

    #[serde(default)]
    pub permanent_pr_nudges: Vec<(u64, PrIssueType)>,

    #[serde(default, rename = "task_session_spans", skip_serializing)]
    pub _task_session_spans: serde_json::Value,

    #[serde(default)]
    pub task_index: HashMap<String, crate::task_store::TaskIndexEntry>,
}

impl DaemonPersistentState {
    /// Load from the unified state file for a repository.
    pub fn load_for_repo(repo: &str) -> io::Result<Self> {
        let path = crate::paths::daemon_state_file_for_repo(repo);
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let mut state: Self = serde_json::from_str(&contents).map_err(|e| {
                    warn!("Failed to parse daemon-state.json: {}", e);
                    io::Error::new(ErrorKind::InvalidData, e)
                })?;
                state.worktree_registry.rebuild_indexes();

                if state.workflow_state.is_empty() {
                    let (migrated, files_to_delete) = Self::migrate_workflow_state_files(repo);
                    if !migrated.is_empty() {
                        debug!(
                            "Migrated {} legacy workflow-state.json file(s) into existing daemon state",
                            migrated.len()
                        );
                        state.workflow_state = migrated;
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
        Ok(())
    }

    /// Returns the active reviewer session for a PR, if any.
    pub fn active_reviewer_for_pr(
        &self,
        pr_number: u64,
        pr_to_task: &HashMap<u64, String>,
    ) -> Option<&SessionRecord> {
        let task_id = pr_to_task.get(&pr_number)?;
        self.sessions
            .values()
            .filter(|s| s.is_active_reviewer())
            .find(|s| s.task_id.as_deref() == Some(task_id.as_str()))
    }

    /// Returns the set of active channel lead names.
    pub fn channel_lead_names(&self) -> HashSet<String> {
        self.channel_lead_sessions.keys().cloned().collect()
    }

    /// Find a session record by coworker name (exact match).
    pub fn session_by_name(&self, name: &str) -> Option<&SessionRecord> {
        self.sessions.values().find(|s| s.name == name)
    }

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

        let (workflow_state, workflow_files_to_delete) = Self::migrate_workflow_state_files(repo);

        let state = Self {
            github,
            reminders,
            workflow_state,
            ..Default::default()
        };

        if let Err(e) = state.save_for_repo(repo) {
            warn!("Failed to save migrated daemon-state.json: {}", e);
            return Err(e);
        }

        let github_path = crate::paths::github_state_file_for_repo(repo);
        if github_path.exists() {
            let _ = fs::remove_file(&github_path);
        }
        if reminder_path.exists() {
            let _ = fs::remove_file(&reminder_path);
        }
        for path in &workflow_files_to_delete {
            let _ = fs::remove_file(path);
        }

        Ok(state)
    }

    fn migrate_workflow_state_files(
        repo: &str,
    ) -> (HashMap<String, serde_json::Value>, Vec<PathBuf>) {
        let channels_dir = crate::paths::projects_dir_for_repo(repo).join("channels");
        Self::migrate_workflow_state_from_dir(&channels_dir)
    }

    fn migrate_workflow_state_from_dir(
        channels_dir: &Path,
    ) -> (HashMap<String, serde_json::Value>, Vec<PathBuf>) {
        let mut workflow_state = HashMap::new();
        let mut files_to_delete = Vec::new();

        let entries = match fs::read_dir(channels_dir) {
            Ok(e) => e,
            Err(_) => return (workflow_state, files_to_delete),
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

        (workflow_state, files_to_delete)
    }
}
