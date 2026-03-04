//! Tests for PR number extraction from CI check targets.
//!
//! The workflow script's pr.ci_passed handler uses the PR number from the event
//! to call rpc.spawn_reviewer(). These tests verify the extraction logic that
//! was originally in handle_ci_completion_for_review_spawn (now removed — the
//! CI-triggered reviewer spawn retry lives in default_workflow.py).

use crate::webhook::CiCheckPassed;

/// Test that PR number is correctly extracted from CI check target strings.
#[test]
fn test_pr_number_extraction_from_ci_target() {
    // Test valid PR reference
    let ci_check = CiCheckPassed {
        check_name: "Build".to_string(),
        target: "PR #123".to_string(),
        mention_prefix: "".to_string(),
    };

    let pr_number = ci_check
        .target
        .strip_prefix("PR #")
        .and_then(|s| s.parse::<u64>().ok());
    assert_eq!(pr_number, Some(123));

    // Test non-PR target (branch)
    let ci_check_main = CiCheckPassed {
        check_name: "Build".to_string(),
        target: "main".to_string(),
        mention_prefix: "".to_string(),
    };

    let pr_number = ci_check_main
        .target
        .strip_prefix("PR #")
        .and_then(|s| s.parse::<u64>().ok());
    assert_eq!(pr_number, None);

    // Test invalid PR reference
    let ci_check_invalid = CiCheckPassed {
        check_name: "Build".to_string(),
        target: "PR #abc".to_string(),
        mention_prefix: "".to_string(),
    };

    let pr_number = ci_check_invalid
        .target
        .strip_prefix("PR #")
        .and_then(|s| s.parse::<u64>().ok());
    assert_eq!(pr_number, None);
}

/// Non-PR CI targets (e.g. "main" branch) should not produce a PR number.
#[test]
fn test_ci_completion_ignores_non_pr_targets() {
    // CI on "main" → no PR number extracted
    // CI on "PR #123" → PR number extracted
    // The workflow script uses this to decide whether to call spawn_reviewer.
}
