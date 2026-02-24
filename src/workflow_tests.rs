use super::*;

// ── Serialization: type discriminant ─────────────────────────────────────────

#[test]
fn test_task_created_type_field() {
    let event = WorkflowEvent::TaskCreated {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        subject: "Add auth".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "task.created");
}

#[test]
fn test_task_assigned_type_field() {
    let event = WorkflowEvent::TaskAssigned {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        coworker: "lexington".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "task.assigned");
}

#[test]
fn test_task_completed_type_field() {
    let event = WorkflowEvent::TaskCompleted {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        coworker: Some("lexington".into()),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "task.completed");
}

#[test]
fn test_pr_opened_type_field() {
    let event = WorkflowEvent::PrOpened {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
        coworker: "lexington".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "pr.opened");
}

#[test]
fn test_pr_approved_type_field() {
    let event = WorkflowEvent::PrApproved {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "pr.approved");
}

#[test]
fn test_pr_changes_requested_type_field() {
    let event = WorkflowEvent::PrChangesRequested {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "pr.changes_requested");
}

#[test]
fn test_pr_merged_type_field() {
    let event = WorkflowEvent::PrMerged {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "pr.merged");
}

#[test]
fn test_pr_ci_passed_type_field() {
    let event = WorkflowEvent::PrCiPassed {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "pr.ci_passed");
}

#[test]
fn test_pr_ci_failed_type_field() {
    let event = WorkflowEvent::PrCiFailed {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
        check_name: Some("CI / build".into()),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "pr.ci_failed");
}

#[test]
fn test_pr_conflict_type_field() {
    let event = WorkflowEvent::PrConflict {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "pr.conflict");
}

#[test]
fn test_coworker_idle_type_field() {
    let event = WorkflowEvent::CoworkerIdle {
        channel: "proj-workflows".into(),
        task_id: Some("42".into()),
        coworker: "lexington".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "coworker.idle");
}

#[test]
fn test_coworker_stuck_type_field() {
    let event = WorkflowEvent::CoworkerStuck {
        channel: "proj-workflows".into(),
        task_id: None,
        coworker: "lexington".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "coworker.stuck");
}

#[test]
fn test_coworker_message_type_field() {
    let event = WorkflowEvent::CoworkerMessage {
        channel: "proj-workflows".into(),
        task_id: Some("42".into()),
        coworker: "lexington".into(),
        message: "PR is ready".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "coworker.message");
}

#[test]
fn test_channel_message_type_field() {
    let event = WorkflowEvent::ChannelMessage {
        channel: "proj-workflows".into(),
        sender: "user".into(),
        message: "Looks good".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "channel.message");
}

#[test]
fn test_timer_tick_type_field() {
    let event = WorkflowEvent::TimerTick {
        channel: "proj-workflows".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "timer.tick");
}

// ── Serialization: field presence ────────────────────────────────────────────

#[test]
fn test_pr_opened_fields() {
    let event = WorkflowEvent::PrOpened {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
        coworker: "lexington".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["channel"], "proj-workflows");
    assert_eq!(json["task_id"], "42");
    assert_eq!(json["pr_number"], 123);
    assert_eq!(json["coworker"], "lexington");
}

#[test]
fn test_pr_ci_failed_optional_check_name_present() {
    let event = WorkflowEvent::PrCiFailed {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
        check_name: Some("CI / build".into()),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["check_name"], "CI / build");
}

#[test]
fn test_pr_ci_failed_optional_check_name_absent() {
    let event = WorkflowEvent::PrCiFailed {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
        check_name: None,
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    // None: key is omitted entirely, not serialized as null
    assert!(json.get("check_name").is_none());
}

#[test]
fn test_coworker_idle_without_task() {
    let event = WorkflowEvent::CoworkerIdle {
        channel: "proj-workflows".into(),
        task_id: None,
        coworker: "lexington".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["channel"], "proj-workflows");
    // None: key is omitted entirely, not serialized as null
    assert!(json.get("task_id").is_none());
    assert_eq!(json["coworker"], "lexington");
}

#[test]
fn test_timer_tick_fields() {
    let event = WorkflowEvent::TimerTick {
        channel: "proj-workflows".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["channel"], "proj-workflows");
}

#[test]
fn test_timer_tick_no_task_id_key() {
    // TimerTick has no task_id field — the key must be absent, not null.
    let event = WorkflowEvent::TimerTick {
        channel: "proj-workflows".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert!(json.get("task_id").is_none());
}

#[test]
fn test_channel_message_no_task_id_key() {
    // ChannelMessage has no task_id field — the key must be absent, not null.
    let event = WorkflowEvent::ChannelMessage {
        channel: "proj-workflows".into(),
        sender: "user".into(),
        message: "hey".into(),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert!(json.get("task_id").is_none());
}

// ── channel() accessor ────────────────────────────────────────────────────────

#[test]
fn test_channel_accessor_task_created() {
    let event = WorkflowEvent::TaskCreated {
        channel: "my-channel".into(),
        task_id: "1".into(),
        subject: "foo".into(),
    };
    assert_eq!(event.channel(), "my-channel");
}

#[test]
fn test_channel_accessor_timer_tick() {
    let event = WorkflowEvent::TimerTick {
        channel: "my-channel".into(),
    };
    assert_eq!(event.channel(), "my-channel");
}

#[test]
fn test_channel_accessor_channel_message() {
    let event = WorkflowEvent::ChannelMessage {
        channel: "general".into(),
        sender: "user".into(),
        message: "hello".into(),
    };
    assert_eq!(event.channel(), "general");
}

// ── task_id() accessor ───────────────────────────────────────────────────────

#[test]
fn test_task_id_present_for_task_event() {
    let event = WorkflowEvent::TaskCreated {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        subject: "foo".into(),
    };
    assert_eq!(event.task_id(), Some("42"));
}

#[test]
fn test_task_id_present_for_pr_event() {
    let event = WorkflowEvent::PrMerged {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
    };
    assert_eq!(event.task_id(), Some("42"));
}

#[test]
fn test_task_id_absent_for_timer_tick() {
    let event = WorkflowEvent::TimerTick {
        channel: "proj-workflows".into(),
    };
    assert_eq!(event.task_id(), None);
}

#[test]
fn test_task_id_absent_for_channel_message() {
    let event = WorkflowEvent::ChannelMessage {
        channel: "proj-workflows".into(),
        sender: "user".into(),
        message: "hey".into(),
    };
    assert_eq!(event.task_id(), None);
}

#[test]
fn test_task_id_optional_for_coworker_idle_with_task() {
    let event = WorkflowEvent::CoworkerIdle {
        channel: "proj-workflows".into(),
        task_id: Some("37".into()),
        coworker: "lexington".into(),
    };
    assert_eq!(event.task_id(), Some("37"));
}

#[test]
fn test_task_id_optional_for_coworker_idle_without_task() {
    let event = WorkflowEvent::CoworkerIdle {
        channel: "proj-workflows".into(),
        task_id: None,
        coworker: "lexington".into(),
    };
    assert_eq!(event.task_id(), None);
}

#[test]
fn test_task_id_optional_for_task_completed_with_coworker() {
    let event = WorkflowEvent::TaskCompleted {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        coworker: Some("lexington".into()),
    };
    assert_eq!(event.task_id(), Some("42"));
}
