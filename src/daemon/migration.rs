//! One-time migration from old `~/.claude/tasks/midtown-<repo>/` format
//! to the new `~/.midtown/<project>/tasks/` format.
//!
//! Reads old tasks and enriches them with metadata from `DaemonPersistentState`
//! HashMap fields (task_channel, task_model, task_plan, etc.).
//! Idempotent — skips tasks that already exist in the new location.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::daemon::state::DaemonPersistentState;
use crate::task_store::TaskStatus;

/// A migrated task in the new format with enriched metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigratedTask {
    pub id: String,
    pub subject: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, rename = "blockedBy")]
    pub blocked_by: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Slugify a subject into a short agent name.
///
/// Takes the first 3 words, lowercases them, joins with hyphens,
/// and strips non-alphanumeric characters (except hyphens).
///
/// Example: "Add auth endpoint" → "add-auth-endpoint"
pub fn slugify_subject(subject: &str) -> String {
    let slug: String = subject
        .split_whitespace()
        .take(3)
        .map(|w| {
            w.to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '-')
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() {
        "unnamed-task".to_string()
    } else {
        slug
    }
}

fn status_to_string(status: TaskStatus) -> String {
    match status {
        TaskStatus::Pending => "pending".to_string(),
        TaskStatus::InProgress => "in_progress".to_string(),
        TaskStatus::Completed => "completed".to_string(),
    }
}

/// Migrate tasks from `~/.claude/tasks/midtown-<repo>/` to `~/.midtown/<project>/tasks/`.
///
/// Reads old tasks and enriches them with metadata from `DaemonPersistentState`
/// HashMap fields to populate new fields. Idempotent — skips tasks that already
/// exist in the new location.
///
/// Returns a list of migrated task IDs.
/// Read old-format tasks from `~/.claude/tasks/midtown-<repo>/` for migration.
///
/// This is a minimal inline reader for the legacy task format. Used only by
/// the migration path; all other code uses `TaskStore`.
fn read_old_format_tasks(dir_key: &str) -> Vec<serde_json::Value> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let task_list_id = crate::paths::task_list_id_for_repo(dir_key);
    let tasks_dir = home.join(".claude").join("tasks").join(&task_list_id);
    let Ok(entries) = std::fs::read_dir(&tasks_dir) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            std::fs::read_to_string(e.path())
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        })
        .collect()
}

pub fn migrate_tasks_if_needed(
    old_tasks: &[serde_json::Value],
    _old_state: &DaemonPersistentState,
    new_tasks_dir: &Path,
) -> Vec<String> {
    if old_tasks.is_empty() {
        return Vec::new();
    }

    // Ensure target directory exists
    if let Err(e) = std::fs::create_dir_all(new_tasks_dir) {
        warn!(
            "Failed to create new tasks directory {}: {}",
            new_tasks_dir.display(),
            e
        );
        return Vec::new();
    }

    let now = Utc::now();
    let mut migrated_ids = Vec::new();

    for task_val in old_tasks {
        let id = task_val
            .get("id")
            .and_then(|v| {
                v.as_str()
                    .map(String::from)
                    .or_else(|| v.as_u64().map(|n| n.to_string()))
            })
            .unwrap_or_default();
        let subject = task_val
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let status_str = task_val
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("pending");
        let status = match status_str {
            "in_progress" => TaskStatus::InProgress,
            "completed" => TaskStatus::Completed,
            _ => TaskStatus::Pending,
        };
        let owner = task_val
            .get("owner")
            .and_then(|v| v.as_str())
            .map(String::from);
        let description = task_val
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from);
        let blocked_by: Vec<String> = task_val
            .get("blockedBy")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.as_str()
                            .map(String::from)
                            .or_else(|| v.as_u64().map(|n| n.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let channel = task_val
            .get("channel")
            .and_then(|v| v.as_str())
            .map(String::from);
        let pr_val = task_val.get("pr").and_then(|v| v.as_u64());

        let new_task_path = new_tasks_dir.join(format!("{}.json", id));

        // Skip if already migrated
        if new_task_path.exists() {
            debug!("Task {} already exists in new location, skipping", id);
            continue;
        }

        let agent_name = if owner.as_ref().is_some_and(|o| !o.is_empty()) {
            owner.clone()
        } else {
            Some(slugify_subject(&subject))
        };

        let agent_type = Some("midtown-code-author".to_string());

        let migrated_channel = channel;

        let model = None;
        let plan = None;
        let thread_id = None;
        let message_id = None;
        let parent = None;
        let pr = pr_val;

        let migrated = MigratedTask {
            id: id.clone(),
            subject,
            status: status_to_string(status),
            owner,
            description,
            blocked_by,
            agent_name,
            agent_type,
            channel: migrated_channel,
            model,
            plan,
            thread_id,
            message_id,
            parent,
            pr,
            session_id: None,
            created_at: now,
            updated_at: now,
        };

        match serde_json::to_string_pretty(&migrated) {
            Ok(content) => match std::fs::write(&new_task_path, content) {
                Ok(()) => {
                    debug!("Migrated task {} to {}", id, new_task_path.display());
                    migrated_ids.push(id);
                }
                Err(e) => {
                    warn!(
                        "Failed to write migrated task {} to {}: {}",
                        migrated.id,
                        new_task_path.display(),
                        e
                    );
                }
            },
            Err(e) => {
                warn!("Failed to serialize migrated task {}: {}", id, e);
            }
        }
    }

    if !migrated_ids.is_empty() {
        // Copy over the highwatermark if present
        if let Some(home) = dirs::home_dir() {
            let old_hwm = home
                .join(".claude")
                .join("tasks")
                .join(format!(
                    "midtown-{}",
                    new_tasks_dir
                        .parent()
                        .and_then(|p| p.file_name())
                        .and_then(|n| n.to_str())
                        .unwrap_or("default")
                ))
                .join(".highwatermark");
            let new_hwm = new_tasks_dir.join(".highwatermark");
            if old_hwm.exists() && !new_hwm.exists() {
                let _ = std::fs::copy(&old_hwm, &new_hwm);
            }
        }

        info!(
            "Migrated {} task(s) to new location: {:?}",
            migrated_ids.len(),
            migrated_ids
        );
    }

    migrated_ids
}

/// Check if migration is needed and run it.
///
/// Called during daemon startup. Only migrates if there are old tasks
/// and the new tasks directory is empty.
pub fn maybe_migrate_tasks(dir_key: &str, persistent_state: &DaemonPersistentState) {
    let old_tasks = read_old_format_tasks(dir_key);
    if old_tasks.is_empty() {
        return;
    }

    let new_tasks_dir = crate::paths::projects_dir_for_repo(dir_key).join("tasks");

    // Only migrate if the new directory doesn't exist or is empty
    let new_dir_empty = !new_tasks_dir.exists()
        || std::fs::read_dir(&new_tasks_dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true);

    if !new_dir_empty {
        debug!("New tasks directory already has content, skipping migration");
        return;
    }

    let migrated = migrate_tasks_if_needed(&old_tasks, persistent_state, &new_tasks_dir);
    if !migrated.is_empty() {
        info!(
            "Task migration complete: {} task(s) migrated for project '{}'",
            migrated.len(),
            dir_key
        );
    }
}

#[path = "migration_tests.rs"]
#[cfg(test)]
mod tests;
