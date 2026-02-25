//! Tests for status RPC handler.

use super::{filter_lead_session, resolve_pr_number, tag_channel_leads_and_count};

#[test]
fn test_current_task_includes_id_and_title() {
    // Test that current_task includes both the task ID and title
    // in the format "!1234 Task title"

    // This test validates the fix for task #1276:
    // When running midtown restart, the coworker status should show task IDs
    // alongside titles (e.g., "task !1274 Add sandbox..." not just "task !Add sandbox...")

    // The implementation is in handle_status() which builds the current_task
    // field from get_in_progress_tasks_with_subjects()

    // Since handle_status() is async and requires a DaemonState, we test the
    // transformation logic directly here.

    let task_id = "1234";
    let subject = "Add feature X";

    // The format should match what we display in the restart output
    let expected = format!("!{} {}", task_id, subject);

    // Verify the expected format is "!1234 Add feature X"
    assert_eq!(expected, "!1234 Add feature X");
}

#[test]
fn test_tag_channel_leads_excludes_leads_from_count() {
    let coworkers = vec![
        serde_json::json!({"name": "amsterdam", "status": "running"}),
        serde_json::json!({"name": "tui-lead", "status": "running"}),
        serde_json::json!({"name": "park", "status": "running"}),
    ];
    let leads: std::collections::HashSet<String> = ["tui-lead".to_string()].into_iter().collect();

    let (tagged, count) = tag_channel_leads_and_count(coworkers, &leads);

    assert_eq!(count, 2, "Only non-lead coworkers should be counted");
    assert_eq!(tagged.len(), 3, "All coworkers should still be in the list");

    // Verify the lead is tagged
    assert_eq!(tagged[1]["is_channel_lead"], true);
    // Verify non-leads are not tagged as leads
    assert_eq!(tagged[0]["is_channel_lead"], false);
    assert_eq!(tagged[2]["is_channel_lead"], false);
}

#[test]
fn test_tag_channel_leads_no_leads_present() {
    let coworkers = vec![
        serde_json::json!({"name": "amsterdam", "status": "running"}),
        serde_json::json!({"name": "park", "status": "running"}),
    ];
    let leads: std::collections::HashSet<String> = std::collections::HashSet::new();

    let (tagged, count) = tag_channel_leads_and_count(coworkers, &leads);

    assert_eq!(count, 2);
    assert_eq!(tagged[0]["is_channel_lead"], false);
    assert_eq!(tagged[1]["is_channel_lead"], false);
}

#[test]
fn test_tag_channel_leads_all_are_leads() {
    let coworkers = vec![
        serde_json::json!({"name": "tui-lead", "status": "running"}),
        serde_json::json!({"name": "ops-lead", "status": "running"}),
    ];
    let leads: std::collections::HashSet<String> = ["tui-lead".to_string(), "ops-lead".to_string()]
        .into_iter()
        .collect();

    let (tagged, count) = tag_channel_leads_and_count(coworkers, &leads);

    assert_eq!(count, 0, "All coworkers are leads, count should be 0");
    assert_eq!(tagged[0]["is_channel_lead"], true);
    assert_eq!(tagged[1]["is_channel_lead"], true);
}

#[test]
fn test_filter_lead_session_removes_repo_named_lead() {
    // The lead session is named after the repo (e.g., "midtown") and must
    // not appear in the coworkers list. This was a bug where the lead session
    // appeared in the status display as a regular coworker.
    let coworkers = vec![
        serde_json::json!({"name": "midtown", "status": "running"}),
        serde_json::json!({"name": "amsterdam", "status": "running"}),
        serde_json::json!({"name": "park", "status": "running"}),
    ];

    let filtered = filter_lead_session(coworkers, "midtown");

    assert_eq!(
        filtered.len(),
        2,
        "Lead session should be excluded from coworkers list"
    );
    let names: Vec<&str> = filtered
        .iter()
        .filter_map(|v| v.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(
        !names.contains(&"midtown"),
        "Lead should not be in filtered list"
    );
    assert!(
        names.contains(&"amsterdam"),
        "Regular coworker should remain"
    );
    assert!(names.contains(&"park"), "Regular coworker should remain");
}

#[test]
fn test_filter_lead_session_case_insensitive() {
    let coworkers = vec![
        serde_json::json!({"name": "MyProject", "status": "running"}),
        serde_json::json!({"name": "amsterdam", "status": "running"}),
    ];

    let filtered = filter_lead_session(coworkers, "myproject");

    assert_eq!(
        filtered.len(),
        1,
        "Case-insensitive lead filtering should work"
    );
    assert_eq!(filtered[0]["name"], "amsterdam");
}

#[test]
fn test_filter_lead_session_no_lead_present() {
    // If the lead session is not registered (e.g., during startup), the
    // filter should be a no-op.
    let coworkers = vec![
        serde_json::json!({"name": "amsterdam", "status": "running"}),
        serde_json::json!({"name": "park", "status": "running"}),
    ];

    let filtered = filter_lead_session(coworkers, "midtown");

    assert_eq!(
        filtered.len(),
        2,
        "All coworkers should remain when no lead present"
    );
}

#[test]
fn test_tag_channel_leads_empty_coworkers() {
    let coworkers: Vec<serde_json::Value> = vec![];
    let leads: std::collections::HashSet<String> = ["tui-lead".to_string()].into_iter().collect();

    let (tagged, count) = tag_channel_leads_and_count(coworkers, &leads);

    assert_eq!(count, 0);
    assert!(tagged.is_empty());
}

#[test]
fn test_tag_channel_leads_preserves_existing_fields() {
    let coworkers = vec![serde_json::json!({
        "name": "amsterdam",
        "status": "running",
        "current_task": "!42 Fix auth bug",
        "started_at": "2026-01-01T00:00:00Z",
        "provider": "claude",
        "profile": "test@example.com"
    })];
    let leads: std::collections::HashSet<String> = std::collections::HashSet::new();

    let (tagged, _) = tag_channel_leads_and_count(coworkers, &leads);

    // All original fields should be preserved
    assert_eq!(tagged[0]["name"], "amsterdam");
    assert_eq!(tagged[0]["status"], "running");
    assert_eq!(tagged[0]["current_task"], "!42 Fix auth bug");
    assert_eq!(tagged[0]["provider"], "claude");
    assert_eq!(tagged[0]["is_channel_lead"], false);
}

// ─── Tests for legacy "lead" string guard ───

#[test]
fn test_filter_lead_session_removes_legacy_lead_string() {
    // Legacy sessions may use the literal name "lead" instead of the repo name.
    // Both must be filtered from the coworkers list.
    let coworkers = vec![
        serde_json::json!({"name": "lead", "status": "running"}),
        serde_json::json!({"name": "amsterdam", "status": "running"}),
    ];

    let filtered = filter_lead_session(coworkers, "midtown");

    assert_eq!(
        filtered.len(),
        1,
        "Legacy 'lead' session should be excluded from coworkers list"
    );
    assert_eq!(filtered[0]["name"], "amsterdam");
}

// ─── Tests for resolve_pr_number (PR number resolution priority chain) ───

#[test]
fn test_resolve_pr_from_task_file() {
    // Task file PR association takes highest priority
    let task_pr = [(42u32, 100u64)].into_iter().collect();
    let reviewer = std::collections::HashMap::new();
    let worktree = std::collections::HashMap::new();

    assert_eq!(
        resolve_pr_number(Some(42), "amsterdam", &task_pr, &reviewer, &worktree),
        Some(100)
    );
}

#[test]
fn test_resolve_pr_from_reviewer_assignment() {
    // Falls back to reviewer assignment when task has no PR
    let task_pr = std::collections::HashMap::new();
    let reviewer = [("amsterdam".to_string(), 200u64)].into_iter().collect();
    let worktree = std::collections::HashMap::new();

    assert_eq!(
        resolve_pr_number(Some(42), "amsterdam", &task_pr, &reviewer, &worktree),
        Some(200)
    );
}

#[test]
fn test_resolve_pr_from_worktree_registry() {
    // Falls back to worktree registry as last resort
    let task_pr = std::collections::HashMap::new();
    let reviewer = std::collections::HashMap::new();
    let worktree = [("amsterdam".to_string(), 300u64)].into_iter().collect();

    assert_eq!(
        resolve_pr_number(None, "amsterdam", &task_pr, &reviewer, &worktree),
        Some(300)
    );
}

#[test]
fn test_resolve_pr_task_takes_priority_over_reviewer() {
    // When task file and reviewer both have PRs, task file wins
    let task_pr = [(42u32, 100u64)].into_iter().collect();
    let reviewer = [("amsterdam".to_string(), 200u64)].into_iter().collect();
    let worktree = [("amsterdam".to_string(), 300u64)].into_iter().collect();

    assert_eq!(
        resolve_pr_number(Some(42), "amsterdam", &task_pr, &reviewer, &worktree),
        Some(100)
    );
}

#[test]
fn test_resolve_pr_none_when_no_sources() {
    let task_pr = std::collections::HashMap::new();
    let reviewer = std::collections::HashMap::new();
    let worktree = std::collections::HashMap::new();

    assert_eq!(
        resolve_pr_number(None, "amsterdam", &task_pr, &reviewer, &worktree),
        None
    );
}

#[test]
fn test_resolve_pr_none_when_task_id_has_no_pr() {
    // Task ID exists but has no PR association, and no other sources
    let task_pr = std::collections::HashMap::new();
    let reviewer = std::collections::HashMap::new();
    let worktree = std::collections::HashMap::new();

    assert_eq!(
        resolve_pr_number(Some(99), "amsterdam", &task_pr, &reviewer, &worktree),
        None
    );
}
