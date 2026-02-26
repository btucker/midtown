// Unit tests for review feedback handling logic (task 1271)
//
// Bug: When review feedback arrives on a PR that's linked to a task,
// the daemon should check if that task has an active owner and nudge them,
// rather than spawning based on the PR's git metadata owner.
//
// NOTE: This bug requires integration-level testing because it involves
// checking task state from the filesystem. The actual fix is in
// `handle_pr_comment_nudge` which needs to resolve the task owner
// before calling the decision function.
//
// These tests document the expected behavior at the decision function level,
// assuming the correct owner is passed in.

#[cfg(test)]
mod tests {
    use crate::rules::{PrAction, decide_pr_comment_action_with_handoff};

    /// Test that when the correct task owner is passed to the decision function,
    /// it correctly nudges them when they're active and idle.
    ///
    /// This documents the expected behavior: when the upstream caller
    /// (handle_pr_comment_nudge) correctly resolves the task owner,
    /// the decision function should nudge them.
    #[test]
    fn test_decision_function_nudges_active_idle_owner() {
        let task_owner = "york"; // Task !1260 owner (correctly resolved by caller)
        let reviewer = "park";
        let active_coworkers = vec!["york".to_string()];
        let idle_coworkers = vec!["york".to_string()];
        let at_dev_limit = false;
        let session_context = None;
        let message = "Review feedback";

        let action = decide_pr_comment_action_with_handoff(
            task_owner,
            reviewer,
            &active_coworkers,
            &idle_coworkers,
            at_dev_limit,
            session_context,
            message,
        );

        // Should nudge york (the task owner who is active and idle)
        match action {
            PrAction::NudgeOwner { owner, .. } => {
                assert_eq!(owner, "york");
            }
            _ => panic!("Expected NudgeOwner, got {:?}", action),
        }
    }

    /// Test that when the task owner is active but busy, the decision
    /// function still nudges them (spawning an active coworker fails).
    #[test]
    fn test_decision_function_nudges_active_busy_owner() {
        let task_owner = "york";
        let reviewer = "park";
        let active_coworkers = vec!["york".to_string()];
        let idle_coworkers = vec![]; // york is busy
        let at_dev_limit = false;
        let session_context = None;
        let message = "Review feedback";

        let action = decide_pr_comment_action_with_handoff(
            task_owner,
            reviewer,
            &active_coworkers,
            &idle_coworkers,
            at_dev_limit,
            session_context,
            message,
        );

        // Should still nudge york even though busy
        match action {
            PrAction::NudgeOwner { owner, .. } => {
                assert_eq!(owner, "york");
            }
            _ => panic!("Expected NudgeOwner for busy owner, got {:?}", action),
        }
    }

    /// Test that when a PR's linked task is completed, a follow-up task should
    /// be created instead of trying to spawn/resume the original coworker.
    ///
    /// Bug !1794: Review comments on PRs with completed tasks were silently dropped
    /// because the daemon tried to spawn/resume the original coworker with stale
    /// session context. The correct behavior is to create a new follow-up task.
    #[test]
    fn test_completed_task_requires_followup_task() {
        use crate::rules::review_comment_creates_followup;
        use crate::tasks::TaskStatus;

        assert!(
            review_comment_creates_followup(&TaskStatus::Completed),
            "A completed task's PR receiving review feedback should trigger a follow-up task"
        );
        assert!(
            !review_comment_creates_followup(&TaskStatus::InProgress),
            "An in-progress task's PR should handle review feedback via normal dispatch"
        );
        assert!(
            !review_comment_creates_followup(&TaskStatus::Pending),
            "A pending task's PR should handle review feedback via normal dispatch"
        );
    }

    /// Test that when the task owner is inactive, the decision function
    /// spawns them (assuming dev limit allows).
    #[test]
    fn test_decision_function_spawns_inactive_owner() {
        let task_owner = "york";
        let reviewer = "park";
        let active_coworkers = vec![]; // york is not active
        let idle_coworkers = vec![];
        let at_dev_limit = false;
        let session_context = None;
        let message = "Review feedback";

        let action = decide_pr_comment_action_with_handoff(
            task_owner,
            reviewer,
            &active_coworkers,
            &idle_coworkers,
            at_dev_limit,
            session_context,
            message,
        );

        // Should spawn york since they're inactive
        match action {
            PrAction::SpawnOwner { owner, .. } => {
                assert_eq!(owner, "york");
            }
            _ => panic!("Expected SpawnOwner for inactive owner, got {:?}", action),
        }
    }

    /// Test that format_review_content returns None when there are no reviews
    /// or issue comments with review signatures.
    #[test]
    fn test_format_review_content_empty() {
        use crate::daemon::helpers::format_review_content;

        let data = serde_json::json!({
            "reviews": [],
            "comments": []
        });
        assert!(format_review_content(&data).is_none());
    }

    /// Test that format_review_content includes a formal review with a non-empty body.
    #[test]
    fn test_format_review_content_formal_review() {
        use crate::daemon::helpers::format_review_content;

        let data = serde_json::json!({
            "reviews": [
                {
                    "author": {"login": "app/codex"},
                    "state": "CHANGES_REQUESTED",
                    "body": "Please add error handling here."
                }
            ],
            "comments": []
        });

        let result = format_review_content(&data).expect("should have content");
        assert!(result.contains("Please add error handling here."));
        assert!(result.contains("app/codex"));
        assert!(result.contains("CHANGES_REQUESTED"));
    }

    /// Test that format_review_content skips formal reviews with empty bodies
    /// (e.g., a pure "Approve" review with no comment).
    #[test]
    fn test_format_review_content_skips_empty_review_body() {
        use crate::daemon::helpers::format_review_content;

        let data = serde_json::json!({
            "reviews": [
                {
                    "author": {"login": "app/codex"},
                    "state": "APPROVED",
                    "body": ""
                }
            ],
            "comments": []
        });
        assert!(format_review_content(&data).is_none());
    }

    /// Test that format_review_content includes Midtown coworker issue comments
    /// (posted with <!-- midtown: --> frontmatter and a Code Review header).
    /// These are the reviews that were being silently missed before the fix.
    #[test]
    fn test_format_review_content_coworker_issue_comment() {
        use crate::daemon::helpers::format_review_content;

        let review_body = "<!-- midtown: park -->\n## Code Review by park\n\nFound a potential null dereference on line 42.\n\n🌃 Co-built with Midtown";
        let data = serde_json::json!({
            "reviews": [],
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": review_body
                }
            ]
        });

        let result = format_review_content(&data).expect("should have content");
        assert!(
            result.contains("null dereference"),
            "should include review body"
        );
        assert!(result.contains("btucker"), "should include commenter login");
    }

    /// Test that format_review_content skips issue comments that are not reviews
    /// (e.g., a status update with <!-- midtown: --> but no Code Review header).
    #[test]
    fn test_format_review_content_skips_non_review_comments() {
        use crate::daemon::helpers::format_review_content;

        let data = serde_json::json!({
            "reviews": [],
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": "<!-- midtown: park -->\nCI is now passing. The build is green."
                }
            ]
        });
        // This comment has midtown frontmatter but no review signature — should be skipped
        assert!(format_review_content(&data).is_none());
    }

    /// Test that format_review_content includes both formal reviews and coworker
    /// issue comment reviews when both are present (the mixed case).
    #[test]
    fn test_format_review_content_both_types() {
        use crate::daemon::helpers::format_review_content;

        let data = serde_json::json!({
            "reviews": [
                {
                    "author": {"login": "app/codex"},
                    "state": "CHANGES_REQUESTED",
                    "body": "Fix the type error in auth.rs."
                }
            ],
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": "<!-- midtown: park -->\n## Code Review by park\n\nAlso check the null case in parser.rs.\n\n🌃 Co-built with Midtown"
                }
            ]
        });

        let result = format_review_content(&data).expect("should have content");
        assert!(
            result.contains("Fix the type error in auth.rs."),
            "should include formal review"
        );
        assert!(
            result.contains("null case in parser.rs"),
            "should include coworker review"
        );
    }
}
