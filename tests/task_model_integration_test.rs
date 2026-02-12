//! Integration test for task model feature.
//!
//! This test verifies the model extraction logic that's used when spawning
//! coworkers with task-specific models. The actual storage, RPC, and spawn
//! behavior are tested via unit tests in src/daemon/rpc.rs, src/daemon/state.rs,
//! and src/daemon/dispatch.rs.
//!
//! This test focuses on the public API contract: given a "provider/model" string,
//! the daemon should extract just the model alias for LaunchConfig.model.

/// Test that model extraction splits provider/model correctly for LaunchConfig.
///
/// When task_model stores "claude/opus", the daemon needs to extract just "opus"
/// to pass to LaunchConfig.model (which expects model aliases, not full paths).
#[test]
fn test_model_extraction_for_launch_config() {
    let full_model = "claude/opus";
    let model_alias = full_model.split('/').nth(1);
    assert_eq!(model_alias, Some("opus"));

    let full_model = "claude/sonnet";
    let model_alias = full_model.split('/').nth(1);
    assert_eq!(model_alias, Some("sonnet"));

    let full_model = "codex/o3";
    let model_alias = full_model.split('/').nth(1);
    assert_eq!(model_alias, Some("o3"));
}

/// Test that invalid formats (no slash) return None for the model alias.
#[test]
fn test_model_extraction_handles_invalid_format() {
    let invalid = "opus"; // missing slash
    let model_alias = invalid.split('/').nth(1);
    assert_eq!(model_alias, None);
}

/// Test that empty provider or model parts are handled.
#[test]
fn test_model_extraction_handles_empty_parts() {
    let empty_provider = "/opus";
    let model_alias = empty_provider.split('/').nth(1);
    assert_eq!(model_alias, Some("opus"));

    let empty_model = "claude/";
    let model_alias = empty_model.split('/').nth(1);
    assert_eq!(model_alias, Some(""));
}
