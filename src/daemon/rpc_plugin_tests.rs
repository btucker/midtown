//! Tests for plugin RPC handlers.
//!
//! Tests the pure data-transformation functions used by the plugin.dashboard,
//! plugin.attach, plugin.detach, and plugin.coworker-stream endpoints.

use midtown_types::{ChannelMessage, CoworkerSummary, DashboardState, TaskSummary};

// ============================================================================
// Dashboard serialization tests
// ============================================================================

#[test]
fn test_dashboard_state_serializes_to_json() {
    let state = DashboardState {
        tasks: vec![TaskSummary {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            status: "in_progress".to_string(),
            owner: Some("madison".to_string()),
            pr_number: Some(123),
            pr_status: Some("open".to_string()),
        }],
        coworkers: vec![CoworkerSummary {
            name: "madison".to_string(),
            status: "developing".to_string(),
            current_task: Some("Add auth endpoint".to_string()),
            session_id: Some("sess-abc".to_string()),
            model: "claude/sonnet".to_string(),
            is_alive: true,
            has_usage_limit: false,
            has_api_error: false,
            last_event_at: None,
        }],
        channel_messages: vec![ChannelMessage {
            from: "madison".to_string(),
            content: "working on auth".to_string(),
            timestamp: chrono::Utc::now(),
            message_type: "user".to_string(),
        }],
        lead_nudge_queue: vec!["PR #42 needs review".to_string()],
        daemon_version: "0.5.4".to_string(),
    };

    let json = serde_json::to_value(&state).unwrap();
    assert_eq!(json["tasks"][0]["id"], "42");
    assert_eq!(json["tasks"][0]["owner"], "madison");
    assert_eq!(json["coworkers"][0]["name"], "madison");
    assert_eq!(json["coworkers"][0]["is_alive"], true);
    assert_eq!(json["lead_nudge_queue"][0], "PR #42 needs review");
    assert_eq!(json["daemon_version"], "0.5.4");
}

#[test]
fn test_dashboard_state_roundtrips() {
    let state = DashboardState {
        tasks: vec![],
        coworkers: vec![],
        channel_messages: vec![],
        lead_nudge_queue: vec![],
        daemon_version: "0.5.4".to_string(),
    };

    let json = serde_json::to_string(&state).unwrap();
    let deserialized: DashboardState = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.daemon_version, "0.5.4");
    assert!(deserialized.tasks.is_empty());
    assert!(deserialized.coworkers.is_empty());
}

// ============================================================================
// Task summary builder tests
// ============================================================================

#[test]
fn test_build_task_summaries_maps_fields() {
    let tasks = vec![crate::tasks::Task {
        id: "42".to_string(),
        subject: "Add auth".to_string(),
        description: Some("Full description".to_string()),
        status: crate::tasks::TaskStatus::InProgress,
        owner: Some("madison".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    }];

    let summaries = super::build_task_summaries(&tasks);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, "42");
    assert_eq!(summaries[0].subject, "Add auth");
    assert_eq!(summaries[0].status, "in_progress");
    assert_eq!(summaries[0].owner, Some("madison".to_string()));
    assert_eq!(summaries[0].pr_number, None);
}

#[test]
fn test_build_task_summaries_with_pr() {
    let tasks = vec![crate::tasks::Task {
        id: "43".to_string(),
        subject: "Fix bug".to_string(),
        description: None,
        status: crate::tasks::TaskStatus::Completed,
        owner: Some("park".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(99),
        created_at: None,
    }];

    let summaries = super::build_task_summaries(&tasks);
    assert_eq!(summaries[0].status, "completed");
    assert_eq!(summaries[0].pr_number, Some(99));
}

#[test]
fn test_build_task_summaries_empty_owner() {
    let tasks = vec![crate::tasks::Task {
        id: "44".to_string(),
        subject: "Pending task".to_string(),
        description: None,
        status: crate::tasks::TaskStatus::Pending,
        owner: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    }];

    let summaries = super::build_task_summaries(&tasks);
    assert_eq!(summaries[0].owner, None);
    assert_eq!(summaries[0].status, "pending");
}

// ============================================================================
// Coworker summary builder tests
// ============================================================================

#[test]
fn test_build_coworker_summaries_basic() {
    use super::CoworkerBuildInput;

    let inputs = vec![CoworkerBuildInput {
        name: "madison".to_string(),
        phase: Some("developing".to_string()),
        current_task: Some("Add auth endpoint".to_string()),
        session_id: Some("sess-abc".to_string()),
        model: "claude/sonnet".to_string(),
        is_alive: true,
        has_usage_limit: false,
        has_api_error: false,
        last_event_at: None,
    }];

    let summaries = super::build_coworker_summaries(&inputs);
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].name, "madison");
    assert_eq!(summaries[0].status, "developing");
    assert!(summaries[0].is_alive);
    assert!(!summaries[0].has_usage_limit);
}

#[test]
fn test_build_coworker_summaries_no_phase_shows_unknown() {
    use super::CoworkerBuildInput;

    let inputs = vec![CoworkerBuildInput {
        name: "park".to_string(),
        phase: None,
        current_task: None,
        session_id: None,
        model: "claude/opus".to_string(),
        is_alive: true,
        has_usage_limit: false,
        has_api_error: false,
        last_event_at: None,
    }];

    let summaries = super::build_coworker_summaries(&inputs);
    assert_eq!(summaries[0].status, "unknown");
}

// ============================================================================
// Channel message builder tests
// ============================================================================

#[test]
fn test_build_channel_messages_from_raw() {
    let now = chrono::Utc::now();
    let raw = vec![crate::message::Message {
        id: "msg-1".to_string(),
        from: "madison".to_string(),
        content: "claimed task 42".to_string(),
        timestamp: now,
        message_type: crate::message::MessageType::default(),
        channel: None,
        source_channel: None,
        session_id: None,
    }];

    let messages = super::build_channel_messages(&raw);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].from, "madison");
    assert_eq!(messages[0].content, "claimed task 42");
    assert_eq!(messages[0].message_type, "text");
}

// ============================================================================
// Lead nudge queue tests
// ============================================================================

#[test]
fn test_lead_nudge_queue_drain() {
    let mut queue: Vec<String> = vec![
        "PR #42 needs review".to_string(),
        "CI failed on PR #43".to_string(),
    ];

    // Drain the queue (simulating what the dashboard handler does)
    let drained: Vec<String> = std::mem::take(&mut queue);
    assert_eq!(drained.len(), 2);
    assert!(queue.is_empty());
}
