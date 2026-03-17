use super::prioritize_pending_tasks;
use crate::tasks::{Task, TaskStatus};
use std::collections::{HashMap, HashSet};

fn make_task(id: &str, created_secs_ago: u64) -> Task {
    Task {
        id: id.to_string(),
        subject: format!("Task {}", id),
        status: TaskStatus::Pending,
        owner: None,
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: Some(
            std::time::SystemTime::now() - std::time::Duration::from_secs(created_secs_ago),
        ),
    }
}

#[test]
fn fifo_ordering_when_no_parents_or_blockers() {
    let tasks = vec![make_task("3", 10), make_task("1", 30), make_task("2", 20)];
    let result =
        prioritize_pending_tasks(&tasks, &HashSet::new(), &HashMap::new(), &HashMap::new());
    assert_eq!(result, vec!["1", "2", "3"]);
}

#[test]
fn children_of_in_progress_parents_come_first() {
    let tasks = vec![make_task("A", 30), make_task("B", 20), make_task("C", 10)];
    let in_progress: HashSet<String> = ["parent-1".to_string()].into();
    let parent_map: HashMap<String, String> = [("C".to_string(), "parent-1".to_string())].into();
    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &HashMap::new());
    assert_eq!(result, vec!["C", "A", "B"]);
}

#[test]
fn child_of_non_in_progress_parent_is_fifo() {
    let tasks = vec![make_task("A", 30), make_task("B", 20)];
    let parent_map: HashMap<String, String> = [("B".to_string(), "parent-1".to_string())].into();
    let result = prioritize_pending_tasks(&tasks, &HashSet::new(), &parent_map, &HashMap::new());
    assert_eq!(result, vec!["A", "B"]);
}

#[test]
fn blockers_come_before_fifo() {
    let tasks = vec![make_task("A", 30), make_task("B", 20), make_task("C", 10)];
    let blocks_map: HashMap<String, Vec<String>> =
        [("B".to_string(), vec!["X".to_string()])].into();
    let result = prioritize_pending_tasks(&tasks, &HashSet::new(), &HashMap::new(), &blocks_map);
    assert_eq!(result, vec!["B", "A", "C"]);
}

#[test]
fn children_of_in_progress_beat_blockers() {
    let tasks = vec![make_task("A", 30), make_task("B", 20), make_task("C", 10)];
    let in_progress: HashSet<String> = ["parent-1".to_string()].into();
    let parent_map: HashMap<String, String> = [("C".to_string(), "parent-1".to_string())].into();
    let blocks_map: HashMap<String, Vec<String>> =
        [("A".to_string(), vec!["X".to_string()])].into();
    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &blocks_map);
    assert_eq!(result, vec!["C", "A", "B"]);
}

#[test]
fn fifo_within_same_tier() {
    let tasks = vec![make_task("C1", 20), make_task("C2", 10)];
    let in_progress: HashSet<String> = ["p".to_string()].into();
    let parent_map: HashMap<String, String> = [
        ("C1".to_string(), "p".to_string()),
        ("C2".to_string(), "p".to_string()),
    ]
    .into();
    let result = prioritize_pending_tasks(&tasks, &in_progress, &parent_map, &HashMap::new());
    assert_eq!(result, vec!["C1", "C2"]);
}

#[test]
fn empty_input_returns_empty() {
    let result = prioritize_pending_tasks(&[], &HashSet::new(), &HashMap::new(), &HashMap::new());
    assert!(result.is_empty());
}
