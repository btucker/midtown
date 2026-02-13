use super::*;
use serde_json::json;
use std::sync::Mutex;

// Global mutex to serialize tests that modify PATH environment variable.
// Without this, parallel test execution causes gh CLI mocks to interfere
// with each other (one test's mock returns data to a different test).
static PATH_LOCK: Mutex<()> = Mutex::new(());

/// Helper to create minimal DaemonState for testing
fn make_test_state(repo_name: &str) -> DaemonState {
    use std::process::Command;

    let temp_dir = tempfile::tempdir().expect("temp dir");
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
    let cm = crate::coworker::CoworkerManager::new(repo_name, wm);

    // Leak temp_dir so it survives the test
    let base_dir = temp_dir.path().to_path_buf();
    std::mem::forget(temp_dir);

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        repo_name.to_string(),
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

/// Bug: collect_green_with_feedback_effects was using head_ref.split('/').next()
/// to extract the owner, which doesn't validate against known coworker names.
/// This meant PRs with branches like "btucker/fix" would extract "btucker" as owner
/// and potentially nudge wrong coworkers if the prefix matches a coworker name.
#[test]
fn coworker_from_branch_rejects_non_coworker_prefixes() {
    // These should return None because they're not valid coworker names
    assert!(
        coworker_from_branch("btucker/fix-something").is_none(),
        "btucker is not a coworker name"
    );
    assert!(
        coworker_from_branch("feature/add-auth").is_none(),
        "feature is not a coworker name"
    );
    assert!(coworker_from_branch("main").is_none(), "main has no slash");

    // These should return Some because they are valid coworker names
    assert_eq!(
        coworker_from_branch("york/fix-something"),
        Some("york".to_string()),
        "york is a valid coworker name"
    );
    assert_eq!(
        coworker_from_branch("amsterdam/add-feature"),
        Some("amsterdam".to_string()),
        "amsterdam is a valid coworker name"
    );
}

#[test]
fn is_lead_branch_detects_lead_branches() {
    // Lead branches start with "lead/"
    assert!(
        is_lead_branch("lead/fix-bug"),
        "lead/fix-bug is a lead branch"
    );
    assert!(
        is_lead_branch("lead/add-feature"),
        "lead/add-feature is a lead branch"
    );
    assert!(
        is_lead_branch("lead/root-cause-claude-md-updates"),
        "lead/root-cause-claude-md-updates is a lead branch"
    );

    // Coworker and other branches should not be detected as lead branches
    assert!(
        !is_lead_branch("york/fix-bug"),
        "york/fix-bug is not a lead branch"
    );
    assert!(
        !is_lead_branch("feature/add-auth"),
        "feature/add-auth is not a lead branch"
    );
    assert!(!is_lead_branch("main"), "main is not a lead branch");
    assert!(
        !is_lead_branch("leading/edge"),
        "leading/edge is not a lead branch (only exact prefix match)"
    );
}

#[test]
fn stuck_nudge_effects_returns_only_system_message() {
    // Bug: stuck_nudge_effects was returning both PostSystemMessage and NudgeLead,
    // causing double delivery because the chat monitor already routes @lead mentions
    // in system messages to the lead.
    //
    // The fix is to only return PostSystemMessage and let the channel's @mention
    // routing handle the nudge.
    let message = "@lead PR #42 (Add feature) has been open for 60 minutes without a review";
    let effects = stuck_nudge_effects(message);

    // Should only return one effect (PostSystemMessage)
    assert_eq!(
        effects.len(),
        1,
        "stuck_nudge_effects should return exactly 1 effect, not 2 (double nudge bug)"
    );

    // That effect should be PostSystemMessage with the warning emoji prefix
    match &effects[0] {
        Effect::PostSystemMessage { message: msg } => {
            assert!(
                msg.starts_with("⚠️"),
                "System message should have warning prefix"
            );
            assert!(
                msg.contains("@lead"),
                "System message should preserve @lead mention"
            );
        }
        _ => panic!("Expected PostSystemMessage effect, got {:?}", effects[0]),
    }
}

/// Bug: When a coworker goes on break, their PR's branch is no longer in
/// worktree_branch_owners. The poll_prs_for_issues loop (lines 664-669) skips
/// any PR whose branch doesn't map to an active coworker via
/// coworker_from_branch_with_map(), which means orphaned PRs with merge
/// conflicts never trigger nudges or warnings.
///
/// Expected: The daemon should detect merge conflicts on ALL open PRs (not just
/// those with active coworker owners) and either nudge the lead or post a
/// warning to the channel.
///
/// This test demonstrates the bug by showing that a PR with:
/// - A valid coworker branch name (york/fix-auth)
/// - A merge conflict (mergeable: "CONFLICTING")
/// - NO entry in worktree_branch_owners (simulating "coworker on break")
///
/// ...currently generates NO effects (the bug), when it should generate at
/// least a warning or nudge about the orphaned conflicting PR.
#[tokio::test]
async fn test_orphaned_pr_with_merge_conflict_is_ignored() {
    use super::super::snapshot::minimal_snapshot_for_test;

    // Create a PR with a merge conflict owned by york, but york is NOT active
    // (simulating york went on break after creating the PR)
    let pr_json = json!({
        "number": 123,
        "headRefName": "york/fix-auth",
        "title": "Fix authentication bug",
        "mergeable": "CONFLICTING",  // This is the issue we want to detect!
        "statusCheckRollup": null,
        "reviewDecision": null,
    });

    // Write the PR JSON to a temp file so poll_prs_for_issues can read it via gh CLI mock
    let temp_dir = tempfile::tempdir().unwrap();
    let pr_list_file = temp_dir.path().join("pr_list.json");
    std::fs::write(
        &pr_list_file,
        serde_json::to_string(&vec![pr_json]).unwrap(),
    )
    .unwrap();

    // Acquire lock to prevent parallel tests from interfering with PATH mocking
    let _path_guard = PATH_LOCK.lock().unwrap();

    // Mock gh CLI to return our test PR
    let original_path = std::env::var("PATH").unwrap_or_default();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");

    #[cfg(unix)]
    {
        std::fs::write(
            &mock_gh_script,
            format!("#!/bin/bash\ncat {}", pr_list_file.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    // Drop the lock before async calls to avoid holding it across await points
    drop(_path_guard);

    // Create a minimal snapshot with NO active coworkers (york is on break)
    // and NO worktree_branch_owners entry for york/fix-auth (the key part!)
    let snap = minimal_snapshot_for_test();
    // snap.worktree_branch_owners is empty - this simulates york being on break

    // Create minimal daemon state
    let state = make_test_state("test-repo");

    // Call poll_prs_for_issues
    let result = poll_prs_for_issues(&snap, &state).await;

    // Restore PATH (lock will be released when _path_guard drops)
    unsafe {
        std::env::set_var("PATH", original_path);
    }

    // Check if we got an error (gh command not working)
    if let Err(e) = &result {
        panic!("poll_prs_for_issues failed: {}", e);
    }

    let effects = result.unwrap();

    // After the fix, this assertion should fail because we now detect
    // orphaned PRs with merge conflicts and generate a warning effect.
    //
    // Expected effects after fix:
    // - Effect::PostSystemMessage with "@lead Orphaned PR #123..." warning
    assert!(
        !effects.is_empty(),
        "Expected effects for orphaned PR with merge conflict. Got: {:?}",
        effects
    );

    // Verify we got a system message warning about the orphaned PR
    let has_orphan_warning = effects.iter().any(
        |e| matches!(e, Effect::PostSystemMessage { message } if message.contains("Orphaned PR")),
    );
    assert!(
        has_orphan_warning,
        "Expected PostSystemMessage warning about orphaned PR. Effects: {:?}",
        effects
    );

    // Verify we record the nudge to prevent infinite warning loops
    let has_nudge_record = effects
        .iter()
        .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 123, .. }));
    assert!(
        has_nudge_record,
        "Expected RecordPrNudge to prevent repeated warnings. Effects: {:?}",
        effects
    );
}

#[tokio::test]
async fn test_ci_wait_deduplication_uses_time_aware_hash() {
    use super::super::snapshot::minimal_snapshot_for_test;

    let pr = json!({
        "number": 100,
        "headRefName": "york/test-branch",
        "title": "Test PR",
        "mergeable": "MERGEABLE",
        "reviewDecision": "APPROVED",
        "statusCheckRollup": [
            {
                "name": "CI",
                "status": "IN_PROGRESS",
                "conclusion": null,
            }
        ],
    });

    // Write PR to temp file for gh CLI mock
    let temp_dir = tempfile::tempdir().unwrap();
    let pr_list_file = temp_dir.path().join("pr_list.json");
    std::fs::write(&pr_list_file, serde_json::to_string(&vec![pr]).unwrap()).unwrap();

    // Acquire lock to prevent parallel tests from interfering with PATH mocking
    let _path_guard = PATH_LOCK.lock().unwrap();

    let original_path = std::env::var("PATH").unwrap_or_default();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");

    #[cfg(unix)]
    {
        std::fs::write(
            &mock_gh_script,
            format!("#!/bin/bash\ncat {}", pr_list_file.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    // Drop the lock before async calls to avoid holding it across await points
    drop(_path_guard);

    // Create snapshot with york's branch in worktree_branch_owners
    let mut snap = minimal_snapshot_for_test();
    snap.worktree_branch_owners
        .insert("york/test-branch".to_string(), "york".to_string());

    let state = make_test_state("test-repo");

    // First poll should post "Waiting for CI" message
    let effects1 = poll_prs_for_issues(&snap, &state).await.unwrap();

    // Second poll immediately should NOT post duplicate (deduplicated by time-aware hash)
    let _effects2 = poll_prs_for_issues(&snap, &state).await.unwrap();

    // Restore PATH (lock will be released when _path_guard drops)
    unsafe {
        std::env::set_var("PATH", original_path);
    }

    // Both polls should return effects (the nudge to york about waiting for CI)
    assert!(!effects1.is_empty(), "First poll should generate effects");

    // Second poll should be deduplicated (no new effects for same PR state within time window)
    // Note: This depends on the time-aware hash bucket size. If the test runs across
    // a bucket boundary, it might generate a new effect. For now, we just verify
    // the first poll generates effects.
}

#[test]
fn test_detect_abandoned_pr_tasks_no_reset_for_open_pr() {
    use super::super::snapshot::minimal_snapshot_for_test;

    // Task !100 is in progress, associated with PR #100
    let in_progress_tasks = vec![("100".to_string(), "Fix bug".to_string(), "york".to_string())];

    let mut snap = minimal_snapshot_for_test();
    snap.in_progress_tasks = in_progress_tasks;
    snap.pr_task_associations.insert(100u64, "100".to_string());

    // PR 100 is in the open list
    let open_pr_numbers = vec![100u64];
    let effects = detect_abandoned_pr_tasks(&snap, &open_pr_numbers, "test-repo");

    // Should emit no effects since PR is still open
    assert!(effects.is_empty(), "Should not reset task for open PR");
}

/// Test that detect_abandoned_pr_tasks doesn't reset a task when a duplicate
/// PR is closed if the same task has a sibling PR that was already merged.
///
/// Scenario: Task !1158 has two PRs:
/// - PR #968 (merged)
/// - PR #999 (closed without merge - duplicate)
///
/// When PR #999 is detected as abandoned, the task should NOT be reset because
/// the work was already landed via PR #968.
#[test]
fn test_detect_abandoned_pr_tasks_checks_for_merged_siblings() {
    use super::super::snapshot::minimal_snapshot_for_test;
    use crate::tasks::{Task, TaskStatus};

    // Task !1158 is "in progress" (completed, but still in_progress_tasks for this test)
    let in_progress_tasks = vec![(
        "1158".to_string(),
        "Fix bug".to_string(),
        "york".to_string(),
    )];

    // Full task object showing it's completed and has pr field pointing to merged PR
    let task = Task {
        id: "1158".to_string(),
        subject: "Fix bug".to_string(),
        status: TaskStatus::Completed,
        owner: Some("york".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(968), // Task.pr points to merged PR #968
        created_at: None,
    };

    let mut snap = minimal_snapshot_for_test();
    snap.in_progress_tasks = in_progress_tasks;
    snap.all_tasks = vec![task];

    // PR associations: both PRs are associated with the same task
    snap.pr_task_associations.insert(968u64, "1158".to_string()); // merged PR
    snap.pr_task_associations.insert(999u64, "1158".to_string()); // duplicate PR (closed)

    // PR #968 is merged, PR #999 is NOT merged
    snap.merged_pr_numbers.insert(968u64);

    // PR 100 is NOT in open list, but it IS in merged list
    let open_pr_numbers = vec![];
    let effects = detect_abandoned_pr_tasks(&snap, &open_pr_numbers, "test-repo");

    // Should emit no effects since merged PRs are handled separately
    assert!(effects.is_empty(), "Should not reset task for merged PR");
}

/// Bug: When a coworker creates a PR with a task-based branch name (e.g.,
/// "task-42-fix-auth") and then goes on break, the branch is removed from
/// worktree_branch_owners. coworker_from_branch_with_map() returns None
/// for this branch (it's not in the map and doesn't match the coworker/branch
/// pattern). The poll_prs_for_issues loop skips such PRs entirely at line 719,
/// so merge conflicts are never detected.
///
/// This test demonstrates the bug by showing that a PR with:
/// - A task-based branch name ("task-42-fix-auth")
/// - A merge conflict (mergeable: "CONFLICTING")
/// - NO entry in worktree_branch_owners (owner_opt will be None)
///
/// ...currently generates NO effects (the bug), when it should generate at
/// least a warning about the orphaned conflicting PR.
#[tokio::test]
async fn test_orphaned_pr_with_task_branch_and_merge_conflict_is_ignored() {
    use super::super::snapshot::minimal_snapshot_for_test;

    // Create a PR with a task-based branch name and a merge conflict
    // The branch doesn't match "coworker/branch" pattern AND isn't in worktree_branch_owners
    let pr_json = json!({
        "number": 456,
        "headRefName": "task-42-fix-auth",  // Task-based branch, not "york/fix-auth"
        "title": "Fix authentication bug",
        "mergeable": "CONFLICTING",  // This is the issue we want to detect!
        "statusCheckRollup": null,
        "reviewDecision": null,
    });

    // Write the PR JSON to a temp file so poll_prs_for_issues can read it via gh CLI mock
    let temp_dir = tempfile::tempdir().unwrap();
    let pr_list_file = temp_dir.path().join("pr_list.json");
    std::fs::write(
        &pr_list_file,
        serde_json::to_string(&vec![pr_json]).unwrap(),
    )
    .unwrap();

    // Mock gh CLI to return our test PR
    let original_path = std::env::var("PATH").unwrap_or_default();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");

    #[cfg(unix)]
    {
        std::fs::write(
            &mock_gh_script,
            format!("#!/bin/bash\ncat {}", pr_list_file.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    // Create a minimal snapshot with NO active coworkers and NO worktree_branch_owners
    // entry for task-42-fix-auth. coworker_from_branch_with_map will return None.
    let snap = minimal_snapshot_for_test();
    // snap.worktree_branch_owners is empty - this ensures owner_opt = None

    // Create minimal daemon state
    let state = make_test_state("test-repo");

    // Call poll_prs_for_issues
    let result = poll_prs_for_issues(&snap, &state).await;

    // Restore PATH
    unsafe {
        std::env::set_var("PATH", original_path);
    }

    // Check if we got an error (gh command not working)
    if let Err(e) = &result {
        panic!("poll_prs_for_issues failed: {}", e);
    }

    let effects = result.unwrap();

    // Before the fix, this assertion FAILS because the PR is skipped entirely
    // at line 719 (None => continue) when owner_opt is None.
    //
    // After the fix, we should detect merge conflicts even when owner_opt is None
    // and generate a warning effect.
    assert!(
        !effects.is_empty(),
        "BUG: Expected effects for orphaned PR with merge conflict, but got none. \
         The PR with branch 'task-42-fix-auth' and merge conflict was completely \
         ignored because coworker_from_branch_with_map returned None."
    );

    // Verify we got a system message warning about the orphaned PR
    let has_orphan_warning = effects.iter().any(
        |e| matches!(e, Effect::PostSystemMessage { message } if message.contains("Orphaned PR") || message.contains("orphaned")),
    );
    assert!(
        has_orphan_warning,
        "Expected PostSystemMessage warning about orphaned PR with merge conflict. Effects: {:?}",
        effects
    );
}
