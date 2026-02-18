use super::*;
use serde_json::json;

// ── extract_lead_text tests ─────────────────────────────────────────

#[test]
fn test_extract_lead_text_single_text_block() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{"type": "text", "text": "Hello world"}]
        }),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(extract_lead_text(&events), "Hello world");
}

#[test]
fn test_extract_lead_text_aggregates_multiple_events() {
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello "}]
            }),
            session_id: None,
            extra: json!(null),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "world"}]
            }),
            session_id: None,
            extra: json!(null),
        },
    ];
    assert_eq!(extract_lead_text(&events), "Hello world");
}

#[test]
fn test_extract_lead_text_skips_non_text_blocks() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [
                {"type": "tool_use", "id": "123", "name": "Read"},
                {"type": "text", "text": "Reading file..."}
            ]
        }),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(extract_lead_text(&events), "Reading file...");
}

#[test]
fn test_extract_lead_text_empty_content_array() {
    let events = vec![StreamEvent::Assistant {
        message: json!({"content": []}),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(extract_lead_text(&events), "");
}

#[test]
fn test_extract_lead_text_no_text_blocks() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{"type": "tool_use", "id": "123", "name": "Read"}]
        }),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(extract_lead_text(&events), "");
}

#[test]
fn test_extract_lead_text_non_assistant_events() {
    let events = vec![
        StreamEvent::System {
            subtype: "init".to_string(),
            session_id: Some("abc-123".to_string()),
            model: Some("sonnet".to_string()),
            extra: json!({}),
        },
        StreamEvent::User {
            message: json!({"content": "user input"}),
            extra: json!({}),
        },
    ];
    assert_eq!(extract_lead_text(&events), "");
}

// ── process_lead_output tests ───────────────────────────────────────

#[test]
fn test_process_lead_output_no_events() {
    let events = HashMap::new();
    let effects = process_lead_output(&events);
    assert!(effects.is_empty());
}

#[test]
fn test_process_lead_output_no_lead_events() {
    let mut events = HashMap::new();
    events.insert("coworker".to_string(), vec![]);
    let effects = process_lead_output(&events);
    assert!(effects.is_empty());
}

#[test]
fn test_process_lead_output_returns_post_effect() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello from lead"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );

    let effects = process_lead_output(&events);
    assert_eq!(effects.len(), 1);

    match &effects[0] {
        Effect::PostToChannel {
            sender,
            message,
            channel,
        } => {
            assert_eq!(sender, "lead");
            assert_eq!(message, "Hello from lead");
            assert!(channel.is_none());
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_lead_output_aggregates_multiple_events() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![
            StreamEvent::Assistant {
                message: json!({
                    "content": [{"type": "text", "text": "First "}]
                }),
                session_id: None,
                extra: json!(null),
            },
            StreamEvent::Assistant {
                message: json!({
                    "content": [{"type": "text", "text": "Second"}]
                }),
                session_id: None,
                extra: json!(null),
            },
        ],
    );

    let effects = process_lead_output(&events);
    assert_eq!(effects.len(), 1);

    match &effects[0] {
        Effect::PostToChannel { message, .. } => {
            assert_eq!(message, "First Second");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_lead_output_empty_text_not_posted() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "name": "Read", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );

    let effects = process_lead_output(&events);
    assert!(
        effects.is_empty(),
        "Should not post if no text content found"
    );
}

// ── process_universal_events tests ───────────────────────────────────

#[test]
fn test_process_universal_events_no_events() {
    let events = HashMap::new();
    let channel_leads = HashMap::new();
    let effects = process_universal_events(&events, &channel_leads);
    assert!(effects.is_empty());
}

#[test]
fn test_process_universal_events_text_only_no_effects() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let effects = process_universal_events(&events, &HashMap::new());
    assert!(effects.is_empty());
}

#[test]
fn test_process_universal_events_lead_tool_use_produces_effect() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {"path": "/foo"}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let effects = process_universal_events(&events, &HashMap::new());
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            items,
        } => {
            assert_eq!(agent_name, "lead");
            assert!(channel.is_none(), "Main lead should have no channel");
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_coworker_tool_use_ignored() {
    let mut events = HashMap::new();
    events.insert(
        "lexington".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {"path": "/foo"}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    // Coworker tool calls are not shown to the user — only lead and channel lead tool calls are.
    let effects = process_universal_events(&events, &HashMap::new());
    assert!(effects.is_empty());
}

#[test]
fn test_process_universal_events_only_lead_when_multiple_agents() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Edit", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_2", "name": "Bash", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    // Only the lead's tool calls produce an effect; coworker events are ignored.
    let effects = process_universal_events(&events, &HashMap::new());
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            ..
        } => {
            assert_eq!(agent_name, "lead");
            assert!(channel.is_none());
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_channel_lead_tool_use_produces_channel_scoped_effect() {
    let mut events = HashMap::new();
    events.insert(
        "web".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {"path": "/foo"}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let mut channel_leads = HashMap::new();
    channel_leads.insert("web".to_string(), "some-session-id".to_string());

    let effects = process_universal_events(&events, &channel_leads);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            items,
        } => {
            assert_eq!(agent_name, "web");
            assert_eq!(
                channel.as_deref(),
                Some("web"),
                "Channel lead should be tagged with its channel"
            );
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_channel_lead_not_in_sessions_is_ignored() {
    // If a session named "web" is in events but NOT in channel_lead_sessions, it's a coworker.
    let mut events = HashMap::new();
    events.insert(
        "web".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    // No channel leads registered → "web" session is treated as a regular coworker.
    let effects = process_universal_events(&events, &HashMap::new());
    assert!(effects.is_empty());
}

#[test]
fn test_process_universal_events_lead_and_channel_lead_produce_separate_effects() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Edit", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    events.insert(
        "features".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_2", "name": "Bash", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let mut channel_leads = HashMap::new();
    channel_leads.insert("features".to_string(), "cl-session-id".to_string());

    let effects = process_universal_events(&events, &channel_leads);
    assert_eq!(effects.len(), 2);

    // Verify both effects are present with correct scoping.
    let lead_effect = effects.iter().find(|e| {
        matches!(e,
            Effect::BroadcastUniversalItems { agent_name, .. } if agent_name == "lead"
        )
    });
    assert!(lead_effect.is_some());
    if let Some(Effect::BroadcastUniversalItems { channel, .. }) = lead_effect {
        assert!(channel.is_none());
    }

    let cl_effect = effects.iter().find(|e| {
        matches!(e,
            Effect::BroadcastUniversalItems { agent_name, .. } if agent_name == "features"
        )
    });
    assert!(cl_effect.is_some());
    if let Some(Effect::BroadcastUniversalItems { channel, .. }) = cl_effect {
        assert_eq!(channel.as_deref(), Some("features"));
    }
}

#[test]
fn test_process_universal_events_channel_lead_registered_but_no_events_produces_no_effect() {
    // A channel lead is registered in the session map but didn't produce any events this tick.
    let events = HashMap::new(); // no events at all
    let mut channel_leads = HashMap::new();
    channel_leads.insert("web".to_string(), "some-session-id".to_string());

    let effects = process_universal_events(&events, &channel_leads);
    assert!(
        effects.is_empty(),
        "No effect when channel lead has no events this tick"
    );
}
