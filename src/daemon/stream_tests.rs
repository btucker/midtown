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

#[test]
fn test_extract_lead_text_codex_completed_supersedes_deltas() {
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

    assert_eq!(extract_lead_text(&events), "smoke-ack");
}

#[test]
fn test_extract_lead_text_codex_ignores_delta_after_completed() {
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

    assert_eq!(extract_lead_text(&events), "Done");
}

#[test]
fn test_extract_lead_text_codex_delta_only_not_emitted() {
    let events = vec![StreamEvent::Assistant {
        message: json!({
            "content": [{"type": "text", "text": "delta-only"}]
        }),
        session_id: None,
        extra: json!({"provider": "codex"}),
    }];

    assert_eq!(extract_lead_text(&events), "");
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
        } => {
            assert_eq!(sender, "myproject");
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
        } => {
            assert_eq!(sender, "web");
            assert_eq!(message, "Hello from web channel lead");
            assert_eq!(channel.as_deref(), Some("web"));
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
    }) = fork_effect
    {
        assert_eq!(sender, "fork-1234");
        assert_eq!(message, "Reply from fork");
        assert_eq!(channel.as_deref(), Some("topic-omega"));
    }
}

// ── process_universal_events tests ───────────────────────────────────

#[test]
fn test_process_universal_events_no_events() {
    let events = HashMap::new();
    let channel_leads = HashMap::new();
    let effects = process_universal_events(&events, &channel_leads, "lead", &HashMap::new());
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
    let effects = process_universal_events(&events, &HashMap::new(), "lead", &HashMap::new());
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
    let effects = process_universal_events(&events, &HashMap::new(), "lead", &HashMap::new());
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
    let effects = process_universal_events(&events, &HashMap::new(), "lead", &HashMap::new());
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
    let effects = process_universal_events(&events, &HashMap::new(), "lead", &HashMap::new());
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

    let effects = process_universal_events(&events, &channel_leads, "lead", &HashMap::new());
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
    let effects = process_universal_events(&events, &HashMap::new(), "lead", &HashMap::new());
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

    let effects = process_universal_events(&events, &channel_leads, "lead", &HashMap::new());
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

    let effects = process_universal_events(&events, &channel_leads, "lead", &HashMap::new());
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

    let effects = process_universal_events(&events, &HashMap::new(), "lead", &fork_bound_channels);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::BroadcastUniversalItems {
            agent_name,
            channel,
            items,
        } => {
            assert_eq!(agent_name, "fork-1234");
            assert_eq!(channel.as_deref(), Some("topic-omega"));
            assert_eq!(items.len(), 1);
        }
        _ => panic!("Expected BroadcastUniversalItems effect"),
    }
}

// ── process_coworker_output tests ────────────────────────────────────

#[test]
fn test_process_coworker_output_no_events() {
    let events = HashMap::new();
    let coworker_names = HashSet::from(["park".to_string()]);
    let effects = process_coworker_output(&events, &coworker_names);
    assert!(effects.is_empty());
}

#[test]
fn test_process_coworker_output_posts_to_dm_channel() {
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

    let effects = process_coworker_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel {
            sender,
            message,
            channel,
        } => {
            assert_eq!(sender, "park");
            assert_eq!(message, "Working on auth endpoint");
            assert_eq!(channel.as_deref(), Some("dm-park"));
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_coworker_output_multiple_coworkers() {
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

    let effects = process_coworker_output(&events, &coworker_names);
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
fn test_process_coworker_output_empty_text_not_posted() {
    let mut events = HashMap::new();
    events.insert(
        "park".to_string(),
        vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "tc_1", "name": "Read", "input": {}}]
            }),
            session_id: None,
            extra: json!(null),
        }],
    );
    let coworker_names = HashSet::from(["park".to_string()]);

    let effects = process_coworker_output(&events, &coworker_names);
    assert!(
        effects.is_empty(),
        "Should not post if no text content found"
    );
}

#[test]
fn test_process_coworker_output_whitespace_only_not_posted() {
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

    let effects = process_coworker_output(&events, &coworker_names);
    assert!(
        effects.is_empty(),
        "Should not post whitespace-only messages"
    );
}

#[test]
fn test_process_coworker_output_ignores_non_coworker_events() {
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

    let effects = process_coworker_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { sender, .. } => {
            assert_eq!(sender, "park");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}

#[test]
fn test_process_coworker_output_trims_text() {
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

    let effects = process_coworker_output(&events, &coworker_names);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::PostToChannel { message, .. } => {
            assert_eq!(message, "Hello from park");
        }
        _ => panic!("Expected PostToChannel effect"),
    }
}
