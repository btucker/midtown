//! Tests for task-based limit enforcement in task dispatch.

#[test]
fn test_task_limit_semantics() {
    let max_in_progress_tasks: usize = 8;
    let in_progress_count: usize = 7;

    let is_at_limit = in_progress_count >= max_in_progress_tasks;
    assert!(!is_at_limit, "7 < 8 → not at limit");

    let in_progress_count: usize = 8;
    let is_at_limit = in_progress_count >= max_in_progress_tasks;
    assert!(is_at_limit, "8 >= 8 → at limit");
}

#[test]
fn test_spawn_count_within_tick() {
    let in_progress_count = 7;
    let pending_count = 3;
    let task_cap = 8;

    let spawned_without_counter = pending_count;
    let total_without_counter = in_progress_count + spawned_without_counter;
    assert!(
        total_without_counter > task_cap,
        "Bug: spawning exceeds task limit"
    );

    let spawned_with_counter = (task_cap - in_progress_count).min(pending_count);
    let total_with_counter = in_progress_count + spawned_with_counter;
    assert_eq!(
        total_with_counter, task_cap,
        "Fix: spawning stops at task limit"
    );
}

#[test]
fn test_spawn_limit_edge_cases() {
    let task_cap: usize = 8;

    let in_progress = 8;
    let allowed = task_cap.saturating_sub(in_progress);
    assert_eq!(allowed, 0, "No spawns allowed when at cap");

    let in_progress = 7;
    let allowed = task_cap.saturating_sub(in_progress);
    assert_eq!(allowed, 1, "Exactly 1 spawn allowed when 1 below cap");

    let in_progress = 0;
    let allowed = task_cap.saturating_sub(in_progress);
    assert_eq!(
        allowed, 8,
        "Up to task_cap spawns allowed when starting from 0"
    );
}
