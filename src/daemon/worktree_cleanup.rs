//! Time-based worktree cleanup after task completion.
//!
//! Worktrees are retained for a configurable period (default 24h) after their
//! associated task completes. This preserves build caches and allows easy
//! reopening while preventing unbounded disk growth.
//!
//! ## Cleanup Strategy
//!
//! - **PR-based worktrees** (review-pr-*): Cleaned up immediately when PR merges/closes
//! - **Task-based worktrees** (task-*): Retained for 24h after task completion
//!
//! Safety checks ensure we never remove worktrees that are actively in use.

use chrono::Duration;
use tracing::debug;

use super::effects::Effect;
use super::snapshot::WorldSnapshot;

/// Collect cleanup effects for worktrees whose retention period has expired.
///
/// Returns `Effect::CleanupStaleWorktree` variants for worktrees that:
/// - Have a completed_at timestamp (task was completed)
/// - Are older than the retention period (default 24h)
/// - Are not currently bound to an active coworker
/// - Are not associated with an open PR (those are handled by PR cleanup)
///
/// This is a pure decision function - it reads immutable snapshot data and
/// returns effects without performing I/O.
pub(super) fn collect_stale_worktree_cleanup_effects(
    snap: &WorldSnapshot,
    retention_hours: u64,
) -> Vec<Effect> {
    let mut effects = Vec::new();
    let retention_duration = Duration::hours(retention_hours as i64);
    let cutoff_time = snap.now_utc - retention_duration;

    debug!(
        "Checking for stale worktrees (retention: {}h, cutoff: {}, now: {})",
        retention_hours, cutoff_time, snap.now_utc
    );

    for assignment in &snap.worktree_assignments {
        // Skip if no completion time (task not completed yet)
        let Some(completed_at) = assignment.completed_at else {
            continue;
        };

        // Skip if not past retention period
        if completed_at > cutoff_time {
            let remaining = completed_at + retention_duration - snap.now_utc;
            debug!(
                "Worktree {} completed recently ({}h remaining)",
                assignment.worktree_id,
                remaining.num_hours()
            );
            continue;
        }

        // Skip if actively bound to a coworker
        if let Some(ref coworker) = assignment.current_coworker
            && snap.active_names.contains(&coworker.to_lowercase())
        {
            debug!(
                "Skipping cleanup of {} - actively bound to coworker {}",
                assignment.worktree_id, coworker
            );
            continue;
        }

        // Skip if associated with an open PR (those are handled by PR merge cleanup)
        if let Some(pr_number) = assignment.pr_number
            && !snap.merged_pr_numbers.contains(&pr_number)
        {
            debug!(
                "Skipping cleanup of {} - PR #{} is still open",
                assignment.worktree_id, pr_number
            );
            continue;
        }

        // This worktree is stale and ready for cleanup
        let task_id = assignment
            .task_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let age = snap.now_utc - completed_at;
        debug!(
            "Marking worktree {} for cleanup (task !{}, completed {}h ago)",
            assignment.worktree_id,
            task_id,
            age.num_hours()
        );

        effects.push(Effect::CleanupStaleWorktree {
            worktree_id: assignment.worktree_id.clone(),
            task_id,
        });
    }

    if !effects.is_empty() {
        debug!("Generated {} stale worktree cleanup effects", effects.len());
    }

    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn test_collect_stale_worktree_cleanup_effects_no_stale_worktrees() {
        // Build a snapshot with no completed tasks
        let snap = WorldSnapshot {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            session_name: "test".to_string(),
            repo_name: "test-repo".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_numbers: HashSet::new(),
            merged_pr_branches: HashMap::new(),
            worktree_assignments: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: Utc::now(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let effects = collect_stale_worktree_cleanup_effects(&snap, 24);
        assert!(
            effects.is_empty(),
            "should not generate cleanup effects when no worktrees are stale"
        );
    }

    #[test]
    fn test_collect_stale_worktree_cleanup_effects_finds_stale_worktrees() {
        use crate::worktree_registry::WorktreeAssignment;
        use chrono::Duration;

        let now = Utc::now();
        let stale_time = now - Duration::hours(25); // Completed 25h ago
        let recent_time = now - Duration::hours(12); // Completed 12h ago

        // Build worktree assignments for testing
        let mut assignments = vec![];

        // Worktree 1: Stale (completed 25h ago, no active coworker, no open PR)
        assignments.push(WorktreeAssignment {
            worktree_id: "task-42-old-work".to_string(),
            branch_name: "task-42-old-work".to_string(),
            task_id: Some("42".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: now - Duration::hours(30),
            completed_at: Some(stale_time),
        });

        // Worktree 2: Not stale (completed 12h ago, within retention)
        assignments.push(WorktreeAssignment {
            worktree_id: "task-100-recent".to_string(),
            branch_name: "task-100-recent".to_string(),
            task_id: Some("100".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: now - Duration::hours(15),
            completed_at: Some(recent_time),
        });

        // Worktree 3: Stale but actively bound to a coworker
        let mut active_names = HashSet::new();
        active_names.insert("park".to_string());
        assignments.push(WorktreeAssignment {
            worktree_id: "task-200-active".to_string(),
            branch_name: "task-200-active".to_string(),
            task_id: Some("200".to_string()),
            current_coworker: Some("park".to_string()),
            pr_number: None,
            created_at: now - Duration::hours(30),
            completed_at: Some(stale_time),
        });

        // Worktree 4: Stale but has open PR
        assignments.push(WorktreeAssignment {
            worktree_id: "task-300-open-pr".to_string(),
            branch_name: "task-300-open-pr".to_string(),
            task_id: Some("300".to_string()),
            current_coworker: None,
            pr_number: Some(555),
            created_at: now - Duration::hours(30),
            completed_at: Some(stale_time),
        });

        // Worktree 5: No completion time (task not completed)
        assignments.push(WorktreeAssignment {
            worktree_id: "task-400-in-progress".to_string(),
            branch_name: "task-400-in-progress".to_string(),
            task_id: Some("400".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: now - Duration::hours(10),
            completed_at: None,
        });

        let snap = WorldSnapshot {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names,
            active_session_ids: HashSet::new(),
            session_name: "test".to_string(),
            repo_name: "test-repo".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_numbers: HashSet::new(),
            merged_pr_branches: HashMap::new(),
            worktree_assignments: assignments,
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: now,
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let effects = collect_stale_worktree_cleanup_effects(&snap, 24);

        // Should only cleanup worktree 1 (task-42-old-work)
        // - task-100 is too recent
        // - task-200 is actively bound to a coworker
        // - task-300 has an open PR
        // - task-400 has no completion time
        assert_eq!(effects.len(), 1, "should generate 1 cleanup effect");

        if let Effect::CleanupStaleWorktree {
            worktree_id,
            task_id,
        } = &effects[0]
        {
            assert_eq!(worktree_id, "task-42-old-work");
            assert_eq!(task_id, "42");
        } else {
            panic!("Expected CleanupStaleWorktree effect");
        }
    }
}
