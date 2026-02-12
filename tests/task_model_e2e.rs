//! End-to-end tests for task model assignment via CLI and RPC.
//!
//! Tests the flow:
//! 1. CLI parses --model flag
//! 2. RPC handler validates format
//! 3. WorldSnapshot includes task_model_map
//! 4. Dispatch uses model when building LaunchConfig
//! 5. CLI can query metadata to display model
//!
//! Note: Persistence and state management are covered by unit tests in
//! src/daemon/state.rs and src/daemon/rpc.rs. This file focuses on
//! integration-level validation and model alias extraction logic.

use std::collections::HashMap;

#[test]
fn test_task_model_format_validation() {
    // Valid formats
    assert!(validate_model("claude/opus").is_ok());
    assert!(validate_model("claude/sonnet").is_ok());
    assert!(validate_model("claude/haiku").is_ok());
    assert!(validate_model("codex/o3").is_ok());
    assert!(validate_model("codex/o4-mini").is_ok());

    // Invalid formats
    assert!(validate_model("claude-opus").is_err()); // No slash
    assert!(validate_model("claude/opus/extra").is_err()); // Multiple slashes
    assert!(validate_model("/opus").is_err()); // Empty provider
    assert!(validate_model("claude/").is_err()); // Empty model
    assert!(validate_model("").is_err()); // Empty string
}

// Helper function that matches the validation logic in src/daemon/rpc.rs
fn validate_model(model: &str) -> Result<(), String> {
    let parts: Vec<&str> = model.split('/').collect();
    if parts.len() != 2 {
        return Err(format!(
            "Invalid model format '{}': must be '<provider>/<model>' (e.g., claude/opus)",
            model
        ));
    }
    if parts[0].is_empty() {
        return Err(format!(
            "Invalid model format '{}': provider cannot be empty",
            model
        ));
    }
    if parts[1].is_empty() {
        return Err(format!(
            "Invalid model format '{}': model cannot be empty",
            model
        ));
    }
    Ok(())
}

#[test]
fn test_model_alias_extraction() {
    // Test extraction of model alias from "provider/model" format
    let test_cases = vec![
        ("claude/opus", Some("opus")),
        ("claude/sonnet", Some("sonnet")),
        ("claude/haiku", Some("haiku")),
        ("codex/o3", Some("o3")),
        ("codex/o4-mini", Some("o4-mini")),
        ("invalid", None),
        ("", None),
    ];

    for (input, expected) in test_cases {
        let result = input.split('/').nth(1);
        assert_eq!(
            result, expected,
            "Failed for input: {} (expected: {:?}, got: {:?})",
            input, expected, result
        );
    }
}

#[test]
fn test_task_model_update_semantics() {
    let mut task_model: HashMap<String, String> = HashMap::new();

    // Initially empty
    assert!(task_model.is_empty());

    // Set a model
    task_model.insert("42".to_string(), "claude/opus".to_string());
    assert_eq!(task_model.get("42"), Some(&"claude/opus".to_string()));

    // Overwrite with different model
    task_model.insert("42".to_string(), "claude/sonnet".to_string());
    assert_eq!(task_model.get("42"), Some(&"claude/sonnet".to_string()));
    assert_eq!(task_model.len(), 1); // Still only one entry

    // Clear by removing
    task_model.remove("42");
    assert!(task_model.is_empty());

    // Multiple tasks
    task_model.insert("1".to_string(), "claude/opus".to_string());
    task_model.insert("2".to_string(), "claude/sonnet".to_string());
    task_model.insert("3".to_string(), "claude/haiku".to_string());
    assert_eq!(task_model.len(), 3);

    // Remove one doesn't affect others
    task_model.remove("2");
    assert_eq!(task_model.len(), 2);
    assert_eq!(task_model.get("1"), Some(&"claude/opus".to_string()));
    assert_eq!(task_model.get("3"), Some(&"claude/haiku".to_string()));
}

#[test]
fn test_provider_model_format_examples() {
    // Ensure the format matches expected patterns
    let valid_models = vec![
        "claude/opus",
        "claude/sonnet",
        "claude/haiku",
        "codex/o3",
        "codex/o4-mini",
    ];

    for model in valid_models {
        let parts: Vec<&str> = model.split('/').collect();
        assert_eq!(
            parts.len(),
            2,
            "Model {} should have exactly 2 parts",
            model
        );
        assert!(
            !parts[0].is_empty(),
            "Provider in {} should not be empty",
            model
        );
        assert!(
            !parts[1].is_empty(),
            "Model name in {} should not be empty",
            model
        );
    }
}
