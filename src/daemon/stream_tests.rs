use super::*;
use serde_json::json;

// ── extract_assistant_text tests ─────────────────────────────────────────

#[test]
fn test_extract_assistant_text_single_text_block() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{"type": "text", "text": "Hello world"}]
        }),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(extract_assistant_text(&events), "Hello world");
}

#[test]
fn test_extract_assistant_text_aggregates_multiple_events() {
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
    assert_eq!(extract_assistant_text(&events), "Hello world");
}

#[test]
fn test_extract_assistant_text_skips_non_text_blocks() {
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
    assert_eq!(extract_assistant_text(&events), "Reading file...");
}

#[test]
fn test_extract_assistant_text_empty_content_array() {
    let events = vec![StreamEvent::Assistant {
        message: json!({"content": []}),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(extract_assistant_text(&events), "");
}

#[test]
fn test_extract_assistant_text_no_text_blocks() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{"type": "tool_use", "id": "123", "name": "Read"}]
        }),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(extract_assistant_text(&events), "");
}

#[test]
fn test_extract_assistant_text_non_assistant_events() {
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
    assert_eq!(extract_assistant_text(&events), "");
}

#[test]
fn test_extract_assistant_text_codex_completed_supersedes_deltas() {
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "smoke-"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex"}),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "ack"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex"}),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "smoke-ack"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex", "event": "item/completed"}),
        },
    ];

    assert_eq!(extract_assistant_text(&events), "smoke-ack");
}

#[test]
fn test_extract_assistant_text_codex_ignores_delta_after_completed() {
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Done"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex", "event": "item/completed"}),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": " Next"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex"}),
        },
    ];

    assert_eq!(extract_assistant_text(&events), "Done");
}

#[test]
fn test_extract_assistant_text_codex_delta_only_not_emitted() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{"type": "text", "text": "delta-only"}]
        }),
        session_id: None,
        extra: json!({"provider": "codex"}),
    }];

    assert_eq!(extract_assistant_text(&events), "");
}

// ── Codex turn/completed fallback tests ─────────────────────────────

#[test]
fn test_extract_assistant_text_codex_delta_plus_turn_completed_uses_result_text() {
    // Codex normal flow: delta events + turn/completed (no item/completed for agentMessage).
    // The text should be extracted from the turn/completed Result event.
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello "}]
            }),
            session_id: None,
            extra: json!({"provider": "codex"}),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "world"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex"}),
        },
        StreamEvent::Result {
            subtype: "success".to_string(),
            is_error: false,
            result: Some("Hello world".to_string()),
            duration_ms: None,
            total_cost_usd: None,
            session_id: Some("thread_123".to_string()),
            usage: None,
            extra: json!({"provider": "codex", "status": "completed"}),
        },
    ];

    assert_eq!(extract_assistant_text(&events), "Hello world");
}

#[test]
fn test_extract_assistant_text_codex_completed_takes_precedence_over_result() {
    // When item/completed IS available, it should be used (not the Result text).
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "delta"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex"}),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "completed text"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex", "event": "item/completed"}),
        },
        StreamEvent::Result {
            subtype: "success".to_string(),
            is_error: false,
            result: Some("completed text".to_string()),
            duration_ms: None,
            total_cost_usd: None,
            session_id: Some("thread_123".to_string()),
            usage: None,
            extra: json!({"provider": "codex", "status": "completed"}),
        },
    ];

    // Should use item/completed text, not double up with Result text
    assert_eq!(extract_assistant_text(&events), "completed text");
}

#[test]
fn test_extract_assistant_text_codex_error_result_not_used_as_fallback() {
    // Error results should NOT be posted as channel text.
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "delta"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex"}),
        },
        StreamEvent::Result {
            subtype: "error".to_string(),
            is_error: true,
            result: Some("turn failed: timeout".to_string()),
            duration_ms: None,
            total_cost_usd: None,
            session_id: Some("thread_123".to_string()),
            usage: None,
            extra: json!({"provider": "codex", "status": "failed"}),
        },
    ];

    assert_eq!(extract_assistant_text(&events), "");
}

#[test]
fn test_extract_assistant_text_codex_result_without_text_ignored() {
    // Result with None text should not produce output.
    let events = vec![StreamEvent::Result {
        subtype: "success".to_string(),
        is_error: false,
        result: None,
        duration_ms: None,
        total_cost_usd: None,
        session_id: Some("thread_123".to_string()),
        usage: None,
        extra: json!({"provider": "codex", "status": "completed"}),
    }];

    assert_eq!(extract_assistant_text(&events), "");
}

#[test]
fn test_extract_assistant_text_non_codex_result_ignored() {
    // Non-Codex Result events should not contribute text.
    let events = vec![StreamEvent::Result {
        subtype: "success".to_string(),
        is_error: false,
        result: Some("Claude result".to_string()),
        duration_ms: None,
        total_cost_usd: None,
        session_id: Some("session_123".to_string()),
        usage: None,
        extra: json!({}),
    }];

    assert_eq!(extract_assistant_text(&events), "");
}

#[test]
fn test_extract_assistant_text_codex_bare_result_no_deltas_ignored() {
    // Cross-drain dedup: a bare Result event (no deltas in this drain cycle)
    // should NOT produce output. This prevents duplicate posts when
    // item/completed was already handled in a previous drain cycle.
    let events = vec![StreamEvent::Result {
        subtype: "success".to_string(),
        is_error: false,
        result: Some("Already posted text".to_string()),
        duration_ms: None,
        total_cost_usd: None,
        session_id: Some("thread_123".to_string()),
        usage: None,
        extra: json!({"provider": "codex", "status": "completed"}),
    }];

    assert_eq!(extract_assistant_text(&events), "");
}

// ── process_lead_output tests ───────────────────────────────────────

#[test]
fn test_process_lead_output_no_events() {
    let events = HashMap::new();
    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
    assert!(effects.is_empty());
}

#[test]
fn test_process_lead_output_no_lead_events() {
    let mut events = HashMap::new();
    events.insert("coworker".to_string(), vec![]);
    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
    assert!(effects.is_empty());
}

#[test]
fn test_process_lead_output_returns_post_effect() {
    // Use a project-name lead session (not "lead") to verify the parameter is actually used.
    let mut events = HashMap::new();
    events.insert(
        "myproject".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello from lead"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );

    let effects = process_lead_output(&events, &HashMap::new(), "myproject", &HashMap::new());
    assert_eq!(effects.len(), 1);

    match &effects[0] {
        Effect::PostToChannel {
            sender,
            message,
            channel,
            auto_output,
            ..
        } => {
            assert_eq!(sender, "myproject");
            assert_eq!(message, "Hello from lead");
            assert!(channel.is_none());
            assert!(auto_output, "stream output should be auto_output");
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

    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
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

    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
    assert!(
        effects.is_empty(),
        "Should not post if no text content found"
    );
}

// ── leading newline trimming tests ──────────────────────────────────

#[test]
fn test_process_lead_output_trims_leading_newlines() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "\n\nGood, amsterdam confirmed."}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );

    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { message, .. } => {
            assert_eq!(message, "Good, amsterdam confirmed.");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_lead_output_trims_trailing_newlines() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Done.\n\n"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );

    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { message, .. } => {
            assert_eq!(message, "Done.");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_lead_output_whitespace_only_not_posted() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "\n\n  \n"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );

    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
    assert!(
        effects.is_empty(),
        "Should not post a message that is only whitespace after trimming"
    );
}

// ── channel lead text output tests ──────────────────────────────────

#[test]
fn test_process_lead_output_channel_lead_text_posted_to_channel() {
    let mut events = HashMap::new();
    events.insert(
        "web".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello from web channel lead"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let mut channel_leads = HashMap::new();
    channel_leads.insert("web".to_string(), "some-session-id".to_string());

    let effects = process_lead_output(&events, &channel_leads, "lead", &HashMap::new());
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            sender,
            message,
            channel,
            auto_output,
            ..
        } => {
            assert_eq!(sender, "web");
            assert_eq!(message, "Hello from web channel lead");
            assert_eq!(channel.as_deref(), Some("web"));
            assert!(auto_output, "stream output should be auto_output");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_lead_output_channel_lead_empty_text_not_posted() {
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
    let mut channel_leads = HashMap::new();
    channel_leads.insert("web".to_string(), "some-session-id".to_string());

    let effects = process_lead_output(&events, &channel_leads, "lead", &HashMap::new());
    assert!(
        effects.is_empty(),
        "Should not post empty text for channel lead"
    );
}

#[test]
fn test_process_lead_output_main_and_channel_lead_both_post() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Main lead message"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    events.insert(
        "features".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Features lead message"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let mut channel_leads = HashMap::new();
    channel_leads.insert("features".to_string(), "cl-session-id".to_string());

    let effects = process_lead_output(&events, &channel_leads, "lead", &HashMap::new());
    assert_eq!(effects.len(), 2);

    let main_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::PostToChannel { sender, .. } if sender == "lead"));
    assert!(main_effect.is_some());
    if let Some(Effect::PostToChannel { channel, .. }) = main_effect {
        assert!(channel.is_none(), "Main lead posts to main channel");
    }

    let cl_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::PostToChannel { sender, .. } if sender == "features"));
    assert!(cl_effect.is_some());
    if let Some(Effect::PostToChannel { channel, .. }) = cl_effect {
        assert_eq!(channel.as_deref(), Some("features"));
    }
}

#[test]
fn test_process_lead_output_coworker_not_treated_as_channel_lead() {
    // A session named "park" is a coworker, not a channel lead — its text should NOT be posted.
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Coworker message"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    // No channel leads registered
    let effects = process_lead_output(&events, &HashMap::new(), "lead", &HashMap::new());
    assert!(
        effects.is_empty(),
        "Coworker text should not be posted to channel"
    );
}

#[test]
fn test_process_lead_output_channel_lead_no_events_in_drain() {
    // A channel lead is registered but produced no events in this drain cycle.
    let events = HashMap::new(); // No events for any session
    let mut channel_leads = HashMap::new();
    channel_leads.insert("web".to_string(), "some-session-id".to_string());

    let effects = process_lead_output(&events, &channel_leads, "lead", &HashMap::new());
    assert!(
        effects.is_empty(),
        "Should not post when channel lead has no events in current drain"
    );
}

#[test]
fn test_process_lead_output_forked_session_is_inherited_to_channel() {
    let mut events = HashMap::new();
    events.insert(
        "fork-1234".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Reply from fork"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let mut fork_bound_channels = HashMap::new();
    fork_bound_channels.insert("fork-1234".to_string(), "topic-omega".to_string());

    let effects = process_lead_output(&events, &HashMap::new(), "lead", &fork_bound_channels);
    let fork_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::PostToChannel { sender, .. } if sender == "fork-1234"));
    assert!(fork_effect.is_some());
    if let Some(Effect::PostToChannel {
        sender,
        message,
        channel,
        auto_output,
        ..
    }) = fork_effect
    {
        assert_eq!(sender, "fork-1234");
        assert_eq!(message, "Reply from fork");
        assert_eq!(channel.as_deref(), Some("topic-omega"));
        assert!(auto_output, "stream output should be auto_output");
    }
}

// ── process_universal_events tests ───────────────────────────────────

#[test]
fn test_process_universal_events_no_events() {
    let events = HashMap::new();
    let channel_leads = HashMap::new();
    let effects = process_universal_events(
        &events,
        &channel_leads,
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
    );
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
    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
    );
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
    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
    );
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            thread_parent_id,
            items,
        } => {
            assert_eq!(agent_name, "lead");
            assert!(channel.is_none(), "Main lead should have no channel");
            assert!(
                thread_parent_id.is_none(),
                "Main lead should have no thread_parent_id"
            );
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_coworker_tool_use_broadcast_to_dm() {
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
    let coworker_names = HashSet::from(["lexington".to_string()]);
    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &coworker_names,
    );
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            thread_parent_id,
            items,
        } => {
            assert_eq!(agent_name, "lexington");
            assert_eq!(
                channel.as_deref(),
                Some("dm-lexington"),
                "Coworker tool calls should be scoped to dm-<name>"
            );
            assert!(
                thread_parent_id.is_none(),
                "Coworker should have no thread_parent_id"
            );
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_lead_and_coworker_both_produce_effects() {
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
    let coworker_names = HashSet::from(["park".to_string()]);
    // Both lead and coworker produce effects — lead in main channel, coworker in DM.
    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &coworker_names,
    );
    assert_eq!(effects.len(), 2);

    let lead_effect = effects.iter().find(|e| {
        matches!(e,
            Effect::BroadcastUniversalItems { agent_name, .. } if agent_name == "lead"
        )
    });
    assert!(lead_effect.is_some());
    if let Some(Effect::BroadcastUniversalItems { channel, .. }) = lead_effect {
        assert!(channel.is_none(), "Lead should be in main channel");
    }

    let cw_effect = effects.iter().find(|e| {
        matches!(e,
            Effect::BroadcastUniversalItems { agent_name, .. } if agent_name == "park"
        )
    });
    assert!(cw_effect.is_some());
    if let Some(Effect::BroadcastUniversalItems { channel, .. }) = cw_effect {
        assert_eq!(channel.as_deref(), Some("dm-park"));
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

    let effects = process_universal_events(
        &events,
        &channel_leads,
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
    );
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            thread_parent_id,
            items,
        } => {
            assert_eq!(agent_name, "web");
            assert_eq!(
                channel.as_deref(),
                Some("web"),
                "Channel lead should be tagged with its channel"
            );
            assert!(
                thread_parent_id.is_none(),
                "Channel lead should have no thread_parent_id"
            );
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_unknown_session_not_broadcast() {
    // A session in events but NOT in any named set (lead, channel leads, forks, coworkers)
    // produces no broadcast effect.
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
    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
    );
    assert!(
        effects.is_empty(),
        "Sessions not in any named set should not produce broadcast effects"
    );
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

    let effects = process_universal_events(
        &events,
        &channel_leads,
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
    );
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

    let effects = process_universal_events(
        &events,
        &channel_leads,
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &HashSet::new(),
    );
    assert!(
        effects.is_empty(),
        "No effect when channel lead has no events this tick"
    );
}

#[test]
fn test_process_universal_events_forked_session_tool_use_is_scoped_to_channel() {
    let mut events = HashMap::new();
    events.insert(
        "fork-1234".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let mut fork_bound_channels = HashMap::new();
    fork_bound_channels.insert("fork-1234".to_string(), "topic-omega".to_string());

    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &fork_bound_channels,
        &HashMap::new(),
        &HashSet::new(),
    );
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            thread_parent_id,
            items,
        } => {
            assert_eq!(agent_name, "fork-1234");
            assert_eq!(channel.as_deref(), Some("topic-omega"));
            assert!(
                thread_parent_id.is_none(),
                "Fork without thread binding should have no thread_parent_id"
            );
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_forked_session_with_thread_binding_includes_thread_parent_id() {
    let mut events = HashMap::new();
    events.insert(
        "fork-abcd".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let mut fork_bound_channels = HashMap::new();
    fork_bound_channels.insert("fork-abcd".to_string(), "web".to_string());
    let mut fork_bound_threads = HashMap::new();
    fork_bound_threads.insert("fork-abcd".to_string(), "msg-9999".to_string());

    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &fork_bound_channels,
        &fork_bound_threads,
        &HashSet::new(),
    );
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            thread_parent_id,
            items,
        } => {
            assert_eq!(agent_name, "fork-abcd");
            assert_eq!(channel.as_deref(), Some("web"));
            assert_eq!(
                thread_parent_id.as_deref(),
                Some("msg-9999"),
                "Fork with thread binding should include thread_parent_id"
            );
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

#[test]
fn test_process_universal_events_multiple_coworkers_produce_separate_dm_effects() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Edit", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    events.insert(
        "madison".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_2", "name": "Bash", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string(), "madison".to_string()]);

    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &coworker_names,
    );
    assert_eq!(effects.len(), 2);

    let park_effect = effects.iter().find(|e| {
        matches!(e,
            Effect::BroadcastUniversalItems { agent_name, .. } if agent_name == "park"
        )
    });
    assert!(park_effect.is_some());
    if let Some(Effect::BroadcastUniversalItems { channel, .. }) = park_effect {
        assert_eq!(channel.as_deref(), Some("dm-park"));
    }

    let madison_effect = effects.iter().find(|e| {
        matches!(e,
            Effect::BroadcastUniversalItems { agent_name, .. } if agent_name == "madison"
        )
    });
    assert!(madison_effect.is_some());
    if let Some(Effect::BroadcastUniversalItems { channel, .. }) = madison_effect {
        assert_eq!(channel.as_deref(), Some("dm-madison"));
    }
}

#[test]
fn test_process_universal_events_coworker_text_only_no_tool_effect() {
    // Coworker with only text content (no tool_use) should not produce a universal items effect.
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Working on it"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);
    let effects = process_universal_events(
        &events,
        &HashMap::new(),
        "lead",
        &HashMap::new(),
        &HashMap::new(),
        &coworker_names,
    );
    assert!(
        effects.is_empty(),
        "Text-only coworker events should not produce universal items"
    );
}

// ── process_agent_output tests ────────────────────────────────────

#[test]
fn test_process_agent_output_no_events() {
    let events = HashMap::new();
    let coworker_names = HashSet::from(["park".to_string()]);
    let effects = process_agent_output(&events, &coworker_names);
    assert!(effects.is_empty());
}

#[test]
fn test_process_agent_output_posts_to_dm_channel() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Working on auth endpoint"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            sender,
            message,
            channel,
            auto_output,
            ..
        } => {
            assert_eq!(sender, "park");
            assert_eq!(message, "Working on auth endpoint");
            assert_eq!(channel.as_deref(), Some("dm-park"));
            assert!(!auto_output, "DM channel output should not be auto_output");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_agent_output_multiple_coworkers() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Park's message"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    events.insert(
        "madison".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Madison's message"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string(), "madison".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert_eq!(effects.len(), 2);

    let park_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::PostToChannel { sender, .. } if sender == "park"));
    assert!(park_effect.is_some());
    if let Some(Effect::PostToChannel { channel, .. }) = park_effect {
        assert_eq!(channel.as_deref(), Some("dm-park"));
    }

    let madison_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::PostToChannel { sender, .. } if sender == "madison"));
    assert!(madison_effect.is_some());
    if let Some(Effect::PostToChannel { channel, .. }) = madison_effect {
        assert_eq!(channel.as_deref(), Some("dm-madison"));
    }
}

#[test]
fn test_process_agent_output_empty_text_not_posted() {
    // When events contain only tool_use blocks (no text), only the tool call
    // message is posted — no separate text message.
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {"file_path": "src/main.rs"}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    // Should have exactly 1 effect — the tool_data message (no text effect).
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            message, tool_data, ..
        } => {
            assert!(
                message.is_empty(),
                "DM tool message should have empty content"
            );
            let blocks = tool_data.as_ref().expect("should have tool_data");
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].tool_name, "Read");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_agent_output_no_content_produces_no_effects() {
    // Truly empty events (no text, no tool calls) should produce no effects.
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({"content": []}),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert!(effects.is_empty(), "Should not post if no content at all");
}

#[test]
fn test_process_agent_output_whitespace_only_not_posted() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "\n\n  \n"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert!(
        effects.is_empty(),
        "Should not post whitespace-only messages"
    );
}

#[test]
fn test_process_agent_output_ignores_non_coworker_events() {
    let mut events = HashMap::new();
    events.insert(
        "lead".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Lead message"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Coworker message"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    // Only "park" is a coworker — "lead" events should be ignored.
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { sender, .. } => {
            assert_eq!(sender, "park");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_agent_output_channel_lead_gets_dm() {
    // A channel lead named "auth" should get output posted to "dm-auth"
    let mut events = HashMap::new();
    events.insert(
        "auth".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Channel lead checking auth module"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let agent_names = HashSet::from(["auth".to_string()]);
    let effects = process_agent_output(&events, &agent_names);

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            channel, sender, ..
        } => {
            assert_eq!(channel.as_deref(), Some("dm-auth"));
            assert_eq!(sender, "auth");
        }
        other => panic!("Expected PostToChannel, got {:?}", other),
    }
}

#[test]
fn test_process_agent_output_trims_text() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "\n\nHello from park\n\n"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { message, .. } => {
            assert_eq!(message, "Hello from park");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_dm_tool_text_and_tools_produce_separate_effects() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [
                    {"type": "text", "text": "Working on it"},
                    {"type": "tool_use", "id": "tc_1", "name": "Bash", "input": {"command": "cargo test"}}
                ]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    // Should produce 2 effects: text message + tool_data-only message.
    assert_eq!(
        effects.len(),
        2,
        "text and tools should be separate effects"
    );
    match &effects[0] {
        Effect::PostToChannel {
            message,
            auto_output,
            ..
        } => {
            assert_eq!(message, "Working on it");
            assert!(!auto_output, "DM messages should not be auto_output");
        }
        _ => panic!("Expected text PostToChannel"),
    }
    match &effects[1] {
        Effect::PostToChannel {
            message,
            tool_data,
            auto_output,
            ..
        } => {
            assert!(
                message.is_empty(),
                "DM tool message should have empty content"
            );
            let blocks = tool_data.as_ref().expect("should have tool_data");
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].tool_name, "Bash");
            assert!(!auto_output, "DM messages should not be auto_output");
        }
        _ => panic!("Expected tool PostToChannel"),
    }
}

// ── detect_provider tests ────────────────────────────────────────────

#[test]
fn test_detect_provider_claude() {
    let events = vec![StreamEvent::Assistant {
        message: json!({"content": [{"type": "text", "text": "hello"}]}),
        session_id: None,
        extra: json!(null),
    }];
    assert_eq!(detect_provider(&events), Some("claude".to_string()));
}

#[test]
fn test_detect_provider_codex() {
    let events = vec![StreamEvent::Assistant {
        message: json!({"content": [{"type": "text", "text": "hello"}]}),
        session_id: None,
        extra: json!({"provider": "codex"}),
    }];
    assert_eq!(detect_provider(&events), Some("codex".to_string()));
}

#[test]
fn test_detect_provider_no_assistant_events() {
    let events = vec![StreamEvent::User {
        message: json!({"content": "user input"}),
        extra: json!({}),
    }];
    assert_eq!(detect_provider(&events), None);
}

// ── extract_tool_blocks tests ────────────────────────────────────────

#[test]
fn test_extract_tool_blocks_bash_with_result() {
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "tc_1",
                    "name": "Bash",
                    "input": {"command": "cargo test"}
                }]
            }),
            session_id: None,
            extra: json!(null),
        },
        StreamEvent::User {
            message: json!({
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tc_1",
                    "content": "test result: ok"
                }]
            }),
            extra: json!(null),
        },
    ];

    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].tool_name, "Bash");
    assert_eq!(
        blocks[0].input.get("command").and_then(|v| v.as_str()),
        Some("cargo test")
    );
    assert!(blocks[0].output.is_some());
    assert!(!blocks[0].error);
}

#[test]
fn test_extract_tool_blocks_error_result() {
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "tc_1",
                    "name": "Read",
                    "input": {"file_path": "missing.rs"}
                }]
            }),
            session_id: None,
            extra: json!(null),
        },
        StreamEvent::User {
            message: json!({
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "tc_1",
                    "is_error": true,
                    "content": "file not found"
                }]
            }),
            extra: json!(null),
        },
    ];

    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].tool_name, "Read");
    assert!(blocks[0].error);
    assert_eq!(
        blocks[0].output.as_ref().and_then(|v| v.as_str()),
        Some("file not found")
    );
}

#[test]
fn test_extract_tool_blocks_no_result() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{
                "type": "tool_use",
                "id": "tc_1",
                "name": "Edit",
                "input": {"file_path": "src/main.rs", "old_string": "a", "new_string": "b"}
            }]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].tool_name, "Edit");
    assert!(blocks[0].output.is_none());
    assert!(!blocks[0].error);
}

#[test]
fn test_extract_tool_blocks_multiple_calls() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [
                {"type": "tool_use", "id": "tc_1", "name": "Read", "input": {"file_path": "a.rs"}},
                {"type": "tool_use", "id": "tc_2", "name": "Bash", "input": {"command": "ls"}}
            ]
        }),
        session_id: None,
        extra: json!(null),
    }];

    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].tool_name, "Read");
    assert_eq!(blocks[1].tool_name, "Bash");
}

// ── process_agent_output tool_data tests ──────────────────────────

#[test]
fn test_process_agent_output_tool_data_populated() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [
                    {"type": "tool_use", "id": "tc_1", "name": "Bash", "input": {"command": "cargo test"}}
                ]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            tool_data,
            provider,
            ..
        } => {
            assert!(tool_data.is_some(), "tool_data should be populated");
            let blocks = tool_data.as_ref().unwrap();
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].tool_name, "Bash");
            assert_eq!(provider.as_deref(), Some("claude"));
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_agent_output_text_has_no_tool_data() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello"}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            tool_data,
            provider,
            ..
        } => {
            assert!(
                tool_data.is_none(),
                "text messages should not have tool_data"
            );
            assert_eq!(provider.as_deref(), Some("claude"));
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_agent_output_codex_provider() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello"}]
            }),
            session_id: None,
            extra: json!({"provider": "codex", "event": "item/completed"}),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { provider, .. } => {
            assert_eq!(provider.as_deref(), Some("codex"));
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

// ── ToolBlock serialization tests ─────────────────────────────────────

#[test]
fn test_tool_block_serialization() {
    let block = crate::message::ToolBlock {
        tool_name: "Edit".to_string(),
        input: json!({"file_path": "src/main.rs", "old_string": "a", "new_string": "b"}),
        output: None,
        error: false,
        call_id: None,
        parent_tool_use_id: None,
    };
    let json = serde_json::to_string(&block).unwrap();
    assert!(json.contains("\"tool_name\":\"Edit\""));
    assert!(
        !json.contains("\"output\""),
        "None output should be skipped"
    );
    assert!(json.contains("\"error\":false"));

    let parsed: crate::message::ToolBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.tool_name, "Edit");
    assert!(parsed.output.is_none());
}

#[test]
fn test_tool_block_with_output_serialization() {
    let block = crate::message::ToolBlock {
        tool_name: "Bash".to_string(),
        input: json!({"command": "echo hi"}),
        output: Some(json!("hi\n")),
        error: false,
        call_id: None,
        parent_tool_use_id: None,
    };
    let json = serde_json::to_string(&block).unwrap();
    let parsed: crate::message::ToolBlock = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.tool_name, "Bash");
    assert_eq!(parsed.output, Some(json!("hi\n")));
}

// ── extract_insights tests ──────────────────────────────────────────

#[test]
fn test_extract_insights_single() {
    let text = r#"Some text before

`★ Insight ─────────────────────────────────────`
This is an insight about something important.
It can span multiple lines.
`─────────────────────────────────────────────────`

Some text after"#;

    let insights = extract_insights(text);
    assert_eq!(insights.len(), 1);
    assert!(insights[0].contains("This is an insight"));
}

#[test]
fn test_extract_insights_multiple() {
    let text = r#"
`★ Insight ─────────────────────────────────────`
First insight
`─────────────────────────────────────────────────`

Some middle text

`★ Insight ─────────────────────────────────────`
Second insight
`─────────────────────────────────────────────────`
"#;

    let insights = extract_insights(text);
    assert_eq!(insights.len(), 2);
    assert!(insights[0].contains("First"));
    assert!(insights[1].contains("Second"));
}

#[test]
fn test_extract_insights_none() {
    let text = "Just some regular text without any insights.";
    let insights = extract_insights(text);
    assert!(insights.is_empty());
}

#[test]
fn test_extract_insights_no_backticks() {
    let text = "★ Insight ─────────────────────────────────────\nBare insight without backticks\n─────────────────────────────────────────────────";
    let insights = extract_insights(text);
    assert_eq!(insights.len(), 1);
    assert!(insights[0].contains("Bare insight"));
}

// ── process_agent_output insight extraction tests ────────────────

#[test]
fn test_process_agent_output_extracts_insights() {
    let text_with_insight = "Working on the feature.\n\n`★ Insight ─────────────────────────────────────`\nThe auth module uses JWT tokens.\n`─────────────────────────────────────────────────`\n\nDone.";
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": text_with_insight}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);

    // Should have a PostInsight effect and a PostToChannel effect
    let insight_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::PostInsight { .. }))
        .collect();
    assert_eq!(insight_effects.len(), 1);
    if let Effect::PostInsight { agent, insight } = insight_effects[0] {
        assert_eq!(agent, "park");
        assert!(insight.contains("JWT tokens"));
    }

    let channel_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::PostToChannel { .. }))
        .collect();
    assert_eq!(channel_effects.len(), 1);
}

#[test]
fn test_process_agent_output_no_insight_in_regular_text() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Just regular text, no insights."}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);

    let insight_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::PostInsight { .. }))
        .collect();
    assert!(insight_effects.is_empty());
}

// ── Sub-agent threading tests ───────────────────────────────────────

#[test]
fn test_extract_tool_blocks_includes_call_id() {
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_abc123",
                    "name": "Bash",
                    "input": {"command": "ls"}
                }]
            }),
            session_id: None,
            extra: json!(null),
        },
        StreamEvent::User {
            message: json!({
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_abc123",
                    "content": "file1\nfile2"
                }]
            }),
            extra: json!(null),
        },
    ];
    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].tool_name, "Bash");
    assert_eq!(blocks[0].call_id, Some("toolu_abc123".to_string()));
    assert!(blocks[0].parent_tool_use_id.is_none());
}

#[test]
fn test_extract_tool_blocks_captures_parent_tool_use_id() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_child",
                "name": "Read",
                "input": {"file_path": "src/main.rs"}
            }]
        }),
        session_id: None,
        extra: json!({"parentToolUseID": "toolu_parent_agent"}),
    }];
    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].call_id, Some("toolu_child".to_string()));
    assert_eq!(
        blocks[0].parent_tool_use_id,
        Some("toolu_parent_agent".to_string())
    );
}

#[test]
fn test_process_agent_output_splits_subagent_tool_blocks() {
    // Top-level tool_use + sub-agent tool_use (with parentToolUseID)
    let events_vec = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_parent",
                    "name": "Agent",
                    "input": {"prompt": "investigate"}
                }]
            }),
            session_id: None,
            extra: json!(null),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_child",
                    "name": "Bash",
                    "input": {"command": "ls"}
                }]
            }),
            session_id: None,
            extra: json!({"parentToolUseID": "toolu_parent"}),
        },
    ];
    let mut events = HashMap::new();
    events.insert("park".to_string(), events_vec);
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);

    // Should have separate effects: top-level tool block + sub-agent tool block
    let channel_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::PostToChannel { .. }))
        .collect();
    assert_eq!(channel_effects.len(), 2);

    // First effect: top-level tool block with tool_use_id set
    if let Effect::PostToChannel {
        tool_data,
        tool_use_id,
        parent_tool_use_id,
        ..
    } = &channel_effects[0]
    {
        assert_eq!(*tool_use_id, Some("toolu_parent".to_string()));
        assert!(parent_tool_use_id.is_none());
        let blocks = tool_data.as_ref().unwrap();
        assert_eq!(blocks[0].tool_name, "Agent");
    } else {
        panic!("Expected PostToChannel");
    }

    // Second effect: sub-agent tool block with parent_tool_use_id set
    if let Effect::PostToChannel {
        tool_data,
        parent_tool_use_id,
        ..
    } = &channel_effects[1]
    {
        assert_eq!(*parent_tool_use_id, Some("toolu_parent".to_string()));
        let blocks = tool_data.as_ref().unwrap();
        assert_eq!(blocks[0].tool_name, "Bash");
    } else {
        panic!("Expected PostToChannel");
    }
}

#[test]
fn test_process_agent_output_splits_subagent_text() {
    let events_vec = vec![
        // Top-level text
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Starting agent..."}]
            }),
            session_id: None,
            extra: json!(null),
        },
        // Sub-agent text (with parentToolUseID)
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Reading files..."}]
            }),
            session_id: None,
            extra: json!({"parentToolUseID": "toolu_parent"}),
        },
    ];
    let mut events = HashMap::new();
    events.insert("park".to_string(), events_vec);
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);

    let channel_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::PostToChannel { .. }))
        .collect();
    assert_eq!(channel_effects.len(), 2);

    // Top-level text
    if let Effect::PostToChannel {
        message,
        parent_tool_use_id,
        ..
    } = &channel_effects[0]
    {
        assert_eq!(message, "Starting agent...");
        assert!(parent_tool_use_id.is_none());
    } else {
        panic!("Expected PostToChannel");
    }

    // Sub-agent text as thread reply
    if let Effect::PostToChannel {
        message,
        parent_tool_use_id,
        ..
    } = &channel_effects[1]
    {
        assert_eq!(message, "Reading files...");
        assert_eq!(*parent_tool_use_id, Some("toolu_parent".to_string()));
    } else {
        panic!("Expected PostToChannel");
    }
}

#[test]
fn test_process_agent_output_no_subagent_when_no_parent_id() {
    // All events are top-level (no parentToolUseID)
    let events_vec = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "Bash",
                    "input": {"command": "ls"}
                }]
            }),
            session_id: None,
            extra: json!(null),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_2",
                    "name": "Read",
                    "input": {"file_path": "foo.rs"}
                }]
            }),
            session_id: None,
            extra: json!(null),
        },
    ];
    let mut events = HashMap::new();
    events.insert("park".to_string(), events_vec);
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_agent_output(&events, &coworker_names);

    let channel_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::PostToChannel { .. }))
        .collect();
    // All blocks should be in a single top-level effect
    assert_eq!(channel_effects.len(), 1);
    if let Effect::PostToChannel {
        tool_data,
        tool_use_id,
        parent_tool_use_id,
        ..
    } = &channel_effects[0]
    {
        assert_eq!(*tool_use_id, Some("toolu_1".to_string()));
        assert!(parent_tool_use_id.is_none());
        assert_eq!(tool_data.as_ref().unwrap().len(), 2);
    } else {
        panic!("Expected PostToChannel");
    }
}

#[test]
fn test_get_parent_tool_use_id_extracts_from_extra() {
    let extra = json!({"parentToolUseID": "toolu_abc"});
    assert_eq!(
        get_parent_tool_use_id(&extra),
        Some("toolu_abc".to_string())
    );
}

#[test]
fn test_get_parent_tool_use_id_returns_none_when_absent() {
    assert!(get_parent_tool_use_id(&json!(null)).is_none());
    assert!(get_parent_tool_use_id(&json!({})).is_none());
    assert!(get_parent_tool_use_id(&json!({"provider": "claude"})).is_none());
}

// ── sub-agent extraction tests (parent_tool_use_id on regular events) ──

/// Sub-agent events appear as regular assistant/user events with
/// `parent_tool_use_id` in the `extra` field (captured via serde flatten).
/// This is the format used by Claude Code 2.1.70+.
#[test]
fn test_extract_tool_blocks_with_parent_tool_use_id_on_events() {
    let events = vec![
        // Top-level Agent tool_use
        StreamEvent::Assistant {
            message: json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent", "input": {"prompt": "explore"}}]
            }),
            session_id: Some("sess1".into()),
            extra: json!({"parent_tool_use_id": null}),
        },
        // Sub-agent assistant: Bash tool_use (parent_tool_use_id set)
        StreamEvent::Assistant {
            message: json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "toolu_bash", "name": "Bash", "input": {"command": "ls"}}]
            }),
            session_id: Some("sess1".into()),
            extra: json!({"parent_tool_use_id": "toolu_agent"}),
        },
        // Sub-agent user: tool_result for Bash
        StreamEvent::User {
            message: json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_bash", "content": "README.md\nsrc/"}]
            }),
            extra: json!({"parent_tool_use_id": "toolu_agent"}),
        },
    ];

    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 2, "should have Agent + Bash blocks");

    assert_eq!(blocks[0].tool_name, "Agent");
    assert!(blocks[0].parent_tool_use_id.is_none());

    assert_eq!(blocks[1].tool_name, "Bash");
    assert_eq!(blocks[1].call_id.as_deref(), Some("toolu_bash"));
    assert_eq!(
        blocks[1].parent_tool_use_id.as_deref(),
        Some("toolu_agent"),
        "sub-agent block should reference parent via parent_tool_use_id"
    );
    assert!(
        blocks[1].output.is_some(),
        "should have matched tool_result"
    );
}

#[test]
fn test_extract_assistant_text_split_with_parent_tool_use_id() {
    let events = vec![
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Top-level response."}]
            }),
            session_id: None,
            extra: json!({"parent_tool_use_id": null}),
        },
        StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Sub-agent found the files."}]
            }),
            session_id: None,
            extra: json!({"parent_tool_use_id": "toolu_agent"}),
        },
    ];

    let (top, sub) = extract_assistant_text_split(&events);
    assert_eq!(top, "Top-level response.");
    assert_eq!(
        sub.get("toolu_agent").map(|s| s.as_str()),
        Some("Sub-agent found the files."),
    );
}

// ── progress event extraction tests (legacy format) ──────────────────
#[test]
fn test_extract_tool_blocks_from_progress_events() {
    // Progress events with data.type == "agent_progress" carry sub-agent tool_use blocks.
    // extract_tool_blocks should extract these with parent_tool_use_id set.
    let events = vec![
        // Top-level Agent tool_use
        StreamEvent::Assistant {
            message: json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "toolu_agent", "name": "Agent", "input": {"prompt": "list files"}}]
            }),
            session_id: Some("sess1".into()),
            extra: json!({}),
        },
        // Sub-agent progress: assistant with Bash tool_use
        StreamEvent::Progress {
            data: json!({
                "type": "agent_progress",
                "message": {
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "toolu_bash", "name": "Bash", "input": {"command": "ls"}}]
                    }
                }
            }),
            parent_tool_use_id: Some("toolu_agent".into()),
            tool_use_id: Some("toolu_bash".into()),
            extra: json!({}),
        },
        // Sub-agent progress: user with tool_result for Bash
        StreamEvent::Progress {
            data: json!({
                "type": "agent_progress",
                "message": {
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [{"type": "tool_result", "tool_use_id": "toolu_bash", "content": "file1.txt\nfile2.txt"}]
                    }
                }
            }),
            parent_tool_use_id: Some("toolu_agent".into()),
            tool_use_id: Some("toolu_bash".into()),
            extra: json!({}),
        },
    ];

    let blocks = extract_tool_blocks(&events);
    assert_eq!(blocks.len(), 2, "should have Agent + Bash blocks");

    // Top-level Agent block (no parent)
    assert_eq!(blocks[0].tool_name, "Agent");
    assert_eq!(blocks[0].call_id.as_deref(), Some("toolu_agent"));
    assert!(blocks[0].parent_tool_use_id.is_none());

    // Sub-agent Bash block (parent = Agent)
    assert_eq!(blocks[1].tool_name, "Bash");
    assert_eq!(blocks[1].call_id.as_deref(), Some("toolu_bash"));
    assert_eq!(
        blocks[1].parent_tool_use_id.as_deref(),
        Some("toolu_agent"),
        "sub-agent block should reference parent Agent tool_use"
    );
    // Tool result should be matched
    assert!(
        blocks[1].output.is_some(),
        "sub-agent block should have output from tool_result"
    );
}

#[test]
fn test_extract_assistant_text_split_from_progress_events() {
    // Sub-agent text from progress events should be captured in sub_agent_texts.
    let events = vec![
        // Top-level text
        StreamEvent::Assistant {
            message: json!({
                "role": "assistant",
                "content": [{"type": "text", "text": "I'll use an agent to check."}]
            }),
            session_id: Some("sess1".into()),
            extra: json!({}),
        },
        // Sub-agent text from progress event
        StreamEvent::Progress {
            data: json!({
                "type": "agent_progress",
                "message": {
                    "type": "assistant",
                    "message": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": "Found 3 files in the directory."}]
                    }
                }
            }),
            parent_tool_use_id: Some("toolu_agent".into()),
            tool_use_id: Some("agent_msg_1".into()),
            extra: json!({}),
        },
    ];

    let (top_text, sub_texts) = extract_assistant_text_split(&events);
    assert_eq!(top_text, "I'll use an agent to check.");
    assert_eq!(
        sub_texts.get("toolu_agent").map(|s| s.as_str()),
        Some("Found 3 files in the directory."),
        "sub-agent text should be grouped under parent tool_use_id"
    );
}
