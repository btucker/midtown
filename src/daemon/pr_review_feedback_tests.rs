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
}
