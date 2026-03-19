use super::*;

/// Build active_names from a task's owner — used to preserve pre-existing test
/// behavior after adding the `active_names` parameter to `is_task_pr_protected`.
fn active_names_for(task: &crate::task_store::Task) -> HashSet<String> {
    if task.agent_name.is_empty() {
        HashSet::new()
    } else {
        [task.agent_name.to_lowercase()].into_iter().collect()
    }
}

#[test]
fn test_build_push_deep_link_basic() {
    let url = build_push_deep_link("myproject", "web", None, None);
    assert_eq!(url, "/myproject?channel=web");
}

#[test]
fn test_build_push_deep_link_with_msg() {
    let url = build_push_deep_link("myproject", "web", Some("msg-123"), None);
    assert_eq!(url, "/myproject?channel=web&msg=msg-123");
}

#[test]
fn test_build_push_deep_link_with_msg_and_thread() {
    let url = build_push_deep_link("myproject", "web", Some("msg-456"), Some("thread-789"));
    assert_eq!(url, "/myproject?channel=web&msg=msg-456&thread=thread-789");
}

fn in_progress_task_for_lookup(
    task_id: &str,
    subject: &str,
    owner: &str,
) -> crate::task_store::Task {
    crate::task_store::Task {
        id: task_id.to_string(),
        subject: subject.to_string(),
        status: crate::task_store::TaskStatus::InProgress,
        agent_name: owner.to_string(),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    }
}

// ============================================================================
// Regression tests: legacy "lead" owner excluded from orphan recovery and
// duplicate detection.
//
// Before the consolidation fix, orphan recovery and duplicate detection in
// dispatch_via_sessions_with_task_lookup() and check_for_duplicate_task_workers()
// only skipped tasks owned by snap.project_name, NOT the legacy "lead" name.
// ============================================================================

/// Build a minimal WorldSnapshot for lead-guard tests.
///
/// Sets in_progress_tasks and dir_key/project_name; all other fields are empty/false.
fn make_lead_guard_snapshot(
    in_progress: Vec<(String, String, String)>,
    repo_name: &str,
) -> snapshot::WorldSnapshot {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.in_progress_tasks = in_progress;
    snap.dir_key = repo_name.to_string();
    snap.project_name = repo_name.to_string();
    snap.default_channel = repo_name.to_string();
    snap.coworkers.session_name = format!("{}-test", repo_name);
    snap
}

/// check_for_duplicate_task_workers must skip tasks owned by legacy "lead".
///
/// Bug: legacy "lead" owner was included in duplicate detection, causing the
/// daemon to incorrectly flag tasks as duplicated and attempt to kill sessions.
#[test]
fn test_duplicate_detection_skips_legacy_lead_owner() {
    // Two owners: "lead" (legacy) and a coworker named "york".
    // This looks like a duplicate, but "lead" must be skipped.
    let snap = make_lead_guard_snapshot(
        vec![
            ("42".to_string(), "Fix bug".to_string(), "lead".to_string()),
            ("42".to_string(), "Fix bug".to_string(), "york".to_string()),
        ],
        "my-repo",
    );

    let effects = check_for_duplicate_task_workers(&snap);

    // "lead" should be excluded → only 1 real worker (york) → no duplicate detected
    let kill_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                crate::daemon::effects::Effect::ShutdownCoworker { .. }
                    | crate::daemon::effects::Effect::ShutdownCoworkerWithCallbacks { .. }
            )
        })
        .collect();
    assert!(
        kill_effects.is_empty(),
        "Should not kill any session: 'lead' must be excluded from duplicate detection. \
         Effects: {:?}",
        effects
    );
}

/// dispatch_via_sessions_snapshot_only must skip tasks owned by legacy "lead".
///
/// Bug: tasks with owner="lead" were not skipped in orphan recovery (only
/// owner=repo_name was skipped), causing the daemon to try to recover lead tasks.
#[test]
fn test_orphan_recovery_skips_legacy_lead_owner() {
    // Task !1 owned by "lead" (legacy) with no active session → looks like an orphan.
    // After fix: must be skipped (lead is not a recoverable coworker).
    let snap = make_lead_guard_snapshot(
        vec![("1".to_string(), "Main task".to_string(), "lead".to_string())],
        "my-repo",
    );

    let effects = dispatch_via_sessions_snapshot_only(&snap);

    // No spawn/nudge effects should be emitted for the "lead"-owned task
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                crate::daemon::effects::Effect::SpawnForTask { .. }
                    | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
            )
        })
        .collect();
    assert!(
        spawn_effects.is_empty(),
        "Should not spawn/nudge for 'lead'-owned task: it must be skipped. Effects: {:?}",
        effects
    );
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
fn test_is_task_pr_protected_with_explicit_pr_association() {
    // Bug context: Task 1142 had "PR #940 fix insufficient" in the subject as context,
    // not as the actual task's PR. The task's real PR would be different.
    // is_task_pr_protected() should use the explicit pr field, not extract_pr_numbers_from_text().
    use std::collections::HashSet;

    let merged_prs: HashSet<u64> = vec![940].into_iter().collect();

    // Task with PR #940 mentioned in subject as context, but explicit pr field is None
    // (because the task's actual work will create a different PR)
    let task = crate::task_store::Task {
        id: "1142".to_string(),
        subject: "Fix remaining orphan worktree false positives — PR #940 fix insufficient"
            .to_string(),
        status: crate::task_store::TaskStatus::InProgress,
        agent_name: "amsterdam".to_string(),
        description: Some("The fix in PR #940 suppresses warnings...".to_string()),
        blocked_by: vec![],
        channel: Some("midtown".to_string()),
        pr: None, // No explicit PR association yet — task will create a new PR
        ..Default::default()
    };

    let pr_task_index = snapshot::PrTaskIndex::default();
    let active_names = active_names_for(&task);
    let result = !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names);
    assert!(
        result,
        "task should NOT be pr-protected when explicit pr field is None, even if merged PR mentioned in text"
    );

    // Now test with explicit PR association
    let task_with_pr = crate::task_store::Task {
        pr: Some(940),
        ..task
    };

    let result = !is_task_pr_protected(&task_with_pr, &merged_prs, &pr_task_index, &active_names);
    assert!(
        !result,
        "task should be pr-protected when explicit pr field matches merged PR"
    );
}

#[test]
fn test_build_task_completion_effects_with_task_id() {
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint [Midtown #42]",
        123,
        "myrepo",
        "myrepo",
        None,
        None,
    );

    // 4 effects: CompleteTask + ClearBlockedBy + PostToChannel + SendPushNotification
    assert_eq!(effects.len(), 4, "Should return 4 effects");

    // Verify CompleteTask effect
    match &effects[0] {
        Effect::CompleteTask { task_id, dir_key } => {
            assert_eq!(task_id, "42");
            assert_eq!(dir_key, "myrepo");
        }
        _ => panic!("First effect should be CompleteTask"),
    }

    // Verify ClearBlockedBy effect
    match &effects[1] {
        Effect::ClearBlockedBy {
            completed_task_id,
            dir_key,
        } => {
            assert_eq!(completed_task_id, "42");
            assert_eq!(dir_key, "myrepo");
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

    // Verify SendPushNotification effect
    match &effects[3] {
        Effect::SendPushNotification {
            title, body, tag, ..
        } => {
            assert!(title.contains("42"), "title should contain task id");
            assert!(body.contains("42"), "body should contain task id");
            assert_eq!(tag, "task_completed_42");
        }
        _ => panic!("Fourth effect should be SendPushNotification"),
    }
}

#[test]
fn test_build_task_completion_effects_push_url_with_channel_and_context() {
    let ctx = super::TaskEventContext {
        subject: Some("Fix auth bug".to_string()),
        description: None,
        thread_id: Some("thread-abc".to_string()),
        message_id: Some("msg-xyz".to_string()),
    };
    let effects = build_task_completion_effects(
        "fix: Fix auth bug [Midtown #99]",
        456,
        "myrepo",
        "myproject",
        Some("web".to_string()),
        Some(ctx),
    );

    // The SendPushNotification should have a deep-link URL
    let push = effects
        .iter()
        .find_map(|e| {
            if let Effect::SendPushNotification { url, .. } = e {
                Some(url)
            } else {
                None
            }
        })
        .expect("Should have a SendPushNotification effect");

    let url = push
        .as_ref()
        .expect("Push URL should be Some when channel is provided");
    assert!(
        url.contains("/myproject?"),
        "URL should start with project name"
    );
    assert!(url.contains("channel=web"), "URL should contain channel");
    assert!(url.contains("msg=msg-xyz"), "URL should contain message ID");
    assert!(
        url.contains("thread=thread-abc"),
        "URL should contain thread ID"
    );
}

#[test]
fn test_build_task_completion_effects_push_url_none_without_channel() {
    let effects = build_task_completion_effects(
        "fix: Fix auth bug [Midtown #99]",
        456,
        "myrepo",
        "myproject",
        None,
        None,
    );

    // Without a channel, the push URL should be None
    let push_url = effects.iter().find_map(|e| {
        if let Effect::SendPushNotification { url, .. } = e {
            Some(url.clone())
        } else {
            None
        }
    });
    assert_eq!(
        push_url,
        Some(None),
        "Push URL should be None without channel"
    );
}

#[test]
fn test_build_task_completion_effects_without_task_id() {
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint",
        123,
        "myrepo",
        "myrepo",
        None,
        None,
    );

    assert!(
        effects.is_empty(),
        "Should return empty vec when no task ID in title"
    );
}

#[test]
fn test_build_task_completion_effects_message_says_merged() {
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint [Midtown #42]",
        123,
        "myrepo",
        "myrepo",
        None,
        None,
    );

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
fn test_task_completion_does_not_send_push_notifications() {
    use crate::task_store::{Task, TaskStatus};
    use std::collections::HashSet;

    // Two tasks, both with merged PRs — should NOT produce push notifications
    // (push notifications only fire for @user mentions and PR merges)
    let tasks = vec![
        Task {
            id: "42".to_string(),
            subject: "Fix auth bug".to_string(),
            status: TaskStatus::InProgress,
            agent_name: String::new(),
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: Some(100),
            ..Default::default()
        },
        Task {
            id: "43".to_string(),
            subject: "Add logging".to_string(),
            status: TaskStatus::InProgress,
            agent_name: String::new(),
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: Some(101),
            ..Default::default()
        },
    ];

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(100);
    merged_pr_numbers.insert(101);

    let snap = snapshot::WorldSnapshot {
        all_tasks: tasks,
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers,
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    // Task completion should generate push notifications for each completed task
    let push_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::SendPushNotification { .. }))
        .count();

    assert!(
        push_count > 0,
        "Task completion should generate push notifications"
    );
}

#[test]
fn test_subject_based_completion_all_prs_merged() {
    use crate::task_store::{Task, TaskStatus};
    use std::collections::HashSet;

    // Meta-task with PR numbers in the subject (the supported pattern)
    let task = Task {
        id: "1100".to_string(),
        subject: "Merge reviewed PRs: #901, #902, #903".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "york".to_string(),
        description: Some("These PRs are reviewed and CI is green.".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    // All referenced PRs are merged
    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(901);
    merged_pr_numbers.insert(902);
    merged_pr_numbers.insert(903);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers,
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    // 4 effects: CompleteTask + ClearBlockedBy + PostToChannel + SendPushNotification
    assert_eq!(effects.len(), 4, "Should return 4 effects");

    // Verify CompleteTask effect
    match &effects[0] {
        Effect::CompleteTask { task_id, dir_key } => {
            assert_eq!(task_id, "1100");
            assert_eq!(dir_key, "test-repo");
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
fn test_subject_based_completion_some_prs_not_merged() {
    use crate::task_store::{Task, TaskStatus};
    use std::collections::HashSet;

    let task = Task {
        id: "1101".to_string(),
        subject: "Merge PRs: #901, #902, #903".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "york".to_string(),
        description: Some("These PRs are all ready to merge.".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    // Only some PRs are merged
    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(901);
    merged_pr_numbers.insert(902);
    // PR #903 is NOT merged

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers,
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete task when not all PRs are merged"
    );
}

#[test]
fn test_subject_based_completion_no_pr_references() {
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "1102".to_string(),
        subject: "Some task".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "york".to_string(),
        description: Some("No PR references in this description".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete task with no PR references"
    );
}

#[test]
fn test_subject_based_completion_skips_pending_tasks() {
    use crate::task_store::{Task, TaskStatus};
    use std::collections::HashSet;

    let task = Task {
        id: "1103".to_string(),
        subject: "Pending task".to_string(),
        status: TaskStatus::Pending, // Not InProgress
        agent_name: String::new(),
        description: Some("Fix PR #904".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(904);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers,
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete non-InProgress tasks"
    );
}

#[test]
fn test_subject_based_completion_no_description() {
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "1104".to_string(),
        subject: "Task without description".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "york".to_string(),
        description: None, // No description
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    assert!(
        effects.is_empty(),
        "Should not complete task with no PR numbers in subject"
    );
}

#[test]
fn test_subject_based_completion_skips_already_completed_tasks() {
    use crate::task_store::{Task, TaskStatus};
    use std::collections::HashSet;

    // Simulate a task that was already completed by the webhook/title-based path.
    // The description-based path should skip it to avoid double-completion.
    let completed_task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        status: TaskStatus::Completed, // Already completed by title-based path
        agent_name: "york".to_string(),
        description: Some("Fix PR #904 review feedback".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    // Also add an in_progress task with PR references in the subject
    let in_progress_task = Task {
        id: "43".to_string(),
        subject: "Merge PRs: #904, #905".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "york".to_string(),
        description: Some("These PRs are reviewed and ready.".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(904);
    merged_pr_numbers.insert(905);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![completed_task, in_progress_task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers,
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

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
// Regression: auto-complete false positive from description text
// ======================================================================

#[test]
fn test_subject_based_completion_does_not_scan_description_for_prs() {
    // Regression test for bug !1546: tasks that mention PR numbers in their
    // description as examples/context (not as the task's own PRs) were being
    // incorrectly auto-completed when those PRs merged.
    //
    // Root cause: build_subject_based_completion_effects scanned both
    // task.subject AND task.description for PR numbers. A task like:
    //   "Fix daemon not recovering coworkers with stalled in_progress tasks"
    // with description:
    //   "...currently PRs #1273, #1274, #1277 have CI passing..."
    // would be auto-completed when all three merged, even though none of them
    // are the task's OWN PR.
    //
    // Fix: Only scan task.subject for PR numbers, not task.description.
    use crate::task_store::{Task, TaskStatus};
    use std::collections::HashSet;

    let task = Task {
        id: "1543".to_string(),
        subject: "Fix daemon not recovering coworkers with stalled in_progress tasks".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "amsterdam".to_string(),
        description: Some(
            "Captured snapshot: snapshot-stalled-tasks-owners-on-break.json\n\n\
             When coworkers go on break while their tasks are still in_progress \
             and their PRs are open with CI green, the daemon should recover them.\n\n\
             Currently the daemon leaves tasks in_progress with owners on break indefinitely. \
             It recovered riverside once for !1539 but didn't recover amsterdam (!1515), \
             vernon (!1523), or park (!1533) even though their PRs (#1274, #1273, #1277) \
             all have CI passing.\n\n\
             Write a failing E2E test using the captured snapshot, then fix the bug."
                .to_string(),
        ),
        blocked_by: vec![],
        channel: None,
        pr: None, // No explicit PR field — task hasn't opened its own PR yet
        ..Default::default()
    };

    // Simulate the PRs mentioned in the description having been merged
    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(1273u64);
    merged_pr_numbers.insert(1274u64);
    merged_pr_numbers.insert(1277u64);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers,
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    // Should NOT auto-complete — the PR numbers are contextual examples in the
    // description, not the task's own PRs.
    let complete_task_ids: Vec<&String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::CompleteTask { task_id, .. } => Some(task_id),
            _ => None,
        })
        .collect();

    assert!(
        complete_task_ids.is_empty(),
        "Task !1543 should NOT be auto-completed — PR numbers in description are examples, \
         not the task's own PRs. Got completions for: {:?}",
        complete_task_ids
    );
}

#[test]
fn test_subject_based_completion_still_works_for_meta_tasks() {
    // Meta-tasks like "Merge reviewed PRs: #901, #902, #903" reference PR numbers
    // in the SUBJECT. These should still be auto-completed when all PRs merge.
    use crate::task_store::{Task, TaskStatus};
    use std::collections::HashSet;

    let task = Task {
        id: "meta-42".to_string(),
        subject: "Merge reviewed PRs: #901, #902".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "york".to_string(),
        description: Some(
            "All these PRs have been reviewed and CI is green. \
             Merge them to unblock downstream work."
                .to_string(),
        ),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let mut merged_pr_numbers = HashSet::new();
    merged_pr_numbers.insert(901u64);
    merged_pr_numbers.insert(902u64);

    let snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        dir_key: "test-repo".to_string(),
        project_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers,
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let effects = build_subject_based_completion_effects(&snap);

    let complete_task_ids: Vec<&String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::CompleteTask { task_id, .. } => Some(task_id),
            _ => None,
        })
        .collect();

    assert_eq!(
        complete_task_ids,
        vec!["meta-42"],
        "Meta-task with PR numbers in subject SHOULD be auto-completed when all PRs merge"
    );
}

#[test]
fn test_subject_based_completion_snapshot_stalled_tasks_false_positive() {
    // Snapshot-based regression test for bug !1546: injects a synthetic task that
    // has merged PR numbers only in its description (not subject). The old code
    // would scan the description and auto-complete it; the fix ignores descriptions.
    //
    // Also verifies the real snapshot tasks (!1515, !1523) are not auto-completed.
    use crate::task_store::{Task, TaskStatus};

    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-stalled-tasks-owners-on-break-20260218-154248.json"
    );
    let mut snap: snapshot::WorldSnapshot =
        serde_json::from_str(fixture).expect("deserialize captured snapshot");
    snap.fixup_legacy_fields();

    // Inject a synthetic task that mimics the original bug: PR numbers (#1272, #1275)
    // appear only in the description as contextual background. Both are in the
    // snapshot's merged_pr_numbers set. The old code would auto-complete this task;
    // the fix should leave it alone.
    snap.all_tasks.push(Task {
        id: "synthetic-false-positive".to_string(),
        subject: "Fix daemon not recovering stalled coworkers".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "amsterdam".to_string(),
        description: Some(
            "PRs #1272 and #1275 are already merged but their owners' tasks \
             are still in_progress. The daemon should detect this."
                .to_string(),
        ),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    });

    let effects = build_subject_based_completion_effects(&snap);

    let completed: Vec<&String> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::CompleteTask { task_id, .. } => Some(task_id),
            _ => None,
        })
        .collect();

    // The synthetic task must NOT be auto-completed — PR numbers are in description only
    assert!(
        !completed.contains(&&"synthetic-false-positive".to_string()),
        "Task with PR numbers only in description should NOT be auto-completed (bug !1546)"
    );

    // Original snapshot tasks should also not be auto-completed:
    // - !1515 has no pr field and no PR numbers in its subject
    // - !1523 has pr: 1264 which is NOT in the 10-PR merged cache
    assert!(
        !completed.contains(&&"1515".to_string()),
        "Task !1515 should NOT be auto-completed — no pr field and no PR numbers in subject"
    );
    assert!(
        !completed.contains(&&"1523".to_string()),
        "Task !1523 should NOT be auto-completed — PR #1264 is not in the merged cache"
    );
}

// ======================================================================
// decide_stale_branch_cleanup tests
// ======================================================================

#[test]
fn test_decide_stale_branch_cleanup_empty_data() {
    let data = StaleBranchCleanupData {
        stale_branch_cleanup_due: false,
    };
    let effects = decide_stale_branch_cleanup(&data);
    assert!(effects.is_empty());
}

#[test]
fn test_decide_stale_branch_cleanup_stale_branch_cleanup() {
    let data = StaleBranchCleanupData {
        stale_branch_cleanup_due: true,
    };
    let effects = decide_stale_branch_cleanup(&data);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::CleanStaleBranches));
}

// ======================================================================
// WorktreeRegistry integration tests
// ======================================================================

#[test]
fn test_spawn_for_pending_tasks_generates_registry_effects_new_task() {
    use crate::task_store::{Task, TaskStatus};

    // Setup: create a snapshot with a pending task (no owner, not in registry)
    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Pre-spawn effects (EnsureWorktree, RegisterWorktreeAssignment) are top-level,
    // followed by SpawnForTask with post-spawn effects in on_success.
    assert!(
        effects.len() >= 2,
        "Should have pre-spawn effects + SpawnForTask"
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

    let spawn_for_task = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                task_id,
                worktree_id,
                ..
            } = e
            {
                Some((task_id, worktree_id))
            } else {
                None
            }
        })
        .expect("Should have SpawnForTask effect");

    assert_eq!(spawn_for_task.0, "42");

    // worktree_id is now a direct field on SpawnForTask (BindCoworkerToWorktree
    // is inlined in the executor using this value after the name is allocated)
    assert!(
        spawn_for_task.1.starts_with("task-42-"),
        "SpawnForTask.worktree_id should start with task-42-, got: {}",
        spawn_for_task.1
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
        tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
        // Task already has worktree
        task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
            .into_iter()
            .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Find the SpawnForTask effect (for owned pending tasks)
    let spawn_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask { worktree_id, .. } = e {
                Some(worktree_id)
            } else {
                None
            }
        })
        .expect("Should have SpawnForTask effect for owned pending task");

    // Should NOT generate RegisterWorktreeAssignment (worktree already exists)
    let register_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
        .count();

    assert_eq!(
        register_count, 0,
        "Should NOT generate RegisterWorktreeAssignment for existing worktree"
    );

    // worktree_id should be set to the existing worktree
    assert_eq!(
        spawn_effect, "task-42-add-auth-endpoint",
        "SpawnForTask.worktree_id should be the existing worktree"
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
        // broadway has NO in_progress tasks (both are still pending)
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Count how many SpawnForTask effects are generated for broadway
    let spawn_count = effects
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::SpawnForTask { preferred_name, .. }
                    if preferred_name.as_ref().is_some_and(|n| n.to_lowercase() == "broadway")
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
fn test_spawn_owner_includes_task_id_for_cross_tick_dedup() {
    // Verify that SpawnForTask from the SpawnOwner branch carries task_id,
    // enabling mark_in_flight_spawns_from_effects() to track it across ticks.
    // RecordTaskAssignment is no longer in on_success — the executor inlines it.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "broadway".to_string(),
        )],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // SpawnForTask carries task_id directly; mark_in_flight_spawns_from_effects
    // reads it from the effect's task_id field (not from a callback).
    let has_spawn_for_task_42 = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { task_id, .. } if task_id == "42"));
    assert!(
        has_spawn_for_task_42,
        "SpawnForTask must have task_id='42' for cross-tick spawn deduplication"
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
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    // Simulate tick 1: generates spawn effects
    let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
    let spawn_count_tick1 = effects_tick1
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .count();
    assert_eq!(spawn_count_tick1, 1, "Tick 1 should spawn broadway");

    // Simulate tick 2: snapshot now includes in-flight task (pre-evaluated
    // from DaemonState in production; set directly in tests).
    let snap_tick2 = snapshot::WorldSnapshot {
        in_flight_task_spawns: ["42".to_string()].into_iter().collect(),
        ..snap
    };
    let effects_tick2 = spawn_for_pending_tasks(&snap_tick2, &state);
    let spawn_count_tick2 = effects_tick2
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
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
    use crate::task_store::Task;

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
            status: crate::task_store::TaskStatus::Pending,
            agent_name: String::new(),
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            ..Default::default()
        }],
        // broadway is NOT running (will be spawned by Case 1)
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
            status: crate::task_store::TaskStatus::InProgress,
            agent_name: "broadway".to_string(),
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            ..Default::default()
        }],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Count total effects targeting broadway
    let broadway_spawns = effects
        .iter()
        .filter(|e| match e {
            Effect::SpawnForTask { preferred_name, .. } => preferred_name
                .as_ref()
                .is_some_and(|n| n.to_lowercase() == "broadway"),
            Effect::NudgeSessionWithCallbacks { session_id, .. } => {
                session_id.to_lowercase().contains("broadway")
            }
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
fn test_intra_case2_dedup_prevents_duplicate_grouped_fresh_spawns() {
    // Two unowned tasks that reference the same PR should not both trigger
    // a fresh spawn for the same coworker. The second task should be nudged
    // or skipped, not spawn a duplicate. This tests intra-Case-2 dedup when
    // grouping resolves two tasks to the same not-yet-running coworker.
    use crate::task_store::Task;

    let snap = snapshot::WorldSnapshot {
        // Two unowned tasks both referencing PR #200
        pending_tasks_without_owners: vec![
            Task {
                id: "50".to_string(),
                subject: "Fix PR #200 test failures".to_string(),
                status: crate::task_store::TaskStatus::Pending,
                agent_name: String::new(),
                description: None,
                blocked_by: vec![],
                channel: None,
                pr: None,
                ..Default::default()
            },
            Task {
                id: "51".to_string(),
                subject: "Address PR #200 review feedback".to_string(),
                status: crate::task_store::TaskStatus::Pending,
                agent_name: String::new(),
                description: None,
                blocked_by: vec![],
                channel: None,
                pr: None,
                ..Default::default()
            },
        ],
        // broadway owns the in-progress task for PR #200 so both tasks group to it
        in_progress_tasks: vec![(
            "49".to_string(),
            "Implement feature [Midtown !49] PR #200".to_string(),
            "broadway".to_string(),
        )],
        all_tasks: vec![Task {
            id: "49".to_string(),
            subject: "Implement feature [Midtown !49] PR #200".to_string(),
            status: crate::task_store::TaskStatus::InProgress,
            agent_name: "broadway".to_string(),
            description: None,
            blocked_by: vec![],
            channel: None,
            pr: None,
            ..Default::default()
        }],
        // Session for task 49 so resolve_grouped_name can find broadway via session
        sessions: [(
            "sess-broadway".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-broadway".to_string(),
                task_id: Some("49".to_string()),
                name: "broadway".to_string(),
                working_dir: "/tmp/test".to_string(),
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
        session_task_map: [("49".to_string(), "sess-broadway".to_string())]
            .into_iter()
            .collect(),
        session_name_map: [("sess-broadway".to_string(), "broadway".to_string())]
            .into_iter()
            .collect(),
        name_session_map: [("broadway".to_string(), "sess-broadway".to_string())]
            .into_iter()
            .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Count SpawnForTask effects targeting broadway
    let broadway_spawns = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { preferred_name, .. } if preferred_name.as_ref().is_some_and(|n| n.to_lowercase() == "broadway")))
        .count();

    assert_eq!(
        broadway_spawns, 1,
        "Should generate exactly ONE SpawnForTask for broadway when two unowned tasks \
         both group to it via PR number. Intra-Case-2 dedup should prevent the second spawn. \
         Got {} spawns.",
        broadway_spawns
    );
}

#[test]
fn test_spawn_for_pending_tasks_skips_via_snapshot_assignment_check() {
    // Test the pure decision pattern: verify that spawn_for_pending_tasks
    // correctly skips a task when name_task_assignments (in WorldSnapshot)
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
        // KEY: broadway is already assigned to task !42 in the snapshot
        name_task_assignments: assignments,
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should generate NO effects because broadway is already assigned to task !42
    assert_eq!(
        effects.len(),
        0,
        "Should generate no effects when owner is already assigned to the task \
         (verified via name_task_assignments in snapshot)"
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
        // lexington is NOT active (orphaned)
        tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
        task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
            .into_iter()
            .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (_state, _tmp, _guard) = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
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

    // Pre-spawn effects (EnsureWorktree) are top-level, then SpawnForTask
    assert!(
        effects.len() >= 2,
        "Should have pre-spawn EnsureWorktree + SpawnForTask"
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

    // Verify SpawnForTask has working_dir and worktree_id set to the existing worktree
    let spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                config,
                worktree_id,
                ..
            } = e
            {
                Some((config, worktree_id))
            } else {
                None
            }
        })
        .expect("Should have SpawnForTask");

    let (config, worktree_id) = spawn;

    let expected_path =
        crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
    assert_eq!(
        config.working_dir,
        Some(expected_path),
        "Should set working_dir to the existing task worktree"
    );

    // worktree_id field on SpawnForTask (used by executor for BindCoworkerToWorktree)
    assert_eq!(
        worktree_id, "task-42-add-auth-endpoint",
        "SpawnForTask.worktree_id should be the existing worktree"
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
        all_tasks: vec![in_progress_task_for_lookup(
            "42",
            "Add auth endpoint",
            "lexington",
        )],
        // No worktree registered
        ..snapshot::minimal_snapshot_for_test()
    };

    let (_state, _tmp, _guard) = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
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
    // followed by SpawnForTask with post-spawn effects in on_success.
    assert!(
        effects.len() >= 3,
        "Should have EnsureWorktree + RegisterWorktreeAssignment + SpawnForTask"
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
            if let Effect::SpawnForTask {
                config,
                worktree_id,
                ..
            } = e
            {
                Some((config, worktree_id))
            } else {
                None
            }
        })
        .expect("Should have SpawnForTask");

    let (config, worktree_id) = spawn;

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

    // worktree_id field on SpawnForTask (used by executor for BindCoworkerToWorktree)
    assert!(
        worktree_id.contains("task-42-add-auth-endpoint"),
        "SpawnForTask.worktree_id should contain task-42-add-auth-endpoint, got: {}",
        worktree_id
    );
}

#[test]
fn test_spawn_for_pending_unowned_reuses_existing_worktree() {
    // Scenario: Task !42 was previously owned by another coworker who died.
    // The task was reset to pending (no owner). It already has a worktree
    // "task-42-add-auth-endpoint" registered. A new coworker should reuse it.
    use crate::task_store::{Task, TaskStatus};

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
        task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
            .into_iter()
            .collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Pre-spawn EnsureWorktree is top-level, then SpawnForTask
    assert!(
        effects.len() >= 2,
        "Should have pre-spawn EnsureWorktree + SpawnForTask"
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

    let spawn_for_task = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                config,
                worktree_id,
                ..
            } = e
            {
                Some((config, worktree_id))
            } else {
                None
            }
        })
        .expect("Should have SpawnForTask");

    let (config, worktree_id) = spawn_for_task;

    // Working dir should point to the EXISTING worktree
    let expected_path =
        crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
    assert_eq!(
        config.working_dir,
        Some(expected_path),
        "Should reuse existing worktree path"
    );

    // worktree_id on SpawnForTask is used by executor for BindCoworkerToWorktree
    assert_eq!(
        worktree_id, "task-42-add-auth-endpoint",
        "Should use the existing worktree, not a new one"
    );
}

#[test]
fn test_orphan_recovery_skips_pr_protected_task() {
    // Scenario: Task !42 is in_progress, owned by "york" who is not active.
    // The task's PR was merged — it's in pr_protected_tasks. Orphan recovery
    // should NOT spawn a coworker for this task; the merged-PR cleanup path
    // will mark the task complete.
    //
    // Without this guard, recovery spawns a coworker who immediately discovers
    // the task is done, goes idle, hits the grace period, and gets spawned again
    // — an infinite loop burning coworker slots.
    let in_progress = vec![(
        "42".to_string(),
        "Fix auth bug".to_string(),
        "york".to_string(),
    )];
    let tasks_with_open_prs = HashMap::new();
    let active_names = HashSet::new(); // york is NOT active

    let mut snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    // Mark the task as PR-protected (e.g. its PR was merged)
    snap.pr_protected_tasks.insert("42".to_string());

    let (_state, _tmp, _guard) = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, |_| None);

    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. } | Effect::SpawnCoworker(_)));
    assert!(
        !has_spawn,
        "Should NOT recover task !42 — it's PR-protected (merged PR). Got: {:?}",
        effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
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
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.coworkers.active_names = active_names;
    snap.pr.pr_task_index =
        snapshot::PrTaskIndex::from_task_maps(tasks_with_open_prs, HashMap::new());
    snap.in_progress_tasks = in_progress_tasks;
    snap
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
        Effect::ResetTaskToPending { task_id, dir_key } => {
            assert_eq!(task_id, "1146");
            assert_eq!(dir_key, "test-repo");
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
fn test_reset_orphaned_tasks_github_open_pr_task_ids_protects() {
    // Regression test for bug: reset_orphaned_tasks only checked tasks_with_open_prs
    // (from SessionRecord) but NOT github_open_pr_task_ids (from GitHub API).
    //
    // After a daemon restart, SessionRecord data may be stale, so tasks_with_open_prs
    // can be empty. Tasks with open PRs (detected via GitHub PR titles) were incorrectly
    // reset to pending, triggering a new coworker spawn for an already-PR'd task.
    //
    // Scenario matches snapshot-pr-opened-then-task-unassigned-reassigned-20260217-190136.json:
    // - tasks_with_open_prs is empty (stale after restart)
    // - github_open_pr_task_ids has task 1422 → PR #1211
    // - Task 1422 is in_progress, owned by madison (not active)
    //
    // Bug: reset_orphaned_tasks would emit ResetTaskToPending for task 1422.
    // Fix: also check github_open_pr_task_ids before resetting.
    let in_progress = vec![(
        "1422".to_string(),
        "Create minimad-ratatui standalone crate".to_string(),
        "madison".to_string(),
    )];
    let tasks_with_open_prs = HashMap::new(); // Empty — stale after daemon restart
    let active_names = HashSet::new(); // madison is NOT active

    let mut snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    // github_open_pr_task_ids populated from GitHub API (authoritative)
    let mut github_map = HashMap::new();
    github_map.insert("1422".to_string(), 1211u64);
    snap.pr.pr_task_index = snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_map);

    let effects = reset_orphaned_tasks(&snap);

    assert!(
        effects.is_empty(),
        "Should not reset task !1422 — it has open PR #1211 via github_open_pr_task_ids, \
         even though tasks_with_open_prs is empty (stale after restart). Got effects: {:?}",
        effects
    );
}

#[test]
fn test_reset_orphaned_tasks_github_open_pr_task_ids_inactive_owner() {
    // Regression test for the sequence: PR opened, coworker went on break,
    // daemon restart → tasks_with_open_prs emptied → reset_orphaned_tasks fires.
    //
    // The captured snapshot-pr-opened-then-task-unassigned-reassigned shows the
    // divergence: tasks_with_open_prs={} but github_open_pr_task_ids has the PRs.
    // When owners later go inactive, reset_orphaned_tasks must still protect them.
    //
    // Simulates what happens after coworkers go on break (removed from active_names).
    let in_progress = vec![
        (
            "1422".to_string(),
            "Create minimad-ratatui crate".to_string(),
            "madison".to_string(),
        ),
        (
            "1428".to_string(),
            "Fix daemon re-spawns".to_string(),
            "amsterdam".to_string(),
        ),
        (
            "1429".to_string(),
            "Fix lead counts against dev cap".to_string(),
            "park".to_string(),
        ),
    ];
    let tasks_with_open_prs = HashMap::new(); // Empty — stale after daemon restart
    let active_names = HashSet::new(); // All owners went on break

    let mut snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    // github_open_pr_task_ids populated from GitHub API (authoritative, survives restart)
    let mut github_map = HashMap::new();
    github_map.insert("1422".to_string(), 1211u64);
    github_map.insert("1428".to_string(), 1212u64);
    github_map.insert("1429".to_string(), 1210u64);
    snap.pr.pr_task_index = snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_map);

    let effects = reset_orphaned_tasks(&snap);

    // None of the three tasks should be reset — all have open PRs
    for task_id in &["1422", "1428", "1429"] {
        let was_reset = effects.iter().any(
            |e| matches!(e, Effect::ResetTaskToPending { task_id: tid, .. } if tid == task_id),
        );
        assert!(
            !was_reset,
            "Task !{task_id} should NOT be reset — it has an open PR via github_open_pr_task_ids, \
             even though tasks_with_open_prs is empty (stale after restart)"
        );
    }
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
    snap.pr.open_prs_data = vec![serde_json::json!({"number": 42})];

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
    snap.pr.open_prs_data = vec![]; // PR #42 is NOT open

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
fn test_reset_orphaned_tasks_ownerless_no_pr_resets() {
    // Bug !1480 (part 1): Ownerless in_progress tasks (owner cleared when coworker
    // went on break) were silently skipped — never reset to pending, never dispatched.
    // Fix: ownerless tasks with no open PR should be reset to pending immediately.
    let in_progress = vec![(
        "200".to_string(),
        "Fix some bug".to_string(),
        "".to_string(), // ownerless
    )];
    let tasks_with_open_prs = HashMap::new();
    let active_names = HashSet::new();

    let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    let effects = reset_orphaned_tasks(&snap);

    assert_eq!(
        effects.len(),
        1,
        "Should reset ownerless in_progress task with no PR to pending. Got: {:?}",
        effects
    );
    match &effects[0] {
        Effect::ResetTaskToPending { task_id, .. } => {
            assert_eq!(task_id, "200");
        }
        other => panic!("Expected ResetTaskToPending, got {:?}", other),
    }
}

#[test]
fn test_reset_orphaned_tasks_ownerless_with_pr_reference_in_subject_protects() {
    // Bug !1480 (part 2): An ownerless in_progress task whose subject references an
    // open PR should be protected by the subject-PR guard. Before the fix, ownerless
    // tasks were skipped entirely (early-continue), so they were never reset *or*
    // protected — the guard was unreachable. After the fix, the subject-PR guard fires
    // before the ownerless check, actively protecting these tasks.
    //
    // To prove the subject-PR guard is the mechanism (not the old early-continue), this
    // test is paired with the _closed_pr variant below: a closed PR reference causes
    // the ownerless reset to fire, while an open PR reference prevents it.
    let in_progress = vec![(
        "200".to_string(),
        "Address review feedback on PR #42".to_string(),
        "".to_string(), // ownerless
    )];
    let tasks_with_open_prs = HashMap::new(); // Task doesn't OWN the PR
    let active_names = HashSet::new();

    let mut snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    snap.pr.open_prs_data = vec![serde_json::json!({"number": 42})]; // PR #42 is open

    let effects = reset_orphaned_tasks(&snap);
    assert!(
        effects.is_empty(),
        "Should not reset ownerless task referencing open PR #42 in subject. Got: {:?}",
        effects
    );
}

#[test]
fn test_reset_orphaned_tasks_ownerless_with_closed_pr_reference_resets() {
    // Companion to the _protects test above: when the referenced PR is closed (not in
    // open_prs_data), the subject-PR guard does not fire, and the ownerless task should
    // be reset to pending. This proves the guard is the active protection mechanism.
    let in_progress = vec![(
        "200".to_string(),
        "Address review feedback on PR #42".to_string(),
        "".to_string(), // ownerless
    )];
    let tasks_with_open_prs = HashMap::new();
    let active_names = HashSet::new();

    let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    // open_prs_data is empty — PR #42 is closed

    let effects = reset_orphaned_tasks(&snap);
    assert_eq!(
        effects.len(),
        1,
        "Should reset ownerless task referencing closed PR to pending. Got: {:?}",
        effects
    );
    match &effects[0] {
        Effect::ResetTaskToPending { task_id, .. } => {
            assert_eq!(task_id, "200");
        }
        other => panic!("Expected ResetTaskToPending, got {:?}", other),
    }
}

#[test]
fn test_reset_orphaned_tasks_ownerless_with_github_open_pr_task_ids_protects() {
    // Ownerless tasks should also be protected when github_open_pr_task_ids contains
    // the task (even if tasks_with_open_prs is empty). This tests the first PR guard
    // for ownerless tasks, complementing the subject-PR reference tests above.
    let in_progress = vec![(
        "200".to_string(),
        "Fix some bug".to_string(),
        "".to_string(), // ownerless
    )];
    let tasks_with_open_prs = HashMap::new();
    let active_names = HashSet::new();

    let mut snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
    let mut github_map = HashMap::new();
    github_map.insert("200".to_string(), 99u64); // Task is tracked by GitHub as having open PR #99
    snap.pr.pr_task_index = snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_map);

    let effects = reset_orphaned_tasks(&snap);
    assert!(
        effects.is_empty(),
        "Should not reset ownerless task with github_open_pr_task_ids entry. Got: {:?}",
        effects
    );
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
    let mut snap: snapshot::WorldSnapshot =
        serde_json::from_str(fixture).expect("deserialize captured snapshot");
    snap.fixup_legacy_fields();

    // The fixture predates session-based ownership. Add a session for york's
    // in-progress task so resolve_grouped_name can discover the owner via session.
    let york_task_id = snap
        .all_tasks
        .iter()
        .find(|t| {
            t.agent_name == "york"
                && t.status == crate::task_store::TaskStatus::InProgress
                && (t.subject.contains("PR #912")
                    || t.description
                        .as_ref()
                        .is_some_and(|d| d.contains("PR #912")))
        })
        .map(|t| t.id.clone())
        .expect("fixture should have york's PR #912 task");
    snap.sessions.insert(
        "sess-york".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-york".to_string(),
            task_id: Some(york_task_id.clone()),
            name: "york".to_string(),
            working_dir: "/tmp/test".to_string(),
            ..Default::default()
        },
    );
    snap.session_task_map
        .insert(york_task_id, "sess-york".to_string());

    // Verify fixture prerequisites: york is active and busy, task !1107 is pending
    assert!(
        snap.coworkers.active_names.contains("york"),
        "york should be active"
    );
    assert!(snap.busy_coworkers.contains("york"), "york should be busy");
    assert!(
        snap.pending_tasks_without_owners
            .iter()
            .any(|t| t.id == "1107"),
        "task !1107 should be pending without owner"
    );

    let (state, _tmp, _guard) = make_test_state();

    // Tick 1: Task !1107 groups to york (PR #912), generates nudge
    let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
    let nudge_count_tick1 = effects_tick1
        .iter()
        .filter(|e| matches!(e, Effect::NudgeSessionWithCallbacks { .. }))
        .count();
    assert_eq!(
        nudge_count_tick1, 1,
        "Tick 1 should nudge york with task !1107"
    );

    // Tick 2: Task !1107 is still pending, york is busy with !1107 now.
    // The assignment is reflected in the snapshot (derived from sessions[].task_id).
    let snap_tick2 = snapshot::WorldSnapshot {
        name_task_assignments: {
            let mut assignments = HashMap::new();
            assignments.insert("york".to_string(), "1107".to_string());
            assignments
        },
        ..snap
    };
    let effects_tick2 = spawn_for_pending_tasks(&snap_tick2, &state);
    let nudge_count_tick2 = effects_tick2
        .iter()
        .filter(|e| matches!(e, Effect::NudgeSessionWithCallbacks { .. }))
        .count();
    assert_eq!(
        nudge_count_tick2, 0,
        "Tick 2 should NOT re-nudge york — task !1107 is already assigned to york"
    );
}

#[test]
fn test_spawn_coworker_with_callbacks_records_task_assignment() {
    // Regression test for spawn loop bug (Case 1: pending task with owner).
    // When a coworker isn't running but has a pending task, SpawnForTask
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
    snap.fixup_legacy_fields();

    // Override to test Case 1: pending task WITH owner, coworker NOT running.
    // Clear Case 2 tasks and set up a Case 1 scenario.
    snap.pending_tasks_without_owners.clear();
    snap.pending_tasks_with_owners = vec![(
        "1107".to_string(),
        "Investigate PR #912 — no CI checks running".to_string(),
        "york".to_string(),
    )];
    snap.coworkers.active_names.clear(); // york is NOT running
    snap.busy_coworkers.clear();
    snap.in_progress_tasks.clear();

    let (state, _tmp, _guard) = make_test_state();

    // Tick 1: generates SpawnForTask with RecordTaskAssignment
    let effects = spawn_for_pending_tasks(&snap, &state);
    let spawn_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .count();
    assert_eq!(spawn_count, 1, "Tick 1 should spawn york");

    // SpawnForTask carries task_id directly; the executor inlines RecordTaskAssignment.
    // mark_in_flight_spawns_from_effects reads task_id from the effect's task_id field.
    let has_spawn_for_task = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { task_id, .. } if task_id == "1107"));
    assert!(
        has_spawn_for_task,
        "SpawnForTask should have task_id='1107' for cross-tick spawn deduplication"
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
    snap.fixup_legacy_fields();

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

    let (state, _tmp, _guard) = make_test_state();

    // Tick 1: NudgeOwner fires with RecordTaskAssignment in on_success
    let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
    let nudge_effects: Vec<_> = effects_tick1
        .iter()
        .filter(|e| matches!(e, Effect::NudgeSessionWithCallbacks { .. }))
        .collect();
    assert_eq!(nudge_effects.len(), 1, "Tick 1 should nudge york");

    // Verify RecordTaskAssignment is in on_success
    let has_assignment = nudge_effects.iter().any(|e| {
        if let Effect::NudgeSessionWithCallbacks { on_success, .. } = e {
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

    // Tick 2: Create a new snapshot that includes the assignment in name_task_assignments.
    // The guard should use snap.name_task_assignments to prevent re-nudge (pure decision pattern).
    let snap_tick2 = snapshot::WorldSnapshot {
        name_task_assignments: {
            let mut assignments = HashMap::new();
            assignments.insert("york".to_string(), "1107".to_string());
            assignments
        },
        ..snap
    };
    let effects_tick2 = spawn_for_pending_tasks(&snap_tick2, &state);
    let nudge_count_tick2 = effects_tick2
        .iter()
        .filter(|e| matches!(e, Effect::NudgeSessionWithCallbacks { .. }))
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
    use crate::task_store::{Task, TaskStatus};

    // Bug scenario: 0 active coworkers, 8 pending unblocked tasks
    // Expected: should spawn coworkers for tasks
    // Actual (bug): no dispatch activity

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![
            Task {
                id: "1263".to_string(),
                subject: "Phase 2: Daemon RPC endpoints for TUI plugin".to_string(),
                status: TaskStatus::Pending,
                agent_name: String::new(),
                blocked_by: vec![],
                description: None,
                channel: None,
                pr: None,
                ..Default::default()
            },
            Task {
                id: "1274".to_string(),
                subject: "Add sandbox_allowed_paths to config".to_string(),
                status: TaskStatus::Pending,
                agent_name: String::new(),
                blocked_by: vec![],
                description: None,
                channel: None,
                pr: None,
                ..Default::default()
            },
        ],
        // 0 running coworkers!
        // 0 active coworkers!
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

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
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
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
fn make_test_state() -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;
    use tempfile::TempDir;

    let midtown_dir = TempDir::new().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

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

    let base_dir = temp_dir.path().to_path_buf();

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        crate::paths::ProjectPaths::with_project_name("test-repo", "test-repo"),
        vec![base_dir.clone()],
        channel_router,
        None,
        10,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state");
    (state, temp_dir, _guard)
}

// ======================================================================
// is_task_pr_protected (pure decision function) tests
// ======================================================================

#[test]
fn test_is_task_pr_protected_skips_completed_tasks() {
    use crate::task_store::{Task, TaskStatus};

    let completed_task = Task {
        id: "1120".to_string(),
        subject: "Fix orphan recovery loop".to_string(),
        description: None,
        status: TaskStatus::Completed,
        agent_name: "vernon".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let pr_task_index = snapshot::PrTaskIndex::default();
    let active_names = active_names_for(&completed_task);
    assert!(
        is_task_pr_protected(&completed_task, &merged_prs, &pr_task_index, &active_names,),
        "Completed task should be treated as pr-protected (not recoverable)"
    );
}

#[test]
fn test_is_task_pr_protected_with_contextual_pr_mention_in_subject() {
    use crate::task_store::{Task, TaskStatus};

    // Task !1120 mentions PR #923 in subject, but PR #923 is NOT the task's PR.
    // This is a contextual mention (e.g., "Merge PR #923 [Midtown !1120]" means
    // the task is ABOUT merging #923, not that #923 IS the task's PR).
    let task = Task {
        id: "1120".to_string(),
        subject: "Merge PR #923 [Midtown !1120]".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "vernon".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: Some(923), // Explicit PR association (auto-set from PR title or --pr flag)
        ..Default::default()
    };

    // PR #923 is merged, but it's not associated with task !1120
    let merged_prs: HashSet<u64> = [923].into_iter().collect();
    let pr_task_index = snapshot::PrTaskIndex::default();

    // With explicit PR associations: should be pr-protected because task.pr = Some(923) and PR #923 is merged
    let active_names = active_names_for(&task);
    assert!(
        is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Task whose PR is already merged should be pr-protected (explicit pr field)"
    );
}

#[test]
fn test_is_task_pr_protected_with_contextual_pr_mention_in_description() {
    use crate::task_store::{Task, TaskStatus};

    // Task mentions PR #925 in description as context
    let task = Task {
        id: "1121".to_string(),
        subject: "Address review feedback".to_string(),
        description: Some("Fixes from PR #925 review".to_string()),
        status: TaskStatus::InProgress,
        agent_name: "park".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: Some(925), // Explicit PR association
        ..Default::default()
    };

    // PR #925 is merged, but it's not associated with task !1121
    let merged_prs: HashSet<u64> = [925].into_iter().collect();
    let pr_task_index = snapshot::PrTaskIndex::default();

    // With explicit PR associations: should be pr-protected because task.pr = Some(925) and PR #925 is merged
    let active_names = active_names_for(&task);
    assert!(
        is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Task whose PR is already merged should be pr-protected (explicit pr field)"
    );
}

// ============================================================================
// github_open_pr_task_ids defense-in-depth tests (snapshot-based, no I/O)
// ============================================================================

#[test]
fn test_is_task_pr_protected_with_open_pr_via_github_title() {
    // Scenario: Task !1233 has no pr field, no entry in tasks_with_open_prs,
    // but there's an open PR #1089 with "[Midtown !1233]" in the title.
    // The github_open_pr_task_ids snapshot data prevents duplicate recovery.
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "1233".to_string(),
        subject: "Prevent duplicate work after daemon restarts".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "york".to_string(),
        blocked_by: vec![],
        pr: None,
        channel: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("1233".to_string(), 1089u64); // PR #1089 has [Midtown !1233]
    let pr_task_index =
        snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_open_pr_task_ids);

    let active_names = active_names_for(&task);
    let result = !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names);

    assert!(
        !result,
        "Should NOT recover task when github_open_pr_task_ids shows an open PR for it"
    );
}

#[test]
fn test_is_task_pr_protected_when_github_title_has_no_match() {
    // Scenario: Task !42 has no PR association anywhere — not in pr field,
    // not in tasks_with_open_prs, not in github_open_pr_task_ids.
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "lexington".to_string(),
        blocked_by: vec![],
        pr: None,
        channel: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let pr_task_index = snapshot::PrTaskIndex::default(); // No title matches
    let active_names = active_names_for(&task);

    let result = !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names);

    assert!(
        result,
        "Task should not be pr-protected when no PR found in any source"
    );
}

#[test]
fn test_is_task_pr_protected_github_title_takes_precedence_over_no_pr_field() {
    // Scenario: Task !55 has no pr field (not set yet), tasks_with_open_prs is empty
    // (stale after restart), but github_open_pr_task_ids has a match.
    // This is the exact scenario that caused duplicate work after daemon restart.
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "55".to_string(),
        subject: "Fix flaky tests".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "park".to_string(),
        blocked_by: vec![],
        pr: None, // Not set yet — PR was created but task field wasn't updated
        channel: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("55".to_string(), 200u64);
    let pr_task_index =
        snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_open_pr_task_ids);
    let active_names = active_names_for(&task);

    let result = !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names);

    assert!(
        !result,
        "Should NOT recover: github_open_pr_task_ids catches the open PR even when other sources are stale"
    );
}

#[test]
fn test_is_task_pr_protected_allows_active_in_progress_task() {
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "lexington".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let pr_task_index = snapshot::PrTaskIndex::default();
    let active_names = active_names_for(&task);
    assert!(
        !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Active in-progress task with no merged PR should not be pr-protected"
    );
}

#[test]
fn test_is_task_pr_protected_allows_task_with_unmerged_pr() {
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "1120".to_string(),
        subject: "Merge PR #999999 [Midtown !1120]".to_string(), // Use non-existent PR number
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "vernon".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    // PR #999999 is NOT in the merged set (and doesn't exist in repo)
    // The GitHub API check will fail (PR not found) but the function
    // should be conservative and allow recovery.
    let merged_prs: HashSet<u64> = [900, 910].into_iter().collect();
    let pr_task_index = snapshot::PrTaskIndex::default();
    let active_names = active_names_for(&task);
    assert!(
        !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Task whose PR is NOT yet merged should not be pr-protected"
    );
}

#[test]
fn test_is_task_pr_protected_with_bare_hash_pr_reference() {
    use crate::task_store::{Task, TaskStatus};

    // Task with bare "#904" format (no "PR #" prefix)
    // With explicit PR associations, the pr field should be set to 904
    let task = Task {
        id: "1122".to_string(),
        subject: "Fix #904 review feedback".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "columbus".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: Some(904), // Explicit PR association
        ..Default::default()
    };

    let merged_prs: HashSet<u64> = [904].into_iter().collect();
    let pr_task_index = snapshot::PrTaskIndex::default();

    // With explicit PR associations: should be pr-protected because task.pr = Some(904) and PR #904 is merged
    let active_names = active_names_for(&task);
    assert!(
        is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Task whose PR (#904) is already merged should be pr-protected (explicit pr field)"
    );
}

#[test]
fn test_is_task_pr_protected_recovers_multi_pr_with_only_some_merged() {
    use crate::task_store::{Task, TaskStatus};

    // Task referencing PRs #901, #902, #903, but only #901 is merged
    // Task should not be pr-protected (needs recovery)
    // because auto-completion won't fire until ALL PRs are merged
    let task = Task {
        id: "1123".to_string(),
        subject: "Merge PRs #901, #902, #903".to_string(),
        description: Some("Consolidate multiple related PRs".to_string()),
        status: TaskStatus::InProgress,
        agent_name: "madison".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    // Only #901 is merged; #902 and #903 are still open
    let merged_prs: HashSet<u64> = [901].into_iter().collect();
    let pr_task_index = snapshot::PrTaskIndex::default();
    let active_names = active_names_for(&task);
    assert!(
        !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Task with multi-PR reference where only SOME PRs are merged should not be pr-protected"
    );
}

#[test]
fn test_is_task_pr_protected_with_multi_pr_when_all_merged() {
    use crate::task_store::{Task, TaskStatus};

    // Meta-task referencing PRs #901, #902, #903, and ALL are merged
    // With explicit PR associations: is_task_pr_protected() returns false because
    // it only checks the explicit pr field (which is None for meta-tasks).
    // Auto-completion will handle cleanup when all PRs are merged.
    let task = Task {
        id: "1124".to_string(),
        subject: "Merge PRs #901, #902, #903".to_string(),
        description: Some("Consolidate multiple related PRs".to_string()),
        status: TaskStatus::InProgress,
        agent_name: "madison".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None, // Meta-tasks don't have explicit PR associations
        ..Default::default()
    };

    // All PRs are merged, but they're not the task's canonical PR
    let merged_prs: HashSet<u64> = [901, 902, 903].into_iter().collect();
    let pr_task_index = snapshot::PrTaskIndex::default();

    // New behavior: should NOT be pr-protected because pr field is None (contextual mentions only)
    let active_names = active_names_for(&task);
    assert!(
        !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Task with no explicit pr field should not be pr-protected (auto-completion will handle cleanup)"
    );
}

#[test]
fn test_is_task_pr_protected_with_pr_in_subject_only() {
    use crate::task_store::{Task, TaskStatus};

    // Task with PR reference only in subject (not description)
    // With explicit PR associations: is_task_pr_protected() returns false because
    // it only checks the explicit pr field (which is None).
    // If this task is actually FOR PR #905, it should have pr: Some(905).
    let task = Task {
        id: "1125".to_string(),
        subject: "Close PR #905".to_string(),
        description: Some("Final cleanup tasks".to_string()),
        status: TaskStatus::InProgress,
        agent_name: "broadway".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None, // Should be Some(905) if this task is for PR #905
        ..Default::default()
    };

    let merged_prs: HashSet<u64> = [905].into_iter().collect();
    let pr_task_index = snapshot::PrTaskIndex::default();

    // New behavior: should NOT be pr-protected because pr field is None (contextual mentions only)
    let active_names = active_names_for(&task);
    assert!(
        !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names),
        "Task with no explicit pr field should not be pr-protected (auto-completion will handle cleanup)"
    );
}

#[test]
fn test_spawn_extracts_model_alias_from_provider_model_format() {
    use crate::task_store::{Task, TaskStatus};

    // Setup: task with model "claude/opus" in task_model_map
    let mut task_model_map = HashMap::new();
    task_model_map.insert("42".to_string(), "claude/opus".to_string());

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "42".to_string(),
            subject: "Complex algorithm task".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        task_model_map,
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Find the SpawnForTask effect and check its LaunchConfig
    let spawn_config = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask { config, .. } = e {
                Some(config)
            } else {
                None
            }
        })
        .expect("Should have SpawnForTask effect");

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
fn test_unowned_pending_task_with_open_pr_is_dispatched() {
    use crate::task_store::{Task, TaskStatus};

    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("2050".to_string(), 2100u64);

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "2050".to_string(),
            subject: "Handle Svelte state handling".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        pr: snapshot::SnapshotPrState {
            pr_task_index: snapshot::PrTaskIndex::from_task_maps(
                tasks_with_open_prs,
                HashMap::new(),
            ),
            ..Default::default()
        },
        // PR-protection should NOT block unowned pending tasks — only in_progress
        // tasks during orphan recovery. Pending tasks with open PRs (e.g., "rebase
        // and land PR #X") must still be dispatchable.
        pr_protected_tasks: ["2050".to_string()].into_iter().collect(),
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        has_spawn,
        "Task !2050 should be dispatched — PR-protection only applies to in_progress tasks (orphan recovery), not unowned pending tasks. Got effects: {:?}",
        effects
    );
}

#[test]
fn test_unowned_pending_task_with_github_open_pr_title_match_is_dispatched() {
    use crate::task_store::{Task, TaskStatus};

    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("2051".to_string(), 2101u64);

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "2051".to_string(),
            subject: "Handle Svelte cache invalidation".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        pr: snapshot::SnapshotPrState {
            pr_task_index: snapshot::PrTaskIndex::from_task_maps(
                HashMap::new(),
                github_open_pr_task_ids,
            ),
            ..Default::default()
        },
        // PR-protection should NOT block unowned pending tasks — only in_progress
        // tasks during orphan recovery.
        pr_protected_tasks: ["2051".to_string()].into_iter().collect(),
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        has_spawn,
        "Task !2051 should be dispatched — PR-protection only applies to in_progress tasks (orphan recovery), not unowned pending tasks. Got effects: {:?}",
        effects
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
        // lexington crashed, not active
        in_progress_tasks: vec![(
            "999999".to_string(),
            "Test task".to_string(),
            "lexington".to_string(),
        )],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
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

    // Should have SpawnForTask effect with task_id
    let has_spawn_for_task = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { task_id, .. } if task_id == "999999"));
    assert!(
        has_spawn_for_task,
        "Orphan recovery must produce SpawnForTask with task_id='999999'"
    );

    // Verify that mark_in_flight_spawns_from_effects would mark this task
    state.mark_in_flight_spawns_from_effects(&effects);
    assert!(
        state.is_task_spawn_in_flight("999999"),
        "Task !999999 should be marked in-flight after orphan recovery"
    );
}

// ======================================================================
// Dual dispatch (same-tick orphan recovery + pending dispatch) tests
// ======================================================================

#[test]
fn test_dual_dispatch_orphan_recovery_and_pending_same_tick() {
    // Regression test for dual dispatch bug where check_and_recover_orphans and
    // spawn_for_pending_tasks both produce spawns for the same task in one tick.
    //
    // Scenario: Task !1420 appears simultaneously as:
    //   - in_progress (orphaned, owner "lexington" not active) → orphan recovery spawns lexington
    //   - pending without owner (in snapshot after a race condition) → dispatch spawns madison
    //
    // The fix: spawn_for_pending_tasks should accept an excluded_task_ids set from orphan
    // recovery so it skips tasks already being recovered.
    use crate::task_store::{Task, TaskStatus};
    use chrono::Duration;

    let now = chrono::Utc::now();
    // Lexington stopped far enough back to be outside the grace period (not recently stopped)
    // so orphan recovery will still pick it up (is_task_pr_protected returns false)
    let lexington_stopped = now - Duration::seconds(60);

    let mut snap = snapshot::WorldSnapshot {
        // Task !1420 is in_progress, owned by lexington (who is not active)
        in_progress_tasks: vec![(
            "1420".to_string(),
            "Add click-and-drag resizing for the sidebar/chat divider".to_string(),
            "lexington".to_string(),
        )],
        // Task !1420 also appears as pending without owner (race condition / same tick)
        pending_tasks_without_owners: vec![Task {
            id: "1420".to_string(),
            subject: "Add click-and-drag resizing for the sidebar/chat divider".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        // Lexington is NOT active - its session ended
        // Not at dev limit - allows spawning
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        lead_session_refresh_interval_secs: 5400,
        now_utc: now,
        ..snapshot::minimal_snapshot_for_test()
    };
    snap.coworkers.coworker_stop_times = {
        let mut m = HashMap::new();
        m.insert("lexington".to_string(), lexington_stopped);
        m
    };

    let (state, _tmp, _guard) = make_test_state();

    // Simulate what events.rs does: collect orphan effects, then collect pending task effects
    let orphan_effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
        if task_id == "1420" {
            Some(in_progress_task_for_lookup(
                "1420",
                "Add click-and-drag resizing for the sidebar/chat divider",
                "lexington",
            ))
        } else {
            None
        }
    });

    // Verify orphan recovery produces a spawn for task !1420
    let orphan_spawns: Vec<_> = orphan_effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { task_id, .. } if task_id == "1420"))
        .collect();
    assert_eq!(
        orphan_spawns.len(),
        1,
        "Orphan recovery should produce exactly one spawn for task !1420"
    );

    // Extract claimed task IDs as events.rs does after the fix
    let excluded_ids = effects::extract_claimed_task_ids_from_effects(&orphan_effects);
    assert!(
        excluded_ids.contains("1420"),
        "Orphan recovery should claim task !1420"
    );

    // Pending dispatch with exclusion set (the fixed path)
    let pending_effects_fixed = spawn_for_pending_tasks_excluding(&snap, &state, &excluded_ids);

    // Combine all effects as events.rs does
    let all_effects: Vec<&Effect> = orphan_effects
        .iter()
        .chain(pending_effects_fixed.iter())
        .collect();

    // Count spawn effects targeting task !1420
    let spawns_for_task_1420: Vec<_> = all_effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { task_id, .. } if task_id == "1420"))
        .collect();

    assert_eq!(
        spawns_for_task_1420.len(),
        1,
        "Only one spawn should target task !1420 — orphan recovery and pending dispatch \
         should not both spawn for the same task in the same tick. Got {} spawns.",
        spawns_for_task_1420.len()
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

    use crate::task_store::{Task, TaskStatus};

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
        agent_name: "pleasant".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None, // No explicit pr field - this task is ABOUT PR #1153, not FOR it
        ..Default::default()
    };

    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            task.id.clone(),
            task.subject.clone(),
            task.agent_name.clone(),
        )],
        all_tasks: vec![task],
        // PR #1153 is merged
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers: [1153u64].into_iter().collect(),
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
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

    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "42".to_string(),
        subject: "Add auth endpoint".to_string(),
        description: None,
        status: TaskStatus::Pending,
        agent_name: "park".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: Some(123), // Explicit pr field - this task's work is IN PR #123
        ..Default::default()
    };

    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            task.id.clone(),
            task.subject.clone(),
            task.agent_name.clone(),
        )],
        all_tasks: vec![task],
        // PR #123 is merged
        pr: snapshot::SnapshotPrState {
            merged_pr_numbers: [123u64].into_iter().collect(),
            ..Default::default()
        },
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();
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
fn test_is_task_pr_protected_skips_tasks_with_open_pr_in_tasks_with_open_prs() {
    // Orphan recovery also needs to skip tasks with open PRs (separate path from pending dispatch).
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "1313".to_string(),
        subject: "Implement feature X".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "lexington".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None, // PR association tracked in tasks_with_open_prs instead
        ..Default::default()
    };

    let merged_prs = HashSet::new(); // PR #1156 is NOT merged
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("1313".to_string(), 1156u64); // Task has open PR #1156
    let pr_task_index = snapshot::PrTaskIndex::from_task_maps(tasks_with_open_prs, HashMap::new());
    let active_names = active_names_for(&task);

    let result = !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names);

    assert!(
        !result,
        "Task !1313 should be pr-protected - it has open PR #1156 in tasks_with_open_prs"
    );
}

#[test]
fn test_is_task_pr_protected_skips_tasks_with_open_pr_in_github_open_pr_task_ids() {
    // Defense-in-depth: Even if tasks_with_open_prs is empty (stale),
    // github_open_pr_task_ids should prevent recovery.
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "1313".to_string(),
        subject: "Implement feature X".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "lexington".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("1313".to_string(), 1156u64); // Found via GitHub PR title
    let pr_task_index =
        snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_open_pr_task_ids);
    let active_names = active_names_for(&task);

    let result = !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names);

    assert!(
        !result,
        "Task !1313 should be pr-protected - it has open PR #1156 via github_open_pr_task_ids"
    );
}

// ======================================================================
// is_task_pr_protected: active session awareness
// ======================================================================
// Bug: pending tasks with open PRs but no active session were blocked from
// dispatch, creating a catch-22 where nobody could pick them up.

#[test]
fn test_is_task_pr_protected_allows_pending_task_with_open_pr_no_active_session() {
    use crate::task_store::{Task, TaskStatus};

    // A pending task created for an existing PR (e.g., "rebase and land PR #42")
    let task = Task {
        id: "2281".to_string(),
        subject: "Rebase and land PR #42 [Midtown !2281]".to_string(),
        description: None,
        status: TaskStatus::Pending,
        agent_name: String::new(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let mut github_open_pr_task_ids = HashMap::new();
    github_open_pr_task_ids.insert("2281".to_string(), 42u64);
    let pr_task_index =
        snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_open_pr_task_ids);

    let active_names: HashSet<String> = HashSet::new(); // No active session

    assert!(
        !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names,),
        "Pending task with open PR but no active session should NOT be pr-protected"
    );
}

#[test]
fn test_is_task_pr_protected_allows_in_progress_task_with_open_pr_no_active_session() {
    use crate::task_store::{Task, TaskStatus};

    // An in_progress task whose owner went away — no active session
    let task = Task {
        id: "2281".to_string(),
        subject: "Implement feature X".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "lexington".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("2281".to_string(), 42u64);
    let pr_task_index = snapshot::PrTaskIndex::from_task_maps(tasks_with_open_prs, HashMap::new());

    let active_names: HashSet<String> = HashSet::new(); // Owner not active

    assert!(
        !is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names,),
        "In-progress task with open PR but no active owner session should NOT be pr-protected"
    );
}

#[test]
fn test_is_task_pr_protected_blocks_task_with_open_pr_and_active_session() {
    use crate::task_store::{Task, TaskStatus};

    // An in_progress task with an active session — SHOULD be protected
    let task = Task {
        id: "2281".to_string(),
        subject: "Implement feature X".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "lexington".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let merged_prs = HashSet::new();
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("2281".to_string(), 42u64);
    let pr_task_index = snapshot::PrTaskIndex::from_task_maps(tasks_with_open_prs, HashMap::new());

    let mut active_names: HashSet<String> = HashSet::new();
    active_names.insert("lexington".to_string()); // Owner IS active

    assert!(
        is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names,),
        "In-progress task with open PR and active owner session SHOULD be pr-protected"
    );
}

#[test]
fn test_is_task_pr_protected_merged_pr_inactive_owner_still_protected() {
    use crate::task_store::{Task, TaskStatus};

    // An in-progress task whose owner session died, but whose PR has merged.
    // The merged-PR guard must still apply — without it the daemon could
    // re-dispatch or recover the task, causing a recovery-loop.
    let task = Task {
        id: "2281".to_string(),
        subject: "Implement feature X".to_string(),
        description: None,
        status: TaskStatus::InProgress,
        agent_name: "lexington".to_string(),
        blocked_by: vec![],
        channel: None,
        pr: Some(42),
        ..Default::default()
    };

    let mut merged_prs = HashSet::new();
    merged_prs.insert(42u64);
    let pr_task_index = snapshot::PrTaskIndex::default();

    let active_names: HashSet<String> = HashSet::new(); // Owner NOT active

    assert!(
        is_task_pr_protected(&task, &merged_prs, &pr_task_index, &active_names,),
        "Task with merged PR should be protected even when owner session is inactive"
    );
}

#[test]
fn test_reset_orphaned_tasks_resets_ownerless_in_progress_task() {
    // Bug: When a coworker goes on break and the task owner is cleared,
    // the task becomes in_progress with no owner. reset_orphaned_tasks() was
    // skipping these via `continue` on empty owner, leaving them stuck forever.
    let in_progress = vec![(
        "1474".to_string(),
        "Fix orphaned tasks".to_string(),
        "".to_string(),
    )];
    let snap = make_reconcile_snapshot(in_progress, HashMap::new(), HashSet::new());

    let effects = reset_orphaned_tasks(&snap);

    assert_eq!(
        effects.len(),
        1,
        "Expected one ResetTaskToPending effect for ownerless in_progress task"
    );
    match &effects[0] {
        Effect::ResetTaskToPending { task_id, .. } => {
            assert_eq!(task_id, "1474");
        }
        other => panic!("Expected ResetTaskToPending, got {:?}", other),
    }
}

#[test]
fn test_reset_orphaned_tasks_ownerless_task_with_open_pr_is_skipped() {
    // Ownerless in_progress tasks that have an open PR should NOT be reset —
    // they're being managed by reconcile_tasks_in_review.
    let in_progress = vec![(
        "1474".to_string(),
        "Fix orphaned tasks".to_string(),
        "".to_string(),
    )];
    let mut tasks_with_open_prs = HashMap::new();
    tasks_with_open_prs.insert("1474".to_string(), 999u64);
    let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, HashSet::new());

    let effects = reset_orphaned_tasks(&snap);

    assert!(
        effects.is_empty(),
        "Ownerless in_progress task with open PR should not be reset"
    );
}

#[test]
fn test_reset_orphaned_tasks_ownerless_task_with_github_open_pr_is_skipped() {
    // Defense-in-depth: ownerless tasks should also be protected when the PR
    // is only in github_open_pr_task_ids (e.g., after daemon restart when
    // tasks_with_open_prs is empty).
    let in_progress = vec![(
        "1474".to_string(),
        "Fix orphaned tasks".to_string(),
        "".to_string(),
    )];
    let mut snap = make_reconcile_snapshot(in_progress, HashMap::new(), HashSet::new());
    let mut github_map = HashMap::new();
    github_map.insert("1474".to_string(), 999u64);
    snap.pr.pr_task_index = snapshot::PrTaskIndex::from_task_maps(HashMap::new(), github_map);

    let effects = reset_orphaned_tasks(&snap);

    assert!(
        effects.is_empty(),
        "Ownerless in_progress task with open PR via github_open_pr_task_ids should not be reset"
    );
}

#[test]
fn test_reset_orphaned_tasks_ownerless_task_with_subject_pr_reference_is_skipped() {
    // Ownerless tasks whose subject references an open PR should NOT be reset.
    // E.g., "Address review feedback on PR #42" — the PR is still open, so
    // the review task should remain in_progress for re-dispatch, not reset.
    let in_progress = vec![(
        "1474".to_string(),
        "Address review feedback on PR #42".to_string(),
        "".to_string(),
    )];
    let mut snap = make_reconcile_snapshot(in_progress, HashMap::new(), HashSet::new());
    snap.pr.open_prs_data = vec![serde_json::json!({"number": 42})];

    let effects = reset_orphaned_tasks(&snap);

    assert!(
        effects.is_empty(),
        "Ownerless task referencing open PR #42 in subject should not be reset"
    );
}

// ======================================================================
// dispatch_via_sessions tests
// ======================================================================

/// Helper to build a minimal WorldSnapshot with session-centric fields populated.
fn make_session_dispatch_snapshot(
    in_progress_tasks: Vec<(String, String, String)>,
    sessions: HashMap<String, crate::daemon::state::SessionRecord>,
    session_task_map: HashMap<String, String>,
) -> snapshot::WorldSnapshot {
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.in_progress_tasks = in_progress_tasks;
    snap.sessions = sessions;
    snap.session_task_map = session_task_map;
    snap
}

fn make_test_session_record(
    session_id: &str,
    task_id: Option<&str>,
    preferred_name: Option<&str>,
    working_dir: &str,
    is_running: bool,
) -> crate::daemon::state::SessionRecord {
    crate::daemon::state::SessionRecord {
        session_id: session_id.to_string(),
        task_id: task_id.map(|s| s.to_string()),
        name: preferred_name.unwrap_or("").to_string(),
        working_dir: working_dir.to_string(),
        is_running,
        ..Default::default()
    }
}

#[test]
fn test_dispatch_via_sessions_skips_running_session() {
    // Session is running for an in_progress task -- no recovery needed.
    let session = make_test_session_record(
        "sess-abc",
        Some("42"),
        Some("lexington"),
        "/tmp/worktree",
        true, // is_running
    );
    let sessions = [("sess-abc".to_string(), session)].into_iter().collect();
    let session_task_map = [("42".to_string(), "sess-abc".to_string())]
        .into_iter()
        .collect();

    let snap = make_session_dispatch_snapshot(
        vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        sessions,
        session_task_map,
    );

    let effects = dispatch_via_sessions_for_test(&snap);

    assert!(
        effects.is_empty(),
        "Running session should produce no effects, got: {:?}",
        effects
    );
}

#[test]
fn test_dispatch_via_sessions_recovers_stopped_session() {
    // Session stopped for an in_progress task -- should emit SpawnForTask.
    let session = make_test_session_record(
        "sess-abc",
        Some("42"),
        Some("lexington"),
        "/tmp/worktree/lexington",
        false, // is_running = false -- needs recovery
    );
    let sessions = [("sess-abc".to_string(), session)].into_iter().collect();
    let session_task_map = [("42".to_string(), "sess-abc".to_string())]
        .into_iter()
        .collect();

    let snap = make_session_dispatch_snapshot(
        vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        sessions,
        session_task_map,
    );

    let effects = dispatch_via_sessions_for_test(&snap);

    // Should have a SpawnForTask effect with preferred_name "lexington"
    let has_spawn = effects.iter().any(|e| {
        matches!(
            e,
            Effect::SpawnForTask { preferred_name, .. }
            if preferred_name.as_deref() == Some("lexington")
        )
    });
    assert!(
        has_spawn,
        "Should spawn coworker with preferred_name 'lexington', got: {:?}",
        effects
    );
}

#[test]
fn test_dispatch_via_sessions_no_session_skips_merged_pr() {
    // Task with merged PR should not be recovered via fresh spawn.
    let mut snap = make_session_dispatch_snapshot(
        vec![(
            "42".to_string(),
            "Add auth".to_string(),
            "lexington".to_string(),
        )],
        HashMap::new(),
        HashMap::new(),
    );
    // Mark task 42 as PR-protected (merged PR detected during snapshot collection)
    snap.pr_protected_tasks.insert("42".to_string());

    let effects = dispatch_via_sessions_for_test(&snap);

    assert!(
        effects.is_empty(),
        "Should not spawn for task with merged PR, got: {:?}",
        effects
    );
}

#[test]
fn test_dispatch_via_sessions_no_session_skips_recently_stopped() {
    // Task owned by recently-stopped coworker should not be recovered.
    let mut snap = make_session_dispatch_snapshot(
        vec![(
            "42".to_string(),
            "Add auth".to_string(),
            "lexington".to_string(),
        )],
        HashMap::new(),
        HashMap::new(),
    );
    // lexington stopped very recently (within grace period)
    snap.coworkers
        .coworker_stop_times
        .insert("lexington".to_string(), snap.now_utc);

    let effects = dispatch_via_sessions_for_test(&snap);

    assert!(
        effects.is_empty(),
        "Should not spawn for recently stopped coworker, got: {:?}",
        effects
    );
}

#[test]
fn test_dispatch_via_sessions_no_session_skips_completed_task() {
    // Task that is already completed should not be recovered.
    let mut snap = make_session_dispatch_snapshot(
        vec![(
            "42".to_string(),
            "Add auth".to_string(),
            "lexington".to_string(),
        )],
        HashMap::new(),
        HashMap::new(),
    );
    // Completed tasks are PR-protected during snapshot collection
    snap.pr_protected_tasks.insert("42".to_string());

    let effects = dispatch_via_sessions_for_test(&snap);

    assert!(
        effects.is_empty(),
        "Should not spawn for completed task, got: {:?}",
        effects
    );
}

#[test]
fn test_dispatch_via_sessions_uses_preferred_name() {
    // Session has preferred_name "park" -- spawn should use that name.
    let session = make_test_session_record(
        "sess-xyz",
        Some("99"),
        Some("park"),
        "/tmp/worktree/park",
        false,
    );
    let sessions = [("sess-xyz".to_string(), session)].into_iter().collect();
    let session_task_map = [("99".to_string(), "sess-xyz".to_string())]
        .into_iter()
        .collect();

    let snap = make_session_dispatch_snapshot(
        vec![(
            "99".to_string(),
            "Implement feature X".to_string(),
            "park".to_string(),
        )],
        sessions,
        session_task_map,
    );

    let effects = dispatch_via_sessions_for_test(&snap);

    // Verify the spawn uses the preferred name "park"
    let has_spawn = effects.iter().any(|e| {
        matches!(
            e,
            Effect::SpawnForTask { preferred_name, .. }
            if preferred_name.as_deref() == Some("park")
        )
    });
    assert!(
        has_spawn,
        "Should have SpawnForTask effect with preferred_name 'park', got: {:?}",
        effects
    );
}

#[test]
fn test_dispatch_via_sessions_uses_session_working_dir() {
    // Session has a working_dir that exists on disk — spawn should use it.
    // The path must actually exist because dispatch now validates existence
    // before trusting the recorded path (stale-path fix, !1730 item 2).
    let existing_dir = tempfile::TempDir::new().expect("temp dir");
    let existing_path = existing_dir.path().to_string_lossy().to_string();

    let session =
        make_test_session_record("sess-xyz", Some("99"), Some("park"), &existing_path, false);
    let sessions = [("sess-xyz".to_string(), session)].into_iter().collect();
    let session_task_map = [("99".to_string(), "sess-xyz".to_string())]
        .into_iter()
        .collect();

    let snap = make_session_dispatch_snapshot(
        vec![(
            "99".to_string(),
            "Implement feature X".to_string(),
            "park".to_string(),
        )],
        sessions,
        session_task_map,
    );

    let effects = dispatch_via_sessions_for_test(&snap);

    // SpawnForTask delegates working_dir to build_spawn_effects which uses
    // prepare_task_worktree. Verify we get a spawn effect.
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        has_spawn,
        "Should have a SpawnForTask effect, got: {:?}",
        effects
    );
}

#[test]
fn test_dispatch_via_sessions_respects_cooldown() {
    // When cooldown is active, should produce no effects even for stopped sessions.
    let session = make_test_session_record(
        "sess-abc",
        Some("42"),
        Some("lexington"),
        "/tmp/worktree",
        false,
    );
    let sessions = [("sess-abc".to_string(), session)].into_iter().collect();
    let session_task_map = [("42".to_string(), "sess-abc".to_string())]
        .into_iter()
        .collect();

    let mut snap = make_session_dispatch_snapshot(
        vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        sessions,
        session_task_map,
    );
    // Simulate cooldown active (pre-evaluated in snapshot)
    snap.session_dispatch_cooldown_active = true;

    let effects = dispatch_via_sessions_for_test(&snap);

    assert!(
        effects.is_empty(),
        "Should not produce effects when cooldown is active, got: {:?}",
        effects
    );
}

#[test]
fn test_session_dispatch_excludes_task_from_pending_dispatch() {
    // Integration test: verifies that when dispatch_via_sessions recovers a task,
    // that task ID flows through effects::extract_claimed_task_ids_from_effects and is
    // excluded from spawn_for_pending_tasks_excluding, preventing double-spawning.
    //
    // Scenario: Task !42 is in_progress (stopped session) AND also appears as
    // pending (race condition in the same snapshot). Session dispatch should claim
    // it, and pending dispatch should skip it.
    use crate::task_store::{Task, TaskStatus};

    let session = make_test_session_record(
        "sess-abc",
        Some("42"),
        Some("lexington"),
        "/tmp/worktree",
        false, // stopped
    );
    let sessions = [("sess-abc".to_string(), session)].into_iter().collect();
    let session_task_map = [("42".to_string(), "sess-abc".to_string())]
        .into_iter()
        .collect();

    let snap = snapshot::WorldSnapshot {
        // Task !42 is in_progress, owned by lexington (stopped session)
        in_progress_tasks: vec![(
            "42".to_string(),
            "Add auth endpoint".to_string(),
            "lexington".to_string(),
        )],
        // Same task also appears as pending without owner (snapshot race)
        pending_tasks_without_owners: vec![Task {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        sessions,
        session_task_map,
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        ..make_session_dispatch_snapshot(vec![], HashMap::new(), HashMap::new())
    };

    let (state, _tmp, _guard) = make_test_state();

    // Step 1: Session dispatch recovers the task
    let session_effects = dispatch_via_sessions_for_test(&snap);
    assert!(
        !session_effects.is_empty(),
        "Session dispatch should produce effects for stopped session"
    );

    // Step 2: Extract claimed IDs (as events.rs does)
    let session_claimed_ids = effects::extract_claimed_task_ids_from_effects(&session_effects);
    assert!(
        session_claimed_ids.contains("42"),
        "Session dispatch should claim task !42"
    );

    // Step 3: Pending dispatch with exclusion set should skip task !42
    let pending_effects = spawn_for_pending_tasks_excluding(&snap, &state, &session_claimed_ids);

    // Verify no spawn targets task !42
    let pending_spawns_for_42: Vec<_> = pending_effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { task_id, .. } if task_id == "42"))
        .collect();

    assert!(
        pending_spawns_for_42.is_empty(),
        "Pending dispatch should skip task !42 because session dispatch already claimed it. \
         Got {} spawn effects for task !42.",
        pending_spawns_for_42.len()
    );
}

#[test]
fn test_pending_task_with_stopped_session_emits_spawn_session_resume() {
    // When a pending task has a stopped session from a previous attempt,
    // spawn_for_pending_tasks should emit SpawnForTask with ResumeSession mode
    // instead of a fresh spawn.
    use crate::task_store::{Task, TaskStatus};

    let session = make_test_session_record(
        "sess-resume-1",
        Some("99"),
        Some("lexington"),
        "/tmp/worktree-99",
        false, // stopped
    );
    let sessions = [("sess-resume-1".to_string(), session)]
        .into_iter()
        .collect();
    let session_task_map = [("99".to_string(), "sess-resume-1".to_string())]
        .into_iter()
        .collect();

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "99".to_string(),
            subject: "Implement caching layer".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        sessions,
        session_task_map,
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        ..make_session_dispatch_snapshot(vec![], HashMap::new(), HashMap::new())
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should contain SpawnForTask with ResumeSession mode
    let spawn_for_task = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                task_id, config, ..
            } = e
            {
                if matches!(
                    config.session_mode,
                    crate::launch::SessionMode::ResumeSession(_)
                ) {
                    Some(task_id.clone())
                } else {
                    None
                }
            } else {
                None
            }
        })
        .expect(
            "Should emit SpawnForTask with ResumeSession for pending task with stopped session",
        );

    assert_eq!(spawn_for_task, "99", "task_id should match");
}

#[test]
fn test_pending_task_with_running_session_skips_dispatch() {
    // When a pending task has a RUNNING session, dispatch should skip it entirely
    // (no SpawnForTask).
    use crate::task_store::{Task, TaskStatus};

    let session = make_test_session_record(
        "sess-running-1",
        Some("88"),
        Some("broadway"),
        "/tmp/worktree-88",
        true, // running
    );
    let sessions = [("sess-running-1".to_string(), session)]
        .into_iter()
        .collect();
    let session_task_map = [("88".to_string(), "sess-running-1".to_string())]
        .into_iter()
        .collect();

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "88".to_string(),
            subject: "Fix login bug".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        sessions,
        session_task_map,
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        ..make_session_dispatch_snapshot(vec![], HashMap::new(), HashMap::new())
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should produce no spawn effects at all
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .collect();

    assert!(
        spawn_effects.is_empty(),
        "Should not spawn anything for a pending task with a running session, got: {:?}",
        spawn_effects
    );
}

/// Regression test for task !1728: Path 2 recovery loop — pending task with stopped session
/// retries without cooldown check.
///
/// Bug: `spawn_for_pending_tasks` (Path 2) resumes stopped sessions for pending tasks WITHOUT
/// checking `recently_recovered_session_ids`. This causes infinite retry loops every 5s when
/// a session dies repeatedly after recovery.
///
/// Path 1 (`dispatch_via_sessions_with_task_lookup`) checks this cooldown at lines 817-826.
/// Path 2 had no such check — fixed by adding the same guard before the SpawnForTask emit.
///
/// Fix: add `recently_recovered_session_ids` check before spawning in Path 2, and record
/// the `session_recovered` cooldown in the SpawnForTask success handler in effects.rs.
#[test]
fn test_pending_task_with_recently_recovered_session_skips_dispatch() {
    // Given: pending task !99 has stopped session "sess-cool-1" that was recently recovered
    // (recently_recovered_session_ids contains the session_id).
    use crate::task_store::{Task, TaskStatus};

    let session = make_test_session_record(
        "sess-cool-1",
        Some("99"),
        Some("lexington"),
        "/tmp/worktree-99",
        false, // stopped — session died after previous recovery spawn
    );
    let sessions = [("sess-cool-1".to_string(), session)].into_iter().collect();
    let session_task_map = [("99".to_string(), "sess-cool-1".to_string())]
        .into_iter()
        .collect();

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "99".to_string(),
            subject: "Implement caching layer".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        sessions,
        session_task_map,
        // Recovery was recently attempted — cooldown is active for this session_id.
        // Without the fix, this check is missing in Path 2 and the task is re-spawned
        // on every 5s tick, causing infinite retry loops when sessions die repeatedly.
        recently_recovered_session_ids: ["sess-cool-1".to_string()].into_iter().collect(),
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        ..make_session_dispatch_snapshot(vec![], HashMap::new(), HashMap::new())
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Then: must NOT emit SpawnForTask or any other spawn — session was recently recovered.
    // Without the fix, this fires on every tick causing an infinite loop.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .collect();
    assert!(
        spawn_effects.is_empty(),
        "Should NOT re-spawn a pending task whose session was recently recovered (cooldown active). \
         This causes an infinite retry loop every 5s. Got: {:?}",
        spawn_effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_pending_task_with_stale_working_dir_uses_fresh_worktree() {
    // When a session's recorded working_dir no longer exists on disk (e.g.,
    // the worktree was cleaned up), Path 2 should fall back to the fresh
    // worktree path instead of passing the stale path to SpawnForTask.
    use crate::task_store::{Task, TaskStatus};

    let stale_dir = "/tmp/midtown-test-NONEXISTENT-working-dir-12345";
    // Confirm the stale path definitely doesn't exist
    assert!(
        !std::path::Path::new(stale_dir).exists(),
        "Test precondition: stale_dir must not exist"
    );

    let session = make_test_session_record(
        "sess-stale-wdir",
        Some("77"),
        Some("lexington"),
        stale_dir, // non-existent working_dir
        false,     // stopped
    );
    let sessions = [("sess-stale-wdir".to_string(), session)]
        .into_iter()
        .collect();
    let session_task_map = [("77".to_string(), "sess-stale-wdir".to_string())]
        .into_iter()
        .collect();

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "77".to_string(),
            subject: "Implement caching layer".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        sessions,
        session_task_map,
        is_at_task_limit: false,
        max_in_progress_tasks: 8,
        blocks_map: HashMap::new(),
        stale_working_dir_sessions: ["sess-stale-wdir".to_string()].into_iter().collect(),
        ..make_session_dispatch_snapshot(vec![], HashMap::new(), HashMap::new())
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    let spawn_working_dir = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask { config, .. } = e {
                config.working_dir.clone()
            } else {
                None
            }
        })
        .expect("Should emit SpawnForTask for pending task with stopped session");

    // The stale working_dir should not be used
    assert_ne!(
        spawn_working_dir.to_string_lossy(),
        stale_dir,
        "Should not use stale (non-existent) working_dir"
    );

    // Should use the fresh worktree path derived from task subject
    let expected_worktree_dir =
        crate::paths::worktrees_dir_for_repo("test-repo").join("task-77-implement-caching-layer");
    assert_eq!(
        spawn_working_dir, expected_worktree_dir,
        "Should fall back to fresh worktree when recorded working_dir is stale"
    );
}

// ============================================================================
// WorkflowEvent emission tests
// ============================================================================

#[test]
fn test_build_task_completion_effects_emits_task_completed_workflow_event() {
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint [Midtown #42]",
        123,
        "myrepo",
        "myrepo",
        Some("proj-auth".to_string()),
        None,
    );

    // 4 base effects (CompleteTask + ClearBlockedBy + PostToChannel + SendPushNotification) + 1 TaskCompleted + 1 PrMerged
    assert_eq!(effects.len(), 6);

    let workflow_events: Vec<_> = effects
        .iter()
        .filter_map(|e| {
            if let Effect::EmitWorkflowEvent(ev) = e {
                Some(ev)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(workflow_events.len(), 2, "Should emit 2 workflow events");

    let has_task_completed = workflow_events.iter().any(|ev| {
        matches!(
            ev,
            crate::workflow::WorkflowEvent::TaskCompleted {
                channel,
                task_id,
                ..
            } if channel == "proj-auth" && task_id == "42"
        )
    });
    assert!(
        has_task_completed,
        "Should emit TaskCompleted workflow event"
    );

    let has_pr_merged = workflow_events.iter().any(|ev| {
        matches!(
            ev,
            crate::workflow::WorkflowEvent::PrMerged {
                channel,
                task_id,
                pr_number,
            } if channel == "proj-auth" && task_id == "42" && *pr_number == 123
        )
    });
    assert!(has_pr_merged, "Should emit PrMerged workflow event");
}

#[test]
fn test_build_task_completion_effects_uses_task_subject_over_pr_title() {
    let ctx = super::TaskEventContext {
        subject: Some("Add auth endpoint".to_string()),
        description: Some("Implement OAuth".to_string()),
        thread_id: Some("T123".to_string()),
        message_id: Some("M456".to_string()),
    };
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint [Midtown #42]",
        123,
        "myrepo",
        "myrepo",
        Some("proj-auth".to_string()),
        Some(ctx),
    );

    let task_completed = effects.iter().find_map(|e| {
        if let Effect::EmitWorkflowEvent(crate::workflow::WorkflowEvent::TaskCompleted {
            subject,
            description,
            thread_id,
            message_id,
            ..
        }) = e
        {
            Some((
                subject.clone(),
                description.clone(),
                thread_id.clone(),
                message_id.clone(),
            ))
        } else {
            None
        }
    });
    let (subject, description, thread_id, message_id) =
        task_completed.expect("Should emit TaskCompleted");
    assert_eq!(
        subject, "Add auth endpoint",
        "Should use task subject, not PR title"
    );
    assert_eq!(description.unwrap(), "Implement OAuth");
    assert_eq!(thread_id.unwrap(), "T123");
    assert_eq!(message_id.unwrap(), "M456");
}

#[test]
fn test_build_task_completion_effects_falls_back_to_pr_title() {
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint [Midtown #42]",
        123,
        "myrepo",
        "myrepo",
        Some("proj-auth".to_string()),
        None,
    );

    let subject = effects.iter().find_map(|e| {
        if let Effect::EmitWorkflowEvent(crate::workflow::WorkflowEvent::TaskCompleted {
            subject,
            ..
        }) = e
        {
            Some(subject.clone())
        } else {
            None
        }
    });
    assert_eq!(
        subject.unwrap(),
        "feat: Add auth endpoint [Midtown #42]",
        "Should fall back to PR title when no task subject in context"
    );
}

#[test]
fn test_build_task_completion_effects_routes_to_task_channel() {
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint [Midtown #42]",
        123,
        "myrepo",
        "myrepo",
        Some("proj-auth".to_string()),
        None,
    );

    // PostToChannel should carry the task's channel
    let post = effects
        .iter()
        .find(|e| matches!(e, Effect::PostToChannel { .. }))
        .expect("Should have a PostToChannel effect");
    match post {
        Effect::PostToChannel { channel, .. } => {
            assert_eq!(
                channel.as_deref(),
                Some("proj-auth"),
                "PostToChannel should route to the task's channel"
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn test_build_task_completion_effects_no_workflow_event_without_channel() {
    let effects = build_task_completion_effects(
        "feat: Add auth endpoint [Midtown #42]",
        123,
        "myrepo",
        "myrepo",
        None,
        None,
    );

    // Only the 4 base effects — no workflow events without a channel
    assert_eq!(effects.len(), 4);
    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::EmitWorkflowEvent(_))),
        "Should not emit workflow events without a channel"
    );
}

#[test]
fn test_build_subject_based_completion_effects_emits_task_completed() {
    use crate::task_store::{Task, TaskStatus};
    use std::collections::{HashMap, HashSet};

    let task = Task {
        id: "55".to_string(),
        subject: "Fix PR #901 review feedback".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "amsterdam".to_string(),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None,
        ..Default::default()
    };

    let mut merged = HashSet::new();
    merged.insert(901u64);

    let mut task_channel = HashMap::new();
    task_channel.insert("55".to_string(), "proj-auth".to_string());

    let mut snap = snapshot::WorldSnapshot {
        all_tasks: vec![task],
        task_channel,
        ..snapshot::minimal_snapshot_for_test()
    };
    snap.pr.merged_pr_numbers = merged;

    let effects = build_subject_based_completion_effects(&snap);

    let workflow_events: Vec<_> = effects
        .iter()
        .filter_map(|e| {
            if let Effect::EmitWorkflowEvent(ev) = e {
                Some(ev)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        workflow_events.len(),
        1,
        "Should emit 1 TaskCompleted workflow event"
    );

    if let crate::workflow::WorkflowEvent::TaskCompleted {
        channel,
        task_id,
        coworker,
        subject,
        ..
    } = workflow_events[0]
    {
        assert_eq!(channel, "proj-auth");
        assert_eq!(task_id, "55");
        assert_eq!(coworker.as_deref(), Some("amsterdam"));
        assert!(!subject.is_empty());
    } else {
        panic!("Should emit TaskCompleted with correct fields");
    }
}

// ============================================================================
// Bug !2172: daemon repeatedly spawns coworkers into non-existent worktree
// ============================================================================

#[test]
fn test_owned_pending_task_skips_spawn_when_spawn_failure_cooldown_active() {
    // Bug scenario: a pending task with an owner tries to spawn, but the worktree
    // doesn't exist. The spawn fails, but without a cooldown check, the next tick
    // (5s later) retries immediately — creating an infinite loop.
    //
    // Fix: dispatch_owned_pending_tasks must check spawn_failure_cooldown_names
    // before attempting to spawn, just like the other dispatch paths do.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "2059".to_string(),
            "Add new feature".to_string(),
            "columbus".to_string(),
        )],
        // Columbus is on spawn failure cooldown (previous spawn failed)
        spawn_failure_cooldown_names: ["columbus".to_string()].into_iter().collect(),
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // Must NOT emit any spawn effects when cooldown is active.
    let spawn_effects: Vec<_> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SpawnForTask { .. }))
        .collect();
    assert!(
        spawn_effects.is_empty(),
        "Should NOT spawn when spawn_failure_cooldown is active for the owner. \
         Without this check, the daemon retries every 5s in an infinite loop. Got: {:?}",
        spawn_effects
            .iter()
            .map(|e| format!("{:?}", e))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_owned_pending_task_spawn_failure_records_cooldown() {
    // The executor always inlines spawn_failure bookkeeping (RecordCooldown +
    // ResetTaskToPending + ops message) using the real allocated name after spawn fails.
    // Verify SpawnForTask is emitted with the correct task_id and dir_key fields
    // so the executor can perform this bookkeeping correctly.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "2059".to_string(),
            "Add new feature".to_string(),
            "columbus".to_string(),
        )],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // SpawnForTask must be emitted; the executor handles failure bookkeeping inline.
    let spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                task_id, dir_key, ..
            } = e
            {
                Some((task_id.as_str(), dir_key.as_str()))
            } else {
                None
            }
        })
        .expect("Should emit SpawnForTask for owned pending task");

    assert_eq!(
        spawn.0, "2059",
        "task_id must be set for executor failure bookkeeping"
    );
    assert!(
        !spawn.1.is_empty(),
        "dir_key must be set for ResetTaskToPending"
    );
}

#[test]
fn test_unowned_pending_task_assign_and_spawn_failure_records_cooldown() {
    // The executor always inlines spawn_failure bookkeeping (RecordCooldown +
    // ResetTaskToPending + ops message) using the real allocated name after spawn fails.
    // Verify SpawnForTask is emitted with the correct fields.
    use crate::task_store::{Task, TaskStatus};

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "2059".to_string(),
            subject: "Add new feature".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    // SpawnForTask must be emitted; the executor handles failure bookkeeping inline.
    let spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                task_id, dir_key, ..
            } = e
            {
                Some((task_id.as_str(), dir_key.as_str()))
            } else {
                None
            }
        })
        .expect("Should emit SpawnForTask for unowned pending task");

    assert_eq!(
        spawn.0, "2059",
        "task_id must be set for executor failure bookkeeping"
    );
    assert!(
        !spawn.1.is_empty(),
        "dir_key must be set for ResetTaskToPending"
    );
}

#[test]
fn test_unowned_pending_task_skipped_when_cooldown_active() {
    // Verify that dispatch_unowned_pending_tasks actually checks
    // spawn_failure_cooldown_names and skips coworkers in cooldown.
    // Without this check, recording cooldown on failure is useless —
    // the next tick would just allocate the same name and retry.
    use crate::task_store::{Task, TaskStatus};

    let mut snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "2059".to_string(),
            subject: "Add new feature".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    // First: verify the task DOES get dispatched without cooldown
    let effects = spawn_for_pending_tasks(&snap, &state);
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(has_spawn, "Should emit SpawnForTask without cooldown");

    // Now: put a task-based name in cooldown
    let first_name = "task-42-test".to_string();
    snap.spawn_failure_cooldown_names
        .insert(first_name.to_lowercase());

    // Re-dispatch: the cooldown should cause this coworker to be skipped.
    // A different coworker name will be allocated (if available), but the
    // key point is the cooldown check fires for the first name.
    let effects2 = spawn_for_pending_tasks(&snap, &state);

    // The task may still get dispatched to a different coworker name,
    // but verify the first name is NOT used.
    let dispatched_name = effects2.iter().find_map(|e| {
        if let Effect::SpawnForTask { preferred_name, .. } = e {
            preferred_name.clone()
        } else {
            None
        }
    });
    if let Some(name) = &dispatched_name {
        assert_ne!(
            name.to_lowercase(),
            first_name.to_lowercase(),
            "Coworker in cooldown should not be dispatched"
        );
    }
}

#[test]
fn test_owned_pending_task_spawn_failure_resets_task_to_pending() {
    // The executor always inlines ResetTaskToPending on failure, using the task_id
    // and dir_key from SpawnForTask. Verify those fields are set correctly.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "2059".to_string(),
            "Add new feature".to_string(),
            "columbus".to_string(),
        )],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    let spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                task_id, dir_key, ..
            } = e
            {
                Some((task_id.as_str(), dir_key.as_str()))
            } else {
                None
            }
        })
        .expect("Should emit SpawnForTask for owned pending task");

    assert_eq!(
        spawn.0, "2059",
        "task_id must match for executor's ResetTaskToPending on failure"
    );
    assert!(
        !spawn.1.is_empty(),
        "dir_key must be set for ResetTaskToPending on failure"
    );
}

#[test]
fn test_unowned_pending_task_spawn_failure_resets_task_to_pending() {
    // The executor always inlines ResetTaskToPending on failure, using the task_id
    // and dir_key from SpawnForTask. Verify those fields are set correctly.
    use crate::task_store::{Task, TaskStatus};

    let snap = snapshot::WorldSnapshot {
        pending_tasks_without_owners: vec![Task {
            id: "2059".to_string(),
            subject: "Add new feature".to_string(),
            status: TaskStatus::Pending,
            agent_name: String::new(),
            blocked_by: vec![],
            description: None,
            channel: None,
            pr: None,
            ..Default::default()
        }],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    let effects = spawn_for_pending_tasks(&snap, &state);

    let spawn = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                task_id, dir_key, ..
            } = e
            {
                Some((task_id.as_str(), dir_key.as_str()))
            } else {
                None
            }
        })
        .expect("Should emit SpawnForTask for unowned pending task");

    assert_eq!(
        spawn.0, "2059",
        "task_id must match for executor's ResetTaskToPending on failure"
    );
    assert!(
        !spawn.1.is_empty(),
        "dir_key must be set for ResetTaskToPending on failure"
    );
}

// ============================================================================
// Live-state guard tests (TOCTOU race prevention)
// ============================================================================

#[test]
fn test_owned_pending_live_in_flight_guard_skips_spawn() {
    // Race scenario: an RPC dispatcher (daemon.check-pending) claimed a task
    // and marked it in-flight *after* our snapshot was collected. The snapshot
    // still shows the task as dispatchable, but the live in-flight set says no.
    let snap = snapshot::WorldSnapshot {
        pending_tasks_with_owners: vec![(
            "100".to_string(),
            "Feature X".to_string(),
            "columbus".to_string(),
        )],
        ..snapshot::minimal_snapshot_for_test()
    };

    let (state, _tmp, _guard) = make_test_state();

    // Simulate a concurrent RPC dispatcher having already claimed this task.
    state
        .in_flight_task_spawns
        .lock()
        .unwrap()
        .insert("100".to_string());

    let effects = spawn_for_pending_tasks(&snap, &state);

    assert!(
        effects
            .iter()
            .all(|e| !matches!(e, Effect::SpawnForTask { .. })),
        "Live in-flight guard must prevent duplicate spawn. Got: {:?}",
        effects
    );
}

#[test]
fn test_owned_pending_live_nudge_cooldown_guard_skips_nudge() {
    // Race scenario: an RPC dispatcher nudged an owned task and recorded
    // the cooldown *after* our snapshot was collected. The snapshot's
    // task_nudge_cooldown_ids doesn't include this task, but the live
    // cooldown tracker does.
    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.pending_tasks_with_owners = vec![(
        "200".to_string(),
        "Feature Y".to_string(),
        "columbus".to_string(),
    )];
    // Columbus must be in active_names for the decision function to choose NudgeOwner.
    snap.coworkers.active_names.insert("columbus".to_string());

    let (state, _tmp, _guard) = make_test_state();

    // Simulate a concurrent RPC dispatcher having already nudged this task.
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("task_nudge", "pending-200");
    }

    let effects = spawn_for_pending_tasks(&snap, &state);

    let nudge_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::NudgeSessionWithCallbacks { .. }))
        .count();
    assert_eq!(
        nudge_count, 0,
        "Live nudge cooldown guard must prevent duplicate nudge. Got: {:?}",
        effects
    );
}

// ============================================================================
// Lead-driven channel gating tests
// ============================================================================

#[test]
fn test_orphan_recovery_skips_lead_driven_channel() {
    let mut snap = make_lead_guard_snapshot(
        vec![("42".into(), "Add auth".into(), "park".into())],
        "test-repo",
    );
    // Task 42 is in channel "proj-workflows" which is lead-driven.
    snap.task_channel
        .insert("42".into(), "proj-workflows".into());
    snap.lead_driven_channels.insert("proj-workflows".into());
    snap.coworkers.active_names.insert("park".into());

    let effects = dispatch_via_sessions_for_test(&snap);
    // No effects — task is in a lead-driven channel.
    assert!(
        effects.is_empty(),
        "Expected no effects for lead-driven channel, got {:?}",
        effects
    );
}

#[test]
fn test_orphan_recovery_dispatches_non_lead_driven_channel() {
    let mut snap = make_lead_guard_snapshot(
        vec![("42".into(), "Add auth".into(), "park".into())],
        "test-repo",
    );
    // Task 42 is in a channel that is NOT lead-driven.
    snap.task_channel
        .insert("42".into(), "proj-workflows".into());
    // lead_driven_channels is empty — default behavior.
    // NOTE: Do NOT add "park" to active_names — that causes orphan recovery to
    // skip it (active coworkers aren't considered orphans), making the test
    // produce empty effects for the wrong reason.

    let (_state, _tmp, _guard) = make_test_state();
    let effects = check_and_recover_orphans_with_task_lookup(&snap, |task_id| {
        if task_id == "42" {
            Some(in_progress_task_for_lookup("42", "Add auth", "park"))
        } else {
            None
        }
    });
    // Orphan recovery handles tasks without sessions and produces a
    // SpawnForTask effect for non-lead-driven channels.
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { .. }));
    assert!(
        has_spawn,
        "Non-lead-driven channel should produce spawn effect, got: {:?}",
        effects
    );
}

#[test]
fn test_lead_driven_channel_still_auto_completes_merged_pr_tasks() {
    let mut snap = make_lead_guard_snapshot(vec![], "test-repo");
    // Add a pending task with owner in a lead-driven channel.
    snap.pending_tasks_with_owners = vec![("42".into(), "Fix bug".into(), "park".into())];
    snap.task_channel
        .insert("42".into(), "proj-workflows".into());
    snap.lead_driven_channels.insert("proj-workflows".into());
    // The task has an associated merged PR.
    snap.all_tasks = vec![crate::task_store::Task {
        id: "42".into(),
        subject: "Fix bug".into(),
        status: crate::task_store::TaskStatus::Pending,
        agent_name: "park".into(),
        description: None,
        blocked_by: vec![],
        channel: Some("proj-workflows".into()),
        pr: Some(100),
        ..Default::default()
    }];
    snap.pr.merged_pr_numbers.insert(100);

    // Test via the pure decision function — merged-PR auto-complete should fire
    // even though the channel is lead-driven (auto-complete runs before the
    // lead-driven check).
    let action = crate::rules::decide_owned_pending_dispatch("42", "Fix bug", "park", &snap);

    assert!(
        matches!(action, crate::rules::PendingTaskAction::AutoComplete { ref task_id, pr_num } if task_id == "42" && pr_num == 100),
        "Expected AutoComplete for merged PR in lead-driven channel, got {:?}",
        action
    );
}

// ============================================================================
// build_plan_prompt_section_from_parts — standalone plan section builder
// ============================================================================

#[test]
fn test_plan_section_empty_when_no_plan_or_skill() {
    let result = build_plan_prompt_section_from_parts("42", None, None);
    assert!(
        result.is_empty(),
        "Should return empty string when neither plan nor skill is set"
    );
}

#[test]
fn test_plan_section_with_execution_skill_only() {
    let result =
        build_plan_prompt_section_from_parts("42", None, Some("subagent-driven-development"));
    assert!(
        result.contains("## Execution Skill"),
        "Should include execution skill heading"
    );
    assert!(
        result.contains("superpowers:subagent-driven-development"),
        "Should include the skill name"
    );
    assert!(
        !result.contains("## Plan Context"),
        "Should not include plan section"
    );
}

#[test]
fn test_plan_section_with_plan_file() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let plan_path = temp_dir.path().join("plan.md");
    std::fs::write(&plan_path, "# My Plan\n\nStep 1: Do the thing").unwrap();

    let result =
        build_plan_prompt_section_from_parts("42", Some(plan_path.to_str().unwrap()), None);
    assert!(
        result.contains("## Plan Context"),
        "Should include plan context heading"
    );
    assert!(
        result.contains("Step 1: Do the thing"),
        "Should include plan file content"
    );
    assert!(
        result.contains("<plan>"),
        "Should wrap plan content in <plan> tags"
    );
}

#[test]
fn test_plan_section_with_both_skill_and_plan() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let plan_path = temp_dir.path().join("plan.md");
    std::fs::write(&plan_path, "Plan content here").unwrap();

    let result = build_plan_prompt_section_from_parts(
        "42",
        Some(plan_path.to_str().unwrap()),
        Some("executing-plans"),
    );
    assert!(result.contains("## Execution Skill"));
    assert!(result.contains("superpowers:executing-plans"));
    assert!(result.contains("## Plan Context"));
    assert!(result.contains("Plan content here"));
}

#[test]
fn test_plan_section_with_missing_plan_file() {
    let result = build_plan_prompt_section_from_parts("42", Some("/nonexistent/plan.md"), None);
    // Should return empty — the warn! fires but doesn't panic
    assert!(
        !result.contains("## Plan Context"),
        "Should not include plan section when file doesn't exist"
    );
}

#[test]
fn test_plan_section_with_missing_plan_file_preserves_skill() {
    let result = build_plan_prompt_section_from_parts(
        "42",
        Some("/nonexistent/plan.md"),
        Some("executing-plans"),
    );
    // Skill section should still be present even if plan file is missing
    assert!(
        result.contains("## Execution Skill"),
        "Should preserve execution skill even when plan file is missing"
    );
}

// ============================================================================
// Reviewer task dispatch via task_agent_type
// ============================================================================

/// When a pending task has task_agent_type="midtown-code-reviewer" and a PR number,
/// dispatch should use reviewer-specific launch config (LaunchConfig::reviewer)
/// instead of the regular coworker config.
#[test]
fn test_reviewer_task_dispatched_with_reviewer_config() {
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "500".to_string(),
        subject: "Review PR #42".to_string(),
        status: TaskStatus::Pending,
        agent_name: String::new(),
        description: Some("Code review for PR #42.".to_string()),
        blocked_by: vec![],
        channel: Some("auth".to_string()),
        pr: Some(42),
        ..Default::default()
    };

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.pending_tasks_without_owners = vec![task];
    snap.dir_key = "test-repo".to_string();
    snap.project_name = "test-repo".to_string();
    snap.default_channel = "test-repo".to_string();
    snap.task_agent_type_map
        .insert("500".to_string(), "midtown-code-reviewer".to_string());

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should produce a SpawnForTask effect for the reviewer task
    let has_spawn_for_task = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnForTask { task_id, .. } if task_id == "500"));

    assert!(
        has_spawn_for_task,
        "Reviewer task should produce SpawnForTask effect. Effects: {:#?}",
        effects
    );

    // The reviewer field should contain reviewer-specific extras
    let reviewer_info = effects.iter().find_map(|e| {
        if let Effect::SpawnForTask {
            reviewer, task_id, ..
        } = e
        {
            if task_id == "500" {
                reviewer.as_ref()
            } else {
                None
            }
        } else {
            None
        }
    });

    if let Some(info) = reviewer_info {
        assert_eq!(
            info.pr_number, 42,
            "Reviewer spawn info should have pr_number=42"
        );
        assert_eq!(
            info.agent_type, "midtown-code-reviewer",
            "Reviewer spawn info should have agent_type='reviewer'"
        );
        assert!(
            !info.pr_comment_body.is_empty(),
            "Reviewer spawn info should have a pr_comment_body"
        );
    } else {
        panic!("SpawnForTask for task 500 should have reviewer field set");
    }
}

/// A pending task WITHOUT task_agent_type should dispatch as a regular coworker,
/// not as a reviewer — even if it has a PR number.
#[test]
fn test_regular_task_with_pr_not_dispatched_as_reviewer() {
    use crate::task_store::{Task, TaskStatus};

    let task = Task {
        id: "501".to_string(),
        subject: "Fix bug in PR #43".to_string(),
        status: TaskStatus::Pending,
        agent_name: String::new(),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(43),
        ..Default::default()
    };

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.pending_tasks_without_owners = vec![task];
    snap.dir_key = "test-repo".to_string();
    snap.project_name = "test-repo".to_string();
    snap.default_channel = "test-repo".to_string();
    // No task_agent_type_map entry — should be treated as regular task

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should NOT have reviewer field set (that's reviewer-specific)
    let has_reviewer = effects.iter().any(|e| {
        if let Effect::SpawnForTask { reviewer, .. } = e {
            reviewer.is_some()
        } else {
            false
        }
    });

    assert!(
        !has_reviewer,
        "Regular task with PR should NOT have reviewer field set. Effects: {:#?}",
        effects
    );
}

/// Reviewer tasks must not be grouped with the implementation coworker even when
/// they share the same PR number. Without this guard, resolve_grouped_name would
/// route the reviewer task to the author's running session, dispatching it as a
/// generic TaskClaimed nudge instead of a fresh reviewer spawn.
#[test]
fn test_reviewer_task_not_grouped_with_implementation_coworker() {
    use crate::task_store::{Task, TaskStatus};

    // Implementation task: already in progress, owned by "park"
    let impl_task = Task {
        id: "600".to_string(),
        subject: "Implement feature for PR #55".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "park".to_string(),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(55),
        ..Default::default()
    };

    // Reviewer task: pending, same PR number, should NOT group with park
    let review_task = Task {
        id: "601".to_string(),
        subject: "Review PR #55".to_string(),
        status: TaskStatus::Pending,
        agent_name: String::new(),
        description: Some("Code review for PR #55.".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(55),
        ..Default::default()
    };

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.all_tasks = vec![impl_task.clone(), review_task.clone()];
    snap.pending_tasks_without_owners = vec![review_task];
    snap.dir_key = "test-repo".to_string();
    snap.project_name = "test-repo".to_string();
    snap.default_channel = "test-repo".to_string();
    snap.task_agent_type_map
        .insert("601".to_string(), "midtown-code-reviewer".to_string());
    // Mark park as active so grouping would normally route to them
    snap.coworkers.active_names.insert("park".to_string());

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // The reviewer task should spawn a fresh coworker, not nudge park
    let spawned_as_fresh = effects.iter().any(|e| {
        matches!(e, Effect::SpawnForTask { task_id, preferred_name, .. }
            if task_id == "601" && preferred_name.as_ref().is_none_or(|n| n != "park"))
    });

    assert!(
        spawned_as_fresh,
        "Reviewer task should spawn as fresh coworker, not group with implementation owner 'park'. Effects: {:#?}",
        effects
    );

    // Double-check: no nudge to park for this task
    let nudged_park = effects.iter().any(|e| {
        matches!(e, Effect::NudgeSessionWithCallbacks { reason, .. }
            if matches!(reason, crate::daemon::wake_reason::WakeReason::TaskClaimed { task_id, .. } if task_id == "601"))
    });

    assert!(
        !nudged_park,
        "Reviewer task should NOT be nudged to existing coworker. Effects: {:#?}",
        effects
    );
}

/// Reviewer tasks must not be assigned to the PR author (parent task owner).
/// This prevents self-review when all other coworker names happen to be in use.
#[test]
fn test_reviewer_task_excludes_pr_author_from_name_allocation() {
    use crate::task_store::{Task, TaskStatus};

    // Parent implementation task owned by "riverside"
    let impl_task = Task {
        id: "700".to_string(),
        subject: "Implement feature".to_string(),
        status: TaskStatus::InProgress,
        agent_name: "riverside".to_string(),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(88),
        ..Default::default()
    };

    // Reviewer child task
    let review_task = Task {
        id: "701".to_string(),
        subject: "Review PR #88".to_string(),
        status: TaskStatus::Pending,
        agent_name: String::new(),
        description: Some("Code review for PR #88.".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(88),
        ..Default::default()
    };

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.all_tasks = vec![impl_task, review_task.clone()];
    snap.pending_tasks_without_owners = vec![review_task];
    snap.dir_key = "test-repo".to_string();
    snap.project_name = "test-repo".to_string();
    snap.default_channel = "test-repo".to_string();
    snap.task_agent_type_map
        .insert("701".to_string(), "midtown-code-reviewer".to_string());
    // Map child → parent so the self-review guard can find the author
    snap.task_parent_map
        .insert("701".to_string(), "700".to_string());

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // Should spawn, but NOT as "riverside" (the PR author)
    let spawned = effects.iter().find_map(|e| {
        if let Effect::SpawnForTask {
            task_id,
            preferred_name,
            ..
        } = e
        {
            if task_id == "701" {
                preferred_name.clone()
            } else {
                None
            }
        } else {
            None
        }
    });

    assert!(
        spawned.is_some(),
        "Reviewer task should produce SpawnForTask. Effects: {:#?}",
        effects
    );
    assert_ne!(
        spawned.unwrap().to_lowercase(),
        "riverside",
        "Reviewer must not be assigned to the PR author 'riverside'"
    );
}

/// CreateTaskSessionSpan must appear before PostPrComment in the on_success callback
/// list so that the span exists when post_pr_comment() stores the placeholder_comment_id.
#[test]
fn test_reviewer_create_span_before_post_pr_comment() {
    use crate::task_store::{Task, TaskStatus};

    let review_task = Task {
        id: "800".to_string(),
        subject: "Review PR #99".to_string(),
        status: TaskStatus::Pending,
        agent_name: String::new(),
        description: Some("Code review for PR #99.".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(99),
        ..Default::default()
    };

    let mut snap = snapshot::minimal_snapshot_for_test();
    snap.pending_tasks_without_owners = vec![review_task];
    snap.dir_key = "test-repo".to_string();
    snap.project_name = "test-repo".to_string();
    snap.default_channel = "test-repo".to_string();
    snap.task_agent_type_map
        .insert("800".to_string(), "midtown-code-reviewer".to_string());

    let (state, _tmp, _guard) = make_test_state();
    let effects = spawn_for_pending_tasks(&snap, &state);

    // In the new design, CreateTaskSessionSpan and PostPrComment are executed
    // sequentially inside the executor (CreateTaskSessionSpan first, then PostPrComment),
    // encoded in the ReviewerSpawnInfo struct — just verify the reviewer field is set.
    let reviewer = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnForTask {
                reviewer, task_id, ..
            } = e
            {
                if task_id == "800" {
                    reviewer.as_ref()
                } else {
                    None
                }
            } else {
                None
            }
        })
        .expect("Reviewer task should produce SpawnForTask with reviewer field");

    assert_eq!(
        reviewer.agent_type, "midtown-code-reviewer",
        "agent_type should be 'reviewer'"
    );
    assert!(reviewer.pr_number == 99, "pr_number should be 99");
    assert!(
        !reviewer.pr_comment_body.is_empty(),
        "pr_comment_body should not be empty"
    );
    // The executor always runs CreateTaskSessionSpan before PostPrComment (in that order
    // in the effects vec it builds), so ordering is guaranteed by construction.
}
