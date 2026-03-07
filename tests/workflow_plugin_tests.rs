//! Integration tests for workflow event serialization.
//!
//! Tests event serialization for Python workflow scripts.

// ── Event serialization for Python dispatch ──────────────────────────────────
//
// These tests verify that WorkflowEvent types serialize to the JSON format
// expected by Python workflow scripts. The Python SDK parses events via
// `event["type"]` and `event.get("task_id")`.

#[test]
fn event_serializes_to_tagged_json_for_python() {
    let event = midtown::workflow::WorkflowEvent::PrOpened {
        channel: "proj-auth".into(),
        task_id: "42".into(),
        pr_number: 123,
        coworker: "lexington".into(),
    };
    let json_str = serde_json::to_string(&event).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    // Python scripts access these fields directly:
    // event["type"], event["channel"], event["pr_number"]
    assert_eq!(parsed["type"], "pr.opened");
    assert_eq!(parsed["channel"], "proj-auth");
    assert_eq!(parsed["task_id"], "42");
    assert_eq!(parsed["pr_number"], 123);
    assert_eq!(parsed["coworker"], "lexington");
}

#[test]
fn event_omits_none_fields_for_python_get_semantics() {
    // Python scripts use event.get("task_id") which returns None if absent.
    // Serializing as null would make event.get("task_id") return null (truthy
    // in Python), breaking the convention. Fields must be omitted entirely.
    let event = midtown::workflow::WorkflowEvent::CoworkerIdle {
        channel: "proj-auth".into(),
        task_id: None,
        coworker: "lexington".into(),
    };
    let json_str = serde_json::to_string(&event).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert!(
        parsed.get("task_id").is_none(),
        "None fields must be omitted (not null) for Python event.get() semantics"
    );
}

#[test]
fn event_type_maps_to_python_hook_name() {
    // Python workflow daemon converts event types to hook names:
    // "pr.opened" -> "on_pr_opened" (replace "." with "_", prepend "on_")
    let test_cases: Vec<(midtown::workflow::WorkflowEvent, &str, &str)> = vec![
        (
            midtown::workflow::WorkflowEvent::TaskCreated {
                channel: "ch".into(),
                task_id: "1".into(),
                subject: "test".into(),
                description: None,
                thread_id: None,
                message_id: None,
            },
            "task.created",
            "on_task_created",
        ),
        (
            midtown::workflow::WorkflowEvent::PrOpened {
                channel: "ch".into(),
                task_id: "1".into(),
                pr_number: 1,
                coworker: "a".into(),
            },
            "pr.opened",
            "on_pr_opened",
        ),
        (
            midtown::workflow::WorkflowEvent::CoworkerIdle {
                channel: "ch".into(),
                task_id: None,
                coworker: "a".into(),
            },
            "coworker.idle",
            "on_coworker_idle",
        ),
        (
            midtown::workflow::WorkflowEvent::TimerTick {
                channel: "ch".into(),
            },
            "timer.tick",
            "on_timer_tick",
        ),
    ];

    for (event, expected_type, expected_hook) in test_cases {
        let json: serde_json::Value = serde_json::to_value(&event).unwrap();
        let event_type = json["type"].as_str().unwrap();
        assert_eq!(event_type, expected_type);

        // Verify the Python hook name derivation works
        let hook_name = format!("on_{}", event_type.replace('.', "_"));
        assert_eq!(hook_name, expected_hook);
    }
}

#[test]
fn event_channel_accessor_is_consistent_with_serialization() {
    let event = midtown::workflow::WorkflowEvent::PrMerged {
        channel: "proj-workflows".into(),
        task_id: "42".into(),
        pr_number: 123,
    };

    // The channel() accessor and serialized "channel" field should agree
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(event.channel(), json["channel"].as_str().unwrap());
}

#[test]
fn workflow_state_file_is_channel_specific() {
    // Each channel gets its own state file to avoid cross-channel state leaks
    let state_a = midtown::paths::workflow_state_file("proj-auth", "myrepo");
    let state_b = midtown::paths::workflow_state_file("proj-frontend", "myrepo");

    assert_ne!(
        state_a, state_b,
        "Different channels must have different state files"
    );
    assert!(
        state_a.to_string_lossy().contains("proj-auth"),
        "State file path should contain channel name"
    );
    assert!(
        state_b.to_string_lossy().contains("proj-frontend"),
        "State file path should contain channel name"
    );
}

// ── Event dispatch to Python (sidecar envelope format) ───────────────────────
//
// These tests verify the envelope format that the sidecar protocol expects.
// Full dispatch tests will be added once socket communication is wired up.

#[test]
fn sidecar_envelope_contains_required_fields() {
    // The sidecar receives JSON envelopes on stdin with this shape:
    // {"event": {...}, "state_file": "/path/to/state.json", "socket": "/path/to/daemon.sock"}
    let event = midtown::workflow::WorkflowEvent::TaskAssigned {
        channel: "proj-auth".into(),
        task_id: "42".into(),
        coworker: "lexington".into(),
        subject: "Add auth endpoint".into(),
        description: Some("Build the /api/auth endpoint".into()),
        thread_id: None,
        message_id: None,
    };
    let event_json = serde_json::to_value(&event).unwrap();

    let envelope = serde_json::json!({
        "event": event_json,
        "state_file": "/tmp/workflow-state.json",
        "socket": "/tmp/daemon.sock",
    });

    // Verify envelope structure matches what Python SDK expects
    assert!(envelope.get("event").is_some());
    assert!(envelope.get("state_file").is_some());
    assert!(envelope.get("socket").is_some());

    // Verify event within envelope is correctly structured
    let inner_event = &envelope["event"];
    assert_eq!(inner_event["type"], "task.assigned");
    assert_eq!(inner_event["channel"], "proj-auth");
    assert_eq!(inner_event["task_id"], "42");
    assert_eq!(inner_event["coworker"], "lexington");
    assert_eq!(inner_event["subject"], "Add auth endpoint");
    assert_eq!(inner_event["description"], "Build the /api/auth endpoint");
}

#[test]
fn sidecar_envelope_event_roundtrips_through_json_string() {
    // The actual dispatch path serializes the event to a JSON string,
    // then embeds it in the envelope. Verify no data is lost.
    let event = midtown::workflow::WorkflowEvent::PrCiFailed {
        channel: "proj-ci".into(),
        task_id: "99".into(),
        pr_number: 456,
        check_name: Some("CI / build".into()),
    };

    // Simulate the dispatch path: event -> JSON string -> parse back -> embed
    let event_json_str = serde_json::to_string(&event).unwrap();
    let reparsed: serde_json::Value = serde_json::from_str(&event_json_str).unwrap();

    let envelope = serde_json::json!({
        "event": reparsed,
        "state_file": "/tmp/state.json",
        "socket": "/tmp/daemon.sock",
    });

    let inner = &envelope["event"];
    assert_eq!(inner["type"], "pr.ci_failed");
    assert_eq!(inner["pr_number"], 456);
    assert_eq!(inner["check_name"], "CI / build");
}

// TODO: Add tests for actual Python dispatch once socket communication is wired up:
// - test_dispatch_event_to_python_sidecar: spawn a real Python sidecar,
//   send an event, verify the response
// - test_dispatch_fallback_to_subprocess: verify subprocess-per-event mode
//   works when sidecar mode is not supported
// - test_plugin_hot_reload: modify a plugin file and verify the sidecar
//   detects the change and reloads
