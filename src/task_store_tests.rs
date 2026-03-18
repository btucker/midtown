use super::*;
use tempfile::TempDir;

fn make_task(id: &str, agent_name: &str, status: TaskStatus) -> Task {
    Task {
        id: id.to_string(),
        subject: format!("Task {}", id),
        status,
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        agent_name: agent_name.to_string(),
        agent_type: "midtown-code-author".to_string(),
        session_id: None,
        parent: None,
        message_id: None,
        thread_id: None,
        model: None,
        plan: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[test]
fn test_save_and_load_round_trip() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    let task = Task {
        id: "1".to_string(),
        subject: "Add auth endpoint".to_string(),
        status: TaskStatus::Pending,
        description: Some("Implement OAuth2 flow".to_string()),
        blocked_by: vec![],
        channel: Some("auth".to_string()),
        pr: None,
        agent_name: "ghost-town".to_string(),
        agent_type: "midtown-code-author".to_string(),
        session_id: None,
        parent: None,
        message_id: None,
        thread_id: None,
        model: None,
        plan: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    store.save(&task).unwrap();
    let loaded = store.load("1").unwrap();
    assert_eq!(loaded.id, "1");
    assert_eq!(loaded.subject, "Add auth endpoint");
    assert_eq!(loaded.agent_name, "ghost-town");
    assert_eq!(loaded.agent_type, "midtown-code-author");
    assert_eq!(loaded.channel, Some("auth".to_string()));
    assert_eq!(
        loaded.description,
        Some("Implement OAuth2 flow".to_string())
    );
}

#[test]
fn test_load_nonexistent_returns_error() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    let result = store.load("nonexistent");
    assert!(result.is_err());
}

#[test]
fn test_load_all() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    store
        .save(&make_task("1", "alpha", TaskStatus::Pending))
        .unwrap();
    store
        .save(&make_task("2", "bravo", TaskStatus::InProgress))
        .unwrap();
    store
        .save(&make_task("3", "charlie", TaskStatus::Completed))
        .unwrap();

    let all = store.load_all();
    assert_eq!(all.len(), 3);
}

#[test]
fn test_load_all_empty_dir() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().join("nonexistent"));

    let all = store.load_all();
    assert!(all.is_empty());
}

#[test]
fn test_build_index() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    let mut task = make_task("1", "alpha", TaskStatus::Pending);
    task.parent = Some("0".to_string());
    store.save(&task).unwrap();
    store
        .save(&make_task("2", "bravo", TaskStatus::InProgress))
        .unwrap();

    let index = store.build_index();
    assert_eq!(index.len(), 2);
    assert_eq!(index["1"].status, TaskStatus::Pending);
    assert_eq!(index["1"].agent_name, "alpha");
    assert_eq!(index["1"].parent, Some("0".to_string()));
    assert_eq!(index["2"].status, TaskStatus::InProgress);
    assert_eq!(index["2"].agent_name, "bravo");
    assert_eq!(index["2"].parent, None);
}

#[test]
fn test_is_name_in_use() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    store
        .save(&make_task("1", "alpha", TaskStatus::Pending))
        .unwrap();
    store
        .save(&make_task("2", "bravo", TaskStatus::Completed))
        .unwrap();

    assert!(store.is_name_in_use("alpha"));
    // Completed tasks don't count as "in use"
    assert!(!store.is_name_in_use("bravo"));
    assert!(!store.is_name_in_use("charlie"));
}

#[test]
fn test_save_sets_updated_at() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    let before = Utc::now();
    let mut task = make_task("1", "alpha", TaskStatus::Pending);
    task.updated_at = before - chrono::Duration::hours(1);
    store.save(&task).unwrap();

    let loaded = store.load("1").unwrap();
    assert!(loaded.updated_at >= before);
}

#[test]
fn test_save_overwrites_existing() {
    let dir = TempDir::new().unwrap();
    let store = TaskStore::new(dir.path().to_path_buf());

    store
        .save(&make_task("1", "alpha", TaskStatus::Pending))
        .unwrap();

    let mut updated = make_task("1", "alpha", TaskStatus::InProgress);
    updated.description = Some("Updated description".to_string());
    store.save(&updated).unwrap();

    let loaded = store.load("1").unwrap();
    assert_eq!(loaded.status, TaskStatus::InProgress);
    assert_eq!(loaded.description, Some("Updated description".to_string()));
}
