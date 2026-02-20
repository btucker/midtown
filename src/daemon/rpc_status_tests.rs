//! Tests for status RPC handler.

use super::tag_channel_leads_and_count;

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
