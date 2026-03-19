//! Midtown task storage — one JSON file per task.
//!
//! Tasks are stored at `~/.midtown/<project>/tasks/<task-id>.json` using
//! Midtown's own schema. This replaces the previous approach of storing
//! tasks in Claude Code's `~/.claude/tasks/` format with metadata scattered
//! across separate HashMaps on DaemonPersistentState.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// A Midtown task.
///
/// Tasks are the unit of work assignment. Each task is bound to exactly one
/// worker session (1:1 mapping). The lead assigns `agent_name` and `agent_type`
/// at creation time; the daemon sets `session_id` at spawn time.
/// Deserialize a Vec<String> that may be null in JSON.
fn deserialize_vec_or_null<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Vec<String>>::deserialize(deserializer).map(|v| v.unwrap_or_default())
}

/// Deserialize a String that may be null in JSON (e.g., old "owner": null).
fn deserialize_string_or_null<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|v| v.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(
        default,
        alias = "blockedBy",
        deserialize_with = "deserialize_vec_or_null"
    )]
    pub blocked_by: Vec<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub pr: Option<u64>,
    /// Creative session name, set by lead at creation. Immutable.
    #[serde(
        default,
        alias = "owner",
        deserialize_with = "deserialize_string_or_null"
    )]
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
    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,
    /// When the task was last modified (set automatically by `TaskStore::save()`).
    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
}

impl Default for Task {
    fn default() -> Self {
        let now = Utc::now();
        Self {
            id: String::new(),
            subject: String::new(),
            status: TaskStatus::Pending,
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            agent_name: String::new(),
            agent_type: String::new(),
            session_id: None,
            parent: None,
            message_id: None,
            thread_id: None,
            model: None,
            plan: None,
            placeholder_comment_id: None,
            restart_count: 0,
            execution_skill: None,
            created_at: now,
            updated_at: now,
        }
    }
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

    /// Delete a task file from disk.
    pub fn delete(&self, id: &str) -> crate::Result<()> {
        let path = self.tasks_dir.join(format!("{}.json", id));
        std::fs::remove_file(&path)?;
        Ok(())
    }

    /// Get the tasks directory path.
    pub fn tasks_dir(&self) -> &std::path::Path {
        &self.tasks_dir
    }

    /// Compute the next sequential task ID from existing tasks.
    pub fn next_task_id(&self) -> u64 {
        let all = self.load_all();
        let max_existing = all
            .iter()
            .filter_map(|t| t.id.parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        max_existing + 1
    }

    /// Mark a task as completed.
    pub fn complete_task(&self, id: &str) -> Result<(), String> {
        let mut task = self
            .load(id)
            .map_err(|e| format!("Failed to load task {}: {}", id, e))?;
        task.status = TaskStatus::Completed;
        self.save(&task)
            .map_err(|e| format!("Failed to save task {}: {}", id, e))
    }

    /// Set a task's status to in_progress.
    pub fn set_task_in_progress(&self, id: &str) -> Result<(), String> {
        let mut task = self
            .load(id)
            .map_err(|e| format!("Failed to load task {}: {}", id, e))?;
        task.status = TaskStatus::InProgress;
        self.save(&task)
            .map_err(|e| format!("Failed to save task {}: {}", id, e))
    }

    /// Reset a task to pending status and clear its agent_name.
    pub fn reset_task_to_pending(&self, id: &str) -> Result<(), String> {
        let mut task = self
            .load(id)
            .map_err(|e| format!("Failed to load task {}: {}", id, e))?;
        task.status = TaskStatus::Pending;
        task.agent_name = String::new();
        self.save(&task)
            .map_err(|e| format!("Failed to save task {}: {}", id, e))
    }

    /// Set the agent_name (owner) of a task.
    pub fn set_agent_name(&self, id: &str, name: &str) -> Result<(), String> {
        let mut task = self
            .load(id)
            .map_err(|e| format!("Failed to load task {}: {}", id, e))?;
        task.agent_name = name.to_string();
        self.save(&task)
            .map_err(|e| format!("Failed to save task {}: {}", id, e))
    }

    /// Clear a task's agent_name without changing its status.
    pub fn unassign_task(&self, id: &str) -> Result<(), String> {
        let mut task = self
            .load(id)
            .map_err(|e| format!("Failed to load task {}: {}", id, e))?;
        task.agent_name = String::new();
        self.save(&task)
            .map_err(|e| format!("Failed to save task {}: {}", id, e))
    }

    /// Clear a completed task ID from all dependent tasks' `blocked_by` arrays.
    pub fn clear_blocked_by(&self, completed_task_id: &str) -> Result<(), String> {
        let all = self.load_all();
        for mut task in all {
            let original_len = task.blocked_by.len();
            task.blocked_by.retain(|id| id != completed_task_id);
            if task.blocked_by.len() < original_len {
                self.save(&task)
                    .map_err(|e| format!("Failed to update task {}: {}", task.id, e))?;
            }
        }
        Ok(())
    }

    /// Update specific fields on a task.
    #[allow(clippy::too_many_arguments)]
    pub fn update_task_fields(
        &self,
        id: &str,
        agent_name: Option<&str>,
        status: Option<TaskStatus>,
        description: Option<&str>,
        blocked_by: Option<&[String]>,
        channel: Option<&str>,
        pr: Option<u64>,
    ) -> Result<(), String> {
        let mut task = self
            .load(id)
            .map_err(|e| format!("Failed to load task {}: {}", id, e))?;
        if let Some(name) = agent_name {
            task.agent_name = name.to_string();
        }
        if let Some(s) = status {
            task.status = s;
        }
        if let Some(d) = description {
            task.description = Some(d.to_string());
        }
        if let Some(bb) = blocked_by {
            task.blocked_by = bb.to_vec();
        }
        if let Some(ch) = channel {
            task.channel = Some(ch.to_string());
        }
        if let Some(pr_num) = pr {
            task.pr = Some(pr_num);
        }
        self.save(&task)
            .map_err(|e| format!("Failed to save task {}: {}", id, e))
    }
}

// ── Utility functions (moved from tasks.rs) ─────────────────────────────

/// Extract task ID from PR title using `[Midtown !XX]` or `[Midtown #XX]` format.
pub fn extract_task_id_from_pr_title(title: &str) -> Option<u64> {
    // Try `[Midtown !XX]` first (canonical format used by coworkers)
    if let Some(start) = title.find("[Midtown !") {
        let rest = &title[start + 10..]; // Skip "[Midtown !"
        if let Some(end) = rest.find(']') {
            let num_str = &rest[..end];
            if let Ok(id) = num_str.parse::<u64>() {
                return Some(id);
            }
        }
    }
    // Fall back to `[Midtown #XX]` for backwards compatibility
    if let Some(start) = title.find("[Midtown #") {
        let rest = &title[start + 10..]; // Skip "[Midtown #"
        if let Some(end) = rest.find(']') {
            let num_str = &rest[..end];
            return num_str.parse::<u64>().ok();
        }
    }
    None
}

/// Extract a PR number from a text string.
///
/// Looks for patterns like "PR #123" in the text.
/// Returns the PR number as a string if found.
pub fn extract_pr_number(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let idx = lower.find("pr #")?;
    let after = &text[idx + 4..];
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(digits)
    }
}

/// Extract a PR number from a task by checking both subject and description.
pub fn extract_pr_number_from_task(task: &Task) -> Option<String> {
    extract_pr_number(&task.subject)
        .or_else(|| task.description.as_deref().and_then(extract_pr_number))
}

/// Extract PR numbers from text (e.g., task description).
///
/// Matches patterns like `PR #123`, `#123` preceded by whitespace or punctuation.
/// Filters out markdown headings and numbers >= 10000.
pub fn extract_pr_numbers_from_text(text: &str) -> Vec<u64> {
    let mut pr_numbers = HashSet::new();

    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') && !trimmed.starts_with("#[") {
            let after_hashes = trimmed.trim_start_matches('#');
            if after_hashes.is_empty() || after_hashes.starts_with(' ') {
                continue;
            }
        }

        let chars: Vec<char> = line.chars().collect();
        let len = chars.len();
        let mut i = 0;

        while i < len {
            if chars[i] == '#' {
                if i > 0 && chars[i - 1].is_alphanumeric() {
                    i += 1;
                    continue;
                }

                let mut num_str = String::new();
                let mut j = i + 1;
                while j < len && chars[j].is_ascii_digit() {
                    num_str.push(chars[j]);
                    j += 1;
                }

                if !num_str.is_empty()
                    && let Ok(num) = num_str.parse::<u64>()
                    && num < 10000
                {
                    pr_numbers.insert(num);
                }

                i = j;
            } else {
                i += 1;
            }
        }
    }

    let mut sorted: Vec<u64> = pr_numbers.into_iter().collect();
    sorted.sort_unstable();
    sorted
}

/// Find the owner (agent_name) of a related task via blockedBy relationships.
///
/// If this task is blocked by another task that has an agent_name, return that name.
pub fn find_owner_via_blocked_by(task: &Task, all_tasks: &[Task]) -> Option<String> {
    for blocked_by_id in &task.blocked_by {
        if let Some(parent) = all_tasks.iter().find(|t| &t.id == blocked_by_id)
            && !parent.agent_name.is_empty()
        {
            return Some(parent.agent_name.clone());
        }
    }
    None
}

/// Check whether a task has unresolved `blockedBy` dependencies.
pub fn has_unresolved_blockers(task: &Task, all_tasks: &[Task]) -> bool {
    if task.blocked_by.is_empty() {
        return false;
    }
    task.blocked_by.iter().any(
        |blocker_id| match all_tasks.iter().find(|t| &t.id == blocker_id) {
            Some(blocker) => blocker.status != TaskStatus::Completed,
            None => true,
        },
    )
}

/// Filter pending tasks without owners from a pre-read task list.
///
/// Skips tasks created within `grace_secs` seconds and tasks with unresolved blockers.
pub fn filter_pending_tasks_without_owners(all_tasks: &[Task], grace_secs: u64) -> Vec<Task> {
    let now = Utc::now();
    let grace = chrono::Duration::seconds(grace_secs as i64);
    all_tasks
        .iter()
        .filter(|t| {
            t.status == TaskStatus::Pending
                && t.agent_name.is_empty()
                && (now - t.created_at) >= grace
                && !has_unresolved_blockers(t, all_tasks)
        })
        .cloned()
        .collect()
}

/// Get coworkers (agent_names) with unblocked dependents from a given task list.
pub fn get_coworkers_with_unblocked_dependents_from_tasks(all_tasks: &[Task]) -> HashSet<String> {
    let mut result = HashSet::new();

    let unblocked_pending: Vec<&Task> = all_tasks
        .iter()
        .filter(|t| {
            t.status == TaskStatus::Pending
                && t.agent_name.is_empty()
                && !t.blocked_by.is_empty()
                && !has_unresolved_blockers(t, all_tasks)
        })
        .collect();

    for task in &unblocked_pending {
        for blocker_id in &task.blocked_by {
            if let Some(blocker) = all_tasks.iter().find(|t| t.id == *blocker_id)
                && blocker.status == TaskStatus::Completed
                && !blocker.agent_name.is_empty()
            {
                result.insert(blocker.agent_name.to_lowercase());
            }
        }
    }

    result
}

#[path = "task_store_tests.rs"]
#[cfg(test)]
mod tests;
