use super::*;

#[test]
fn test_clusterer_schema_parses() {
    let schema: Result<serde_json::Value, _> = serde_json::from_str(CLUSTERER_SCHEMA);
    assert!(schema.is_ok(), "clusterer schema should be valid JSON");
}

#[test]
fn test_clusterer_request_serialization() {
    let request = ClustererRequest {
        task_id: "1234".to_string(),
        task_subject: "Add auth endpoint".to_string(),
        task_description: "Implement JWT authentication".to_string(),
        channels: vec![
            ChannelInfo {
                name: "auth".to_string(),
                active_task_count: 2,
                recent_tasks: vec!["Fix login bug".to_string()],
            },
            ChannelInfo {
                name: "api".to_string(),
                active_task_count: 1,
                recent_tasks: vec![],
            },
        ],
        recent_completions: vec![CompletedTaskInfo {
            subject: "Update tests".to_string(),
            channel: Some("testing".to_string()),
        }],
    };

    let json = serde_json::to_string(&request);
    assert!(json.is_ok());
}

#[test]
fn test_clusterer_response_deserialization() {
    let json = r#"{
        "create_channels": [],
        "archive_channels": ["old-auth"],
        "merge_channels": [
            {
                "from": "auth-v2",
                "into": "auth"
            }
        ],
        "assign_tasks": [
            {
                "task": "1234",
                "channel": "auth"
            }
        ]
    }"#;

    let response: Result<ClustererResponse, _> = serde_json::from_str(json);
    assert!(response.is_ok());

    let response = response.unwrap();
    assert_eq!(response.archive_channels.len(), 1);
    assert_eq!(response.merge_channels.len(), 1);
    assert_eq!(response.assign_tasks.len(), 1);
}

#[test]
fn test_clusterer_response_minimal() {
    let json = r#"{
        "create_channels": [],
        "archive_channels": [],
        "merge_channels": [],
        "assign_tasks": [
            {
                "task": "1234",
                "channel": "midtown"
            }
        ]
    }"#;

    let response: Result<ClustererResponse, _> = serde_json::from_str(json);
    assert!(response.is_ok());

    let response = response.unwrap();
    assert_eq!(response.assign_tasks.len(), 1);
    assert_eq!(response.assign_tasks[0].channel, "midtown");
}

#[test]
fn test_clusterer_role_basics() {
    let role = ClustererRole;

    assert_eq!(role.role_name(), "clusterer");
    assert_eq!(role.model(), "haiku");
    assert!(role.persist_session());
    assert_eq!(role.max_budget_usd(), 0.10);
    assert!(!role.allow_tools());

    let request = ClustererRequest {
        task_id: "100".to_string(),
        task_subject: "Test task".to_string(),
        task_description: "Test description".to_string(),
        channels: vec![],
        recent_completions: vec![],
    };

    let formatted = role.format_request(&request);
    assert!(formatted.contains("Test task"));
    assert!(formatted.contains("Test description"));

    let valid_json = r#"{
        "create_channels": [],
        "archive_channels": [],
        "merge_channels": [],
        "assign_tasks": [{"task": "123", "channel": "test"}]
    }"#;
    let response = role.parse_response(valid_json);
    assert!(response.is_ok());

    let invalid_json = "not json";
    let err = role.parse_response(invalid_json);
    assert!(err.is_err());
}
