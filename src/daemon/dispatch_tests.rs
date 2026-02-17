use super::*;

fn in_progress_task_for_lookup(task_id: &str, subject: &str, owner: &str) -> crate::tasks::Task {
    crate::tasks::Task {
        id: task_id.to_string(),
        subject: subject.to_string(),
        status: crate::tasks::TaskStatus::InProgress,
        owner: Some(owner.to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    }
}

#[test]
fn test_duplicate_worker_sorting_by_start_time() {
    use chrono::{Duration, Utc};

    let now = Utc::now();
    let earlier = now - Duration::minutes(5);
    let later = now + Duration::minutes(5);

    let mut workers: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = vec![
        ("later_worker".to_string(), Some(later)),
        ("earlier_worker".to_string(), Some(earlier)),
        ("now_worker".to_string(), Some(now)),
    ];

    workers.sort_by(|a, b| match (&a.1, &b.1) {
        (Some(t1), Some(t2)) => t1.cmp(t2),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    assert_eq!(workers[0].0, "earlier_worker");
    assert_eq!(workers[1].0, "now_worker");
    assert_eq!(workers[2].0, "later_worker");
}

#[test]
fn test_duplicate_worker_sorting_with_unknown_times() {
    use chrono::Utc;

    let now = Utc::now();

    let mut workers: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = vec![
        ("unknown_worker".to_string(), None),
        ("known_worker".to_string(), Some(now)),
        ("another_unknown".to_string(), None),
    ];

    workers.sort_by(|a, b| match (&a.1, &b.1) {
        (Some(t1), Some(t2)) => t1.cmp(t2),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });

    assert_eq!(workers[0].0, "known_worker");
    assert!(workers[1].1.is_none());
    assert!(workers[2].1.is_none());
}

#[test]
fn test_filter_orphans_with_open_prs_filters_by_owner() {
    let flagged = vec![
        "amsterdam".to_string(),
        "riverside".to_string(),
        "park".to_string(),
    ];
    let open_pr_owners: HashSet<String> = ["riverside".to_string()].into_iter().collect();

    let result = filter_orphans_with_open_prs(flagged, &open_pr_owners);
    assert_eq!(result, vec!["amsterdam", "park"]);
}

#[test]
fn test_filter_orphans_with_open_prs_all_have_open_prs() {
    let flagged = vec!["amsterdam".to_string(), "riverside".to_string()];
    let open_pr_owners: HashSet<String> = ["amsterdam".to_string(), "riverside".to_string()]
        .into_iter()
        .collect();

    let result = filter_orphans_with_open_prs(flagged, &open_pr_owners);
    assert!(result.is_empty());
}

#[test]
fn test_filter_orphans_with_open_prs_none_have_open_prs() {
    let flagged = vec!["amsterdam".to_string(), "park".to_string()];
    let open_pr_owners: HashSet<String> = HashSet::new();

    let result = filter_orphans_with_open_prs(flagged, &open_pr_owners);
    assert_eq!(result, vec!["amsterdam", "park"]);
}

#[test]
fn test_should_recover_task_with_explicit_pr_association() {
    // Bug context: Task 1142 had "PR #940 fix insufficient" in the subject as context,
    // not as the actual task's PR. The task's real PR would be different.
    // should_recover_task() should use the explicit pr field, not extract_pr_numbers_from_text().
    use std::collections::HashSet;
    use std::path::Path;

    let merged_prs: HashSet<u64> = vec![940].into_iter().collect();
    let repo_path = Path::new("/tmp/test-repo");

    // Task with PR #940 mentioned in subject as context, but explicit pr field is None
    // (because the task's actual work will create a different PR)
    let task = crate::tasks::Task {
        id: "1142".to_string(),
        subject: "Fix remaining orphan worktree false positives — PR #940 fix insufficient"
            .to_string(),
        status: crate::tasks::TaskStatus::InProgress,
        owner: Some("amsterdam".to_string()),
        description: Some("The fix in PR #940 suppresses warnings...".to_string()),
        blocked_by: vec![],
        channel: Some("midtown".to_string()),
        pr: None, // No explicit PR association yet — task will create a new PR
        created_at: None,
    };

    // EXPECTED: should_recover_task returns true (allow recovery)
    // ACTUAL (before fix): returns false because extract_pr_numbers_from_text finds #940 in subject
    let tasks_with_open_prs = HashMap::new();
    let result = should_recover_task(
        &task,
        &merged_prs,
        repo_path,
        &tasks_with_open_prs,
        &HashMap::new(),
    );
    assert!(
        result,
        "should_recover_task should return true when explicit pr field is None, even if merged PR mentioned in text"
    );

    // Now test with explicit PR association
    let task_with_pr = crate::tasks::Task {
        pr: Some(940),
        ..task
    };

    let result = should_recover_task(
        &task_with_pr,
        &merged_prs,
        repo_path,
        &tasks_with_open_prs,
        &HashMap::new(),
    );
    assert!(
        !result,
        "should_recover_task should return false when explicit pr field matches merged PR"
    );
}

#[test]
fn test_partition_orphans_by_merged_status_exact_match() {
    // Scenario: york has a squash-merged PR on branch "york/feature-a".
    // The worktree shows "unmerged commits" because commit SHAs differ,
    // but the PR was actually merged.
    // York should be in the "merged" partition, amsterdam in "unmerged".
    let flagged = vec![
        "amsterdam".to_string(), // genuinely orphaned, branch: amsterdam/abandoned
        "york".to_string(),      // has merged PR, branch: york/feature-a
    ];
    let merged_pr_branches: HashSet<String> = ["york/feature-a".to_string()].into_iter().collect();

    // Mock function that returns branch names for each coworker
    let get_branch = |name: &str| -> Option<String> {
        match name {
            "york" => Some("york/feature-a".to_string()),
            "amsterdam" => Some("amsterdam/abandoned".to_string()),
            _ => None,
        }
    };

    let (merged, unmerged) =
        partition_orphans_by_merged_status(flagged, &merged_pr_branches, get_branch);

    // york's exact branch was merged - should be in merged partition
    assert_eq!(merged, vec!["york"]);
    // amsterdam is genuinely orphaned - should be in unmerged partition
    assert_eq!(unmerged, vec!["amsterdam"]);
}

#[test]
fn test_partition_orphans_by_merged_status_different_branch() {
    // Scenario: york has a merged PR on branch "york/old-feature" but is now
    // working on "york/new-feature" which is orphaned.
    // The new branch should be in the "unmerged" partition.
    let flagged = vec!["york".to_string()];
    let merged_pr_branches: HashSet<String> =
        ["york/old-feature".to_string()].into_iter().collect();

    // York's current branch is different from the merged one
    let get_branch = |name: &str| -> Option<String> {
        match name {
            "york" => Some("york/new-feature".to_string()),
            _ => None,
        }
    };

    let (merged, unmerged) =
        partition_orphans_by_merged_status(flagged, &merged_pr_branches, get_branch);

    // york has a different branch - should be in unmerged partition
    assert!(merged.is_empty());
    assert_eq!(unmerged, vec!["york"]);
}

#[test]
fn test_partition_orphans_by_merged_status_detached_head() {
    // Scenario: worktree is in detached HEAD state.
    // Worktrees only reach partition if safe_cleanup() returned false.
    // For detached HEAD, has_commits_beyond_base() returns false, so the only
    // reason it's flagged is has_uncommitted_changes() returned true.
    // We must warn (unmerged) rather than force-delete (merged) to prevent data loss.
    let flagged = vec![
        "columbus".to_string(), // detached HEAD, get_branch returns None
        "york".to_string(),     // has branch with unmerged commits
    ];
    let merged_pr_branches: HashSet<String> = HashSet::new();

    // columbus is in detached HEAD (None), york has a branch
    let get_branch = |name: &str| -> Option<String> {
        match name {
            "columbus" => None, // Detached HEAD
            "york" => Some("york/feature-a".to_string()),
            _ => None,
        }
    };

    let (merged, unmerged) =
        partition_orphans_by_merged_status(flagged, &merged_pr_branches, get_branch);

    // columbus is detached HEAD - goes to unmerged to warn Lead about uncommitted changes
    // york has a branch not in merged list - also goes to unmerged
    assert!(merged.is_empty());
    assert_eq!(unmerged, vec!["columbus", "york"]);
}

#[test]
fn test_should_skip_orphan_flagging_before_pr_poll() {
    // During startup, PR poll hasn't run yet - should skip flagging
    // to avoid false positives (worktrees with open PRs incorrectly
    // flagged as orphaned because we don't have PR data yet)
    assert!(should_skip_orphan_flagging(false));
}

#[test]
fn test_should_not_skip_orphan_flagging_after_pr_poll() {
    // After first PR poll completes, we have open_pr_owners data
    // and can safely flag orphans
    assert!(!should_skip_orphan_flagging(true));
}

#[test]
fn test_compute_orphans_for_reviewer_clearing_skips_before_pr_poll() {
    // Bug scenario: During startup, PR poll hasn't run yet. If we clear
    // reviewer assignments, we'd incorrectly clear them for coworkers who
    // have open PRs (because open_pr_owners is empty until PR poll runs).
    let all_orphaned = vec!["amsterdam".to_string(), "york".to_string()];
    let open_pr_owners: HashSet<String> = HashSet::new(); // Empty during startup

    // Before PR poll initialized, should return None (skip clearing)
    let result = compute_orphans_for_reviewer_clearing(false, all_orphaned, &open_pr_owners);
    assert!(
        result.is_none(),
        "Should skip reviewer clearing before PR poll initializes"
    );
}

#[test]
fn test_compute_orphans_for_reviewer_clearing_filters_open_pr_owners() {
    // After PR poll: amsterdam has an open PR, york doesn't.
    // Only york should have their reviewer assignment cleared.
    let all_orphaned = vec!["amsterdam".to_string(), "york".to_string()];
    let open_pr_owners: HashSet<String> = ["amsterdam".to_string()].into_iter().collect();

    let result = compute_orphans_for_reviewer_clearing(true, all_orphaned, &open_pr_owners);
    assert_eq!(
        result,
        Some(vec!["york".to_string()]),
        "Should only clear reviewer assignments for orphans without open PRs"
    );
}

#[test]
fn test_compute_orphans_for_reviewer_clearing_all_have_open_prs() {
    // All orphaned coworkers have open PRs - should return None
    let all_orphaned = vec!["amsterdam".to_string(), "york".to_string()];
    let open_pr_owners: HashSet<String> = ["amsterdam".to_string(), "york".to_string()]
        .into_iter()
        .collect();

    let result = compute_orphans_for_reviewer_clearing(true, all_orphaned, &open_pr_owners);
    assert!(
        result.is_none(),
        "Should return None when all orphans have open PRs"
    );
}

#[test]
fn test_compute_orphans_for_reviewer_clearing_none_orphaned() {
    // No orphaned worktrees - should return None
    let all_orphaned: Vec<String> = vec![];
    let open_pr_owners: HashSet<String> = HashSet::new();

    let result = compute_orphans_for_reviewer_clearing(true, all_orphaned, &open_pr_owners);
    assert!(result.is_none(), "Should return None when no orphans");
}

#[test]
fn test_build_task_completion_effects_with_task_id() {
    let effects =
        build_task_completion_effects("feat: Add auth endpoint [Midtown #42]", 123, "myrepo");

    assert_eq!(effects.len(), 3, "Should return 3 effects");

    // Verify CompleteTask effect
    match &effects[0] {
        Effect::CompleteTask { task_id, repo_name } => {
            assert_eq!(task_id, "42");
            assert_eq!(repo_name, "myrepo");
        }
        _ => panic!("First effect should be CompleteTask"),
    }

    // Verify ClearBlockedBy effect
    match &effects[1] {
        Effect::ClearBlockedBy {
            completed_task_id,
            repo_name,
        } => {
            assert_eq!(completed_task_id, "42");
            assert_eq!(repo_name, "myrepo");
        }
        _ => panic!("Second effect should be ClearBlockedBy"),
    }

    // Verify PostToChannel effect
    match &effects[2] {
        Effect::PostToChannel {
            sender, message, ..
        } => {
            assert_eq!(sender, "midtown");
            assert!(message.contains("42"));
            assert!(message.contains("123"));
        }
        _ => panic!("Third effect should be PostToChannel"),
    }
}

#[test]
fn test_build_task_completion_effects_without_task_id() {
    let effects = build_task_completion_effects("feat: Add auth endpoint", 123, "myrepo");

    assert!(
        effects.is_empty(),
        "Should return empty vec when no task ID in title"
    );
}

#[test]
fn test_build_task_completion_effects_message_says_merged() {
    let effects =
        build_task_completion_effects("feat: Add auth endpoint [Midtown #42]", 123, "myrepo");

    // Verify the channel message says "merged" not "opened"
    match &effects[2] {
        Effect::PostToChannel {
            sender, message, ..
        } => {
            assert_eq!(sender, "midtown");
            assert!(
                message.contains("merged"),
                "Message should say 'merged', got: {}",
                message
            );
            assert!(
                !message.contains("opened"),
                "Message should not say 'opened', got: {}",
                message
            );
        }
        _ => panic!("Third effect should be PostToChannel"),
    }
}

#[test]
fn test_description_based_completion_all_prs_merged() {
    use crate::tasks::{Task, TaskStatus};
    use std::collections::HashSet;

    // Task with description referencing multiple PRs
    let task = Task {
        id: "1100".to_string(),
        subject: "Meta task".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("york".to_string()),
        description: Some("Merge reviewed PRs: #901, #902, #903".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    // All referenced PRs are merged
    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(901);
    merged_pr_numbers.insert(902);
    merged_pr_numbers.insert(903);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        merged_pr_numbers,
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_description_based_completion_effects(&snap);

    assert_eq!(effects.len(), 3, "Should return 3 effects");

    // Verify CompleteTask effect
    match &effects[0] {
        Effect::CompleteTask { task_id, repo_name } => {
            assert_eq!(task_id, "1100");
            assert_eq!(repo_name, "test-repo");
        }
        _ => panic!("First effect should be CompleteTask"),
    }

    // Verify channel message mentions all PRs
    match &effects[2] {
        Effect::PostToChannel { message, .. } => {
            assert!(message.contains("#901"));
            assert!(message.contains("#902"));
            assert!(message.contains("#903"));
        }
        _ => panic!("Third effect should be PostToChannel"),
    }
}

#[test]
fn test_description_based_completion_some_prs_not_merged() {
    use crate::tasks::{Task, TaskStatus};
    use std::collections::HashSet;

    let task = Task {
        id: "1101".to_string(),
        subject: "Meta task".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("york".to_string()),
        description: Some("Merge PRs: #901, #902, #903".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    // Only some PRs are merged
    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(901);
    merged_pr_numbers.insert(902);
    // PR #903 is NOT merged

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        merged_pr_numbers,
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_description_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete task when not all PRs are merged"
    );
}

#[test]
fn test_description_based_completion_no_pr_references() {
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "1102".to_string(),
        subject: "Some task".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("york".to_string()),
        description: Some("No PR references in this description".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_description_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete task with no PR references"
    );
}

#[test]
fn test_description_based_completion_skips_pending_tasks() {
    use crate::tasks::{Task, TaskStatus};
    use std::collections::HashSet;

    let task = Task {
        id: "1103".to_string(),
        subject: "Pending task".to_string(),
        status: TaskStatus::Pending, // Not InProgress
        owner: None,
        description: Some("Fix PR #904".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(904);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        merged_pr_numbers,
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_description_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete non-InProgress tasks"
    );
}

#[test]
fn test_description_based_completion_no_description() {
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "1104".to_string(),
        subject: "Task without description".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("york".to_string()),
        description: None, // No description
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_description_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete task with no description"
    );
}

#[test]
fn test_description_based_completion_skips_already_completed_tasks() {
    use crate::tasks::{Task, TaskStatus};
    use std::collections::HashSet;

    // Simulate a task that was already completed by the webhook/title-based path.
    // The description-based path should skip it to avoid double-completion.
    let completed_task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        status: TaskStatus::Completed, // Already completed by title-based path
        owner: Some("york".to_string()),
        description: Some("Fix PR #904 review feedback".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    // Also add an in_progress task with PR references
    let in_progress_task = Task {
        id: "43".to_string(),
        subject: "Meta task".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("york".to_string()),
        description: Some("Merge PRs: #904, #905".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(904);
    merged_pr_numbers.insert(905);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![completed_task, in_progress_task],
        merged_pr_numbers,
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_description_based_completion_effects(&snap);

    // Should only produce effects for task 43, not task 42
    let complete_task_ids: Vec<&String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::CompleteTask { task_id, .. } => Some(task_id),
            _ => None,
        })
        .collect();

    assert_eq!(complete_task_ids, vec!["43"]);
    assert!(
        !complete_task_ids.contains(&&"42".to_string()),
        "Should not double-complete already-completed task 42"
    );
}

// ======================================================================
// decide_orphan_cleanup tests
// ======================================================================

#[test]
fn test_decide_orphan_cleanup_empty_data() {
    let data = OrphanCleanupData {
        all_orphaned: vec![],
        merged_worktrees_to_cleanup: vec![],
        pr_poll_initialized: true,
        open_pr_owners: HashSet::new(),
        gh_cleaned: vec![],
        due_for_warning: vec![],
        stale_branch_cleanup_due: false,
    };
    let effects = decide_orphan_cleanup(&data);
    assert!(effects.is_empty());
}

#[test]
fn test_decide_orphan_cleanup_clears_reviewer_assignments() {
    let data = OrphanCleanupData {
        all_orphaned: vec!["amsterdam".to_string(), "york".to_string()],
        merged_worktrees_to_cleanup: vec![],
        pr_poll_initialized: true,
        open_pr_owners: ["amsterdam".to_string()].into_iter().collect(),
        gh_cleaned: vec![],
        due_for_warning: vec![],
        stale_branch_cleanup_due: false,
    };
    let effects = decide_orphan_cleanup(&data);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ClearOrphanedReviewerAssignments { orphaned_coworkers } => {
            assert_eq!(orphaned_coworkers, &vec!["york".to_string()]);
        }
        _ => panic!("Expected ClearOrphanedReviewerAssignments"),
    }
}

#[test]
fn test_decide_orphan_cleanup_force_deletes_merged_worktrees() {
    let data = OrphanCleanupData {
        all_orphaned: vec![],
        merged_worktrees_to_cleanup: vec!["york".to_string(), "park".to_string()],
        pr_poll_initialized: true,
        open_pr_owners: HashSet::new(),
        gh_cleaned: vec![],
        due_for_warning: vec![],
        stale_branch_cleanup_due: false,
    };
    let effects = decide_orphan_cleanup(&data);
    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::ForceCleanupWorktrees { names } => {
            assert_eq!(names, &vec!["york".to_string(), "park".to_string()]);
        }
        _ => panic!("Expected ForceCleanupWorktrees"),
    }
}

#[test]
fn test_decide_orphan_cleanup_warns_about_unmerged() {
    let data = OrphanCleanupData {
        all_orphaned: vec![],
        merged_worktrees_to_cleanup: vec![],
        pr_poll_initialized: true,
        open_pr_owners: HashSet::new(),
        gh_cleaned: vec![],
        due_for_warning: vec!["amsterdam".to_string()],
        stale_branch_cleanup_due: false,
    };
    let effects = decide_orphan_cleanup(&data);
    // Should produce: PostSystemMessage, NudgeLead, SendPushNotification
    assert_eq!(effects.len(), 3);
    assert!(matches!(&effects[0], Effect::PostSystemMessage { .. }));
    assert!(matches!(&effects[1], Effect::NudgeLead { .. }));
    assert!(matches!(
        &effects[2],
        Effect::SendPushNotification { tag, .. } if tag == "orphan_warning"
    ));
}

#[test]
fn test_decide_orphan_cleanup_full_scenario() {
    // All three kinds of effects: reviewer clearing, force cleanup, warnings
    let data = OrphanCleanupData {
        all_orphaned: vec![
            "amsterdam".to_string(),
            "york".to_string(),
            "park".to_string(),
        ],
        merged_worktrees_to_cleanup: vec!["york".to_string()],
        pr_poll_initialized: true,
        open_pr_owners: ["amsterdam".to_string()].into_iter().collect(),
        gh_cleaned: vec![],
        due_for_warning: vec!["park".to_string()],
        stale_branch_cleanup_due: false,
    };
    let effects = decide_orphan_cleanup(&data);
    // ClearOrphanedReviewerAssignments(york, park) + ForceCleanupWorktrees(york) +
    // PostSystemMessage + NudgeLead + SendPushNotification
    assert_eq!(effects.len(), 5);
    assert!(matches!(
        &effects[0],
        Effect::ClearOrphanedReviewerAssignments { .. }
    ));
    assert!(matches!(&effects[1], Effect::ForceCleanupWorktrees { .. }));
    assert!(matches!(&effects[2], Effect::PostSystemMessage { .. }));
    assert!(matches!(&effects[3], Effect::NudgeLead { .. }));
    assert!(matches!(&effects[4], Effect::SendPushNotification { .. }));
}

#[test]
fn test_decide_orphan_cleanup_gh_cleaned_posts_to_channel() {
    let data = OrphanCleanupData {
        all_orphaned: vec![],
        merged_worktrees_to_cleanup: vec![],
        pr_poll_initialized: true,
        open_pr_owners: HashSet::new(),
        gh_cleaned: vec!["york".to_string(), "park".to_string()],
        due_for_warning: vec![],
        stale_branch_cleanup_due: false,
    };
    let effects = decide_orphan_cleanup(&data);
    assert_eq!(effects.len(), 2);
    for effect in &effects {
        assert!(matches!(effect, Effect::PostToChannel { sender, .. } if sender == "midtown"));
    }
}

#[test]
fn test_decide_orphan_cleanup_stale_branch_cleanup() {
    let data = OrphanCleanupData {
        all_orphaned: vec![],
        merged_worktrees_to_cleanup: vec![],
        pr_poll_initialized: true,
        open_pr_owners: HashSet::new(),
        gh_cleaned: vec![],
        due_for_warning: vec![],
        stale_branch_cleanup_due: true,
    };
    let effects = decide_orphan_cleanup(&data);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::CleanStaleBranches));
}

// ======================================================================
// decide_discovered_coworker_nudges tests
// ======================================================================

#[test]
fn test_discovered_nudges_empty() {
    let effects = decide_discovered_coworker_nudges(&[], &HashMap::new(), &HashMap::new());
    assert!(effects.is_empty());
}

#[test]
fn test_discovered_nudges_task_owner() {
    let discovered = vec!["lexington".to_string()];
    let mut owner_tasks = HashMap::new();
    owner_tasks.insert(
        "lexington".to_string(),
        ("42".to_string(), "Fix auth bug".to_string(), None),
    );
    let reviewer_prs = HashMap::new();

    let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
    // NudgeCoworker + PostToChannel
    assert_eq!(effects.len(), 2);
    match &effects[0] {
        Effect::NudgeCoworker { name, message, .. } => {
            assert_eq!(name, "lexington");
            assert!(message.contains("Resume task !42"));
        }
        _ => panic!("Expected NudgeCoworker"),
    }
    match &effects[1] {
        Effect::PostToChannel {
            sender, message, ..
        } => {
            assert_eq!(sender, "midtown");
            assert!(message.contains("lexington"));
            assert!(message.contains("task !42"));
        }
        _ => panic!("Expected PostToChannel"),
    }
}

#[test]
fn test_discovered_nudges_reviewer() {
    let discovered = vec!["park".to_string()];
    let owner_tasks = HashMap::new();
    let mut reviewer_prs = HashMap::new();
    reviewer_prs.insert("park".to_string(), 99);

    let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
    // NudgeCoworker + PostToChannel
    assert_eq!(effects.len(), 2);
    match &effects[0] {
        Effect::NudgeCoworker { name, .. } => {
            assert_eq!(name, "park");
        }
        _ => panic!("Expected NudgeCoworker"),
    }
    match &effects[1] {
        Effect::PostToChannel { message, .. } => {
            assert!(message.contains("PR #99"));
        }
        _ => panic!("Expected PostToChannel"),
    }
}

#[test]
fn test_discovered_nudges_no_assignment() {
    let discovered = vec!["broadway".to_string()];
    let owner_tasks = HashMap::new();
    let reviewer_prs = HashMap::new();

    let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
    assert!(
        effects.is_empty(),
        "Coworker with no task or review should produce no effects"
    );
}

#[test]
fn test_discovered_nudges_mixed() {
    let discovered = vec![
        "lexington".to_string(), // has task
        "park".to_string(),      // has review
        "broadway".to_string(),  // no assignment
    ];
    let mut owner_tasks = HashMap::new();
    owner_tasks.insert(
        "lexington".to_string(),
        ("42".to_string(), "Fix auth bug".to_string(), None),
    );
    let mut reviewer_prs = HashMap::new();
    reviewer_prs.insert("park".to_string(), 99);

    let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
    // lexington: NudgeCoworker + PostToChannel = 2
    // park: NudgeCoworker + PostToChannel = 2
    // broadway: nothing = 0
    assert_eq!(effects.len(), 4);
}

#[test]
fn test_discovered_nudges_task_takes_priority_over_review() {
    // If a coworker has both a task and a review assignment,
    // the task takes priority (task check comes first in code)
    let discovered = vec!["lexington".to_string()];
    let mut owner_tasks = HashMap::new();
    owner_tasks.insert(
        "lexington".to_string(),
        ("42".to_string(), "Fix auth bug".to_string(), None),
    );
    let mut reviewer_prs = HashMap::new();
    reviewer_prs.insert("lexington".to_string(), 99);

    let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
    assert_eq!(effects.len(), 2);
    // Should nudge about the task, not the review
    match &effects[0] {
        Effect::NudgeCoworker { message, .. } => {
            assert!(message.contains("Resume task !42"));
        }
        _ => panic!("Expected NudgeCoworker"),
    }
}

#[test]
fn test_discovered_nudges_routes_to_task_channel() {
    let discovered = vec!["lexington".to_string()];
    let mut owner_tasks = HashMap::new();
    owner_tasks.insert(
        "lexington".to_string(),
        (
            "42".to_string(),
            "Fix auth bug".to_string(),
            Some("feature-auth".to_string()),
        ),
    );
    let reviewer_prs = HashMap::new();

    let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
    assert_eq!(effects.len(), 2);
    // Check that PostToChannel uses the task's channel
    match &effects[1] {
        Effect::PostToChannel { channel, .. } => {
            assert_eq!(channel, &Some("feature-auth".to_string()));
        }
        _ => panic!("Expected PostToChannel"),
    }
}

// ======================================================================
// WorktreeRegistry integration tests
// ======================================================================

#[test]
fn test_spawn_for_pending_tasks_generates_registry_effects_new_task() {
    use crate::tasks::{Task, TaskStatus};
    use std::time::SystemTime;

    // Setup: create a snapshot with a pending task (no owner, not in registry)
    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            created_at: Some(SystemTime::now()),
        }],
        tasks_with_worktrees: HashSet::new(), // Task not in registry yet
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_dev_limit: false,
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Pre-spawn effects (EnsureWorktree, RegisterWorktreeAssignment) are top-level,
    // followed by AssignAndSpawn with post-spawn effects in on_success.
    assert!(
        effects.len() >= 2,
        "Should have pre-spawn effects + AssignAndSpawn"
    );

    // EnsureWorktree should be a top-level effect (before spawn)
    let ensure_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
        .count();
    assert_eq!(ensure_count, 1, "Should have top-level EnsureWorktree");

    // RegisterWorktreeAssignment should be a top-level effect (before spawn)
    let register_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
        .count();
    assert_eq!(
        register_count, 1,
        "Should have top-level RegisterWorktreeAssignment"
    );

    let assign_and_spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::AssignAndSpawn {
                task_id,
                owner,
                on_success,
                ..
            } = e
            {
                Some((task_id, owner, on_success))
            } else {
                None
            }
        })
        .expect("Should have AssignAndSpawn effect");

    assert_eq!(assign_and_spawn.0, "42");

    // BindCoworkerToWorktree stays in on_success (runs after spawn)
    let bind_count = assign_and_spawn
        .2
        .iter()
        .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
        .count();

    assert_eq!(
        bind_count, 1,
        "Should have BindCoworkerToWorktree in on_success"
    );
    assert_eq!(
        bind_count, 1,
        "Should have BindCoworkerToWorktree in on_success"
    );

    // Verify the top-level RegisterWorktreeAssignment has correct fields
    let register_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::RegisterWorktreeAssignment { assignment } = e {
                Some(assignment)
            } else {
                None
            }
        })
        .expect("Should have top-level RegisterWorktreeAssignment");

    assert_eq!(register_effect.task_id, Some("42".to_string()));
    assert!(register_effect.worktree_id.starts_with("task-42-"));
    assert_eq!(register_effect.branch_name, register_effect.worktree_id);
}

#[test]
fn test_spawn_for_pending_tasks_reuses_worktree_for_owned_task() {
    // Setup: pending task with owner, task already in registry
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        tasks_with_worktrees: ["42".to_string()].into_iter().collect(), // Task already has worktree
        task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
            .into_iter()
            .collect(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        worktree_branch_owners: HashMap::new(),
        merged_pr_branches: HashMap::new(),
        is_at_dev_limit: false,
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        active_reviewers: HashSet::new(),
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        merged_pr_numbers: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        all_tasks: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations: HashMap::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Find the SpawnCoworkerWithCallbacks effect (for owned pending tasks, uses this variant)
    let spawn_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                Some(on_success)
            } else {
                None
            }
        })
        .expect("Should have SpawnCoworkerWithCallbacks effect for owned pending task");

    // Should NOT generate RegisterWorktreeAssignment (worktree already exists)
    // SHOULD generate BindCoworkerToWorktree (rebind to new owner)
    let register_count = spawn_effect
        .iter()
        .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
        .count();
    let bind_count = spawn_effect
        .iter()
        .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
        .count();

    assert_eq!(
        register_count, 0,
        "Should NOT generate RegisterWorktreeAssignment for existing worktree"
    );
    assert_eq!(
        bind_count, 1,
        "Should generate BindCoworkerToWorktree to rebind"
    );
}

#[test]
fn test_spawn_for_pending_tasks_skips_when_owner_has_pending_task() {
    // Scenario: Task !1063 is pending with owner=broadway, but broadway ALSO
    // owns task !1062 which is ALSO still pending (not yet in_progress).
    // This happens when:
    // 1. Broadway is spawned for task !1062 (pending → assigned to broadway)
    // 2. Before broadway claims !1062 (sets it to in_progress), task !1063
    //    is assigned to broadway via grouping logic
    // 3. Now both tasks are pending, owner=broadway, but broadway doesn't
    //    exist yet (spawn may have failed or is still starting)
    // 4. The daemon should NOT try to spawn broadway again for !1063
    //
    // This reproduces the bug where the daemon repeatedly tried to spawn
    // broadway for !1063 every 5 seconds.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![
            (
                "1062".to_string(),
                "Some other task".to_string(),
                "broadway".to_string(),
            ),
            (
                "1063".to_string(),
                "Address review feedback on PR #869 and merge".to_string(),
                "broadway".to_string(),
            ),
        ],
        // broadway is NOT active (spawn failed or hasn't completed yet)
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        // broadway has NO in_progress tasks (both are still pending)
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        in_progress_tasks: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        merged_pr_numbers: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        attached_coworkers: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Count how many SpawnCoworkerWithCallbacks effects are generated for broadway
    let spawn_count = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnCoworkerWithCallbacks { config, .. }
                    if config.name.to_lowercase() == "broadway"
            )
        })
        .count();

    // Without the fix, this would generate TWO spawns for broadway (one per pending task).
    // The coworkers_dispatched_this_tick set prevents this by tracking spawned coworkers.
    assert!(
        spawn_count <= 1,
        "Should generate at most ONE spawn for broadway, got {}. Multiple pending tasks \
         with the same owner should not cause duplicate spawns in the same tick.",
        spawn_count
    );
}

#[test]
fn test_spawn_owner_includes_record_task_assignment_for_cross_tick_dedup() {
    // Verify that SpawnCoworkerWithCallbacks from the SpawnOwner branch
    // includes RecordTaskAssignment in on_success, enabling
    // mark_in_flight_spawns_from_effects() to track it across ticks.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "broadway".to_string(),
        )],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        in_progress_tasks: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        merged_pr_numbers: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        attached_coworkers: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Find the SpawnCoworkerWithCallbacks effect
    let spawn_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                Some(on_success)
            } else {
                None
            }
        })
        .expect("Should have SpawnCoworkerWithCallbacks for broadway");

    // Verify RecordTaskAssignment is in on_success
    let has_record = spawn_effect.iter().any(|e| {
        matches!(
            e,
            Effect::RecordTaskAssignment { coworker, task_id }
                if coworker == "broadway" && task_id == "42"
        )
    });
    assert!(
        has_record,
        "SpawnCoworkerWithCallbacks on_success must include RecordTaskAssignment \
         for cross-tick spawn deduplication"
    );
}

#[test]
fn test_cross_tick_dedup_skips_in_flight_owned_task() {
    // Simulate two consecutive ticks: the first tick spawned broadway for
    // task !42 (marking it in-flight), the second tick should skip it.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "broadway".to_string(),
        )],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        in_progress_tasks: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        merged_pr_numbers: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        attached_coworkers: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();

    // Simulate tick 1: generates spawn effects
    let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
    let spawn_count_tick1 = effects_tick1
        .iter()
        .filter(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }))
        .count();
    assert_eq!(spawn_count_tick1, 1, "Tick 1 should spawn broadway");

    // Mark in-flight (normally done by the daemon between ticks)
    state.mark_in_flight_spawns_from_effects(&effects_tick1);

    // Simulate tick 2: should skip because task !42 is already in-flight
    let effects_tick2 = spawn_for_pending_tasks(&snap, &state);
    let spawn_count_tick2 = effects_tick2
        .iter()
        .filter(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }))
        .count();
    assert_eq!(
        spawn_count_tick2, 0,
        "Tick 2 should NOT re-spawn broadway — task !42 is already in-flight"
    );
}

#[test]
fn test_cross_case_dedup_prevents_same_coworker_from_case1_and_case2() {
    // Scenario: Task !42 is pending with owner=broadway (Case 1),
    // and task !43 is pending WITHOUT owner but references PR #100
    // which broadway is working on (Case 2 would group it to broadway).
    // Case 2 should skip broadway because Case 1 already dispatched it.
    use crate::tasks::Task;

    let snap = snapshot::WorldSnapshot {
        // Case 1: broadway has a pending owned task
        pending_tasks_with_owners: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "broadway".to_string(),
        )],
        // Case 2: unowned task referencing PR #100
        pending_tasks_without_owners: vec![Task {
            id: "43".to_string(),
            subject: "Review feedback on PR #100 [Midtown !43]".to_string(),
            status: crate::tasks::TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        }],
        // broadway is NOT running (will be spawned by Case 1)
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        in_progress_tasks: vec![
            // Existing in-progress task for broadway on PR #100 (so Case 2 groups to broadway)
            (
                "40".to_string(),
                "Implement feature [Midtown !40] PR #100".to_string(),
                "broadway".to_string(),
            ),
        ],
        all_tasks: vec![Task {
            id: "40".to_string(),
            subject: "Implement feature [Midtown !40] PR #100".to_string(),
            status: crate::tasks::TaskStatus::InProgress,
            owner: Some("broadway".to_string()),
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            created_at: None,
        }],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        merged_pr_numbers: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        attached_coworkers: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Count total effects targeting broadway
    let broadway_spawns = effects
        .iter()
        .filter(|e| match e {
            Effect::SpawnCoworkerWithCallbacks { config, .. } => {
                config.name.to_lowercase() == "broadway"
            }
            Effect::AssignAndSpawn { owner, .. } => owner.to_lowercase() == "broadway",
            Effect::NudgeCoworkerWithCallbacks { name, .. } => name.to_lowercase() == "broadway",
            _ => false,
        })
        .count();

    assert!(
        broadway_spawns <= 1,
        "Should generate at most ONE spawn/nudge for broadway across both Case 1 and Case 2, \
         got {}. Cross-case deduplication should prevent Case 2 from targeting a coworker \
         already dispatched by Case 1.",
        broadway_spawns
    );
}

#[test]
fn test_spawn_for_pending_tasks_skips_via_snapshot_assignment_check() {
    // Test the pure decision pattern: verify that spawn_for_pending_tasks
    // correctly skips a task when coworker_task_assignments (in WorldSnapshot)
    // shows the owner is already assigned to that specific task.
    // This test verifies the refactored code uses the snapshot data
    // (pure decision) rather than calling state.is_coworker_assigned_to_task()
    // (impure decision with .lock()).

    let mut assignments = HashMap::new();
    assignments.insert("broadway".to_string(), "42".to_string());

    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "broadway".to_string(),
        )],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        busy_coworkers: HashSet::new(),
        // KEY: broadway is already assigned to task !42 in the snapshot
        coworker_task_assignments: assignments,
        in_progress_tasks: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        merged_pr_numbers: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        attached_coworkers: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should generate NO effects because broadway is already assigned to task !42
    assert_eq!(
        effects.len(),
        0,
        "Should generate no effects when owner is already assigned to the task \
         (verified via coworker_task_assignments in snapshot)"
    );
}

// ======================================================================
// Worktree reuse on reassignment tests
// ======================================================================

#[test]
fn test_orphan_recovery_reuses_existing_task_worktree() {
    // Scenario: Task !42 was owned by "lexington" who died. The task has
    // an existing worktree "task-42-add-auth-endpoint" registered. When
    // recovering, the spawn should reuse that worktree and bind the coworker.
    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        active_names: HashSet::new(), // lexington is NOT active (orphaned)
        active_session_ids: HashSet::new(),
        coworkers_with_open_prs: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        attached_coworkers: HashSet::new(),
        tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
        task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
            .into_iter()
            .collect(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        worktree_branch_owners: HashMap::new(),
        merged_pr_branches: HashMap::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, &state, |task_id| {
        if task_id == "42" {
            Some(in_progress_task_for_lookup(
                "42",
                "Add auth endpoint",
                "lexington",
            ))
        } else {
            None
        }
    });

    // Pre-spawn effects (EnsureWorktree) are top-level, then SpawnCoworkerWithCallbacks
    assert!(
        effects.len() >= 2,
        "Should have pre-spawn EnsureWorktree + SpawnCoworkerWithCallbacks"
    );

    // EnsureWorktree should be a top-level effect (before spawn)
    let ensure_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::EnsureWorktree { worktree_id, .. } if worktree_id == "task-42-add-auth-endpoint"))
        .count();
    assert_eq!(
        ensure_count, 1,
        "Should have top-level EnsureWorktree for existing worktree"
    );

    // Should NOT have RegisterWorktreeAssignment (worktree already registered)
    let register_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
        .count();
    assert_eq!(
        register_count, 0,
        "Should NOT register worktree again (already exists)"
    );

    // Verify SpawnCoworkerWithCallbacks has working_dir set to the existing worktree
    let spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnCoworkerWithCallbacks {
                config, on_success, ..
            } = e
            {
                Some((config, on_success))
            } else {
                None
            }
        })
        .expect("Should have SpawnCoworkerWithCallbacks");

    let (config, on_success) = spawn;

    let expected_path =
        crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
    assert_eq!(
        config.working_dir,
        Some(expected_path),
        "Should set working_dir to the existing task worktree"
    );

    // BindCoworkerToWorktree stays in on_success (runs after spawn)
    let bind_count = on_success
        .iter()
        .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { worktree_id, coworker } if worktree_id == "task-42-add-auth-endpoint" && coworker == "lexington"))
        .count();
    assert_eq!(
        bind_count, 1,
        "Should have BindCoworkerToWorktree to rebind"
    );
}

#[test]
fn test_orphan_recovery_creates_new_worktree_when_none_exists() {
    // Scenario: Task !42 was owned by "lexington" who died, but the task
    // has NO worktree registered (legacy/pre-registry task). The spawn
    // should compute a new worktree_id, set working_dir, and emit
    // EnsureWorktree + RegisterWorktreeAssignment + BindCoworkerToWorktree.
    let snap = snapshot::WorldSnapshot {
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        coworkers_with_open_prs: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        attached_coworkers: HashSet::new(),
        tasks_with_worktrees: HashSet::new(), // No worktree registered
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, &state, |task_id| {
        if task_id == "42" {
            Some(in_progress_task_for_lookup(
                "42",
                "Add auth endpoint",
                "lexington",
            ))
        } else {
            None
        }
    });

    // Pre-spawn effects (EnsureWorktree, RegisterWorktreeAssignment) are top-level,
    // followed by SpawnCoworkerWithCallbacks with post-spawn effects in on_success.
    assert!(
        effects.len() >= 3,
        "Should have EnsureWorktree + RegisterWorktreeAssignment + SpawnCoworkerWithCallbacks"
    );

    // EnsureWorktree should be a top-level effect (before spawn)
    let ensure_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
        .count();
    assert_eq!(ensure_count, 1, "Should have top-level EnsureWorktree");

    // RegisterWorktreeAssignment should be a top-level effect (before spawn)
    let register_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::RegisterWorktreeAssignment { assignment } = e {
                Some(assignment)
            } else {
                None
            }
        })
        .expect("Should have top-level RegisterWorktreeAssignment");

    assert_eq!(register_effect.task_id, Some("42".to_string()));
    assert!(
        register_effect
            .worktree_id
            .contains("task-42-add-auth-endpoint")
    );
    assert_eq!(register_effect.current_coworker, None);

    let spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnCoworkerWithCallbacks {
                config, on_success, ..
            } = e
            {
                Some((config, on_success))
            } else {
                None
            }
        })
        .expect("Should have SpawnCoworkerWithCallbacks");

    let (config, on_success) = spawn;

    // Working dir SHOULD be set to computed worktree path
    assert!(
        config.working_dir.is_some(),
        "Should set working_dir to computed worktree path"
    );
    let working_dir = config.working_dir.as_ref().unwrap();
    assert!(
        working_dir
            .to_string_lossy()
            .contains("task-42-add-auth-endpoint"),
        "Working dir should be computed from task subject: {:?}",
        working_dir
    );

    // BindCoworkerToWorktree stays in on_success (runs after spawn)
    let bind_count = on_success
        .iter()
        .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
        .count();
    assert_eq!(
        bind_count, 1,
        "Should have BindCoworkerToWorktree in on_success"
    );
}

#[test]
fn test_spawn_for_pending_unowned_reuses_existing_worktree() {
    // Scenario: Task !42 was previously owned by another coworker who died.
    // The task was reset to pending (no owner). It already has a worktree
    // "task-42-add-auth-endpoint" registered. A new coworker should reuse it.
    use crate::tasks::{Task, TaskStatus};
    use std::time::SystemTime;

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            created_at: Some(SystemTime::now()),
        }],
        tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
        task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
            .into_iter()
            .collect(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        worktree_branch_owners: HashMap::new(),
        merged_pr_branches: HashMap::new(),
        is_at_dev_limit: false,
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Pre-spawn EnsureWorktree is top-level, then AssignAndSpawn
    assert!(
        effects.len() >= 2,
        "Should have pre-spawn EnsureWorktree + AssignAndSpawn"
    );

    // EnsureWorktree should be a top-level effect (before spawn)
    let ensure_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
        .collect();
    assert_eq!(
        ensure_effects.len(),
        1,
        "Should have top-level EnsureWorktree"
    );
    if let Effect::EnsureWorktree { worktree_id, .. } = ensure_effects[0] {
        assert_eq!(
            worktree_id, "task-42-add-auth-endpoint",
            "Should ensure the existing worktree, not a new one"
        );
    }

    // Should NOT have RegisterWorktreeAssignment (worktree already exists)
    let register_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
        .count();
    assert_eq!(
        register_count, 0,
        "Should NOT re-register existing worktree"
    );

    let assign_and_spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::AssignAndSpawn {
                config, on_success, ..
            } = e
            {
                Some((config, on_success))
            } else {
                None
            }
        })
        .expect("Should have AssignAndSpawn");

    let (config, on_success) = assign_and_spawn;

    // Working dir should point to the EXISTING worktree
    let expected_path =
        crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
    assert_eq!(
        config.working_dir,
        Some(expected_path),
        "Should reuse existing worktree path"
    );

    // BindCoworkerToWorktree stays in on_success (runs after spawn)
    let bind_effects: Vec<_> = on_success
        .iter()
        .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
        .collect();
    assert_eq!(
        bind_effects.len(),
        1,
        "Should bind coworker to existing worktree"
    );
    if let Effect::BindCoworkerToWorktree { worktree_id, .. } = bind_effects[0] {
        assert_eq!(
            worktree_id, "task-42-add-auth-endpoint",
            "Should bind to the existing worktree, not a new one"
        );
    }
}

// ======================================================================
// reconcile_tasks_in_review tests
// ======================================================================

/// Helper to create a minimal WorldSnapshot for reconciliation tests.
fn make_reconcile_snapshot(
    in_progress_tasks: Vec<(String, String, String)>,
    tasks_with_open_prs: HashMap<String, u64>,
    active_names: HashSet<String>,
) -> snapshot::WorldSnapshot {
    snapshot::WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names,
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks,
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs,
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    }
}

// ======================================================================
// reset_orphaned_tasks tests
// ======================================================================

#[test]
fn test_reset_orphaned_tasks_inactive_owner_no_pr() {
    // Bug !1157: Task !1146 is in_progress, owned by columbus, NO open PR,
    // columbus is NOT active (went on break) → should reset to pending
    let in_progress = vec![(
        "1146".to_string(),
        "Address review feedback and merge PR #912".to_string(),
        "columbus".to_string(),
    )];
    let tasks_with_open_prs = HashMap::new(); // No PR yet
    let active_names = HashSet::new(); // columbus is NOT active

    let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    let effects = reset_orphaned_tasks(&snap);

    assert_eq!(effects.len(), 1, "Should reset orphaned task");
    match &effects[0] {
        Effect::ResetTaskToPending { task_id, repo_name } => {
            assert_eq!(task_id, "1146");
            assert_eq!(repo_name, "test-repo");
        }
        other => panic!("Expected ResetTaskToPending, got {:?}", other),
    }
}

#[test]
fn test_reset_orphaned_tasks_active_owner_no_effect() {
    // Task !42 is in_progress, owned by york, NO open PR, york IS active
    // Should NOT reset (coworker is still working on it)
    let in_progress = vec![(
        "42".to_string(),
        "Fix auth bug".to_string(),
        "york".to_string(),
    )];
    let tasks_with_open_prs = HashMap::new();
    let mut active_names = HashSet::new();
    active_names.insert("york".to_string());

    let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    let effects = reset_orphaned_tasks(&snap);

    assert!(effects.is_empty(), "Should not reset task for active owner");
}

#[test]
fn test_reset_orphaned_tasks_with_pr_no_effect() {
    // Task !42 is in_progress, owned by york, HAS open PR, york is NOT active
    // Should NOT reset (reconcile_tasks_in_review handles PR cases)
    let in_progress = vec![(
        "42".to_string(),
        "Fix auth bug".to_string(),
        "york".to_string(),
    )];
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("42".to_string(), 100u64);
    let active_names = HashSet::new();

    let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    let effects = reset_orphaned_tasks(&snap);

    assert!(
        effects.is_empty(),
        "Should not reset task with open PR (handled by reconcile_tasks_in_review)"
    );
}

#[test]
fn test_reset_orphaned_tasks_pr_reference_in_subject_protects() {
    // Bug: "Address review feedback on PR #42" tasks reference an open PR
    // but don't own it (not in tasks_with_open_prs). Without the PR-reference
    // guard, this task would be reset when the owner goes inactive, causing
    // duplicate feedback addressing by another coworker.
    let in_progress = vec![(
        "100".to_string(),
        "Address review feedback on PR #42".to_string(),
        "columbus".to_string(),
    )];
    let tasks_with_open_prs = HashMap::new(); // Task doesn't OWN the PR
    let active_names = HashSet::new(); // columbus is NOT active

    let mut snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);

    // PR #42 is open (in open_prs_data but not tasks_with_open_prs)
    snap.open_prs_data = vec![serde_json::json!({"number": 42})];

    let effects = reset_orphaned_tasks(&snap);
    assert!(
        effects.is_empty(),
        "Should not reset task referencing open PR #42 in subject"
    );
}

#[test]
fn test_reset_orphaned_tasks_pr_reference_closed_pr_resets() {
    // Same scenario but PR #42 is closed/merged — task should be reset
    let in_progress = vec![(
        "100".to_string(),
        "Address review feedback on PR #42".to_string(),
        "columbus".to_string(),
    )];
    let tasks_with_open_prs = HashMap::new();
    let active_names = HashSet::new();

    let mut snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    snap.open_prs_data = vec![]; // PR #42 is NOT open

    let effects = reset_orphaned_tasks(&snap);
    assert_eq!(
        effects.len(),
        1,
        "Should reset task when referenced PR is closed"
    );
    match &effects[0] {
        Effect::ResetTaskToPending { task_id, .. } => {
            assert_eq!(task_id, "100");
        }
        other => panic!("Expected ResetTaskToPending, got {:?}", other),
    }
}

#[test]
fn test_grouped_task_skips_if_already_assigned() {
    // Regression test for nudge/spawn loop bug, using captured production snapshot.
    // Scenario: Task !1107 (pending, no owner) references PR #912 in its subject.
    // Task !1106 (in_progress, owned by york) mentions "PR #912" in its description.
    // The grouping logic finds york as the PR owner → groups !1107 to york.
    // York is already running and busy, but grouped tasks bypass the busy check.
    // Without the is_coworker_assigned_to_task guard, this nudge fires every tick.
    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-spawn-loop-york-1107-20260210-205810.json"
    );
    let snap: snapshot::WorldSnapshot =
        serde_json::from_str(fixture).expect("deserialize captured snapshot");

    // Verify fixture prerequisites: york is active and busy, task !1107 is pending
    assert!(snap.active_names.contains("york"), "york should be active");
    assert!(snap.busy_coworkers.contains("york"), "york should be busy");
    assert!(
        snap.pending_tasks_without_owners
            .iter()
            .any(|t| t.id == "1107"),
        "task !1107 should be pending without owner"
    );

    let state = make_test_state();

    // Tick 1: Task !1107 groups to york (PR #912), generates nudge
    let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
    let nudge_count_tick1 = effects_tick1
        .iter()
        .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
        .count();
    assert_eq!(
        nudge_count_tick1, 1,
        "Tick 1 should nudge york with task !1107"
    );

    // Simulate the nudge executing and recording the assignment
    state.record_task_assignment("york", "1107");

    // Tick 2: Task !1107 is still pending, york is busy with !1107 now.
    // Create a new snapshot that includes the assignment.
    let snap_tick2 = snapshot::WorldSnapshot {
        coworker_task_assignments: {
            let mut assignments = HashMap::new();
            assignments.insert("york".to_string(), "1107".to_string());
            assignments
        },
        ..snap
    };
    let effects_tick2 = spawn_for_pending_tasks(&snap_tick2, &state);
    let nudge_count_tick2 = effects_tick2
        .iter()
        .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
        .count();
    assert_eq!(
        nudge_count_tick2, 0,
        "Tick 2 should NOT re-nudge york — task !1107 is already assigned to york"
    );
}

#[test]
fn test_spawn_coworker_with_callbacks_records_task_assignment() {
    // Regression test for spawn loop bug (Case 1: pending task with owner).
    // When a coworker isn't running but has a pending task, SpawnCoworkerWithCallbacks
    // must include RecordTaskAssignment in on_success to prevent re-spawning every tick.
    //
    // Note: The captured fixture snapshot-spawn-loop-york-1110 doesn't contain
    // pending-with-owner tasks (tasks were already in_progress when captured), so
    // this test uses a minimal constructed snapshot to isolate Case 1 behavior.
    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-spawn-loop-york-1107-20260210-205810.json"
    );
    let mut snap: snapshot::WorldSnapshot =
        serde_json::from_str(fixture).expect("deserialize captured snapshot");

    // Override to test Case 1: pending task WITH owner, coworker NOT running.
    // Clear Case 2 tasks and set up a Case 1 scenario.
    snap.pending_tasks_without_owners.clear();
    snap.pending_tasks_with_owners = vec![(
        "1107".to_string(),
        "Investigate PR #912 — no CI checks running".to_string(),
        "york".to_string(),
    )];
    snap.active_names.clear(); // york is NOT running
    snap.busy_coworkers.clear();
    snap.in_progress_tasks.clear();

    let state = make_test_state();

    // Tick 1: generates SpawnCoworkerWithCallbacks with RecordTaskAssignment
    let effects = spawn_for_pending_tasks(&snap, &state);
    let spawn_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }))
        .count();
    assert_eq!(spawn_count, 1, "Tick 1 should spawn york");

    // Verify the effect has RecordTaskAssignment in on_success
    let has_record_assignment = effects.iter().any(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
            on_success
                .iter()
                .any(|e| matches!(e, Effect::RecordTaskAssignment { .. }))
        } else {
            false
        }
    });
    assert!(
        has_record_assignment,
        "SpawnCoworkerWithCallbacks should have RecordTaskAssignment in on_success"
    );

    // Mark in-flight (daemon does this between evaluate_tick and execute_effects)
    state.mark_in_flight_spawns_from_effects(&effects);
    assert!(
        state.is_task_spawn_in_flight("1107"),
        "Task !1107 should be marked in-flight before execution"
    );
}

#[test]
fn test_case1_nudge_records_assignment_and_prevents_loop() {
    // Regression test: Case 1 (pending task with owner) NudgeOwner must include
    // RecordTaskAssignment in on_success, so that after the nudge cooldown
    // expires, the task isn't re-nudged indefinitely.
    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-spawn-loop-york-1107-20260210-205810.json"
    );
    let mut snap: snapshot::WorldSnapshot =
        serde_json::from_str(fixture).expect("deserialize captured snapshot");

    // Set up Case 1 scenario: task with owner, coworker IS running but NOT busy
    // (triggers NudgeOwner rather than Skip due to has_in_progress_task)
    snap.pending_tasks_without_owners.clear();
    snap.pending_tasks_with_owners = vec![(
        "1107".to_string(),
        "Investigate PR #912 — no CI checks running".to_string(),
        "york".to_string(),
    )];
    // york is active (already in fixture), but clear busy state so NudgeOwner fires
    snap.busy_coworkers.clear();
    snap.in_progress_tasks.clear();

    let state = make_test_state();

    // Tick 1: NudgeOwner fires with RecordTaskAssignment in on_success
    let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
    let nudge_effects: Vec<_> = effects_tick1
        .iter()
        .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
        .collect();
    assert_eq!(nudge_effects.len(), 1, "Tick 1 should nudge york");

    // Verify RecordTaskAssignment is in on_success
    let has_assignment = nudge_effects.iter().any(|e| {
        if let Effect::NudgeCoworkerWithCallbacks { on_success, .. } = e {
            on_success
                .iter()
                .any(|e| matches!(e, Effect::RecordTaskAssignment { .. }))
        } else {
            false
        }
    });
    assert!(
        has_assignment,
        "NudgeOwner on_success should include RecordTaskAssignment"
    );

    // Simulate the nudge executing and recording the assignment
    state.record_task_assignment("york", "1107");

    // Tick 2: Create a new snapshot that includes the assignment in coworker_task_assignments.
    // The guard should use snap.coworker_task_assignments to prevent re-nudge (pure decision pattern).
    let snap_tick2 = snapshot::WorldSnapshot {
        coworker_task_assignments: {
            let mut assignments = HashMap::new();
            assignments.insert("york".to_string(), "1107".to_string());
            assignments
        },
        ..snap
    };
    let effects_tick2 = spawn_for_pending_tasks(&snap_tick2, &state);
    let nudge_count_tick2 = effects_tick2
        .iter()
        .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
        .count();
    assert_eq!(
        nudge_count_tick2, 0,
        "Tick 2 should NOT re-nudge york — already assigned to task !1107"
    );
}

// ======================================================================
// Regression test for task !1288: No dispatch when all coworkers are gone
// ======================================================================

#[test]
fn test_spawn_for_pending_tasks_when_all_coworkers_are_gone() {
    use crate::tasks::{Task, TaskStatus};
    use std::time::SystemTime;

    // Bug scenario: 0 active coworkers, 8 pending unblocked tasks
    // Expected: should spawn coworkers for tasks
    // Actual (bug): no dispatch activity

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![
            Task {
                id: "1263".to_string(),
                subject: "Phase 2: Daemon RPC endpoints for Zellij plugin".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                blocked_by: vec![],
                description: None,
                channel: None,
                pr: None,
                created_at: Some(SystemTime::now()),
            },
            Task {
                id: "1274".to_string(),
                subject: "Add sandbox_allowed_paths to config".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                blocked_by: vec![],
                description: None,
                channel: None,
                pr: None,
                created_at: Some(SystemTime::now()),
            },
        ],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_dev_limit: false,
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        running_coworkers: vec![], // 0 running coworkers!
        active_coworkers: vec![],  // 0 active coworkers!
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();

    // The bug: when state.coworkers.list() is empty, spawn_for_pending_tasks
    // should still work — it should spawn fresh coworkers for pending tasks.
    assert_eq!(
        state.coworkers.list().len(),
        0,
        "test precondition: no coworkers registered in DaemonState"
    );

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Expected: should spawn coworkers for pending tasks
    println!("Effects generated: {:?}", effects.len());

    let spawn_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::AssignAndSpawn { .. }))
        .count();

    assert!(
        !effects.is_empty(),
        "spawn_for_pending_tasks should generate effects when there are pending tasks and no active coworkers"
    );
    assert!(
        spawn_count > 0,
        "should spawn at least one coworker for pending tasks; got {} effects total but {} spawns",
        effects.len(),
        spawn_count
    );
}

/// Helper to create minimal DaemonState for testing
fn make_test_state() -> DaemonState {
    use std::process::Command;
    use tempfile::TempDir;

    let temp_dir = TempDir::new().expect("temp dir");
    Command::new("git")
        .args(["init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git init");
    Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git config");
    Command::new("git")
        .args(["commit", "--allow-empty", "-m", "init"])
        .current_dir(temp_dir.path())
        .output()
        .expect("git commit");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new(wm);

    // Leak temp_dir so it survives the test
    let base_dir = temp_dir.path().to_path_buf();
    std::mem::forget(temp_dir);

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        "test-repo".to_string(),
        vec![base_dir.clone()],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state")
}

// ======================================================================
// should_recover_task (pure decision function) tests
// ======================================================================

#[test]
fn test_should_recover_task_skips_completed_tasks() {
    use crate::tasks::{Task, TaskStatus};

    let completed_task = Task {
        id: "1120".to_string(),
        subject: "Fix orphan recovery loop".to_string(),
        description: None,
        status: TaskStatus::Completed,
        owner: Some("vernon".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    let merged_prs = HashSet::new();
    let tasks_with_open_prs = HashMap::new();
    assert!(
        !should_recover_task(
            &completed_task,
            &merged_prs,
            std::path::Path::new("."),
            &tasks_with_open_prs,
            &HashMap::new(),
        ),
        "Should NOT recover a completed task"
    );
}

#[test]
fn test_should_recover_task_with_contextual_pr_mention_in_subject() {
    use crate::tasks::{Task, TaskStatus};

    // Task !1120 mentions PR #923 in subject, but PR #923 is NOT the task's PR.
    // This is a contextual mention (e.g., "Merge PR #923 [Midtown !1120]" means
    // the task is ABOUT merging #923, not that #923 IS the task's PR).
    let task = Task {
        id: "1120".to_string(),
        subject: "Merge PR #923 [Midtown !1120]".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("vernon".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(923), // Explicit PR association (auto-set from PR title or --pr flag)
        created_at: None,
    };

    // PR #923 is merged, but it's not associated with task !1120
    let merged_prs: HashSet<u64> = [923].into_iter().collect();
    let tasks_with_open_prs = HashMap::new();

    // With explicit PR associations: should NOT recover because task.pr = Some(923) and PR #923 is merged
    assert!(
        !should_recover_task(
            &task,
            &merged_prs,
            std::path::Path::new("."),
            &tasks_with_open_prs,
            &HashMap::new(),
        ),
        "Should NOT recover a task whose PR is already merged (explicit pr field)"
    );
}

#[test]
fn test_should_recover_task_with_contextual_pr_mention_in_description() {
    use crate::tasks::{Task, TaskStatus};

    // Task mentions PR #925 in description as context
    let task = Task {
        id: "1121".to_string(),
        subject: "Address review feedback".to_string(),
        description: Some("Fixes from PR #925 review".to_string()),
        status: TaskStatus::InProgress,
        owner: Some("park".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(925), // Explicit PR association
        created_at: None,
    };

    // PR #925 is merged, but it's not associated with task !1121
    let merged_prs: HashSet<u64> = [925].into_iter().collect();
    let tasks_with_open_prs = HashMap::new();

    // With explicit PR associations: should NOT recover because task.pr = Some(925) and PR #925 is merged
    assert!(
        !should_recover_task(
            &task,
            &merged_prs,
            std::path::Path::new("."),
            &tasks_with_open_prs,
            &HashMap::new(),
        ),
        "Should NOT recover a task whose PR is already merged (explicit pr field)"
    );
}

// ============================================================================
// Pure parsing function tests (no I/O required)
// ============================================================================

#[test]
fn test_parse_pr_merged_state_merged() {
    assert!(super::parse_pr_merged_state("MERGED\n"));
    assert!(super::parse_pr_merged_state("MERGED"));
    assert!(super::parse_pr_merged_state("  MERGED  "));
}

#[test]
fn test_parse_pr_merged_state_open() {
    assert!(!super::parse_pr_merged_state("OPEN\n"));
    assert!(!super::parse_pr_merged_state("OPEN"));
}

#[test]
fn test_parse_pr_merged_state_closed() {
    assert!(!super::parse_pr_merged_state("CLOSED\n"));
    assert!(!super::parse_pr_merged_state("CLOSED"));
}

#[test]
fn test_parse_pr_merged_state_empty() {
    assert!(!super::parse_pr_merged_state(""));
    assert!(!super::parse_pr_merged_state("  "));
}

// ============================================================================
// github_open_pr_task_ids defense-in-depth tests (snapshot-based, no I/O)
// ============================================================================

#[test]
fn test_should_not_recover_task_with_open_pr_via_github_title() {
    // Scenario: Task !1233 has no pr field, no entry in tasks_with_open_prs,
    // but there's an open PR #1089 with "[Midtown !1233]" in the title.
    // The github_open_pr_task_ids snapshot data prevents duplicate recovery.
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "1233".to_string(),
        subject: "Prevent duplicate work after daemon restarts".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("york".to_string()),
        blocked_by: vec![],
        pr: None,
        channel: None,
        created_at: None,
    };

    let merged_prs = HashSet::new();
    let tasks_with_open_prs = HashMap::new(); // Empty (stale after daemon restart)
    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("1233".to_string(), 1089u64); // PR #1089 has [Midtown !1233]

    let result = should_recover_task(
        &task,
        &merged_prs,
        std::path::Path::new("."),
        &tasks_with_open_prs,
        &github_open_pr_task_ids,
    );

    assert!(
        !result,
        "Should NOT recover task when github_open_pr_task_ids shows an open PR for it"
    );
}

#[test]
fn test_should_recover_task_when_github_title_has_no_match() {
    // Scenario: Task !42 has no PR association anywhere — not in pr field,
    // not in tasks_with_open_prs, not in github_open_pr_task_ids.
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("lexington".to_string()),
        blocked_by: vec![],
        pr: None,
        channel: None,
        created_at: None,
    };

    let merged_prs = HashSet::new();
    let tasks_with_open_prs = HashMap::new();
    let github_open_pr_task_ids = HashMap::new(); // No title matches

    let result = should_recover_task(
        &task,
        &merged_prs,
        std::path::Path::new("."),
        &tasks_with_open_prs,
        &github_open_pr_task_ids,
    );

    assert!(result, "Should recover task when no PR found in any source");
}

#[test]
fn test_should_not_recover_task_github_title_takes_precedence_over_no_pr_field() {
    // Scenario: Task !55 has no pr field (not set yet), tasks_with_open_prs is empty
    // (stale after restart), but github_open_pr_task_ids has a match.
    // This is the exact scenario that caused duplicate work after daemon restart.
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "55".to_string(),
        subject: "Fix flaky tests".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("park".to_string()),
        blocked_by: vec![],
        pr: None, // Not set yet — PR was created but task field wasn't updated
        channel: None,
        created_at: None,
    };

    let merged_prs = HashSet::new();
    let tasks_with_open_prs = HashMap::new(); // Stale after restart
    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("55".to_string(), 200u64);

    let result = should_recover_task(
        &task,
        &merged_prs,
        std::path::Path::new("."),
        &tasks_with_open_prs,
        &github_open_pr_task_ids,
    );

    assert!(
        !result,
        "Should NOT recover: github_open_pr_task_ids catches the open PR even when other sources are stale"
    );
}

#[test]
fn test_should_recover_task_allows_active_in_progress_task() {
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("lexington".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    let merged_prs = HashSet::new();
    let tasks_with_open_prs = HashMap::new();
    assert!(
        should_recover_task(
            &task,
            &merged_prs,
            std::path::Path::new("."),
            &tasks_with_open_prs,
            &HashMap::new(),
        ),
        "Should recover an active in-progress task with no merged PR"
    );
}

#[test]
fn test_should_recover_task_allows_task_with_unmerged_pr() {
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "1120".to_string(),
        subject: "Merge PR #999999 [Midtown !1120]".to_string(), // Use non-existent PR number
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("vernon".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    // PR #999999 is NOT in the merged set (and doesn't exist in repo)
    // The GitHub API check will fail (PR not found) but the function
    // should be conservative and allow recovery.
    let merged_prs: HashSet<u64> = [900, 910].into_iter().collect();
    let tasks_with_open_prs = HashMap::new();
    assert!(
        should_recover_task(
            &task,
            &merged_prs,
            std::path::Path::new("."),
            &tasks_with_open_prs,
            &HashMap::new(),
        ),
        "Should recover a task whose PR is NOT yet merged (cache miss, API fails)"
    );
}

#[test]
#[ignore] // Obsolete test - no longer does GitHub API checks for contextual PR mentions
fn test_should_recover_task_checks_github_when_cache_stale() {
    use crate::tasks::{Task, TaskStatus};

    // This test is obsolete after the fix for issue #1147.
    // The new behavior no longer checks GitHub API for contextual PR mentions.
    // It only skips recovery when pr_task_associations contains the canonical link.
    let task = Task {
        id: "1129".to_string(),
        subject: "Fix task !1129 [Midtown !1129]".to_string(),
        description: Some("PR #935".to_string()),
        status: TaskStatus::InProgress,
        owner: Some("riverside".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    // PR #935 is NOT in the cache
    let merged_prs: HashSet<u64> = HashSet::new();
    let tasks_with_open_prs = HashMap::new();

    // New behavior: SHOULD recover because PR #935 is just a contextual mention (no explicit pr field)
    assert!(
        should_recover_task(
            &task,
            &merged_prs,
            std::path::Path::new("."),
            &tasks_with_open_prs,
            &HashMap::new(),
        ),
        "Should recover task with contextual PR mention (no longer checks GitHub API)"
    );
}

#[test]
fn test_should_recover_task_with_bare_hash_pr_reference() {
    use crate::tasks::{Task, TaskStatus};

    // Task with bare "#904" format (no "PR #" prefix)
    // With explicit PR associations, the pr field should be set to 904
    let task = Task {
        id: "1122".to_string(),
        subject: "Fix #904 review feedback".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("columbus".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(904), // Explicit PR association
        created_at: None,
    };

    let merged_prs: HashSet<u64> = [904].into_iter().collect();
    let repo_path = std::path::Path::new("/tmp/test-repo");
    let tasks_with_open_prs = HashMap::new();

    // With explicit PR associations: should NOT recover because task.pr = Some(904) and PR #904 is merged
    assert!(
        !should_recover_task(
            &task,
            &merged_prs,
            repo_path,
            &tasks_with_open_prs,
            &HashMap::new()
        ),
        "Should NOT recover a task whose PR (#904) is already merged (explicit pr field)"
    );
}

#[test]
fn test_should_recover_task_recovers_multi_pr_with_only_some_merged() {
    use crate::tasks::{Task, TaskStatus};

    // Task referencing PRs #901, #902, #903, but only #901 is merged
    // should_recover_task() should return true (task needs recovery)
    // because auto-completion won't fire until ALL PRs are merged
    let task = Task {
        id: "1123".to_string(),
        subject: "Merge PRs #901, #902, #903".to_string(),
        description: Some("Consolidate multiple related PRs".to_string()),
        status: TaskStatus::InProgress,
        owner: Some("madison".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    // Only #901 is merged; #902 and #903 are still open
    let merged_prs: HashSet<u64> = [901].into_iter().collect();
    let repo_path = std::path::Path::new("/tmp/test-repo");
    let tasks_with_open_prs = HashMap::new();
    assert!(
        should_recover_task(
            &task,
            &merged_prs,
            repo_path,
            &tasks_with_open_prs,
            &HashMap::new()
        ),
        "Should recover task with multi-PR reference where only SOME PRs are merged"
    );
}

#[test]
fn test_should_recover_task_with_multi_pr_when_all_merged() {
    use crate::tasks::{Task, TaskStatus};

    // Meta-task referencing PRs #901, #902, #903, and ALL are merged
    // With explicit PR associations: should_recover_task() returns true because
    // it only checks the explicit pr field (which is None for meta-tasks).
    // Auto-completion will handle cleanup when all PRs are merged.
    let task = Task {
        id: "1124".to_string(),
        subject: "Merge PRs #901, #902, #903".to_string(),
        description: Some("Consolidate multiple related PRs".to_string()),
        status: TaskStatus::InProgress,
        owner: Some("madison".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None, // Meta-tasks don't have explicit PR associations
        created_at: None,
    };

    // All PRs are merged, but they're not the task's canonical PR
    let merged_prs: HashSet<u64> = [901, 902, 903].into_iter().collect();
    let repo_path = std::path::Path::new("/tmp/test-repo");
    let tasks_with_open_prs = HashMap::new();

    // New behavior: SHOULD recover because pr field is None (contextual mentions only)
    assert!(
        should_recover_task(
            &task,
            &merged_prs,
            repo_path,
            &tasks_with_open_prs,
            &HashMap::new()
        ),
        "Should recover task with no explicit pr field (auto-completion will handle cleanup)"
    );
}

#[test]
fn test_should_recover_task_with_pr_in_subject_only() {
    use crate::tasks::{Task, TaskStatus};

    // Task with PR reference only in subject (not description)
    // With explicit PR associations: should_recover_task() returns true because
    // it only checks the explicit pr field (which is None).
    // If this task is actually FOR PR #905, it should have pr: Some(905).
    let task = Task {
        id: "1125".to_string(),
        subject: "Close PR #905".to_string(),
        description: Some("Final cleanup tasks".to_string()),
        status: TaskStatus::InProgress,
        owner: Some("broadway".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None, // Should be Some(905) if this task is for PR #905
        created_at: None,
    };

    let merged_prs: HashSet<u64> = [905].into_iter().collect();
    let repo_path = std::path::Path::new("/tmp/test-repo");
    let tasks_with_open_prs = HashMap::new();

    // New behavior: SHOULD recover because pr field is None (contextual mentions only)
    assert!(
        should_recover_task(
            &task,
            &merged_prs,
            repo_path,
            &tasks_with_open_prs,
            &HashMap::new()
        ),
        "Should recover task with no explicit pr field (auto-completion will handle cleanup)"
    );
}

#[test]
fn test_spawn_extracts_model_alias_from_provider_model_format() {
    use crate::tasks::{Task, TaskStatus};
    use std::time::SystemTime;

    // Setup: task with model "claude/opus" in task_model_map
    let mut task_model_map = HashMap::new();
    task_model_map.insert("42".to_string(), "claude/opus".to_string());

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "42".to_string(),
            subject: "Complex algorithm task".to_string(),
            status: TaskStatus::Pending,
            owner: None,
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            created_at: Some(SystemTime::now()),
        }],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_branch_owners: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        merged_pr_branches: HashMap::new(),
        is_at_dev_limit: false,
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map,
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        is_at_coworker_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Find the AssignAndSpawn effect and check its LaunchConfig
    let spawn_config = effects
        .iter()
        .find_map(|e| {
            if let Effect::AssignAndSpawn { config, .. } = e {
                Some(config)
            } else {
                None
            }
        })
        .expect("Should have AssignAndSpawn effect");

    // LaunchConfig.model should be just "opus" (not "claude/opus")
    assert_eq!(
        spawn_config.model, "opus",
        "LaunchConfig.model should be just the model alias 'opus', not the full 'claude/opus'"
    );
    // LaunchConfig.auth_provider should be extracted from "claude" portion
    assert_eq!(
        spawn_config.auth_provider,
        crate::auth::AuthProvider::Claude,
        "LaunchConfig.auth_provider should be Claude"
    );
}

#[test]
fn test_orphan_recovery_marks_task_in_flight() {
    // Regression test: Orphan recovery must include RecordTaskAssignment in on_success
    // to prevent task dispatch from double-assigning the task while the recovered
    // coworker is spawning.
    //
    // Timeline without the fix:
    // 1. Coworker crashes, task !999999 is in_progress with owner=lexington
    // 2. Orphan recovery spawns lexington (tick 0s)
    // 3. Task dispatch runs (tick 10s) before lexington claims via RPC
    // 4. Task dispatch sees task as in_progress but lexington not active → spawns another coworker
    // 5. Result: double assignment (lexington + madison both working on !999999)
    let snap = snapshot::WorldSnapshot {
        active_coworkers: vec![],
        running_coworkers: vec![],
        coworker_snapshots: vec![],
        active_names: HashSet::new(), // lexington crashed, not active
        active_session_ids: HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![(
            "999999".to_string(),
            "Test task".to_string(),
            "lexington".to_string(),
        )],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        pending_tasks_without_owners: vec![],
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        merged_pr_numbers: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
        pending_task_owners: HashSet::new(),
        tasks_with_open_prs: HashMap::new(),
        pr_task_associations: HashMap::new(),
        active_reviewers: HashSet::new(),
        reviewer_pr_assignments: HashMap::new(),
        reviewed_prs: HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: HashMap::new(),
        reviewer_escalations_posted: HashSet::new(),
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        coworkers_with_unblocked_deps: HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: HashSet::new(),
        api_error_coworkers: HashSet::new(),
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        worktree_branch_owners: HashMap::new(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        is_at_dev_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
    };

    let state = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, &state, |task_id| {
        if task_id == "1264" {
            Some(in_progress_task_for_lookup(
                "1264",
                "Test task",
                "lexington",
            ))
        } else {
            None
        }
    });

    // Should have SpawnCoworkerWithCallbacks effect
    let spawn_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }))
        .expect("Should have SpawnCoworkerWithCallbacks effect");

    // Extract on_success effects
    let on_success = if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = spawn_effect {
        on_success
    } else {
        panic!("Expected SpawnCoworkerWithCallbacks");
    };

    // Should include RecordTaskAssignment in on_success
    let has_record_assignment = on_success.iter().any(|e| {
        matches!(e, Effect::RecordTaskAssignment { task_id, coworker }
            if task_id == "999999" && coworker == "lexington")
    });

    assert!(
        has_record_assignment,
        "Orphan recovery must include RecordTaskAssignment in on_success to prevent double-assignment race"
    );

    // Verify that mark_in_flight_spawns_from_effects would mark this task
    state.mark_in_flight_spawns_from_effects(&effects);
    assert!(
        state.is_task_spawn_in_flight("999999"),
        "Task !999999 should be marked in-flight after orphan recovery"
    );
}

// ======================================================================
// Stale task cleanup tests
// ======================================================================

#[test]
fn test_stale_task_cleanup_false_positive_task_about_merged_pr() {
    // Reproduces the false positive where a task ABOUT a merged PR (e.g., analyzing a bug
    // that occurred during that PR) gets auto-completed because the PR number appears in
    // the task description.
    //
    // Expected: Task should NOT be auto-completed (it has no explicit pr field set)
    // Actual (before fix): Task gets auto-completed because description mentions "PR #1153"

    use crate::tasks::{Task, TaskStatus};
    use std::time::SystemTime;

    let task = Task {
        id: "1310".to_string(),
        subject: "Fix bug from PR #1153".to_string(), // PR number in subject to trigger pattern matching
        description: Some(
            "Bug: When PR #1153 opened, the daemon queued a reviewer to spawn 45s later. \
             But when it tried, CI was still running so no reviewer spawned. When CI went \
             green, the daemon never re-evaluated the PR. The fix is to re-evaluate when \
             CI status changes."
                .to_string(),
        ),
        status: TaskStatus::Pending,
        owner: Some("pleasant".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None, // No explicit pr field - this task is ABOUT PR #1153, not FOR it
        created_at: Some(SystemTime::now()),
    };

    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            task.id.clone(),
            task.subject.clone(),
            task.owner.clone().unwrap(),
        )],
        pending_tasks_without_owners: vec![],
        all_tasks: vec![task],
        merged_pr_numbers: [1153u64].into_iter().collect(), // PR #1153 is merged
        is_at_dev_limit: false,
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        worktree_branch_owners: HashMap::new(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should NOT auto-complete the task. The task is about a bug that occurred during
    // PR #1153, not a task whose work is in PR #1153.
    let has_complete = effects
        .iter()
        .any(|e| matches!(e, Effect::CompleteTask { task_id, .. } if task_id == "1310"));

    assert!(
        !has_complete,
        "Task !1310 should NOT be auto-completed - it's about PR #1153, not for it"
    );
}

#[test]
fn test_stale_task_cleanup_correct_behavior_with_explicit_pr_field() {
    // Task with explicit pr field set should be auto-completed when that PR merges.

    use crate::tasks::{Task, TaskStatus};
    use std::time::SystemTime;

    let task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        description: None,
        status: TaskStatus::Pending,
        owner: Some("park".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(123), // Explicit pr field - this task's work is IN PR #123
        created_at: Some(SystemTime::now()),
    };

    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            task.id.clone(),
            task.subject.clone(),
            task.owner.clone().unwrap(),
        )],
        pending_tasks_without_owners: vec![],
        all_tasks: vec![task],
        merged_pr_numbers: [123u64].into_iter().collect(), // PR #123 is merged
        is_at_dev_limit: false,
        active_names: HashSet::new(),
        active_session_ids: HashSet::new(),
        running_coworkers: vec![],
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        session_name: "midtown-test".to_string(),
        coworker_start_times: HashMap::new(),
        coworker_stop_times: HashMap::new(),
        headless_process_health: HashMap::new(),
        attached_coworkers: HashSet::new(),
        in_progress_tasks: vec![],
        busy_coworkers: HashSet::new(),
        coworker_task_assignments: HashMap::new(),
        task_channel: HashMap::new(),
        task_model_map: HashMap::new(),
        task_plan_map: HashMap::new(),
        task_execution_skill_map: HashMap::new(),
        coworkers_with_open_prs: HashSet::new(),
        coworkers_with_merged_prs: HashSet::new(),
        ci_passed_pr_coworkers: HashSet::new(),
        review_feedback_pr_coworkers: HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: HashMap::new(),
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
        auth_error_coworkers: HashSet::new(),
        tool_name_conflict_coworkers: HashSet::new(),
        channel_messages: vec![],
        archived_channels: HashSet::new(),
        daemon_logs: vec![],
        tasks_with_worktrees: HashSet::new(),
        task_worktree_map: HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        worktree_branch_owners: HashMap::new(),
        merged_pr_branches: HashMap::new(),
        is_at_coworker_limit: false,
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
    };

    let state = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // SHOULD auto-complete the task because task.pr == 123 and PR #123 is merged
    let has_complete = effects
        .iter()
        .any(|e| matches!(e, Effect::CompleteTask { task_id, .. } if task_id == "42"));

    assert!(
        has_complete,
        "Task !42 should be auto-completed - its explicit pr field matches merged PR #123"
    );
}

// ============================================================================
// Bug !1317: Double-assignment of tasks with open PRs
// ============================================================================
//
// Bug scenario: When a coworker opens a PR and goes idle, reconcile_tasks_in_review()
// unassigns the task (clears owner, sets status to pending). On the next tick,
// pending task dispatch picks it up and assigns it to a different coworker,
// creating duplicate work on the same PR.
//
// Root cause: pending_tasks_without_owners dispatch path did not check
// tasks_with_open_prs or github_open_pr_task_ids before assigning.
//
// Fix: Added PR checks in spawn_for_pending_tasks() Case 2 to skip tasks that
// already have open PRs.

#[test]
fn test_should_recover_task_skips_tasks_with_open_pr_in_tasks_with_open_prs() {
    // Orphan recovery also needs to skip tasks with open PRs (separate path from pending dispatch).
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "1313".to_string(),
        subject: "Implement feature X".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("lexington".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None, // PR association tracked in tasks_with_open_prs instead
        created_at: None,
    };

    let merged_prs = HashSet::new(); // PR #1156 is NOT merged
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("1313".to_string(), 1156u64); // Task has open PR #1156
    let github_open_pr_task_ids = HashMap::new();

    let result = should_recover_task(
        &task,
        &merged_prs,
        std::path::Path::new("."),
        &tasks_with_open_prs,
        &github_open_pr_task_ids,
    );

    assert!(
        !result,
        "Should NOT recover task !1313 - it has open PR #1156 in tasks_with_open_prs"
    );
}

#[test]
fn test_should_recover_task_skips_tasks_with_open_pr_in_github_open_pr_task_ids() {
    // Defense-in-depth: Even if tasks_with_open_prs is empty (stale),
    // github_open_pr_task_ids should prevent recovery.
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "1313".to_string(),
        subject: "Implement feature X".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        owner: Some("lexington".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        created_at: None,
    };

    let merged_prs = HashSet::new();
    let tasks_with_open_prs = HashMap::new(); // Empty (stale after daemon restart)
    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("1313".to_string(), 1156u64); // Found via GitHub PR title

    let result = should_recover_task(
        &task,
        &merged_prs,
        std::path::Path::new("."),
        &tasks_with_open_prs,
        &github_open_pr_task_ids,
    );

    assert!(
        !result,
        "Should NOT recover task !1313 - it has open PR #1156 via github_open_pr_task_ids"
    );
}
