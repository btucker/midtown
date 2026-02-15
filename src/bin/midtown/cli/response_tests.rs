use super::*;

#[test]
fn test_status_response_with_full_info() {
    let json = r#"{
        "daemon_running": true,
        "active_coworkers": 2,
        "pending_tasks": 1,
        "socket_path": "/tmp/midtown.sock",
        "coworkers": [
            {"name": "lex", "status": "running", "current_task": "implement auth", "started_at": "2024-01-01T00:00:00Z", "provider": "claude", "profile": "ben@quotably.com"},
            {"name": "park", "status": "running", "current_task": null, "started_at": "2024-01-01T00:00:00Z", "provider": "zai", "profile": "ben@btucker.net"}
        ],
        "tasks": [
            {"id": "t1", "subject": "implement auth endpoint", "status": "in_progress", "assignee": "lex"}
        ],
        "pull_requests": [
            {"number": 42, "title": "Add auth", "author": "lex", "status": "awaiting review"}
        ],
        "recent_activity": []
    }"#;

    let response: Response = serde_json::from_str(json).unwrap();

    match response {
        Response::Status(status) => {
            assert!(status.daemon_running);
            assert_eq!(status.active_coworkers, 2);
            assert!(status.full_status.is_some());

            let full = status.full_status.unwrap();
            assert_eq!(full.coworkers.len(), 2);
            assert_eq!(full.coworkers[0].name, "lex");
            assert_eq!(
                full.coworkers[0].current_task,
                Some("implement auth".to_string())
            );
            assert_eq!(full.coworkers[0].provider, Some("claude".to_string()));
            assert_eq!(
                full.coworkers[0].profile,
                Some("ben@quotably.com".to_string())
            );
            assert_eq!(full.coworkers[1].current_task, None);
            assert_eq!(full.coworkers[1].provider, Some("zai".to_string()));
            assert_eq!(
                full.coworkers[1].profile,
                Some("ben@btucker.net".to_string())
            );
            assert_eq!(full.tasks.len(), 1);
            assert_eq!(full.pull_requests.len(), 1);
        }
        _ => panic!("Expected Status response"),
    }
}

#[test]
fn test_coworkers_response_parsing() {
    let json = r#"{"coworkers": [{"name": "lexington", "status": "running", "current_task": null, "started_at": "2026-01-26T20:52:06.779326+00:00"}]}"#;
    let response: Response = serde_json::from_str(json).expect("Should parse");

    match response {
        Response::Coworkers { coworkers } => {
            assert_eq!(coworkers.len(), 1);
            assert_eq!(coworkers[0].name, "lexington");
            assert_eq!(coworkers[0].status, "running");
        }
        other => panic!("Expected Coworkers, got {:?}", other),
    }
}

#[test]
fn test_coworkers_response_with_success_field() {
    // Daemon returns "success": true along with coworkers
    let json = r#"{"success": true, "coworkers": [{"name": "lexington", "status": "running", "current_task": null, "started_at": "2026-01-26T20:52:06.779326+00:00"}]}"#;
    let response: Response = serde_json::from_str(json).expect("Should parse with extra fields");

    match response {
        Response::Coworkers { coworkers } => {
            assert_eq!(coworkers.len(), 1);
            assert_eq!(coworkers[0].name, "lexington");
        }
        other => panic!("Expected Coworkers, got {:?}", other),
    }
}

#[test]
fn test_coworkers_response_includes_provider_and_profile() {
    // Test that coworker list includes provider and profile fields
    let json = r#"{"success": true, "coworkers": [
        {"name": "lexington", "status": "running", "current_task": null, "started_at": "2026-01-26T20:52:06.779326+00:00", "provider": "claude", "profile": "ben@quotably.com"},
        {"name": "park", "status": "running", "current_task": "reviewing PR", "started_at": "2026-01-26T20:52:06.779326+00:00", "provider": "zai", "profile": "ben@btucker.net"}
    ]}"#;
    let response: Response = serde_json::from_str(json).expect("Should parse");

    match response {
        Response::Coworkers { coworkers } => {
            assert_eq!(coworkers.len(), 2);
            assert_eq!(coworkers[0].provider, Some("claude".to_string()));
            assert_eq!(coworkers[0].profile, Some("ben@quotably.com".to_string()));
            assert_eq!(coworkers[1].provider, Some("zai".to_string()));
            assert_eq!(coworkers[1].profile, Some("ben@btucker.net".to_string()));
        }
        other => panic!("Expected Coworkers, got {:?}", other),
    }
}

#[test]
fn test_sessions_response_with_success_field() {
    let json = r#"{
        "success": true,
        "sessions": [
            {
                "name": "park",
                "session_id": "abc-123",
                "status": "running",
                "purpose": "task_work",
                "last_active": "2026-02-16T00:00:00Z",
                "task": "42"
            }
        ]
    }"#;
    let response: Response = serde_json::from_str(json).expect("Should parse sessions response");

    match response {
        Response::Sessions { sessions } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].name, "park");
            assert_eq!(sessions[0].session_id, "abc-123");
            assert_eq!(sessions[0].status, "running");
            assert_eq!(sessions[0].task.as_deref(), Some("42"));
        }
        other => panic!("Expected Sessions, got {:?}", other),
    }
}

#[test]
fn test_sessions_response_pretty_format() {
    let response = Response::Sessions {
        sessions: vec![SessionInfo {
            name: "park".to_string(),
            session_id: "abc-123".to_string(),
            status: "running".to_string(),
            purpose: Some("task_work".to_string()),
            last_active: Some("2026-02-16T00:00:00Z".to_string()),
            task: Some("42".to_string()),
        }],
    };

    let pretty = response.to_pretty();
    assert!(pretty.contains("Headless Sessions"));
    assert!(pretty.contains("park"));
    assert!(pretty.contains("running"));
    assert!(pretty.contains("abc-123"));
    assert!(pretty.contains("task:!42"));
}

#[test]
fn test_coworkers_response_display_includes_provider_and_profile() {
    // Test that the pretty-print format includes provider:profile
    let response = Response::Coworkers {
        coworkers: vec![
            CoworkerInfo {
                name: "lexington".to_string(),
                status: "running".to_string(),
                current_task: Some("implementing auth".to_string()),
                started_at: None,
                provider: Some("claude".to_string()),
                profile: Some("ben@quotably.com".to_string()),
            },
            CoworkerInfo {
                name: "park".to_string(),
                status: "running".to_string(),
                current_task: None,
                started_at: None,
                provider: Some("zai".to_string()),
                profile: Some("ben@btucker.net".to_string()),
            },
        ],
    };

    let pretty = response.to_pretty();
    assert!(pretty.contains("lexington"));
    assert!(pretty.contains("(claude: ben@quotably.com)"));
    assert!(pretty.contains("park"));
    assert!(pretty.contains("(zai: ben@btucker.net)"));
}

#[test]
fn test_coworker_view_output_format_does_not_match_response_enum() {
    // The coworker.view RPC returns {"success": true, "output": "..."} which
    // doesn't match any Response variant. This is why coworker_view() uses
    // send_raw() instead of send().
    let json = r#"{"success": true, "output": "some terminal output"}"#;
    let result: Result<Response, _> = serde_json::from_str(json);
    assert!(
        result.is_err(),
        "coworker.view output format should NOT deserialize as Response"
    );
}

#[test]
fn test_coworker_view_output_extraction() {
    // Verify the extraction logic used in DaemonClient::coworker_view()
    let raw: serde_json::Value =
        serde_json::from_str(r#"{"success": true, "output": "terminal content here"}"#).unwrap();
    let output = raw
        .get("output")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "RPC response missing 'output' field".to_string());
    assert_eq!(output.unwrap(), "terminal content here");
}

#[test]
fn test_coworker_view_missing_output_field_returns_error() {
    // If the output field is missing, extraction should fail with a clear error
    let raw: serde_json::Value = serde_json::from_str(r#"{"success": true}"#).unwrap();
    let output = raw
        .get("output")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "RPC response missing 'output' field".to_string());
    assert_eq!(output.unwrap_err(), "RPC response missing 'output' field");
}

#[test]
fn test_task_update_response_with_type_field() {
    // All task RPC handlers return {"type": "message", "message": "..."} but
    // Response::Message expects just {"message": "..."}. Serde's untagged enum
    // deserialization silently skips unknown fields like "type".
    let json = r#"{"type": "message", "message": "Task !1116 updated"}"#;
    let response: Response =
        serde_json::from_str(json).expect("Should parse task.update response with type field");

    match response {
        Response::Message { message } => {
            assert_eq!(message, "Task !1116 updated");
        }
        other => panic!("Expected Message, got {:?}", other),
    }
}

#[test]
fn test_task_create_response_with_type_field() {
    // task.create returns the same {"type": "message", "message": "..."} format
    let json = r#"{"type": "message", "message": "Task !42 created: Add auth endpoint"}"#;
    let response: Response =
        serde_json::from_str(json).expect("Should parse task.create response with type field");

    match response {
        Response::Message { message } => {
            assert_eq!(message, "Task !42 created: Add auth endpoint");
        }
        other => panic!("Expected Message, got {:?}", other),
    }
}

#[test]
fn test_task_done_response_with_type_field() {
    // task.done returns the same {"type": "message", "message": "..."} format
    let json = r#"{"type": "message", "message": "Task !99 completed"}"#;
    let response: Response =
        serde_json::from_str(json).expect("Should parse task.done response with type field");

    match response {
        Response::Message { message } => {
            assert_eq!(message, "Task !99 completed");
        }
        other => panic!("Expected Message, got {:?}", other),
    }
}

#[test]
fn test_status_pretty_format() {
    let status = StatusResponse {
        daemon_running: true,
        active_coworkers: 2,
        max_coworkers: Some(16),
        pending_tasks: 1,
        socket_path: "/tmp/test.sock".to_string(),
        lead_session: Some("midtown-lead".to_string()),
        lead_session_active: Some(true),
        full_status: Some(FullStatusInfo {
            coworkers: vec![
                CoworkerInfo {
                    name: "lex".to_string(),
                    status: "running".to_string(),
                    current_task: Some("implement auth endpoint".to_string()),
                    started_at: None,
                    provider: Some("claude".to_string()),
                    profile: Some("ben@quotably.com".to_string()),
                },
                CoworkerInfo {
                    name: "park".to_string(),
                    status: "running".to_string(),
                    current_task: None,
                    started_at: None,
                    provider: Some("zai".to_string()),
                    profile: Some("ben@btucker.net".to_string()),
                },
            ],
            tasks: vec![TaskInfo {
                id: "t1".to_string(),
                subject: "implement auth endpoint".to_string(),
                status: "in_progress".to_string(),
                assignee: Some("lex".to_string()),
            }],
            pull_requests: vec![PrInfo {
                number: 42,
                title: "Add auth".to_string(),
                author: "lex".to_string(),
                status: "awaiting review".to_string(),
            }],
            recent_activity: vec![],
        }),
    };

    let response = Response::Status(status);
    let pretty = response.to_pretty();

    assert!(pretty.contains("Coworkers: 2/16 active"));
    assert!(
        pretty.contains("lex - working on: implement auth endpoint (claude: ben@quotably.com)")
    );
    assert!(pretty.contains("park - idle (zai: ben@btucker.net)"));
    assert!(pretty.contains("Tasks: 1 open"));
    assert!(pretty.contains("[in_progress] implement auth endpoint (lex)"));
    assert!(pretty.contains("PRs: 1 open"));
    assert!(pretty.contains("PR#42 Add auth (lex) - awaiting review"));
}
