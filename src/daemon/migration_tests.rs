use tempfile::TempDir;

use super::*;
use crate::daemon::state::DaemonPersistentState;
use crate::task_store::TaskStatus;

fn make_task(
    id: &str,
    subject: &str,
    status: TaskStatus,
    owner: Option<&str>,
) -> serde_json::Value {
    let status_str = match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Completed => "completed",
    };
    serde_json::json!({
        "id": id,
        "subject": subject,
        "status": status_str,
        "owner": owner,
        "description": format!("Description for {}", subject),
        "blockedBy": [],
    })
}

fn make_state_with_maps(task_id: &str) -> DaemonPersistentState {
    let mut state = DaemonPersistentState::default();
    state
        .task_channel
        .insert(task_id.to_string(), "auth".to_string());
    state
        .task_model
        .insert(task_id.to_string(), "claude/opus".to_string());
    state
        .task_plan
        .insert(task_id.to_string(), "/path/to/plan.md".to_string());
    state
        .task_thread_id
        .insert(task_id.to_string(), "thread-abc".to_string());
    state
        .task_message_id
        .insert(task_id.to_string(), "msg-xyz".to_string());
    state
        .task_parent
        .insert(task_id.to_string(), "0".to_string());
    state
        .task_agent_type
        .insert(task_id.to_string(), "midtown-code-reviewer".to_string());
    state.task_pr_number.insert(task_id.to_string(), 42);
    state
}

#[test]
fn test_migrate_all_fields_populated() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");

    let task = make_task(
        "1",
        "Add auth endpoint",
        TaskStatus::InProgress,
        Some("park"),
    );
    let state = make_state_with_maps("1");

    let migrated = migrate_tasks_if_needed(&[task], &state, &new_tasks_dir);

    assert_eq!(migrated, vec!["1"]);

    // Read back the migrated file
    let content = std::fs::read_to_string(new_tasks_dir.join("1.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(value["id"], "1");
    assert_eq!(value["subject"], "Add auth endpoint");
    assert_eq!(value["status"], "in_progress");
    assert_eq!(value["owner"], "park");
    assert_eq!(value["agent_name"], "park");
    assert_eq!(value["agent_type"], "midtown-code-reviewer");
    assert_eq!(value["channel"], "auth");
    assert_eq!(value["model"], "claude/opus");
    assert_eq!(value["plan"], "/path/to/plan.md");
    assert_eq!(value["thread_id"], "thread-abc");
    assert_eq!(value["message_id"], "msg-xyz");
    assert_eq!(value["parent"], "0");
    assert_eq!(value["pr"], 42);
    assert!(value["session_id"].is_null());
    assert!(value["created_at"].is_string());
    assert!(value["updated_at"].is_string());
}

#[test]
fn test_migrate_missing_owner_slugifies_subject() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");

    let task = make_task("2", "Fix the broken tests", TaskStatus::Pending, None);
    let state = DaemonPersistentState::default();

    let migrated = migrate_tasks_if_needed(&[task], &state, &new_tasks_dir);

    assert_eq!(migrated, vec!["2"]);

    let content = std::fs::read_to_string(new_tasks_dir.join("2.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(value["agent_name"], "fix-the-broken");
    assert_eq!(value["agent_type"], "midtown-code-author");
    assert!(value["owner"].is_null());
}

#[test]
fn test_migrate_empty_owner_slugifies_subject() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");

    let mut task = make_task("3", "Update README docs", TaskStatus::Pending, Some(""));
    // Empty string owner should be treated like None
    task["owner"] = serde_json::json!("");
    let state = DaemonPersistentState::default();

    let migrated = migrate_tasks_if_needed(&[task], &state, &new_tasks_dir);

    assert_eq!(migrated, vec!["3"]);

    let content = std::fs::read_to_string(new_tasks_dir.join("3.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(value["agent_name"], "update-readme-docs");
}

#[test]
fn test_migrate_idempotent() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");

    let task = make_task("4", "Idempotent task", TaskStatus::Pending, Some("vernon"));
    let state = DaemonPersistentState::default();

    // First migration
    let migrated1 = migrate_tasks_if_needed(std::slice::from_ref(&task), &state, &new_tasks_dir);
    assert_eq!(migrated1, vec!["4"]);

    // Read the content after first migration
    let content1 = std::fs::read_to_string(new_tasks_dir.join("4.json")).unwrap();

    // Second migration — should skip
    let migrated2 = migrate_tasks_if_needed(std::slice::from_ref(&task), &state, &new_tasks_dir);
    assert!(
        migrated2.is_empty(),
        "Second migration should not migrate any tasks"
    );

    // Content should be unchanged
    let content2 = std::fs::read_to_string(new_tasks_dir.join("4.json")).unwrap();
    assert_eq!(content1, content2);
}

#[test]
fn test_status_mapping() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");
    let state = DaemonPersistentState::default();

    let tasks = vec![
        make_task("10", "Pending task", TaskStatus::Pending, Some("a")),
        make_task("11", "In progress task", TaskStatus::InProgress, Some("b")),
        make_task("12", "Completed task", TaskStatus::Completed, Some("c")),
    ];

    let migrated = migrate_tasks_if_needed(&tasks, &state, &new_tasks_dir);
    assert_eq!(migrated.len(), 3);

    let v10: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(new_tasks_dir.join("10.json")).unwrap())
            .unwrap();
    let v11: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(new_tasks_dir.join("11.json")).unwrap())
            .unwrap();
    let v12: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(new_tasks_dir.join("12.json")).unwrap())
            .unwrap();

    assert_eq!(v10["status"], "pending");
    assert_eq!(v11["status"], "in_progress");
    assert_eq!(v12["status"], "completed");
}

#[test]
fn test_migrate_empty_tasks_is_noop() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");
    let state = DaemonPersistentState::default();

    let migrated = migrate_tasks_if_needed(&[], &state, &new_tasks_dir);
    assert!(migrated.is_empty());
    // Directory should not even be created
    assert!(!new_tasks_dir.exists());
}

#[test]
fn test_migrate_uses_task_channel_over_task_field() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");

    let mut task = make_task("5", "Channel test", TaskStatus::Pending, Some("park"));
    task["channel"] = serde_json::json!("old-channel");

    let mut state = DaemonPersistentState::default();
    state
        .task_channel
        .insert("5".to_string(), "new-channel".to_string());

    let migrated = migrate_tasks_if_needed(&[task], &state, &new_tasks_dir);
    assert_eq!(migrated, vec!["5"]);

    let content = std::fs::read_to_string(new_tasks_dir.join("5.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();

    // State map takes precedence
    assert_eq!(value["channel"], "new-channel");
}

#[test]
fn test_migrate_falls_back_to_task_channel_field() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");

    let mut task = make_task("6", "Fallback test", TaskStatus::Pending, Some("park"));
    task["channel"] = serde_json::json!("task-channel");

    let state = DaemonPersistentState::default(); // no task_channel entry

    let migrated = migrate_tasks_if_needed(&[task], &state, &new_tasks_dir);
    assert_eq!(migrated, vec!["6"]);

    let content = std::fs::read_to_string(new_tasks_dir.join("6.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(value["channel"], "task-channel");
}

#[test]
fn test_migrate_pr_from_state_overrides_task() {
    let temp_dir = TempDir::new().unwrap();
    let new_tasks_dir = temp_dir.path().join("tasks");

    let mut task = make_task("7", "PR test", TaskStatus::Pending, Some("park"));
    task["pr"] = serde_json::json!(100);

    let mut state = DaemonPersistentState::default();
    state.task_pr_number.insert("7".to_string(), 200);

    let migrated = migrate_tasks_if_needed(&[task], &state, &new_tasks_dir);
    assert_eq!(migrated, vec!["7"]);

    let content = std::fs::read_to_string(new_tasks_dir.join("7.json")).unwrap();
    let value: serde_json::Value = serde_json::from_str(&content).unwrap();

    assert_eq!(value["pr"], 200);
}

#[test]
fn test_slugify_subject() {
    assert_eq!(slugify_subject("Add auth endpoint"), "add-auth-endpoint");
    assert_eq!(
        slugify_subject("Fix the broken tests now"),
        "fix-the-broken"
    );
    assert_eq!(slugify_subject("Single"), "single");
    assert_eq!(slugify_subject(""), "unnamed-task");
    assert_eq!(slugify_subject("   "), "unnamed-task");
    assert_eq!(
        slugify_subject("Add OAuth2.0 support!"),
        "add-oauth20-support"
    );
}
