//! Midtown task storage — one JSON file per task.
//!
//! Tasks are stored at `~/.midtown/<project>/tasks/<task-id>.json` using
//! Midtown's own schema. This replaces the previous approach of storing
//! tasks in Claude Code's `~/.claude/tasks/` format with metadata scattered
//! across separate HashMaps on DaemonPersistentState.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// A Midtown task.
///
/// Tasks are the unit of work assignment. Each task is bound to exactly one
/// worker session (1:1 mapping). The lead assigns `agent_name` and `agent_type`
/// at creation time; the daemon sets `session_id` at spawn time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub subject: String,
    pub status: TaskStatus,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub pr: Option<u64>,
    /// Creative session name, set by lead at creation. Immutable.
    #[serde(default)]
    pub agent_name: String,
    /// Agent definition for `--agent` flag, set by lead at creation. Immutable.
    #[serde(default)]
    pub agent_type: String,
    /// Bound session ID, set by daemon at spawn time.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Parent task ID (e.g., review task is child of dev task). Immutable.
    #[serde(default)]
    pub parent: Option<String>,
    /// Channel message that spawned this task.
    #[serde(default)]
    pub message_id: Option<String>,
    /// Thread the task is bound to.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Model override for this task's session.
    #[serde(default)]
    pub model: Option<String>,
    /// Path to execution plan.
    #[serde(default)]
    pub plan: Option<String>,
    /// GitHub comment ID for "Review in progress" placeholder (reviewer tasks).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder_comment_id: Option<u64>,
    /// Number of times this task's session has been restarted.
    #[serde(default)]
    pub restart_count: u32,
    /// Execution skill for plan-driven execution (e.g., "subagent-driven-development").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_skill: Option<String>,
    /// When the task was created.
    pub created_at: DateTime<Utc>,
    /// When the task was last modified (set automatically by `TaskStore::save()`).
    pub updated_at: DateTime<Utc>,
}

/// Task status — unchanged from the original.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

/// Lightweight index entry for fast lookups without reading task files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskIndexEntry {
    pub status: TaskStatus,
    pub parent: Option<String>,
    pub agent_name: String,
    pub agent_type: String,
}

/// Persistent task storage — one JSON file per task.
pub struct TaskStore {
    tasks_dir: PathBuf,
}

impl TaskStore {
    pub fn new(tasks_dir: PathBuf) -> Self {
        Self { tasks_dir }
    }

    /// Save a task to disk. Sets `updated_at` automatically.
    pub fn save(&self, task: &Task) -> crate::Result<()> {
        std::fs::create_dir_all(&self.tasks_dir)?;
        let path = self.tasks_dir.join(format!("{}.json", task.id));
        let mut task = task.clone();
        task.updated_at = Utc::now();
        let json = serde_json::to_string_pretty(&task)?;
        // Atomic write via temp file + rename
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Load a single task by ID.
    pub fn load(&self, id: &str) -> crate::Result<Task> {
        let path = self.tasks_dir.join(format!("{}.json", id));
        let json = std::fs::read_to_string(&path)?;
        let task: Task = serde_json::from_str(&json)?;
        Ok(task)
    }

    /// Load all tasks from disk.
    pub fn load_all(&self) -> Vec<Task> {
        let Ok(entries) = std::fs::read_dir(&self.tasks_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|e| {
                std::fs::read_to_string(e.path())
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
            })
            .collect()
    }

    /// Build index from all tasks on disk.
    pub fn build_index(&self) -> HashMap<String, TaskIndexEntry> {
        self.load_all()
            .into_iter()
            .map(|t| {
                (
                    t.id.clone(),
                    TaskIndexEntry {
                        status: t.status,
                        parent: t.parent.clone(),
                        agent_name: t.agent_name.clone(),
                        agent_type: t.agent_type.clone(),
                    },
                )
            })
            .collect()
    }

    /// Check if an agent_name is already in use by any active (non-completed) task.
    pub fn is_name_in_use(&self, name: &str) -> bool {
        self.load_all()
            .iter()
            .any(|t| t.agent_name == name && t.status != TaskStatus::Completed)
    }
}

#[path = "task_store_tests.rs"]
#[cfg(test)]
mod tests;
