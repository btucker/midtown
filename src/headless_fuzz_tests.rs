use super::*;
use proptest::prelude::*;
use serde_json::Value;
use std::collections::VecDeque;

fn ascii_string(max_len: usize) -> impl Strategy<Value = String> {
    proptest::collection::vec(0x20u8..0x7fu8, 0..=max_len)
        .prop_map(|bytes| bytes.into_iter().map(char::from).collect())
}

fn json_value_strategy() -> BoxedStrategy<Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| Value::Number(n.into())),
        ascii_string(24).prop_map(Value::String),
    ];

    leaf.prop_recursive(4, 64, 8, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            proptest::collection::vec((ascii_string(12), inner.clone()), 0..4).prop_map(
                |entries| {
                    let mut map = serde_json::Map::new();
                    for (key, value) in entries {
                        map.insert(key, value);
                    }
                    Value::Object(map)
                }
            ),
        ]
    })
    .boxed()
}

fn codex_state_strategy() -> impl Strategy<Value = CodexProtocolState> {
    (
        prop::option::of(0u64..64),
        prop::option::of(ascii_string(16)),
        any::<bool>(),
        1u64..1024,
        proptest::collection::vec(ascii_string(24), 0..4),
        prop::option::of(ascii_string(64)),
        ascii_string(24),
        ascii_string(24),
    )
        .prop_map(
            |(
                start_request_id,
                thread_id,
                turn_in_progress,
                next_request_id,
                pending_messages,
                latest_agent_message,
                model,
                start_phase,
            )| {
                CodexProtocolState {
                    initialized: true,
                    start_request_id,
                    thread_id,
                    turn_in_progress,
                    next_request_id,
                    pending_messages: VecDeque::from(pending_messages),
                    latest_agent_message,
                    resume_thread_id: None,
                    fork_session: false,
                    allow_tools: true,
                    model: if model.is_empty() {
                        "gpt-5.3-codex".to_string()
                    } else {
                        model
                    },
                    cwd: None,
                    system_prompt: String::new(),
                    output_schema: None,
                    start_phase: if start_phase.is_empty() {
                        "thread/start".to_string()
                    } else {
                        start_phase
                    },
                }
            },
        )
}

#[derive(Clone, Debug)]
enum CodexStimulus {
    StartOk {
        thread_id: String,
    },
    StartErr {
        message: String,
    },
    TurnStartErr {
        message: String,
    },
    AgentDelta {
        delta: String,
    },
    TurnCompletedOk,
    TurnCompletedErr {
        message: String,
    },
    CommandStarted {
        call_id: String,
        command: String,
    },
    CommandCompleted {
        call_id: String,
        output: String,
        exit_code: i64,
    },
    UnknownMethod {
        method: String,
    },
}

fn codex_stimulus_strategy() -> impl Strategy<Value = CodexStimulus> {
    prop_oneof![
        ascii_string(16).prop_map(|thread_id| CodexStimulus::StartOk {
            thread_id: if thread_id.is_empty() {
                "thread_fuzz".to_string()
            } else {
                thread_id
            }
        }),
        ascii_string(24).prop_map(|message| CodexStimulus::StartErr {
            message: if message.is_empty() {
                "start failed".to_string()
            } else {
                message
            }
        }),
        ascii_string(24).prop_map(|message| CodexStimulus::TurnStartErr {
            message: if message.is_empty() {
                "turn failed".to_string()
            } else {
                message
            }
        }),
        ascii_string(24).prop_map(|delta| CodexStimulus::AgentDelta { delta }),
        Just(CodexStimulus::TurnCompletedOk),
        ascii_string(24).prop_map(|message| CodexStimulus::TurnCompletedErr {
            message: if message.is_empty() {
                "turn failed".to_string()
            } else {
                message
            }
        }),
        (ascii_string(16), ascii_string(24)).prop_map(|(call_id, command)| {
            CodexStimulus::CommandStarted {
                call_id: if call_id.is_empty() {
                    "call_fuzz".to_string()
                } else {
                    call_id
                },
                command,
            }
        }),
        (ascii_string(16), ascii_string(24), -1i64..=2i64).prop_map(
            |(call_id, output, exit_code)| {
                CodexStimulus::CommandCompleted {
                    call_id: if call_id.is_empty() {
                        "call_fuzz".to_string()
                    } else {
                        call_id
                    },
                    output,
                    exit_code,
                }
            }
        ),
        ascii_string(16).prop_map(|method| CodexStimulus::UnknownMethod {
            method: if method.is_empty() {
                "item/unknown".to_string()
            } else {
                method
            }
        }),
    ]
}

fn stimulus_to_json(stimulus: &CodexStimulus, start_request_id: u64) -> Value {
    match stimulus {
        CodexStimulus::StartOk { thread_id } => serde_json::json!({
            "jsonrpc": "2.0",
            "id": start_request_id,
            "result": {
                "thread": {"id": thread_id}
            }
        }),
        CodexStimulus::StartErr { message } => serde_json::json!({
            "jsonrpc": "2.0",
            "id": start_request_id,
            "error": { "message": message }
        }),
        CodexStimulus::TurnStartErr { message } => serde_json::json!({
            "jsonrpc": "2.0",
            "id": start_request_id + 1,
            "error": { "message": message }
        }),
        CodexStimulus::AgentDelta { delta } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/agentMessage/delta",
            "params": { "delta": delta }
        }),
        CodexStimulus::TurnCompletedOk => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": { "turn": { "status": "completed" } }
        }),
        CodexStimulus::TurnCompletedErr { message } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {
                "turn": {
                    "status": "failed",
                    "error": { "message": message }
                }
            }
        }),
        CodexStimulus::CommandStarted { call_id, command } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/started",
            "params": {
                "item": {
                    "type": "commandExecution",
                    "id": call_id,
                    "commandActions": [{ "command": command }]
                }
            }
        }),
        CodexStimulus::CommandCompleted {
            call_id,
            output,
            exit_code,
        } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "item/completed",
            "params": {
                "item": {
                    "type": "commandExecution",
                    "id": call_id,
                    "aggregatedOutput": output,
                    "status": if *exit_code == 0 { "completed" } else { "failed" },
                    "exitCode": exit_code
                }
            }
        }),
        CodexStimulus::UnknownMethod { method } => serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": {}
        }),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn fuzz_codex_translate_event_no_panics(
        parsed in json_value_strategy(),
        state_seed in codex_state_strategy(),
        session_seed in prop::option::of(ascii_string(20)),
    ) {
        let mut state = state_seed;
        let mut session_id = session_seed;

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            codex_translate_event(&parsed, &mut state, &mut session_id)
        }));

        prop_assert!(outcome.is_ok(), "codex_translate_event panicked on input: {parsed}");

        let (event, _post_action) = outcome.expect("checked is_ok above");
        prop_assert!(event.is_some(), "codex_translate_event unexpectedly returned no event");
    }

    #[test]
    fn fuzz_codex_translate_event_sequence_no_panics(
        parsed_events in proptest::collection::vec(json_value_strategy(), 0..32),
        state_seed in codex_state_strategy(),
        session_seed in prop::option::of(ascii_string(20)),
    ) {
        let mut state = state_seed;
        let mut session_id = session_seed;

        for parsed in parsed_events {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                codex_translate_event(&parsed, &mut state, &mut session_id)
            }));
            prop_assert!(outcome.is_ok(), "codex_translate_event panicked in sequence on input: {parsed}");
        }
    }

    #[test]
    fn fuzz_codex_thread_init_request_invariants(
        resume_thread_id in prop::option::of(ascii_string(16)),
        fork_session in any::<bool>(),
        allow_tools in any::<bool>(),
        cwd in prop::option::of(ascii_string(24)),
        model in ascii_string(24),
        system_prompt in ascii_string(64),
    ) {
        let model = if model.is_empty() { "gpt-5.3-codex".to_string() } else { model };

        let (method, params) = codex_thread_init_request(
            resume_thread_id.as_deref(),
            fork_session,
            allow_tools,
            cwd.as_deref(),
            &model,
            &system_prompt,
        );

        match (resume_thread_id.as_deref(), fork_session) {
            (Some(_), true) => prop_assert_eq!(method, "thread/fork"),
            (Some(_), false) => prop_assert_eq!(method, "thread/resume"),
            (None, _) => prop_assert_eq!(method, "thread/start"),
        }

        if let Some(thread_id) = resume_thread_id {
            prop_assert_eq!(params["threadId"].as_str(), Some(thread_id.as_str()));
        } else {
            prop_assert!(params.get("threadId").is_none());
        }

        if allow_tools {
            prop_assert_eq!(params["approvalPolicy"].as_str(), Some("never"));
            prop_assert_eq!(params["sandbox"].as_str(), Some("danger-full-access"));
        } else {
            prop_assert_eq!(params["approvalPolicy"].as_str(), Some("never"));
            prop_assert_eq!(params["sandbox"].as_str(), Some("read-only"));
        }

        if system_prompt.is_empty() {
            prop_assert!(params["developerInstructions"].is_null());
        }
    }

    #[test]
    fn fuzz_codex_translate_stateful_sequence(
        stimuli in proptest::collection::vec(codex_stimulus_strategy(), 1..64),
    ) {
        let start_request_id = 42u64;
        let mut state = CodexProtocolState {
            initialized: true,
            start_request_id: Some(start_request_id),
            thread_id: None,
            turn_in_progress: true,
            next_request_id: 100,
            pending_messages: VecDeque::new(),
            latest_agent_message: None,
            resume_thread_id: None,
            fork_session: false,
            allow_tools: true,
            model: "gpt-5.3-codex".to_string(),
            cwd: None,
            system_prompt: String::new(),
            output_schema: None,
            start_phase: "thread/start".to_string(),
        };
        let mut session_id: Option<String> = None;

        for stimulus in stimuli {
            let parsed = stimulus_to_json(&stimulus, start_request_id);
            let turn_in_progress_before = state.turn_in_progress;
            let (event, post_action) = codex_translate_event(&parsed, &mut state, &mut session_id);

            prop_assert!(event.is_some(), "expected translated event for stimulus: {stimulus:?}");

            match stimulus {
                CodexStimulus::StartOk { ref thread_id } => {
                    prop_assert_eq!(session_id.as_deref(), Some(thread_id.as_str()));
                    prop_assert_eq!(state.thread_id.as_deref(), Some(thread_id.as_str()));
                    if let Some(StreamEvent::System { subtype, .. }) = event {
                        prop_assert_eq!(subtype, "init");
                    } else {
                        prop_assert!(false, "start ok should emit init system event");
                    }
                }
                CodexStimulus::TurnStartErr { .. } => {
                    if turn_in_progress_before {
                        prop_assert!(!state.turn_in_progress);
                        prop_assert_eq!(post_action, CodexPostAction::DispatchPendingTurns);
                    }
                }
                CodexStimulus::TurnCompletedOk | CodexStimulus::TurnCompletedErr { .. } => {
                    prop_assert!(!state.turn_in_progress);
                    prop_assert_eq!(post_action, CodexPostAction::DispatchPendingTurns);
                    if let Some(StreamEvent::Result { .. }) = event {
                    } else {
                        prop_assert!(false, "turn completion should emit result");
                    }
                }
                CodexStimulus::CommandStarted { .. } => {
                    if let Some(StreamEvent::Assistant { message, .. }) = event {
                        prop_assert_eq!(message["content"][0]["type"].as_str(), Some("tool_use"));
                    } else {
                        prop_assert!(false, "command started should emit assistant tool_use");
                    }
                }
                CodexStimulus::CommandCompleted { exit_code, .. } => {
                    if let Some(StreamEvent::User { message, .. }) = event {
                        prop_assert_eq!(
                            message["content"][0]["type"].as_str(),
                            Some("tool_result")
                        );
                        prop_assert_eq!(message["content"][0]["is_error"].as_bool(), Some(exit_code != 0));
                    } else {
                        prop_assert!(false, "command completed should emit user tool_result");
                    }
                }
                CodexStimulus::AgentDelta { .. }
                | CodexStimulus::StartErr { .. }
                | CodexStimulus::UnknownMethod { .. } => {}
            }
        }
    }
}
