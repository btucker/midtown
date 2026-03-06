use super::*;

/// Helper to create a minimal HeadlessConfig for testing.
fn test_config() -> HeadlessConfig {
    HeadlessConfig {
        model: "haiku".to_string(),
        system_prompt: "You are a test assistant.".to_string(),
        json_schema: None,
        cwd: None,
        project_name: Some("midtown".to_string()),
        max_budget_usd: None,
        allow_tools: false,
        persist_session: false,
        resume_session_id: None,
        session_id: None,
        inactivity_timeout: None,
        team_name: None,
        agent_id: None,
        agent_name: None,
        settings_path: None,
        setting_sources: None,
        auth_provider: crate::auth::AuthProvider::Claude,
        env: std::collections::BTreeMap::new(),
        fork_session: false,
        disallowed_tools: vec![],
    }
}

fn test_codex_state() -> CodexProtocolState {
    CodexProtocolState {
        initialized: true,
        start_request_id: Some(42),
        thread_id: None,
        turn_in_progress: true,
        next_request_id: 100,
        pending_messages: VecDeque::new(),
        latest_agent_message: None,
        resume_thread_id: None,
        fork_session: false,
        allow_tools: true,
        model: "gpt-5-codex".to_string(),
        cwd: None,
        system_prompt: String::new(),
        output_schema: None,
        start_phase: "thread/start".to_string(),
        retried_fresh_start: false,
    }
}

fn test_claude_session() -> HeadlessSession {
    HeadlessSession {
        child: None,
        stdout_rx: None,
        stderr_rx: None,
        stdin: None,
        session_id: None,
        backend: HeadlessSessionBackend::Claude,
        protocol: SessionProtocol::Claude,
        codex_session: None,
        detach_on_drop: false,
    }
}

fn test_codex_session() -> HeadlessSession {
    HeadlessSession {
        child: None,
        stdout_rx: None,
        stderr_rx: None,
        stdin: None,
        session_id: None,
        backend: HeadlessSessionBackend::Codex,
        protocol: SessionProtocol::Codex(Box::new(test_codex_state())),
        codex_session: None,
        detach_on_drop: false,
    }
}

#[test]
fn test_headless_session_protocol_flags() {
    let claude_session = test_claude_session();
    let codex_session = test_codex_session();

    assert!(!claude_session.is_codex_session());
    assert!(codex_session.is_codex_session());

    assert!(claude_session.should_wait_for_exit_on_result());
    assert!(!codex_session.should_wait_for_exit_on_result());
}

#[tokio::test]
async fn test_codex_session_runtime_methods_require_runtime() {
    // Also serves as a regression test: block_on inside a Tokio runtime panics
    // with "Cannot start a runtime from within a runtime". Codex try_wait/pid
    // must not use block_on — running them here in #[tokio::test] confirms that.
    let mut session = test_codex_session();
    session
        .codex_state_mut()
        .expect("expected codex protocol")
        .thread_id = Some("thread-1".to_string());
    session
        .codex_state_mut()
        .expect("expected codex protocol")
        .initialized = false;

    assert!(session.next_event().await.is_none());
    assert!(session.drain_stderr().await.is_empty());
    assert!(session.kill().await.is_ok());
    assert_eq!(session.pid(), None);
    assert_eq!(
        session.wait().await.err().unwrap().to_string(),
        "missing codex runtime".to_string()
    );
    // Codex try_wait returns Ok(None) — sessions don't own a process.
    assert_eq!(session.try_wait().unwrap(), None);
    assert_eq!(
        session
            .send_message("hello")
            .await
            .err()
            .unwrap()
            .to_string(),
        "missing codex runtime".to_string()
    );

    session.close_stdin();
    assert!(session.stdin.is_none());
}

#[tokio::test]
async fn test_shutdown_codex_runtime_noop_when_not_started() {
    shutdown_codex_runtime().await;
}

#[test]
fn test_codex_launch_plan_rejects_unsupported_fields() {
    let config = HeadlessConfig {
        auth_provider: crate::auth::AuthProvider::Codex,
        max_budget_usd: Some(1.0),
        settings_path: Some("/tmp/settings.json".to_string()),
        setting_sources: Some("project,local".to_string()),
        session_id: Some("session-123".to_string()),
        disallowed_tools: vec!["Edit".to_string()],
        ..test_config()
    };

    let error = codex_launch_plan_from_config(&config).unwrap_err();
    assert!(
        error.contains("max_budget_usd")
            && error.contains("settings_path")
            && error.contains("setting_sources")
            && error.contains("session_id")
            && error.contains("disallowed_tools"),
        "Error should mention all unsupported fields, got: {}",
        error
    );
}

#[test]
fn test_codex_launch_plan_accepts_supported_fields() {
    let config = HeadlessConfig {
        auth_provider: crate::auth::AuthProvider::Codex,
        model: "gpt-5.3-codex".to_string(),
        system_prompt: "System".to_string(),
        json_schema: Some(serde_json::json!({"type":"object"})),
        cwd: Some("/tmp/project".to_string()),
        resume_session_id: Some("thread-parent".to_string()),
        fork_session: true,
        allow_tools: false,
        ..test_config()
    };

    let plan = codex_launch_plan_from_config(&config).unwrap();
    assert_eq!(plan.model, "gpt-5.3-codex");
    assert_eq!(plan.system_prompt, "System");
    assert_eq!(plan.cwd, Some("/tmp/project".to_string()));
    assert_eq!(plan.resume_thread_id, Some("thread-parent".to_string()));
    assert!(plan.fork_session);
    assert!(!plan.allow_tools);
    assert!(plan.output_schema.is_some());
}

#[test]
fn test_headless_config_all_fields_roundtrip() {
    // Set every optional field to a non-default value and verify they all survive
    // serialization. Consolidates the per-field roundtrip tests.
    let config = HeadlessConfig {
        json_schema: Some(serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } },
            "required": ["answer"]
        })),
        max_budget_usd: Some(0.10),
        persist_session: true,
        resume_session_id: Some("abc-123".to_string()),
        inactivity_timeout: Some(Duration::from_secs(300)),
        team_name: Some("midtown-myrepo".to_string()),
        agent_id: Some("park@midtown-myrepo".to_string()),
        agent_name: Some("park".to_string()),
        settings_path: Some("/tmp/settings.json".to_string()),
        setting_sources: Some("project,local".to_string()),
        auth_provider: crate::auth::AuthProvider::Codex,
        ..test_config()
    };

    let json = serde_json::to_string(&config).unwrap();
    let parsed: HeadlessConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed.model, "haiku");
    assert!(parsed.json_schema.is_some());
    assert!(parsed.persist_session);
    assert_eq!(parsed.resume_session_id, Some("abc-123".to_string()));
    assert_eq!(parsed.inactivity_timeout, Some(Duration::from_secs(300)));
    assert_eq!(parsed.team_name, Some("midtown-myrepo".to_string()));
    assert_eq!(parsed.agent_id, Some("park@midtown-myrepo".to_string()));
    assert_eq!(parsed.agent_name, Some("park".to_string()));
    assert_eq!(parsed.settings_path, Some("/tmp/settings.json".to_string()));
    assert_eq!(parsed.setting_sources, Some("project,local".to_string()));
    assert_eq!(parsed.auth_provider, crate::auth::AuthProvider::Codex);
}

#[test]
fn test_headless_config_defaults_from_minimal_json() {
    // Minimal JSON with only required fields — all optional fields should get
    // their defaults. Also covers backward compatibility for older configs.
    let json = r#"{"model":"haiku","system_prompt":"test","allow_tools":false}"#;
    let config: HeadlessConfig = serde_json::from_str(json).unwrap();
    assert!(!config.persist_session);
    assert!(config.resume_session_id.is_none());
    assert!(config.inactivity_timeout.is_none());
    assert!(config.team_name.is_none());
    assert!(config.agent_id.is_none());
    assert!(config.agent_name.is_none());
    assert!(config.settings_path.is_none());
    assert!(config.setting_sources.is_none());
    assert_eq!(config.auth_provider, crate::auth::AuthProvider::Claude);
}

#[test]
fn test_stream_event_parsing_init() {
    let json = r#"{"type":"system","subtype":"init","cwd":"/tmp","session_id":"abc-123","model":"haiku","tools":[],"mcp_servers":[]}"#;
    let event: StreamEvent = serde_json::from_str(json).unwrap();
    match event {
        StreamEvent::System {
            subtype,
            session_id,
            model,
            ..
        } => {
            assert_eq!(subtype, "init");
            assert_eq!(session_id, Some("abc-123".to_string()));
            assert_eq!(model, Some("haiku".to_string()));
        }
        _ => panic!("Expected System event"),
    }
}

#[test]
fn test_stream_event_parsing_result() {
    let json = r#"{"type":"result","subtype":"success","is_error":false,"result":"Hello!","duration_ms":1234,"total_cost_usd":0.001,"session_id":"abc-123"}"#;
    let event: StreamEvent = serde_json::from_str(json).unwrap();
    match event {
        StreamEvent::Result {
            subtype,
            is_error,
            result,
            duration_ms,
            total_cost_usd,
            ..
        } => {
            assert_eq!(subtype, "success");
            assert!(!is_error);
            assert_eq!(result, Some("Hello!".to_string()));
            assert_eq!(duration_ms, Some(1234));
            assert_eq!(total_cost_usd, Some(0.001));
        }
        _ => panic!("Expected Result event"),
    }
}

#[test]
fn test_stream_event_parsing_assistant() {
    let json = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}]},"session_id":"abc"}"#;
    let event: StreamEvent = serde_json::from_str(json).unwrap();
    match event {
        StreamEvent::Assistant { session_id, .. } => {
            assert_eq!(session_id, Some("abc".to_string()));
        }
        _ => panic!("Expected Assistant event"),
    }
}

#[test]
fn test_stream_event_parsing_assistant_with_parent_tool_use_id() {
    // Claude Code emits parentToolUseID on events inside sub-agents.
    // Verify it ends up in the `extra` field via serde(flatten).
    let json = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_child","name":"Bash","input":{"command":"ls"}}]},"session_id":"abc","parentToolUseID":"toolu_parent_agent"}"#;
    let event: StreamEvent = serde_json::from_str(json).unwrap();
    match event {
        StreamEvent::Assistant { extra, .. } => {
            let parent_id = extra.get("parentToolUseID").and_then(|v| v.as_str());
            assert_eq!(
                parent_id,
                Some("toolu_parent_agent"),
                "parentToolUseID should be captured in extra via serde(flatten). Got extra: {extra}"
            );
        }
        _ => panic!("Expected Assistant event"),
    }
}

#[test]
fn test_stream_event_parsing_progress_agent_progress() {
    // Claude Code emits progress events with data.type == "agent_progress" for sub-agent activity.
    // These carry parentToolUseID pointing to the Agent tool_use block.
    let json = r#"{"type":"progress","data":{"type":"agent_progress","message":{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_child","name":"Bash","input":{"command":"ls"}}]}},"agentId":"agent-1"},"parentToolUseID":"toolu_parent_agent","toolUseID":"toolu_child"}"#;
    let event: StreamEvent = serde_json::from_str(json).unwrap();
    match event {
        StreamEvent::Progress {
            data,
            parent_tool_use_id,
            tool_use_id,
            ..
        } => {
            assert_eq!(
                parent_tool_use_id.as_deref(),
                Some("toolu_parent_agent"),
                "parentToolUseID should be parsed"
            );
            assert_eq!(
                tool_use_id.as_deref(),
                Some("toolu_child"),
                "toolUseID should be parsed"
            );
            assert_eq!(
                data.get("type").and_then(|t| t.as_str()),
                Some("agent_progress"),
                "data.type should be agent_progress"
            );
            // Verify inner message structure
            let inner_msg = data
                .get("message")
                .and_then(|m| m.get("message"))
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
                .expect("should have content array");
            assert_eq!(inner_msg[0]["name"].as_str(), Some("Bash"));
        }
        _ => panic!("Expected Progress event, got {event:?}"),
    }
}

#[test]
fn test_stream_event_parsing_unknown_type_is_not_error() {
    // Regression: Claude CLI added `rate_limit_event` which caused 17k+ parse
    // failures because StreamEvent only recognized system/assistant/user/result.
    // Unknown event types should deserialize successfully and be skippable.
    let json = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1771941600,"rateLimitType":"seven_day","utilization":0.8},"uuid":"12767dec","session_id":"968bb2ee"}"#;
    let result = serde_json::from_str::<StreamEvent>(json);
    assert!(
        result.is_ok(),
        "Unknown event types must not fail deserialization: {result:?}"
    );
}

#[test]
fn test_headless_result_serialization() {
    let result = HeadlessResult {
        result: Some("42".to_string()),
        cost_usd: Some(0.005),
        duration_ms: Some(2000),
        is_error: false,
        session_id: Some("test-session".to_string()),
    };

    let json = serde_json::to_string(&result).unwrap();
    let parsed: HeadlessResult = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.result, Some("42".to_string()));
    assert!(!parsed.is_error);
}

#[test]
fn test_codex_translate_start_response_emits_init_and_dispatches() {
    let mut state = test_codex_state();
    let mut session_id = None;
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "result": { "thread": { "id": "thread_123" } }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    assert_eq!(session_id, Some("thread_123".to_string()));
    assert_eq!(state.thread_id, Some("thread_123".to_string()));
    assert_eq!(post_action, CodexPostAction::DispatchPendingTurns);

    match event {
        Some(StreamEvent::System {
            subtype,
            session_id,
            model,
            ..
        }) => {
            assert_eq!(subtype, "init");
            assert_eq!(session_id, Some("thread_123".to_string()));
            assert_eq!(model, Some("gpt-5-codex".to_string()));
        }
        _ => panic!("Expected codex start response to emit init system event"),
    }
}

#[test]
fn test_codex_translate_start_response_error_emits_result_error() {
    let mut state = test_codex_state();
    let mut session_id = Some("existing".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "start failed" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::Result {
            subtype,
            is_error,
            result,
            ..
        }) => {
            assert_eq!(subtype, "error");
            assert!(is_error);
            assert_eq!(result, Some("start failed".to_string()));
        }
        _ => panic!("Expected codex start error to emit result error event"),
    }
}

#[test]
fn test_codex_translate_turn_start_error_clears_in_flight_turn() {
    let mut state = test_codex_state();
    state.start_request_id = Some(42);
    state.turn_in_progress = true;
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 99,
        "error": { "message": "turn failed" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    assert_eq!(post_action, CodexPostAction::DispatchPendingTurns);
    assert!(!state.turn_in_progress);
    match event {
        Some(StreamEvent::Result {
            subtype,
            is_error,
            result,
            extra,
            ..
        }) => {
            assert_eq!(subtype, "error");
            assert!(is_error);
            assert_eq!(result, Some("turn failed".to_string()));
            assert_eq!(extra["phase"], "turn/start");
            assert_eq!(extra["request_id"], 99);
        }
        _ => panic!("Expected codex turn error to emit result error event"),
    }
}

#[test]
fn test_codex_translate_delta_then_turn_completed_uses_accumulated_text() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let delta = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/agentMessage/delta",
        "params": { "delta": "Hello" }
    });
    let turn_completed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "turn/completed",
        "params": { "turn": { "status": "completed" } }
    });

    let (delta_event, delta_action) = codex_translate_event(&delta, &mut state, &mut session_id);
    assert_eq!(delta_action, CodexPostAction::None);
    match delta_event {
        Some(StreamEvent::Assistant { .. }) => {}
        _ => panic!("Expected assistant delta event"),
    }
    assert_eq!(state.latest_agent_message, Some("Hello".to_string()));

    let (result_event, result_action) =
        codex_translate_event(&turn_completed, &mut state, &mut session_id);
    assert_eq!(result_action, CodexPostAction::DispatchPendingTurns);
    assert!(!state.turn_in_progress);
    match result_event {
        Some(StreamEvent::Result {
            subtype,
            is_error,
            result,
            ..
        }) => {
            assert_eq!(subtype, "success");
            assert!(!is_error);
            assert_eq!(result, Some("Hello".to_string()));
        }
        _ => panic!("Expected result event after turn completion"),
    }
}

#[test]
fn test_codex_translate_unknown_event_emits_heartbeat() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/thinking/delta",
        "params": { "delta": "..." }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::System { subtype, extra, .. }) => {
            assert_eq!(subtype, "heartbeat");
            assert_eq!(extra["provider"], "codex");
            assert_eq!(extra["event"], "heartbeat");
            assert_eq!(extra["detail"]["method"], "item/thinking/delta");
        }
        _ => panic!("Expected codex unknown event to emit heartbeat system event"),
    }
}

#[test]
fn test_codex_translate_command_execution_started_emits_tool_use() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/started",
        "params": {
            "item": {
                "type": "commandExecution",
                "id": "call_abc",
                "commandActions": [{"type": "unknown", "command": "pwd"}]
            }
        }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::Assistant { message, extra, .. }) => {
            assert_eq!(extra["provider"], "codex");
            assert_eq!(extra["event"], "item/started");
            assert_eq!(message["role"], "assistant");
            assert_eq!(message["content"][0]["type"], "tool_use");
            assert_eq!(message["content"][0]["id"], "call_abc");
            assert_eq!(message["content"][0]["name"], "Bash");
            assert_eq!(message["content"][0]["input"]["command"], "pwd");
        }
        _ => panic!("Expected commandExecution start to emit assistant tool_use"),
    }
}

#[test]
fn test_codex_translate_command_execution_completed_emits_tool_result() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/completed",
        "params": {
            "item": {
                "type": "commandExecution",
                "id": "call_abc",
                "aggregatedOutput": "/tmp\n",
                "exitCode": 0,
                "status": "completed"
            }
        }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::User { message, extra, .. }) => {
            assert_eq!(extra["provider"], "codex");
            assert_eq!(extra["event"], "item/completed");
            assert_eq!(message["role"], "user");
            assert_eq!(message["content"][0]["type"], "tool_result");
            assert_eq!(message["content"][0]["tool_use_id"], "call_abc");
            assert_eq!(message["content"][0]["content"], "/tmp\n");
            assert_eq!(message["content"][0]["is_error"], false);
        }
        _ => panic!("Expected commandExecution completion to emit user tool_result"),
    }
}

#[test]
fn test_codex_translate_command_execution_started_with_numeric_id() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/started",
        "params": {
            "item": {
                "type": "commandExecution",
                "id": 1001,
                "command": "ls -la"
            }
        }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::Assistant { message, extra, .. }) => {
            assert_eq!(extra["provider"], "codex");
            assert_eq!(message["content"][0]["id"], "1001");
            assert_eq!(message["content"][0]["name"], "Bash");
            assert_eq!(message["content"][0]["input"]["command"], "ls -la");
        }
        _ => panic!("Expected commandExecution start to emit assistant tool_use"),
    }
}

#[test]
fn test_codex_translate_command_execution_started_without_type() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/started",
        "params": {
            "item": {
                "call_id": "cmd-plain",
                "commandActions": [{ "command": "pwd" }]
            }
        }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::Assistant { message, extra, .. }) => {
            assert_eq!(extra["provider"], "codex");
            assert_eq!(message["content"][0]["id"], "cmd-plain");
        }
        _ => panic!("Expected commandExecution start fallback path to emit tool_use"),
    }
}

#[test]
fn test_codex_translate_command_execution_completed_with_alt_id_field() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/completed",
        "params": {
            "item": {
                "type": "commandExecution",
                "callId": "call_xyz",
                "aggregatedOutput": "ok\n",
                "exitCode": 0,
                "status": "completed"
            }
        }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::User { message, .. }) => {
            assert_eq!(message["content"][0]["tool_use_id"], "call_xyz");
        }
        _ => panic!("Expected commandExecution completion with alt id to emit user tool_result"),
    }
}

#[test]
fn test_codex_translate_command_execution_failed_sets_tool_result_error() {
    let mut state = test_codex_state();
    let mut session_id = Some("thread_123".to_string());
    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "item/completed",
        "params": {
            "item": {
                "type": "commandExecution",
                "id": "call_abc",
                "aggregatedOutput": "boom\\n",
                "exitCode": 1,
                "status": "failed"
            }
        }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);
    assert_eq!(post_action, CodexPostAction::None);
    match event {
        Some(StreamEvent::User { message, .. }) => {
            assert_eq!(message["content"][0]["is_error"], true);
        }
        _ => panic!("Expected commandExecution failure to emit user tool_result"),
    }
}

#[test]
fn test_codex_thread_init_request_selects_fork_for_resume_fork() {
    let (method, params) = codex_thread_init_request(
        Some("thread_parent"),
        true,
        true,
        Some("/tmp/project"),
        "gpt-5.3-codex",
        "system prompt",
    );

    assert_eq!(method, "thread/fork");
    assert_eq!(params["threadId"], "thread_parent");
    assert_eq!(params["cwd"], "/tmp/project");
    assert_eq!(params["model"], "gpt-5.3-codex");
    assert_eq!(params["approvalPolicy"], "never");
    assert_eq!(params["sandbox"], "danger-full-access");
    assert_eq!(params["developerInstructions"], "system prompt");
}

#[test]
fn test_codex_thread_init_request_selects_resume_without_fork() {
    let (method, params) = codex_thread_init_request(
        Some("thread_parent"),
        false,
        true,
        Some("/tmp/project"),
        "gpt-5.3-codex",
        "",
    );

    assert_eq!(method, "thread/resume");
    assert_eq!(params["threadId"], "thread_parent");
    assert_eq!(params["developerInstructions"], serde_json::Value::Null);
}

#[test]
fn test_codex_thread_init_request_selects_start_when_not_resuming() {
    let (method, params) =
        codex_thread_init_request(None, true, true, None, "gpt-5.3-codex", "system prompt");

    assert_eq!(method, "thread/start");
    assert_eq!(params.get("threadId"), None);
    assert_eq!(params["cwd"], serde_json::Value::Null);
    assert_eq!(params["model"], "gpt-5.3-codex");
}

#[test]
fn test_codex_thread_init_request_disables_tools_when_allow_tools_false() {
    let (method, params) = codex_thread_init_request(
        None,
        false,
        false,
        Some("/tmp/project"),
        "gpt-5.3-codex",
        "system prompt",
    );

    assert_eq!(method, "thread/start");
    assert_eq!(params["approvalPolicy"], "never");
    assert_eq!(params["sandbox"], "read-only");
}

// ── Stale thread retry tests ─────────────────────────────────────────

#[test]
fn test_is_stale_codex_thread_error_detects_rollout_missing() {
    assert!(is_stale_codex_thread_error(
        "no rollout found for thread id abc-123"
    ));
}

#[test]
fn test_is_stale_codex_thread_error_case_insensitive() {
    assert!(is_stale_codex_thread_error(
        "No Rollout Found For Thread Id xyz"
    ));
}

#[test]
fn test_is_stale_codex_thread_error_ignores_generic_errors() {
    assert!(!is_stale_codex_thread_error("network timeout"));
    assert!(!is_stale_codex_thread_error("start failed"));
}

#[test]
fn test_codex_translate_stale_resume_triggers_retry() {
    let mut state = test_codex_state();
    state.start_phase = "thread/resume".to_string();
    state.resume_thread_id = Some("stale-thread-123".to_string());
    let mut session_id = None;

    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "no rollout found for thread id stale-thread-123" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    // Should NOT emit an error event — swallowed for retry.
    assert!(event.is_none());
    assert_eq!(post_action, CodexPostAction::RetryThreadStart);
    // State should be ready for a fresh thread/start (initialized stays true
    // because the process-level initialize handshake was already completed).
    assert!(state.initialized);
    assert!(state.start_request_id.is_none());
    assert!(state.resume_thread_id.is_none());
    assert!(state.retried_fresh_start);
}

#[test]
fn test_codex_translate_stale_resume_only_retries_once() {
    let mut state = test_codex_state();
    state.start_phase = "thread/resume".to_string();
    state.resume_thread_id = Some("stale-thread-123".to_string());
    state.retried_fresh_start = true; // Already retried once.
    let mut session_id = None;

    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "no rollout found for thread id stale-thread-123" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    // Second time: should NOT retry — emit the error normally.
    assert!(event.is_some());
    assert_ne!(post_action, CodexPostAction::RetryThreadStart);
    match event {
        Some(StreamEvent::Result {
            is_error, result, ..
        }) => {
            assert!(is_error);
            assert!(result.unwrap().contains("no rollout found"));
        }
        _ => panic!("Expected error result event on second stale-thread attempt"),
    }
}

#[test]
fn test_codex_translate_non_resume_stale_error_not_retried() {
    let mut state = test_codex_state();
    state.start_phase = "thread/start".to_string(); // Not a resume or fork.
    let mut session_id = None;

    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "no rollout found for thread id stale-thread-123" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    // Should NOT retry — this was a fresh start, not a resume or fork.
    assert!(event.is_some());
    assert_ne!(post_action, CodexPostAction::RetryThreadStart);
}

#[test]
fn test_codex_translate_resume_generic_error_not_retried() {
    let mut state = test_codex_state();
    state.start_phase = "thread/resume".to_string();
    state.resume_thread_id = Some("thread-123".to_string());
    let mut session_id = None;

    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "some other error" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    // Generic resume errors should NOT trigger retry.
    assert!(event.is_some());
    assert_ne!(post_action, CodexPostAction::RetryThreadStart);
}

// ── Fork stale thread retry tests ───────────────────────────────────

#[test]
fn test_is_stale_codex_thread_error_detects_thread_not_found() {
    assert!(is_stale_codex_thread_error("thread not found"));
    assert!(is_stale_codex_thread_error("Thread Not Found"));
    assert!(is_stale_codex_thread_error("thread_not_found"));
}

#[test]
fn test_codex_translate_stale_fork_triggers_retry() {
    let mut state = test_codex_state();
    state.start_phase = "thread/fork".to_string();
    state.resume_thread_id = Some("parent-thread-123".to_string());
    let mut session_id = None;

    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "no rollout found for thread id parent-thread-123" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    // Fork with stale parent should trigger retry, just like resume.
    assert!(event.is_none());
    assert_eq!(post_action, CodexPostAction::RetryThreadStart);
    assert!(state.retried_fresh_start);
    assert!(state.resume_thread_id.is_none());
}

#[test]
fn test_codex_translate_stale_fork_thread_not_found_triggers_retry() {
    let mut state = test_codex_state();
    state.start_phase = "thread/fork".to_string();
    state.resume_thread_id = Some("parent-thread-456".to_string());
    let mut session_id = None;

    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "thread not found" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    assert!(event.is_none());
    assert_eq!(post_action, CodexPostAction::RetryThreadStart);
}

#[test]
fn test_codex_translate_stale_fork_only_retries_once() {
    let mut state = test_codex_state();
    state.start_phase = "thread/fork".to_string();
    state.resume_thread_id = Some("parent-thread-123".to_string());
    state.retried_fresh_start = true; // Already retried once.
    let mut session_id = None;

    let parsed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 42,
        "error": { "message": "no rollout found for thread id parent-thread-123" }
    });

    let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

    // Second time: should NOT retry — emit the error normally.
    assert!(event.is_some());
    assert_ne!(post_action, CodexPostAction::RetryThreadStart);
}
