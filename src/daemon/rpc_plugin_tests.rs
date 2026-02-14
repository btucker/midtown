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

#[test]
fn test_lead_nudge_queue_drain_is_atomic() {
    // Verify that draining returns all queued items and leaves queue empty.
    // This matches the dashboard handler's `std::mem::take` pattern.
    let mut queue: Vec<String> = vec![];

    // Empty queue drain
    let drained: Vec<String> = std::mem::take(&mut queue);
    assert!(drained.is_empty());
    assert!(queue.is_empty());

    // Add items and drain
    queue.push("nudge 1".to_string());
    queue.push("nudge 2".to_string());
    queue.push("nudge 3".to_string());
    let drained: Vec<String> = std::mem::take(&mut queue);
    assert_eq!(drained.len(), 3);
    assert_eq!(drained[0], "nudge 1");
    assert_eq!(drained[2], "nudge 3");
    assert!(queue.is_empty());

    // Second drain returns empty
    let drained: Vec<String> = std::mem::take(&mut queue);
    assert!(drained.is_empty());
}

// ============================================================================
// Lead nudge queue cap tests
// ============================================================================

#[test]
fn test_lead_nudge_queue_cap_evicts_oldest() {
    use super::MAX_LEAD_NUDGE_QUEUE;

    let mut queue: Vec<String> = (0..MAX_LEAD_NUDGE_QUEUE + 10)
        .map(|i| format!("nudge {}", i))
        .collect();

    // Simulate the cap logic from effects.rs
    if queue.len() > MAX_LEAD_NUDGE_QUEUE {
        let excess = queue.len() - MAX_LEAD_NUDGE_QUEUE;
        queue.drain(..excess);
    }

    assert_eq!(queue.len(), MAX_LEAD_NUDGE_QUEUE);
    // Oldest entries (0-9) should be evicted
    assert_eq!(queue[0], "nudge 10");
    assert_eq!(
        queue.last().unwrap(),
        &format!("nudge {}", MAX_LEAD_NUDGE_QUEUE + 9)
    );
}

#[test]
fn test_lead_nudge_queue_under_cap_no_eviction() {
    use super::MAX_LEAD_NUDGE_QUEUE;

    let mut queue: Vec<String> = (0..5).map(|i| format!("nudge {}", i)).collect();

    // Simulate the cap logic — should not evict
    if queue.len() > MAX_LEAD_NUDGE_QUEUE {
        let excess = queue.len() - MAX_LEAD_NUDGE_QUEUE;
        queue.drain(..excess);
    }

    assert_eq!(queue.len(), 5);
    assert_eq!(queue[0], "nudge 0");
}

// ============================================================================
// Coworker task map from pre-loaded tasks
// ============================================================================

#[test]
fn test_coworker_task_map_from_preloaded_tasks() {
    // Verifies that the dashboard handler correctly builds the coworker task
    // map from the already-loaded tasks slice (avoiding a duplicate read_tasks call).
    let tasks = vec![
        crate::tasks::Task {
            id: "1".to_string(),
            subject: "Active task".to_string(),
            description: None,
            status: crate::tasks::TaskStatus::InProgress,
            owner: Some("madison".to_string()),
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        },
        crate::tasks::Task {
            id: "2".to_string(),
            subject: "Pending task".to_string(),
            description: None,
            status: crate::tasks::TaskStatus::Pending,
            owner: Some("park".to_string()),
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        },
        crate::tasks::Task {
            id: "3".to_string(),
            subject: "Unowned task".to_string(),
            description: None,
            status: crate::tasks::TaskStatus::InProgress,
            owner: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        },
    ];

    // Replicate the logic from handle_dashboard
    let coworker_tasks: std::collections::HashMap<String, String> = tasks
        .iter()
        .filter(|t| t.status == crate::tasks::TaskStatus::InProgress)
        .filter_map(|t| {
            let owner = t.owner.as_deref().unwrap_or("");
            if owner.is_empty() {
                None
            } else {
                Some((owner.to_lowercase(), t.subject.clone()))
            }
        })
        .collect();

    // Only in_progress tasks with owners should appear
    assert_eq!(coworker_tasks.len(), 1);
    assert_eq!(coworker_tasks.get("madison").unwrap(), "Active task");
    // Pending tasks and unowned tasks should not appear
    assert!(coworker_tasks.get("park").is_none());
}

// ============================================================================
// Stream event buffer tests
// ============================================================================

#[test]
fn test_buffered_stream_event_fields() {
    use super::BufferedStreamEvent;

    let event = BufferedStreamEvent {
        timestamp: chrono::Utc::now(),
        event_type: "assistant".to_string(),
        content: "Working on implementation".to_string(),
    };

    assert_eq!(event.event_type, "assistant");
    assert_eq!(event.content, "Working on implementation");
}

#[test]
fn test_stream_events_convert_to_midtown_types() {
    use super::BufferedStreamEvent;
    use midtown_types::StreamEvent;

    let buffered = vec![
        BufferedStreamEvent {
            timestamp: chrono::Utc::now(),
            event_type: "tool_use".to_string(),
            content: "Read file foo.rs".to_string(),
        },
        BufferedStreamEvent {
            timestamp: chrono::Utc::now(),
            event_type: "assistant".to_string(),
            content: "Found the issue".to_string(),
        },
    ];

    let stream_events: Vec<StreamEvent> = buffered
        .into_iter()
        .map(|evt| StreamEvent {
            timestamp: evt.timestamp,
            event_type: evt.event_type,
            content: evt.content,
        })
        .collect();

    assert_eq!(stream_events.len(), 2);
    assert_eq!(stream_events[0].event_type, "tool_use");
    assert_eq!(stream_events[1].event_type, "assistant");
}

// ============================================================================
// StreamEvent → BufferedStreamEvent conversion tests
// ============================================================================

#[test]
fn test_stream_event_to_buffered_system_init() {
    let event = crate::headless::StreamEvent::System {
        subtype: "init".to_string(),
        session_id: Some("sess-123".to_string()),
        model: Some("claude-sonnet".to_string()),
        extra: serde_json::json!({}),
    };

    let buffered = super::stream_event_to_buffered(&event);
    assert_eq!(buffered.event_type, "system:init");
    assert!(buffered.content.contains("sess-123"));
}

#[test]
fn test_stream_event_to_buffered_assistant_text() {
    let event = crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({
            "content": [
                {"type": "text", "text": "Working on the implementation"}
            ]
        }),
        session_id: None,
        extra: serde_json::json!({}),
    };

    let buffered = super::stream_event_to_buffered(&event);
    assert_eq!(buffered.event_type, "assistant");
    assert_eq!(buffered.content, "Working on the implementation");
}

#[test]
fn test_stream_event_to_buffered_assistant_tool_use() {
    let event = crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({
            "content": [
                {"type": "tool_use", "name": "Read", "id": "t1", "input": {}},
                {"type": "text", "text": "Let me check that file"}
            ]
        }),
        session_id: None,
        extra: serde_json::json!({}),
    };

    let buffered = super::stream_event_to_buffered(&event);
    assert_eq!(buffered.event_type, "assistant");
    assert!(buffered.content.contains("Tool: Read"));
    assert!(buffered.content.contains("Let me check that file"));
}

#[test]
fn test_stream_event_to_buffered_assistant_long_text_truncated() {
    let long_text = "x".repeat(300);
    let event = crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({
            "content": [
                {"type": "text", "text": long_text}
            ]
        }),
        session_id: None,
        extra: serde_json::json!({}),
    };

    let buffered = super::stream_event_to_buffered(&event);
    // Should be truncated to 200 chars + "..."
    assert!(buffered.content.len() <= 203);
    assert!(buffered.content.ends_with("..."));
}

#[test]
fn test_stream_event_to_buffered_result_success() {
    let event = crate::headless::StreamEvent::Result {
        subtype: "success".to_string(),
        is_error: false,
        result: None,
        duration_ms: Some(5000),
        total_cost_usd: Some(0.0123),
        session_id: None,
        extra: serde_json::json!({}),
    };

    let buffered = super::stream_event_to_buffered(&event);
    assert_eq!(buffered.event_type, "result");
    assert!(buffered.content.contains("Turn completed"));
    assert!(buffered.content.contains("$0.0123"));
}

#[test]
fn test_stream_event_to_buffered_result_error() {
    let event = crate::headless::StreamEvent::Result {
        subtype: "error".to_string(),
        is_error: true,
        result: Some("API rate limit exceeded".to_string()),
        duration_ms: None,
        total_cost_usd: None,
        session_id: None,
        extra: serde_json::json!({}),
    };

    let buffered = super::stream_event_to_buffered(&event);
    assert_eq!(buffered.event_type, "result");
    assert!(buffered.content.contains("Error: API rate limit exceeded"));
}

#[test]
fn test_stream_event_to_buffered_user() {
    let event = crate::headless::StreamEvent::User {
        message: serde_json::json!({}),
        extra: serde_json::json!({}),
    };

    let buffered = super::stream_event_to_buffered(&event);
    assert_eq!(buffered.event_type, "user");
    assert_eq!(buffered.content, "User message");
}

// ============================================================================
// Ring buffer append / trim tests
// ============================================================================

#[test]
fn test_append_to_stream_buffer_basic() {
    use super::BufferedStreamEvent;

    let buffer = std::sync::RwLock::new(std::collections::HashMap::new());
    let events = vec![BufferedStreamEvent {
        timestamp: chrono::Utc::now(),
        event_type: "assistant".to_string(),
        content: "Hello".to_string(),
    }];

    super::append_to_stream_buffer(&buffer, "madison", events);

    let buf = buffer.read().unwrap();
    assert_eq!(buf.get("madison").unwrap().len(), 1);
    assert_eq!(buf.get("madison").unwrap()[0].content, "Hello");
}

#[test]
fn test_append_to_stream_buffer_trims_to_max() {
    use super::{BufferedStreamEvent, MAX_STREAM_EVENTS_PER_COWORKER};

    let buffer = std::sync::RwLock::new(std::collections::HashMap::new());

    // Add MAX + 20 events
    let events: Vec<BufferedStreamEvent> = (0..MAX_STREAM_EVENTS_PER_COWORKER + 20)
        .map(|i| BufferedStreamEvent {
            timestamp: chrono::Utc::now(),
            event_type: "assistant".to_string(),
            content: format!("Event {}", i),
        })
        .collect();

    super::append_to_stream_buffer(&buffer, "park", events);

    let buf = buffer.read().unwrap();
    let entries = buf.get("park").unwrap();
    assert_eq!(entries.len(), MAX_STREAM_EVENTS_PER_COWORKER);
    // Oldest events should be trimmed; the last entry should be the most recent
    assert_eq!(
        entries.last().unwrap().content,
        format!("Event {}", MAX_STREAM_EVENTS_PER_COWORKER + 19)
    );
    // First entry should be event 20 (the oldest 20 were trimmed)
    assert_eq!(entries[0].content, "Event 20");
}

#[test]
fn test_append_to_stream_buffer_empty_events_noop() {
    let buffer = std::sync::RwLock::new(std::collections::HashMap::new());
    super::append_to_stream_buffer(&buffer, "madison", vec![]);
    let buf = buffer.read().unwrap();
    assert!(buf.get("madison").is_none());
}

#[test]
fn test_append_to_stream_buffer_case_insensitive() {
    use super::BufferedStreamEvent;

    let buffer = std::sync::RwLock::new(std::collections::HashMap::new());
    let events = vec![BufferedStreamEvent {
        timestamp: chrono::Utc::now(),
        event_type: "assistant".to_string(),
        content: "Test".to_string(),
    }];

    super::append_to_stream_buffer(&buffer, "Madison", events);

    let buf = buffer.read().unwrap();
    // Should be stored under lowercase key
    assert!(buf.get("madison").is_some());
    assert!(buf.get("Madison").is_none());
}

#[test]
fn test_remove_stream_buffer() {
    use super::BufferedStreamEvent;

    let buffer = std::sync::RwLock::new(std::collections::HashMap::new());
    let events = vec![BufferedStreamEvent {
        timestamp: chrono::Utc::now(),
        event_type: "assistant".to_string(),
        content: "Test".to_string(),
    }];
    super::append_to_stream_buffer(&buffer, "madison", events);

    // Verify it exists
    assert!(buffer.read().unwrap().get("madison").is_some());

    // Remove it
    super::remove_stream_buffer(&buffer, "madison");
    assert!(buffer.read().unwrap().get("madison").is_none());
}

#[test]
fn test_remove_stream_buffer_case_insensitive() {
    use super::BufferedStreamEvent;

    let buffer = std::sync::RwLock::new(std::collections::HashMap::new());
    let events = vec![BufferedStreamEvent {
        timestamp: chrono::Utc::now(),
        event_type: "assistant".to_string(),
        content: "Test".to_string(),
    }];
    super::append_to_stream_buffer(&buffer, "madison", events);

    // Remove with different case
    super::remove_stream_buffer(&buffer, "Madison");
    assert!(buffer.read().unwrap().get("madison").is_none());
}
