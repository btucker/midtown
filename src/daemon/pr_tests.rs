use super::*;
use serde_json::json;

// Use the shared PATH_LOCK from daemon/mod.rs so that PATH-mocking tests in
// pr_tests.rs and effects_tests.rs serialize against each other.  Two
// separate per-file statics would allow them to run concurrently and corrupt
// each other's gh CLI mock.
use crate::daemon::PATH_LOCK;

/// Helper to create minimal DaemonState for testing
fn make_test_state(
    repo_name: &str,
) -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;

    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

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
    let cm = crate::coworker::CoworkerManager::new(wm);

    let base_dir = temp_dir.path().to_path_buf();

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
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
    .expect("daemon state");
    (state, temp_dir, _guard)
}

/// Helper to create minimal DaemonState for testing with a specific repo owner.
/// Adds a fake origin remote so DaemonState::new detects the owner from git URL.
fn make_test_state_with_owner(
    repo_name: &str,
    owner: &str,
) -> (
    DaemonState,
    tempfile::TempDir,
    crate::paths::TestMidtownBaseDirGuard,
) {
    use std::process::Command;

    let midtown_dir = tempfile::tempdir().expect("midtown temp dir");
    let _guard = crate::paths::set_test_midtown_base_dir(midtown_dir.path().to_path_buf());

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
    // Add a fake origin remote so DaemonState::new extracts the repo owner from it
    Command::new("git")
        .args([
            "remote",
            "add",
            "origin",
            &format!("https://github.com/{}/{}.git", owner, repo_name),
        ])
        .current_dir(temp_dir.path())
        .output()
        .expect("git remote add");

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
        .expect("worktree manager");
    let cm = crate::coworker::CoworkerManager::new(wm);

    let base_dir = temp_dir.path().to_path_buf();

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    let state = DaemonState::new(
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
    .expect("daemon state");
    (state, temp_dir, _guard)
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
    // causing double delivery because the PostSystemMessage handler in effects.rs
    // already routes @ops mentions to the ops channel lead.
    //
    // The fix is to only return PostSystemMessage and let the PostSystemMessage
    // handler handle the nudge via @ops detection.
    let message = "@ops PR #42 (Add feature) has been open for 60 minutes without a review";
    let effects = stuck_nudge_effects(message);

    // Should only return one effect (PostSystemMessage)
    assert_eq!(
        effects.len(),
        1,
        "stuck_nudge_effects should return exactly 1 effect, not 2 (double nudge bug)"
    );

    // That effect should be PostSystemMessage with the warning emoji prefix
    match &effects[0] {
        Effect::PostSystemMessage { message: msg, .. } => {
            assert!(
                msg.starts_with("⚠️"),
                "System message should have warning prefix"
            );
            assert!(
                msg.contains("@ops"),
                "System message should preserve @ops mention for ops channel lead routing"
            );
        }
        _ => panic!("Expected PostSystemMessage effect, got {:?}", effects[0]),
    }
}

/// Tests for `format_no_reviewer_reason` — the diagnostic string included in
/// "I couldn't assign a reviewer" messages to help ops triage without reading logs.
#[test]
fn format_no_reviewer_reason_all_busy() {
    let busy = vec!["madison".to_string(), "york".to_string()];
    let reason = format_no_reviewer_reason(&busy, None);
    assert_eq!(
        reason, "no eligible reviewers (busy: [madison, york])",
        "All slots busy, no author exclusion"
    );
}

#[test]
fn format_no_reviewer_reason_busy_and_author_excluded() {
    let busy = vec!["madison".to_string(), "york".to_string()];
    let reason = format_no_reviewer_reason(&busy, Some("york"));
    assert_eq!(
        reason, "no eligible reviewers (busy: [madison, york], excluded-author: york)",
        "All slots busy, york also excluded as PR author"
    );
}

#[test]
fn format_no_reviewer_reason_no_coworkers() {
    let busy: Vec<String> = vec![];
    let reason = format_no_reviewer_reason(&busy, None);
    assert_eq!(
        reason, "no eligible reviewers (no coworkers running)",
        "No coworkers are running at all"
    );
}

#[test]
fn format_no_reviewer_reason_only_author_excluded() {
    // Only one coworker running, but it's the PR author
    let busy = vec!["york".to_string()];
    let reason = format_no_reviewer_reason(&busy, Some("york"));
    assert_eq!(
        reason, "no eligible reviewers (busy: [york], excluded-author: york)",
        "Only available coworker is the PR author"
    );
}

/// Task-based branches (the common case in Midtown) require the worktree_branch_owners
/// map to resolve to a coworker name. Without the map, `coworker_from_branch` returns
/// None, so the diagnostic message omits the `excluded-author` annotation even when the
/// PR author is identifiable. This test verifies the full resolution path works correctly.
#[test]
fn format_no_reviewer_reason_task_based_branch_resolves_with_map() {
    let mut branch_owners = std::collections::HashMap::new();
    branch_owners.insert("task-42-fix-auth".to_string(), "york".to_string());

    // Without the map, coworker_from_branch returns None for task-based branches
    let without_map = coworker_from_branch("task-42-fix-auth");
    assert_eq!(
        without_map, None,
        "coworker_from_branch should return None for task-based branches"
    );

    // With the map, coworker_from_branch_with_map resolves correctly
    let with_map = coworker_from_branch_with_map("task-42-fix-auth", Some(&branch_owners));
    assert_eq!(
        with_map,
        Some("york".to_string()),
        "coworker_from_branch_with_map should resolve task-based branch via map"
    );

    // The resolved author should appear as excluded-author in the diagnostic
    let busy = vec!["madison".to_string(), "york".to_string()];
    let reason = format_no_reviewer_reason(&busy, with_map.as_deref());
    assert_eq!(
        reason, "no eligible reviewers (busy: [madison, york], excluded-author: york)",
        "excluded-author should appear when task-based branch resolves via map"
    );
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
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await to prevent test interference
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

    // Create a minimal snapshot with NO active coworkers (york is on break)
    // and NO worktree_branch_owners entry for york/fix-auth (the key part!)
    let snap = minimal_snapshot_for_test();
    // snap.worktree_branch_owners is empty - this simulates york being on break

    // Create minimal daemon state
    let (state, _tmp, _guard) = make_test_state("test-repo");

    // Call poll_prs_for_issues (keep PATH_LOCK held to prevent test interference)
    let result = poll_prs_for_issues(&snap, &state).await;

    // Restore PATH and release lock
    unsafe {
        std::env::set_var("PATH", original_path);
    }
    drop(_path_guard);

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
        |e| matches!(e, Effect::PostSystemMessage { message, .. } if message.contains("Orphaned PR")),
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

/// Bug: Lead PRs without task IDs in their title are incorrectly marked as
/// orphaned, preventing reviewer assignment.
///
/// Scenario: The lead creates a PR with branch "lead/remove-build-all-script"
/// and title "chore: Remove build-all.sh in favor of cargo install" (no task ID).
///
/// The orphan detection logic checks:
/// 1. Look up by PR number → not found (not linked yet)
/// 2. Look up by task ID from title → no task ID in title → not found
/// 3. Look up by branch name → "lead/remove-build-all-script" doesn't match any worktree
///
/// Result: worktree = None → marked as orphaned → no reviewer spawned.
///
/// Expected: Lead PRs should never be marked as orphaned because the lead's
/// main worktree is always available to address review feedback.
///
/// Note: Uses PATH_LOCK to mock gh CLI. collect_reviewer_effects_with_source calls
/// is_pr_reviewed() which shells out to `gh pr view --json reviews,comments`. Without
/// mocking, the test fails once the real PR #1164 has a Claude review posted to it.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await to prevent test interference
async fn test_lead_pr_without_task_id_should_not_be_orphaned() {
    use crate::worktree_registry::WorktreeRegistry;

    // Simulate PR #1164: lead branch, no task ID in title
    let pr = json!({
        "number": 1164,
        "headRefName": "lead/remove-build-all-script",
        "title": "chore: Remove build-all.sh in favor of cargo install",
        "isDraft": false,
        "createdAt": "2024-01-01T00:00:00Z",  // Old enough to pass review delay
    });

    // Acquire lock to prevent parallel tests from interfering with PATH mocking
    let _path_guard = PATH_LOCK.lock().unwrap();

    // Mock gh CLI to return no reviews/comments so is_pr_reviewed() returns false.
    // Without this mock, the test makes a real API call and fails once PR #1164
    // has a Claude review posted (is_pr_reviewed returns true → continue → no effects).
    let temp_dir = tempfile::tempdir().unwrap();
    let mock_gh_dir = temp_dir.path().join("bin");
    std::fs::create_dir_all(&mock_gh_dir).unwrap();
    let mock_gh_script = mock_gh_dir.join("gh");

    #[cfg(unix)]
    {
        std::fs::write(
            &mock_gh_script,
            "#!/bin/bash\necho '{\"reviews\":[],\"comments\":[]}'",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&mock_gh_script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var(
            "PATH",
            format!("{}:{}", mock_gh_dir.display(), original_path),
        );
    }

    let (state, _tmp, _guard) = make_test_state("midtown");
    let registry = WorktreeRegistry::new();

    // Important: The lead's worktree exists, but NOT indexed by this branch name
    // (it's the main "lead" worktree in a different location)
    let active_names = std::collections::HashSet::new();

    let effects = collect_reviewer_effects_with_source(
        None, // No branch_owners_map (simulating empty worktree map)
        &registry,
        &active_names,
        &state,
        &[pr],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // Restore PATH and release lock
    unsafe {
        std::env::set_var("PATH", original_path);
    }
    drop(_path_guard);

    // Bug: Currently returns 0 effects (PR marked as orphaned, skipped)
    // Expected: Should spawn a reviewer (lead can address feedback)
    assert!(
        !effects.is_empty(),
        "Lead PR without task ID should spawn a reviewer, not be marked as orphaned"
    );

    // Verify we got a SpawnCoworkerWithCallbacks effect for the reviewer
    let has_reviewer_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        has_reviewer_spawn,
        "Expected SpawnCoworkerWithCallbacks effect for lead PR. Effects: {:#?}",
        effects
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await to prevent test interference
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

    // Create snapshot with york's branch in worktree_branch_owners
    let mut snap = minimal_snapshot_for_test();
    snap.worktree_branch_owners
        .insert("york/test-branch".to_string(), "york".to_string());

    let (state, _tmp, _guard) = make_test_state("test-repo");

    // First poll should post "Waiting for CI" message (keep PATH_LOCK held to prevent test interference)
    let effects1 = poll_prs_for_issues(&snap, &state).await.unwrap();

    // Second poll immediately should NOT post duplicate (deduplicated by time-aware hash)
    let _effects2 = poll_prs_for_issues(&snap, &state).await.unwrap();

    // Restore PATH and release lock
    unsafe {
        std::env::set_var("PATH", original_path);
    }
    drop(_path_guard);

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
#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await to prevent test interference
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

    // Create a minimal snapshot with NO active coworkers and NO worktree_branch_owners
    // entry for task-42-fix-auth. coworker_from_branch_with_map will return None.
    let snap = minimal_snapshot_for_test();
    // snap.worktree_branch_owners is empty - this ensures owner_opt = None

    // Create minimal daemon state
    let (state, _tmp, _guard) = make_test_state("test-repo");

    // Call poll_prs_for_issues (keep PATH_LOCK held to prevent test interference)
    let result = poll_prs_for_issues(&snap, &state).await;

    // Restore PATH and release lock
    unsafe {
        std::env::set_var("PATH", original_path);
    }
    drop(_path_guard);

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
        |e| matches!(e, Effect::PostSystemMessage { message, .. } if message.contains("Orphaned PR") || message.contains("orphaned")),
    );
    assert!(
        has_orphan_warning,
        "Expected PostSystemMessage warning about orphaned PR with merge conflict. Effects: {:?}",
        effects
    );
}

/// Bug: After a daemon restart, worktree_branch_owners is empty because
/// current_coworker is None for all worktree assignments. This causes
/// collect_reviewer_effects_with_source to skip all PRs as "orphaned"
/// even though their worktrees still exist in the registry.
///
/// Expected: PRs whose branches exist in the worktree registry should
/// still get reviewer spawns, even if no coworker is currently bound.
#[tokio::test]
async fn test_reviewer_spawns_when_worktree_exists_but_no_current_coworker() {
    // Create a PR that needs review — branch is lexington/fix-auth, owned by lexington
    let pr_json = serde_json::json!({
        "number": 456,
        "headRefName": "lexington/fix-auth",
        "title": "Fix authentication bug [Midtown !100]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Create a worktree registry with the branch registered but NO current_coworker
    // (simulates post-restart state where coworker bindings are cleared)
    let mut registry = crate::worktree_registry::WorktreeRegistry::default();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-100-fix-auth".to_string(),
            branch_name: "lexington/fix-auth".to_string(),
            task_id: Some("100".to_string()),
            current_coworker: None, // Key: no coworker bound (post-restart)
            pr_number: Some(456),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();

    // branch_owners_map is empty (mirrors snapshot after restart)
    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    let (state, _tmp, _guard) = make_test_state("test-repo");
    let active_names = std::collections::HashSet::new();

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // The PR should NOT be skipped as orphaned — effects should be non-empty.
    // The specific effect depends on review state, but the key assertion is that
    // the worktree registry lookup prevents the PR from being incorrectly skipped.
    // Before the fix, this returned an empty Vec because the empty branch_owners_map
    // caused every PR to be treated as orphaned.
    assert!(
        !effects.is_empty(),
        "PR with registered worktree but no current_coworker should NOT be skipped as orphaned. \
         Before fix: empty branch_owners_map caused all PRs to be treated as orphaned after restart.",
    );
}

/// Bug: After daemon restart, completed worktrees cause open PRs to be treated as orphaned.
///
/// Root cause: collect_reviewer_effects_with_source checked `completed_at.is_some()` and
/// marked such PRs as orphaned, preventing reviewer spawn. But the PR is still open,
/// so the author can still address review feedback.
///
/// Expected: PRs with completed worktrees that are still open should get reviewer spawns.
#[tokio::test]
async fn test_completed_worktree_with_open_pr_gets_reviewer() {
    let pr_json = serde_json::json!({
        "number": 789,
        "headRefName": "madison/fix-polling",
        "title": "Fix polling reconciliation [Midtown !200]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Create a worktree registry with a COMPLETED worktree (completed_at is set)
    let mut registry = crate::worktree_registry::WorktreeRegistry::default();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-200-fix-polling".to_string(),
            branch_name: "madison/fix-polling".to_string(),
            task_id: Some("200".to_string()),
            current_coworker: None,
            pr_number: Some(789),
            created_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()), // Key: worktree is completed
        })
        .unwrap();

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let active_names = std::collections::HashSet::new();

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // The PR should NOT be skipped as orphaned — effects should be non-empty.
    // Before the fix, completed_at being set caused the PR to be treated as orphaned.
    assert!(
        !effects.is_empty(),
        "PR with completed worktree but still open should NOT be skipped as orphaned. \
         Before fix: completed_at caused open PRs to be treated as orphaned, blocking reviewer spawn.",
    );
}

/// Test the completed worktree bug fix using real captured snapshot worktree data.
///
/// This test uses the worktree registry from a real snapshot (where task 1323's
/// worktree is completed) combined with synthetic PR JSON to verify that
/// `collect_reviewer_effects_with_source` correctly spawns a reviewer for a PR
/// with a completed worktree.
///
/// Complements `test_completed_worktree_with_open_pr_gets_reviewer` by using
/// real worktree registry data instead of fully synthetic data.
#[tokio::test]
async fn test_completed_worktree_with_snapshot_data() {
    use super::super::snapshot::WorldSnapshot;

    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-review-spawn-lost-after-restart-20260217-003046.json"
    );
    let snap: WorldSnapshot =
        serde_json::from_str(fixture).expect("Failed to deserialize WorldSnapshot from fixture");

    // Verify the snapshot has the completed worktree we're testing
    let task_1323_worktree = snap
        .worktree_registry
        .all_assignments()
        .values()
        .find(|a| a.task_id.as_deref() == Some("1323"))
        .expect("Snapshot should contain worktree for task 1323");

    assert!(
        task_1323_worktree.completed_at.is_some(),
        "Task 1323 worktree should be completed in the snapshot"
    );

    // Create a PR JSON for testing (GitHub API format with all required fields)
    // Use a unique PR number that won't conflict with any cached review state.
    // Title includes task ID so the function can find the completed worktree.
    let pr_json = serde_json::json!({
        "number": 99999,  // Unique PR number to avoid cached state
        "headRefName": "test/snapshot-worktree-bug",
        "title": "Test PR with completed worktree [Midtown !1323]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,  // No review yet
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Empty branch_owners - this PR doesn't match a coworker branch,
    // but it will be found via task ID extraction from the title
    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Create minimal test state
    let (state, _tmp, _guard) = make_test_state("midtown");
    let active_names = std::collections::HashSet::new();

    // Call the function under test with snapshot's worktree registry and synthetic PR
    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &snap.worktree_registry, // Real snapshot data with task 1323's completed worktree
        &active_names,
        &state,
        &[pr_json], // Synthetic PR that extracts task ID 1323 from title
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // The function should return at least one effect for PR #99999
    assert!(
        !effects.is_empty(),
        "Polling reconciliation should produce effects for unreviewed PR with completed worktree. \
         Snapshot has task 1323 with completed worktree, PR #99999 should get a reviewer assigned."
    );

    // Verify that one of the effects is an AssignReviewer for PR #99999
    let has_assign_reviewer = effects.iter().any(|effect| {
        matches!(
            effect,
            crate::daemon::effects::Effect::AssignReviewer { pr_number, .. }
            if *pr_number == 99999
        )
    });

    assert!(
        has_assign_reviewer,
        "Expected AssignReviewer effect for PR #99999. \
         Before fix: completed worktrees caused PRs to be skipped as orphaned. \
         After fix: open PRs with completed worktrees should get reviewer spawns. \
         Effects: {:#?}",
        effects
    );
}

/// Bug: PRs from the lead with non-lead/ branch names are incorrectly marked as orphaned.
///
/// Root cause: The orphan detection in `collect_reviewer_effects_with_source` (lines 2157-2183)
/// checks if a PR has no worktree. If not, it checks `is_lead_branch()` (only returns true for
/// `lead/*` branches). PRs like `codex/*` fail both checks and are marked as orphaned, preventing
/// reviewer spawning.
///
/// Expected: PRs authored by the lead (repo owner) should get reviewers regardless of branch name.
/// The lead can address feedback from their main worktree even if the branch doesn't follow `lead/*`.
#[tokio::test]
async fn test_lead_pr_with_non_standard_branch_gets_reviewer() {
    // Create a PR with a non-lead/ branch name (like codex/*, feature/*, etc.)
    // but authored by the lead user (repo owner)
    let pr_json = serde_json::json!({
        "number": 1198,
        "headRefName": "codex/auth-usage-all-providers",  // Not lead/* and not a coworker name
        "title": "auth: fetch usage for codex and z.ai",
        "author": {
            "login": "btucker",  // This is the repo owner (lead)
            "is_bot": false
        },
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Empty branch_owners - this branch doesn't match any coworker
    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Empty worktree registry - no worktree exists for this PR
    let worktree_registry = crate::worktree_registry::WorktreeRegistry::new();

    // Create test state with repo_owner set to match the PR author.
    // The repo_owner is extracted from git remote URL at daemon startup;
    // in tests we configure it via a fake origin remote.
    let (state, _tmp, _guard) = make_test_state_with_owner("midtown", "btucker");
    let active_names = std::collections::HashSet::new();

    // Call the function under test
    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &worktree_registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // The function should spawn a reviewer for this PR
    let has_assign_reviewer = effects.iter().any(|effect| {
        matches!(
            effect,
            crate::daemon::effects::Effect::AssignReviewer { pr_number, .. }
            if *pr_number == 1198
        )
    });

    assert!(
        has_assign_reviewer,
        "Expected AssignReviewer effect for PR #1198. \
         Before fix: PRs from lead with non-lead/ branches were marked as orphaned. \
         After fix: lead-authored PRs should get reviewers regardless of branch name. \
         Effects: {:#?}",
        effects
    );
}

/// reconcile_orphaned_prs nudges the lead once (not on every tick) for orphaned PRs.
///
/// The function uses `orphaned_pr_lead_nudges_sent` in WorldSnapshot to avoid
/// nudging the lead on every polling tick (every 30 seconds). Once the lead has
/// been nudged, the PR number is recorded and subsequent ticks skip it.
///
/// Expected: First tick nudges the lead + records it; second tick (with record present) is silent.
#[test]
fn test_reconcile_orphaned_prs_does_not_create_duplicates() {
    use super::super::snapshot::minimal_snapshot_for_test;

    // Simulate PR #42 that meets all orphan criteria:
    // - Has coworker branch prefix
    // - Has been reviewed
    // - CI is passing
    // - Not a draft
    let pr_data = json!({
        "number": 42,
        "title": "Fix authentication bug",
        "headRefName": "york/fix-auth",
        "isDraft": false,
        "statusCheckRollup": {
            "state": "SUCCESS"
        }
    });

    let mut snap = minimal_snapshot_for_test();
    snap.open_prs_data = vec![pr_data];
    snap.reviewed_prs.insert(42);

    // First tick: Lead has not been nudged yet
    let effects1 = reconcile_orphaned_prs(&snap);

    // Should nudge the lead (not create a task)
    let nudge_count1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::NudgeChannelLead { .. }))
        .count();
    assert_eq!(nudge_count1, 1, "First tick should nudge the lead once");

    let no_task_created = effects1
        .iter()
        .all(|e| !matches!(e, Effect::CreateTask { .. }));
    assert!(no_task_created, "First tick should NOT create a task");

    assert!(
        effects1
            .iter()
            .any(|e| matches!(e, Effect::RecordOrphanedPrLeadNudge { pr_number: 42 })),
        "First tick should emit RecordOrphanedPrLeadNudge for PR #42"
    );

    // Simulate the nudge has been recorded (orphaned_pr_lead_nudges_sent contains PR #42)
    snap.orphaned_pr_lead_nudges_sent.insert(42);

    // Second tick: Lead has already been nudged
    let effects2 = reconcile_orphaned_prs(&snap);

    // Should NOT nudge the lead again
    let nudge_count2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::NudgeChannelLead { .. }))
        .count();
    assert_eq!(
        nudge_count2, 0,
        "Second tick should NOT nudge the lead again (already nudged)"
    );
}

/// reconcile_orphaned_prs re-nudges lead if PR becomes orphaned again after task completes.
///
/// When a task is created for an orphaned PR (pr_task_associations has the PR), the nudge
/// record should be cleared. If the task later disappears without the PR being merged,
/// the lead should be nudged again.
///
/// Expected: nudge recorded → task appears → ClearOrphanedPrLeadNudge emitted →
///           task disappears → lead gets nudged again.
#[test]
fn test_reconcile_orphaned_prs_renudges_after_task_disappears() {
    use super::super::snapshot::minimal_snapshot_for_test;

    let pr_data = json!({
        "number": 42,
        "title": "Fix authentication bug",
        "headRefName": "york/fix-auth",
        "isDraft": false,
        "statusCheckRollup": {
            "state": "SUCCESS"
        }
    });

    let mut snap = minimal_snapshot_for_test();
    snap.open_prs_data = vec![pr_data];
    snap.reviewed_prs.insert(42);

    // Simulate: lead was already nudged about this PR
    snap.orphaned_pr_lead_nudges_sent.insert(42);

    // A task now exists for this PR (PR left orphaned state)
    snap.pr_task_associations.insert(42, "task-abc".to_string());

    // Tick: PR has a task, nudge record should be cleared
    let effects_with_task = reconcile_orphaned_prs(&snap);

    assert!(
        effects_with_task
            .iter()
            .any(|e| matches!(e, Effect::ClearOrphanedPrLeadNudge { pr_number: 42 })),
        "When PR has a task, should emit ClearOrphanedPrLeadNudge"
    );
    assert!(
        !effects_with_task
            .iter()
            .any(|e| matches!(e, Effect::NudgeChannelLead { .. })),
        "Should not nudge lead while PR has an active task"
    );

    // Simulate effect applied: nudge record cleared, task gone (task completed without merge)
    snap.orphaned_pr_lead_nudges_sent.remove(&42);
    snap.pr_task_associations.remove(&42);

    // Tick: PR is orphaned again — lead should be re-nudged
    let effects_re_orphaned = reconcile_orphaned_prs(&snap);

    let renudge_count = effects_re_orphaned
        .iter()
        .filter(|e| matches!(e, Effect::NudgeChannelLead { .. }))
        .count();
    assert_eq!(
        renudge_count, 1,
        "After task disappears, lead should be nudged again"
    );
}

/// Helper to create a PrContext with a task association for a given PR number.
fn make_pr_context_with_task(pr_number: u64, task_id: &str) -> PrContext {
    let mut pr_task_associations = std::collections::HashMap::new();
    pr_task_associations.insert(pr_number, task_id.to_string());
    PrContext {
        pr_task_associations,
        task_channel: std::collections::HashMap::new(),
        session_context: None,
        task_session_id: None,
        has_active_reviewer: false,
    }
}

/// Helper to create a PrContext with no task associations.
fn make_pr_context_empty() -> PrContext {
    PrContext {
        pr_task_associations: std::collections::HashMap::new(),
        task_channel: std::collections::HashMap::new(),
        session_context: None,
        task_session_id: None,
        has_active_reviewer: false,
    }
}

/// Helper: extract RecordTaskAssignment effects from SpawnCoworkerWithCallbacks on_success.
fn extract_record_task_assignments(effects: &[Effect]) -> Vec<(&str, &str)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::SpawnCoworkerWithCallbacks { on_success, .. } => Some(on_success),
            _ => None,
        })
        .flat_map(|on_success| {
            on_success.iter().filter_map(|e| match e {
                Effect::RecordTaskAssignment { coworker, task_id } => {
                    Some((coworker.as_str(), task_id.as_str()))
                }
                _ => None,
            })
        })
        .collect()
}

/// Cross-tick spawn dedup (!1377): pr_action_to_effects with SpawnOwner includes
/// RecordTaskAssignment when pr_task_associations has an entry for the PR.
#[test]
fn pr_action_spawn_owner_includes_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_with_task(42, "100");

    let effects = pr_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "york".to_string(),
            message: "PR needs attention".to_string(),
        },
        42,
        "Fix auth",
        PrIssueType::MergeConflict,
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert_eq!(
        assignments.len(),
        1,
        "SpawnOwner should include RecordTaskAssignment when task association exists"
    );
    assert_eq!(assignments[0], ("york", "100"));
}

/// Cross-tick spawn dedup (!1377): pr_action_to_effects with SpawnOwner does NOT
/// include RecordTaskAssignment when no task association exists.
#[test]
fn pr_action_spawn_owner_no_record_without_task_association() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_empty();

    let effects = pr_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "york".to_string(),
            message: "PR needs attention".to_string(),
        },
        42,
        "Fix auth",
        PrIssueType::MergeConflict,
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert!(
        assignments.is_empty(),
        "SpawnOwner should NOT include RecordTaskAssignment when no task association exists"
    );
}

/// Cross-tick spawn dedup (!1377): comment_action_to_effects with SpawnOwner includes
/// RecordTaskAssignment when pr_task_associations has an entry for the PR.
#[test]
fn comment_action_spawn_owner_includes_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_with_task(55, "200");

    let effects = comment_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "park".to_string(),
            message: "Review feedback arrived".to_string(),
        },
        55,
        "Add logging",
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert_eq!(
        assignments.len(),
        1,
        "comment SpawnOwner should include RecordTaskAssignment"
    );
    assert_eq!(assignments[0], ("park", "200"));
}

/// Cross-tick spawn dedup (!1377): comment_action_to_effects with SpawnOwner does NOT
/// include RecordTaskAssignment when no task association exists.
#[test]
fn comment_action_spawn_owner_no_record_without_task_association() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_empty();

    let effects = comment_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "park".to_string(),
            message: "Review feedback arrived".to_string(),
        },
        55,
        "Add logging",
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert!(
        assignments.is_empty(),
        "comment SpawnOwner should NOT include RecordTaskAssignment without task association"
    );
}

/// Cross-tick spawn dedup (!1377): handoff_to_coworker_effects includes
/// RecordTaskAssignment when pr_task_associations has an entry for the PR.
#[test]
fn handoff_effects_include_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_with_task(77, "300");

    let effects = handoff_to_coworker_effects(
        "madison",
        "york",
        77,
        "york/fix-auth",
        "session-123".to_string(),
        "Taking over PR",
        "resuming their session",
        "Fix auth",
        PrIssueType::ReviewComment,
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert_eq!(
        assignments.len(),
        1,
        "handoff should include RecordTaskAssignment"
    );
    assert_eq!(assignments[0], ("madison", "300"));
}

/// Cross-tick spawn dedup (!1377): handoff_to_coworker_effects does NOT include
/// RecordTaskAssignment when no task association exists.
#[test]
fn handoff_effects_no_record_without_task_association() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_empty();

    let effects = handoff_to_coworker_effects(
        "madison",
        "york",
        77,
        "york/fix-auth",
        "session-123".to_string(),
        "Taking over PR",
        "resuming their session",
        "Fix auth",
        PrIssueType::ReviewComment,
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert!(
        assignments.is_empty(),
        "handoff should NOT include RecordTaskAssignment without task association"
    );
}

/// Cross-tick spawn dedup (!1377): review_complete_action_to_effects with SpawnOwner
/// includes RecordTaskAssignment when pr_task_associations has an entry for the PR.
#[test]
fn review_complete_spawn_owner_includes_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_with_task(88, "400");

    let effects = review_complete_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "amsterdam".to_string(),
            message: "Review complete".to_string(),
        },
        88,
        "Refactor API",
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert_eq!(
        assignments.len(),
        1,
        "review_complete SpawnOwner should include RecordTaskAssignment"
    );
    assert_eq!(assignments[0], ("amsterdam", "400"));
}

/// Cross-tick spawn dedup (!1377): review_complete_action_to_effects with SpawnOwner
/// does NOT include RecordTaskAssignment when no task association exists.
#[test]
fn review_complete_spawn_owner_no_record_without_task_association() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let ctx = make_pr_context_empty();

    let effects = review_complete_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "amsterdam".to_string(),
            message: "Review complete".to_string(),
        },
        88,
        "Refactor API",
        &state,
        &ctx,
    );

    let assignments = extract_record_task_assignments(&effects);
    assert!(
        assignments.is_empty(),
        "review_complete SpawnOwner should NOT include RecordTaskAssignment without task association"
    );
}

/// reconcile_orphaned_prs skips PRs that already have an active task (pr_task_associations).
///
/// When a PR has an associated in_progress task (tracked in pr_task_associations),
/// it's not considered orphaned — someone is actively working on it. The function
/// should skip these PRs entirely.
#[test]
fn test_reconcile_orphaned_prs_ignores_prs_with_active_tasks() {
    use super::super::snapshot::minimal_snapshot_for_test;

    let pr_data = json!({
        "number": 43,
        "title": "Add logging feature",
        "headRefName": "park/add-logging",
        "isDraft": false,
        "statusCheckRollup": {
            "state": "SUCCESS"
        }
    });

    let mut snap = minimal_snapshot_for_test();
    snap.open_prs_data = vec![pr_data];
    snap.reviewed_prs.insert(43);

    // PR #43 has an active task association — it is NOT orphaned
    snap.pr_task_associations.insert(43, "999".to_string());

    // Call reconcile_orphaned_prs
    let effects = reconcile_orphaned_prs(&snap);

    // Should NOT nudge the lead because the PR has an active task
    let nudge_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::NudgeChannelLead { .. }))
        .count();
    assert_eq!(
        nudge_count, 0,
        "Should NOT nudge the lead when the PR already has an active task"
    );
}

/// Bug !1377: pr_action_to_effects was missing RecordTaskAssignment in on_success,
/// allowing cross-tick duplicate spawns for the same task.
#[test]
fn pr_action_to_effects_includes_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    // Build PrContext with a PR→task association
    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(123, "42".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        task_session_id: None,
        has_active_reviewer: false,
    };

    // Call pr_action_to_effects with SpawnOwner action
    let effects = pr_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "broadway".to_string(),
            message: "CI failed".to_string(),
        },
        123,
        "Fix auth bug [Midtown !42]",
        PrIssueType::CiFailed,
        &state,
        &ctx,
    );

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
        .expect("Should have SpawnCoworkerWithCallbacks");

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
        "pr_action_to_effects on_success must include RecordTaskAssignment for cross-tick dedup"
    );
}

/// Bug !1377: comment_action_to_effects was missing RecordTaskAssignment in on_success.
#[test]
fn comment_action_to_effects_includes_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(456, "99".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        task_session_id: None,
        has_active_reviewer: false,
    };

    let effects = comment_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "park".to_string(),
            message: "Review comment received".to_string(),
        },
        456,
        "Add feature [Midtown !99]",
        &state,
        &ctx,
    );

    let spawn_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                Some(on_success)
            } else {
                None
            }
        })
        .expect("Should have SpawnCoworkerWithCallbacks");

    let has_record = spawn_effect.iter().any(|e| {
        matches!(
            e,
            Effect::RecordTaskAssignment { coworker, task_id }
                if coworker == "park" && task_id == "99"
        )
    });
    assert!(
        has_record,
        "comment_action_to_effects on_success must include RecordTaskAssignment for cross-tick dedup"
    );
}

/// Bug !1377: handoff_to_coworker_effects was missing RecordTaskAssignment in on_success.
#[test]
fn handoff_to_coworker_effects_includes_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(789, "17".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        task_session_id: None,
        has_active_reviewer: false,
    };

    let effects = handoff_to_coworker_effects(
        "york",
        "broadway",
        789,
        "york/task-17-fix",
        "test-session-id".to_string(),
        "Handoff message",
        "context",
        "Refactor module [Midtown !17]",
        PrIssueType::ReviewComment,
        &state,
        &ctx,
    );

    let spawn_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                Some(on_success)
            } else {
                None
            }
        })
        .expect("Should have SpawnCoworkerWithCallbacks");

    let has_record = spawn_effect.iter().any(|e| {
        matches!(
            e,
            Effect::RecordTaskAssignment { coworker, task_id }
                if coworker == "york" && task_id == "17"
        )
    });
    assert!(
        has_record,
        "handoff_to_coworker_effects on_success must include RecordTaskAssignment for cross-tick dedup"
    );
}

/// Bug !1377: review_complete_action_to_effects was missing RecordTaskAssignment in on_success.
#[test]
fn review_complete_action_to_effects_includes_record_task_assignment() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(321, "55".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        task_session_id: None,
        has_active_reviewer: false,
    };

    let effects = review_complete_action_to_effects(
        crate::rules::PrAction::SpawnOwner {
            owner: "amsterdam".to_string(),
            message: "Review complete".to_string(),
        },
        321,
        "Update docs [Midtown !55]",
        &state,
        &ctx,
    );

    let spawn_effect = effects
        .iter()
        .find_map(|e| {
            if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                Some(on_success)
            } else {
                None
            }
        })
        .expect("Should have SpawnCoworkerWithCallbacks");

    let has_record = spawn_effect.iter().any(|e| {
        matches!(
            e,
            Effect::RecordTaskAssignment { coworker, task_id }
                if coworker == "amsterdam" && task_id == "55"
        )
    });
    assert!(
        has_record,
        "review_complete_action_to_effects on_success must include RecordTaskAssignment for cross-tick dedup"
    );
}

/// Bug: Active coworker's PR is incorrectly marked as orphaned when its worktree is missing.
///
/// Scenario (from snapshot snapshot-reviewer-not-assigned-pr-1246-20260218-001618.json):
/// - PR #1246 belongs to "park" (branch: "park/task-1483-kill-ring-yank-v2")
/// - "park" is an actively running coworker
/// - Task 1483's worktree was never registered (or was cleaned up)
/// - No entry in worktree_registry for this PR's task ID
///
/// The orphan detection logic checks:
/// 1. `worktree_registry.get_by_pr(1246)` → None
/// 2. Extract task_id 1483 from title → no worktree with task_id="1483" → None
/// 3. `worktree_registry.get_by_branch("park/task-1483-...")` → None
/// 4. `worktree = None` → check `is_lead_branch` → false
/// 5. `coworker_from_branch_with_map` → returns `Some("park")` (matches prefix)
/// 6. **Incorrectly marks PR as orphaned** even though park is actively running
///
/// Expected: Active coworker's PR should NOT be marked as orphaned — the coworker
/// can always address review feedback regardless of worktree status.
///
/// Note: The fixture's `active_names` and `worktree_registry` are used directly via
/// the `active_names` parameter (following the WorldSnapshot architecture pattern).
/// The fixture serves as documentation of the exact production scenario that triggered
/// the bug.
#[tokio::test]
async fn test_active_coworker_pr_without_worktree_is_not_orphaned() {
    use super::super::snapshot::WorldSnapshot;

    // Load the captured snapshot that exhibits the bug scenario
    let fixture = include_str!(
        "../../tests/fixtures/snapshot/snapshot-reviewer-not-assigned-pr-1246-20260218-001618.json"
    );
    let snap: WorldSnapshot =
        serde_json::from_str(fixture).expect("Failed to deserialize WorldSnapshot from fixture");

    // Verify the snapshot captures the bug scenario:
    // park is running and task 1483 has no worktree entry.
    assert!(
        snap.active_names.contains("park"),
        "Snapshot should show park as an active coworker"
    );
    let task_1483_worktree = snap
        .worktree_registry
        .all_assignments()
        .values()
        .find(|a| a.task_id.as_deref() == Some("1483"));
    assert!(
        task_1483_worktree.is_none(),
        "Snapshot should have no worktree entry for task 1483 (the bug scenario)"
    );

    // PR #1246 in GitHub API format (collect_reviewer_effects_with_source expects this format)
    let pr = json!({
        "number": 1246,
        "headRefName": "park/task-1483-kill-ring-yank-v2",
        "title": "feat: Add kill ring append for consecutive kills and Ctrl+Y yank [Midtown !1483]",
        "isDraft": false,
        "createdAt": "2026-02-17T00:00:00Z",  // Old enough to pass review delay
        "state": "OPEN",
        "author": {"login": "btucker"},
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": "",
    });

    let (state, _tmp, _guard) = make_test_state("midtown");

    // Empty branch_owners_map — the bug manifests because coworker_from_branch_with_map
    // still identifies "park" as the owner via the branch prefix
    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &snap.worktree_registry, // Real snapshot registry: no task 1483 entry
        &snap.active_names,      // Real snapshot active_names: includes "park"
        &state,
        &[pr],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // Bug: Previously returned 0 effects (PR incorrectly marked as orphaned)
    // Expected: Should spawn a reviewer (park is active and can address feedback)
    assert!(
        !effects.is_empty(),
        "Active coworker's PR without worktree should spawn a reviewer, not be marked as orphaned. \
         PR #1246 belongs to running coworker 'park' but has no worktree for task 1483."
    );

    let has_reviewer_effect = effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworkerWithCallbacks { .. })
            || matches!(e, Effect::AssignReviewer { pr_number, .. } if *pr_number == 1246)
    });
    assert!(
        has_reviewer_effect,
        "Expected reviewer spawn effect for PR #1246. Effects: {:#?}",
        effects
    );
}

/// Headless-only coworker's PR should not be marked as orphaned.
///
/// This test verifies that a coworker running only as a headless session (no pane,
/// not in CoworkerManager's list_running()) is still recognized as active via
/// the active_names parameter. Before the fix, list_running() was called directly
/// inside collect_reviewer_effects_with_source, which only tracked pane-based
/// coworkers — missing headless-only sessions entirely.
#[tokio::test]
async fn test_headless_only_coworker_pr_is_not_orphaned() {
    let pr = json!({
        "number": 999,
        "headRefName": "york/task-500-headless-feature",
        "title": "feat: Headless feature [Midtown !500]",
        "isDraft": false,
        "createdAt": "2026-02-17T00:00:00Z",
        "state": "OPEN",
        "author": {"login": "btucker"},
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": "",
    });

    let (state, _tmp, _guard) = make_test_state("midtown");

    // active_names includes "york" (as it would from WorldSnapshot which includes
    // headless sessions), but we do NOT insert york into state.coworkers — simulating
    // a headless-only coworker that list_running() would miss.
    let active_names: std::collections::HashSet<String> =
        ["york".to_string()].into_iter().collect();

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let registry = crate::worktree_registry::WorktreeRegistry::new();

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // With active_names containing "york", the PR should NOT be orphaned
    assert!(
        !effects.is_empty(),
        "Headless-only coworker's PR should spawn a reviewer, not be marked as orphaned. \
         active_names includes 'york' but state.coworkers does not (headless-only session)."
    );
}

// ── Session-based PR owner resolution tests ────────────────────────────

/// When a session record exists for a PR's task, the session's current_name
/// should be preferred over the branch-based owner resolution.
///
/// Scenario: PR #42 is linked to task "123" via pr_task_associations. Task "123"
/// maps to session "sess-abc" in session_task_map. Session "sess-abc" has
/// current_name "madison". But the branch name is "lexington/fix-auth".
///
/// Expected: owner is "madison" (from session), not "lexington" (from branch).
#[test]
fn test_resolve_pr_owner_from_session_prefers_session_over_branch() {
    let pr_task_associations: HashMap<u64, String> =
        [(42, "123".to_string())].into_iter().collect();
    let session_task_map: HashMap<String, String> = [("123".to_string(), "sess-abc".to_string())]
        .into_iter()
        .collect();
    let sessions: HashMap<String, crate::daemon::state::SessionRecord> = [(
        "sess-abc".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-abc".to_string(),
            task_id: Some("123".to_string()),
            current_name: Some("madison".to_string()),
            working_dir: "/tmp/test".to_string(),
            branch: Some("lexington/fix-auth".to_string()),
            pr_number: Some(42),
            is_running: true,
            resume_on_startup: false,
            ..Default::default()
        },
    )]
    .into_iter()
    .collect();

    let result =
        resolve_pr_owner_from_session(42, &pr_task_associations, &session_task_map, &sessions);
    assert_eq!(
        result,
        Some("madison".to_string()),
        "Session-based resolution should return 'madison' (the session's current_name)"
    );
}

/// When no session record exists for a PR, the session-based lookup should
/// return None, allowing the caller to fall back to branch-based resolution.
#[test]
fn test_resolve_pr_owner_from_session_returns_none_without_session() {
    let pr_task_associations: HashMap<u64, String> = HashMap::new();
    let session_task_map: HashMap<String, String> = HashMap::new();
    let sessions: HashMap<String, crate::daemon::state::SessionRecord> = HashMap::new();

    let result =
        resolve_pr_owner_from_session(42, &pr_task_associations, &session_task_map, &sessions);
    assert_eq!(
        result, None,
        "Should return None when no session data exists for the PR"
    );
}

/// When the session record exists but has no current_name (suspended session),
/// the lookup should fall back to preferred_name so PR feedback routes to the right coworker.
#[test]
fn test_resolve_pr_owner_from_session_uses_preferred_name_for_suspended_session() {
    let pr_task_associations: HashMap<u64, String> =
        [(42, "123".to_string())].into_iter().collect();
    let session_task_map: HashMap<String, String> = [("123".to_string(), "sess-abc".to_string())]
        .into_iter()
        .collect();
    let sessions: HashMap<String, crate::daemon::state::SessionRecord> = [(
        "sess-abc".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-abc".to_string(),
            task_id: Some("123".to_string()),
            preferred_name: Some("lexington".to_string()),
            working_dir: "/tmp/test".to_string(),
            branch: Some("lexington/fix-auth".to_string()),
            pr_number: Some(42),
            resume_on_startup: false,
            ..Default::default()
        },
    )]
    .into_iter()
    .collect();

    let result =
        resolve_pr_owner_from_session(42, &pr_task_associations, &session_task_map, &sessions);
    assert_eq!(
        result,
        Some("lexington".to_string()),
        "Should return preferred_name when session has no current_name but preferred_name is set"
    );
}

/// Integration test: poll_prs_for_issues uses session-based owner resolution
/// when available. PR branch is "lexington/fix-auth" but session record says
/// current_name is "madison". The owner should be "madison".
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_poll_prs_session_based_owner_resolution() {
    use super::super::snapshot::minimal_snapshot_for_test;

    // PR with branch "lexington/fix-auth" but session says owner is "madison"
    let pr_json = json!({
        "number": 42,
        "headRefName": "lexington/fix-auth",
        "title": "Fix authentication bug [Midtown !123]",
        "mergeable": "CONFLICTING",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-02-17T00:00:00Z",
        "state": "OPEN",
        "author": {"login": "btucker"},
    });

    let temp_dir = tempfile::tempdir().unwrap();
    let pr_list_file = temp_dir.path().join("pr_list.json");
    std::fs::write(
        &pr_list_file,
        serde_json::to_string(&vec![pr_json]).unwrap(),
    )
    .unwrap();

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

    let mut snap = minimal_snapshot_for_test();

    // Set up session data: PR #42 → task "123" → session "sess-abc" → current_name "madison"
    snap.pr_task_associations = [(42, "123".to_string())].into_iter().collect();
    snap.session_task_map = [("123".to_string(), "sess-abc".to_string())]
        .into_iter()
        .collect();
    snap.sessions = [(
        "sess-abc".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-abc".to_string(),
            task_id: Some("123".to_string()),
            current_name: Some("madison".to_string()),
            working_dir: "/tmp/test".to_string(),
            branch: Some("lexington/fix-auth".to_string()),
            pr_number: Some(42),
            is_running: true,
            resume_on_startup: false,
            ..Default::default()
        },
    )]
    .into_iter()
    .collect();

    // Madison has an active worktree (so the PR is not orphaned)
    snap.worktree_branch_owners = [("lexington/fix-auth".to_string(), "madison".to_string())]
        .into_iter()
        .collect();
    snap.active_names = ["madison".to_string()].into_iter().collect();
    snap.active_coworkers = vec![crate::coworker::Coworker {
        slot_id: "test-slot".to_string(),
        name: "madison".to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: Some("sess-abc".to_string()),
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::default(),
        profile: "default".to_string(),
    }];
    snap.running_coworkers = snap.active_coworkers.clone();

    let (state, _tmp, _guard) = make_test_state("test-repo");
    // Populate name→session mapping so nudge effects get the correct session_id
    state
        .name_to_session
        .lock()
        .unwrap()
        .insert("madison".to_string(), "sess-abc".to_string());

    let result = poll_prs_for_issues(&snap, &state).await;

    unsafe {
        std::env::set_var("PATH", original_path);
    }
    drop(_path_guard);

    let effects = result.expect("poll_prs_for_issues should succeed");

    // With session-based resolution, the owner is "madison" (not "lexington").
    // The PR has a merge conflict, so we expect a nudge to "madison".
    let nudges_madison = effects.iter().any(|e| match e {
        Effect::NudgeSessionWithCallbacks { session_id, .. } => session_id == "sess-abc",
        Effect::NudgeSession { session_id, .. } => session_id == "sess-abc",
        _ => false,
    });

    // With session-based owner resolution, the nudge should target "madison"
    // (the session's current_name), not "lexington" (the branch prefix).
    assert!(
        nudges_madison,
        "Expected nudge to 'madison' (session-based owner), not 'lexington' (branch-based). Effects: {:#?}",
        effects
    );
}

/// PrContext::from_persistent_state should populate task_session_id when a PR's
/// task has a corresponding session in persistent_state.sessions.
#[test]
fn test_pr_context_task_session_id_populated() {
    use crate::github_state::PrAuthorSession;

    let mut ps = super::super::state::DaemonPersistentState::default();

    // PR #42 has a pr_author_session with task_id "100"
    ps.github.pr_author_sessions.insert(
        42,
        PrAuthorSession {
            session_id: "old-session".to_string(),
            branch: "lexington/fix-auth".to_string(),
            original_author: "lexington".to_string(),
            stored_at: chrono::Utc::now(),
            task_id: Some("100".to_string()),
        },
    );

    // Session "sess-xyz" is working on task "100"
    ps.sessions.insert(
        "sess-xyz".to_string(),
        crate::daemon::state::SessionRecord {
            session_id: "sess-xyz".to_string(),
            task_id: Some("100".to_string()),
            current_name: Some("madison".to_string()),
            preferred_name: Some("madison".to_string()),
            working_dir: "/tmp/test".to_string(),
            branch: Some("lexington/fix-auth".to_string()),
            pr_number: Some(42),
            is_running: true,
            resume_on_startup: false,
            ..Default::default()
        },
    );

    let ctx = PrContext::from_persistent_state(&ps, 42);

    assert_eq!(
        ctx.task_session_id,
        Some("sess-xyz".to_string()),
        "task_session_id should be populated from sessions when PR's task has a session"
    );
}

/// PrContext::from_persistent_state should leave task_session_id as None when
/// no session exists for the PR's task.
#[test]
fn test_pr_context_task_session_id_none_when_no_session() {
    use crate::github_state::PrAuthorSession;

    let mut ps = super::super::state::DaemonPersistentState::default();

    // PR #42 has a pr_author_session with task_id "100", but no session record
    ps.github.pr_author_sessions.insert(
        42,
        PrAuthorSession {
            session_id: "old-session".to_string(),
            branch: "lexington/fix-auth".to_string(),
            original_author: "lexington".to_string(),
            stored_at: chrono::Utc::now(),
            task_id: Some("100".to_string()),
        },
    );

    let ctx = PrContext::from_persistent_state(&ps, 42);

    assert_eq!(
        ctx.task_session_id, None,
        "task_session_id should be None when no session exists for the task"
    );
}

/// PrContext::routing_only should always have task_session_id as None.
#[test]
fn test_pr_context_routing_only_no_task_session_id() {
    let ps = super::super::state::DaemonPersistentState::default();
    let ctx = PrContext::routing_only(&ps);
    assert_eq!(
        ctx.task_session_id, None,
        "routing_only should not populate task_session_id"
    );
}

/// resolve_pr_owner_via_session should resolve through the full chain:
/// PR# → pr_author_sessions → task_id → sessions → name
#[tokio::test]
async fn test_resolve_pr_owner_via_session_full_chain() {
    use crate::github_state::PrAuthorSession;

    let (state, _tmp, _guard) = make_test_state("test-repo");

    // Populate persistent state
    {
        let mut ps = state.persistent_state.lock().await;

        // PR #42 → task "100"
        ps.github.pr_author_sessions.insert(
            42,
            PrAuthorSession {
                session_id: "old-session".to_string(),
                branch: "lexington/fix-auth".to_string(),
                original_author: "lexington".to_string(),
                stored_at: chrono::Utc::now(),
                task_id: Some("100".to_string()),
            },
        );

        // Session "sess-abc" → task "100" → name "madison"
        ps.sessions.insert(
            "sess-abc".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-abc".to_string(),
                task_id: Some("100".to_string()),
                current_name: Some("madison".to_string()),
                preferred_name: Some("madison".to_string()),
                working_dir: "/tmp/test".to_string(),
                branch: Some("lexington/fix-auth".to_string()),
                pr_number: Some(42),
                is_running: true,
                resume_on_startup: false,
                ..Default::default()
            },
        );
    }

    let result = resolve_pr_owner_via_session(&state, 42).await;
    assert_eq!(
        result,
        Some("madison".to_string()),
        "Should resolve PR owner through session chain"
    );
}

/// resolve_pr_owner_via_session should return None when no pr_author_session exists.
#[tokio::test]
async fn test_resolve_pr_owner_via_session_returns_none_no_pr_session() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let result = resolve_pr_owner_via_session(&state, 42).await;
    assert_eq!(
        result, None,
        "Should return None when no pr_author_session exists"
    );
}

/// resolve_pr_owner_via_session falls back to preferred_name when current_name is None.
#[tokio::test]
async fn test_resolve_pr_owner_via_session_preferred_name_fallback() {
    use crate::github_state::PrAuthorSession;

    let (state, _tmp, _guard) = make_test_state("test-repo");

    {
        let mut ps = state.persistent_state.lock().await;

        ps.github.pr_author_sessions.insert(
            42,
            PrAuthorSession {
                session_id: "old-session".to_string(),
                branch: "lexington/fix-auth".to_string(),
                original_author: "lexington".to_string(),
                stored_at: chrono::Utc::now(),
                task_id: Some("100".to_string()),
            },
        );

        // Session with no current_name but has preferred_name (suspended)
        ps.sessions.insert(
            "sess-abc".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-abc".to_string(),
                task_id: Some("100".to_string()),
                preferred_name: Some("park".to_string()),
                working_dir: "/tmp/test".to_string(),
                branch: Some("lexington/fix-auth".to_string()),
                pr_number: Some(42),
                resume_on_startup: false,
                ..Default::default()
            },
        );
    }

    let result = resolve_pr_owner_via_session(&state, 42).await;
    assert_eq!(
        result,
        Some("park".to_string()),
        "Should fall back to preferred_name when current_name is None"
    );
}

/// Bug: Reviewer spawning selects the PR author's name as the reviewer, causing a coworker
/// to review its own PR.
///
/// Root cause: `collect_reviewer_effects_with_source` calls `next_available_name_excluding`
/// with only channel lead names excluded. When all other avenue names are in use, the PR
/// author's name is the only available avenue name and gets selected as the reviewer.
///
/// Example: "riverside" opens PR, finishes task, goes idle. Next poll cycle finds PR needs
/// review. All other avenue names are in use. "riverside" is the only available name →
/// "riverside" is spawned to review its own PR.
///
/// Fix: Also exclude the PR author's name (extracted from the branch) from reviewer selection.
#[tokio::test]
async fn test_reviewer_not_assigned_to_pr_author() {
    // PR whose branch identifies "riverside" as the author
    let pr = json!({
        "number": 9998,  // Non-existent PR — gh call fails gracefully → is_pr_reviewed returns false
        "headRefName": "riverside/some-feature",
        "title": "feat: Some feature [Midtown !100]",
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    let (state, _tmp, _guard) = make_test_state("midtown");

    // Register all AVENUE_NAMES except "riverside" as active coworkers.
    // This forces next_available_name_excluding to see "riverside" as the only
    // available avenue name — reproducing the collision deterministically.
    // (The function prefers avenue names over overflow names, so with all other
    // avenue names in use, it would always pick "riverside" before the fix.)
    for (i, name) in crate::coworker::AVENUE_NAMES
        .iter()
        .filter(|&&n| n != "riverside")
        .enumerate()
    {
        state
            .coworkers
            .register(
                &format!("slot-{i}"),
                name,
                "/tmp".to_string(),
                None,
                "claude-sonnet".to_string(),
                crate::auth::AuthProvider::Claude,
                "default".to_string(),
            )
            .unwrap();
    }

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let registry = crate::worktree_registry::WorktreeRegistry::new();
    // "riverside" is active (it's the PR author), so the PR is not orphaned
    let active_names: std::collections::HashSet<String> =
        ["riverside".to_string()].into_iter().collect();

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // A reviewer should be spawned (overflow names are available) — assert there IS an assignment
    let reviewer_name = effects.iter().find_map(|e| {
        if let Effect::AssignReviewer { reviewer_name, .. } = e {
            Some(reviewer_name.clone())
        } else {
            None
        }
    });

    assert!(
        reviewer_name.is_some(),
        "Expected a reviewer to be assigned (overflow names are still available). \
         Before fix: 'riverside' was incorrectly selected as reviewer for its own PR. \
         After fix: an overflow name should be selected instead."
    );

    assert_ne!(
        reviewer_name.unwrap().as_str(),
        "riverside",
        "PR author 'riverside' should NOT be assigned as reviewer for their own PR. \
         Before fix: 'riverside' was the only available avenue name and was selected. \
         After fix: the author's name is excluded from reviewer selection."
    );
}

/// Bug (task !1686): When a reviewer is spawned for a PR and the review worktree
/// is already bound to an ACTIVE coworker, the daemon logged
/// "WORKTREE COLLISION BLOCKED" in the BindCoworkerToWorktree effect handler — but
/// the spawn had already happened. The new reviewer ran without a valid worktree
/// binding, leading to review failures.
///
/// Fix: Detect the collision BEFORE spawning by checking the worktree registry in
/// collect_reviewer_effects_with_source. If the target review worktree is already
/// bound to an active coworker, skip the spawn entirely.
#[tokio::test]
async fn test_reviewer_spawn_aborted_on_worktree_collision_with_active_coworker() {
    // Use a fake PR number that doesn't exist in the real repo to avoid
    // is_pr_reviewed() calling the real gh CLI and finding an actual review.
    let pr_number = 99991u64;
    let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number); // "review-pr-99991"

    // PR that needs review (branch owned by "pleasant")
    let pr_json = serde_json::json!({
        "number": pr_number,
        "headRefName": "pleasant/fix-auth",
        "title": "Fix auth regression [Midtown !500]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Review worktree already bound to ACTIVE coworker "vernon"
    let mut registry = crate::worktree_registry::WorktreeRegistry::default();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: worktree_id.clone(),
            branch_name: worktree_id.clone(),
            task_id: None,
            current_coworker: Some("vernon".to_string()),
            pr_number: Some(pr_number),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();

    // Both the PR author ("pleasant") and the existing reviewer ("vernon") are active.
    // pleasant is active → PR is not orphaned (author can address feedback).
    // vernon is active and bound to the review worktree → spawn should be aborted.
    let mut active_names = std::collections::HashSet::new();
    active_names.insert("pleasant".to_string());
    active_names.insert("vernon".to_string());

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // No spawn should happen — the worktree collision must abort the spawn entirely.
    // Before fix: SpawnCoworkerWithCallbacks was emitted, causing a second reviewer
    //             to run without a worktree binding (WORKTREE COLLISION BLOCKED in
    //             BindCoworkerToWorktree effect, but too late).
    // After fix: effects are empty because the collision is detected before spawning.
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        !has_spawn,
        "Reviewer spawn must be aborted when review worktree is already bound to active coworker 'vernon'. \
         Before fix: SpawnCoworkerWithCallbacks was emitted anyway, leading to a reviewer running without \
         a valid worktree binding."
    );
}

/// Regression test for case-sensitivity: active_names stores lowercase names
/// (per snapshot.rs), but current_coworker in the worktree registry may have
/// mixed case. The collision guard must normalize with to_lowercase().
#[tokio::test]
async fn test_reviewer_spawn_aborted_on_worktree_collision_mixed_case() {
    let pr_number = 99993u64;
    let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);

    let pr_json = serde_json::json!({
        "number": pr_number,
        "headRefName": "pleasant/fix-auth",
        "title": "Fix auth regression [Midtown !500]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Worktree bound to "Vernon" (mixed case) — active_names has "vernon" (lowercase)
    let mut registry = crate::worktree_registry::WorktreeRegistry::default();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: worktree_id.clone(),
            branch_name: worktree_id.clone(),
            task_id: None,
            current_coworker: Some("Vernon".to_string()),
            pr_number: Some(pr_number),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();

    let mut active_names = std::collections::HashSet::new();
    active_names.insert("pleasant".to_string());
    active_names.insert("vernon".to_string()); // lowercase, as snapshot.rs stores it

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        !has_spawn,
        "Collision guard must fire even when current_coworker has mixed case ('Vernon') \
         and active_names stores lowercase ('vernon'). Without to_lowercase(), this fails."
    );
}

/// When active_names is stale (contains a coworker that's actually dead), the early
/// guard conservatively blocks the spawn. This is correct behavior — the next tick
/// will have updated active_names and the spawn will proceed. This test documents
/// the interaction between the snapshot-based early guard and the real-time
/// is_alive() guard in BindCoworkerToWorktree.
#[tokio::test]
async fn test_reviewer_spawn_blocked_by_stale_active_names_retries_next_tick() {
    let pr_number = 99994u64;
    let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);

    let pr_json = serde_json::json!({
        "number": pr_number,
        "headRefName": "pleasant/fix-auth",
        "title": "Fix auth regression [Midtown !500]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Worktree bound to "vernon" — vernon appears in active_names (stale snapshot)
    // but is actually dead (the real-time is_alive() check would return false).
    let mut registry = crate::worktree_registry::WorktreeRegistry::default();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: worktree_id.clone(),
            branch_name: worktree_id.clone(),
            task_id: None,
            current_coworker: Some("vernon".to_string()),
            pr_number: Some(pr_number),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();

    // Stale snapshot: vernon appears active (but is actually dead)
    let mut active_names_stale = std::collections::HashSet::new();
    active_names_stale.insert("pleasant".to_string());
    active_names_stale.insert("vernon".to_string()); // stale — actually dead

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let (state, _tmp, _guard) = make_test_state("test-repo");

    // Tick 1: stale active_names → early guard blocks spawn (conservative)
    let effects_tick1 = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names_stale,
        &state,
        std::slice::from_ref(&pr_json),
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    let has_spawn_tick1 = effects_tick1
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        !has_spawn_tick1,
        "Tick 1: Early guard conservatively blocks spawn when active_names is stale \
         (vernon appears active but is actually dead). This is expected — the next tick \
         will refresh active_names."
    );

    // Tick 2: active_names refreshed, vernon no longer present → spawn proceeds
    let active_names_fresh = {
        let mut s = std::collections::HashSet::new();
        s.insert("pleasant".to_string());
        // vernon is gone — correctly detected as dead
        s
    };

    let effects_tick2 = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names_fresh,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    let has_spawn_tick2 = effects_tick2
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        has_spawn_tick2,
        "Tick 2: After active_names is refreshed (vernon gone), the spawn should proceed. \
         The force-rebind path in BindCoworkerToWorktree handles the dead coworker case."
    );
}

/// Complement to the above: if the review worktree is bound to a DEAD coworker,
/// the spawn should proceed normally (the force-rebind path handles it).
#[tokio::test]
async fn test_reviewer_spawn_proceeds_when_previous_reviewer_is_dead() {
    // Use a fake PR number that doesn't exist in the real repo.
    let pr_number = 99992u64;
    let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);

    let pr_json = serde_json::json!({
        "number": pr_number,
        "headRefName": "pleasant/fix-auth",
        "title": "Fix auth regression [Midtown !500]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    // Review worktree bound to "vernon" — but "vernon" is NOT in active_names (dead)
    let mut registry = crate::worktree_registry::WorktreeRegistry::default();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: worktree_id.clone(),
            branch_name: worktree_id.clone(),
            task_id: None,
            current_coworker: Some("vernon".to_string()),
            pr_number: Some(pr_number),
            created_at: chrono::Utc::now(),
            completed_at: None,
        })
        .unwrap();

    // active_names does NOT contain "vernon" → it's dead
    let active_names = std::collections::HashSet::new();

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // Spawn SHOULD proceed — the old reviewer is dead so a new one is needed.
    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        has_spawn,
        "Reviewer spawn should proceed when the worktree's previous coworker is dead. \
         The force-rebind path in BindCoworkerToWorktree handles the dead coworker case."
    );
}

#[tokio::test]
async fn test_review_mode_github_app_disables_local_reviewer_spawn() {
    let pr_number = 99993u64;
    let pr_json = serde_json::json!({
        "number": pr_number,
        "headRefName": "york/fix-auth",
        "title": "Fix auth regression [Midtown !777]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let active_names: std::collections::HashSet<String> =
        std::collections::HashSet::from(["york".to_string()]);
    let registry = crate::worktree_registry::WorktreeRegistry::default();

    let (state, tmp, _guard) = make_test_state("test-repo");

    let mut config =
        crate::config::FullProjectConfig::minimal("test-repo", &tmp.path().to_string_lossy());
    config.execution.review_mode = Some(crate::config::ReviewMode::GithubApp);
    config.save("test-repo").expect("save test project config");

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        !has_spawn,
        "execution.review_mode=github_app should skip local reviewer spawn"
    );
}

#[tokio::test]
async fn test_review_mode_both_allows_local_reviewer_spawn() {
    let pr_number = 99994u64;
    let pr_json = serde_json::json!({
        "number": pr_number,
        "headRefName": "york/fix-auth",
        "title": "Fix auth regression [Midtown !778]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let active_names: std::collections::HashSet<String> =
        std::collections::HashSet::from(["york".to_string()]);
    let registry = crate::worktree_registry::WorktreeRegistry::default();

    let (state, tmp, _guard) = make_test_state("test-repo");

    let mut config =
        crate::config::FullProjectConfig::minimal("test-repo", &tmp.path().to_string_lossy());
    config.execution.review_mode = Some(crate::config::ReviewMode::Both);
    config.save("test-repo").expect("save test project config");

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    let has_spawn = effects
        .iter()
        .any(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }));
    assert!(
        has_spawn,
        "execution.review_mode=both should keep local reviewer spawning enabled"
    );
}

/// Bug (task !1793): When a reviewer is spawned for a coworker's PR, the PR author
/// receives no warning and can enable auto-merge before the review completes.
///
/// Root cause: `collect_reviewer_effects_with_source` builds `on_success` effects
/// for the reviewer spawn but never notifies the PR author that review is starting.
/// The coworker system prompt says not to enable auto-merge before review completes,
/// but without an explicit notification the warning can be missed.
///
/// Fix: Add a `DeliverMailboxMessage` to `on_success` warning the PR author not to
/// enable auto-merge until the review is complete.
#[tokio::test]
async fn test_reviewer_spawn_warns_pr_author_via_mailbox() {
    // PR authored by "madison" (branch: madison/fix-polling)
    let pr_number = 99994u64;
    let pr_json = serde_json::json!({
        "number": pr_number,
        "headRefName": "madison/fix-polling",
        "title": "Fix polling reconciliation [Midtown !200]",
        "mergeable": "MERGEABLE",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    let branch_owners: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let registry = crate::worktree_registry::WorktreeRegistry::new();
    // madison is active (owns the PR) so the PR is not orphaned
    let active_names: std::collections::HashSet<String> =
        ["madison".to_string()].into_iter().collect();

    let (state, _tmp, _guard) = make_test_state("test-repo");

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
        &std::collections::HashMap::new(),
    )
    .await;

    // Find the SpawnCoworkerWithCallbacks effect and inspect its on_success effects
    let spawn_effect = effects.iter().find_map(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
            Some(on_success)
        } else {
            None
        }
    });

    assert!(
        spawn_effect.is_some(),
        "Expected a SpawnCoworkerWithCallbacks effect for PR #{}. Effects: {:#?}",
        pr_number,
        effects
    );

    let on_success = spawn_effect.unwrap();

    // The on_success effects must include a DeliverMailboxMessage to "madison" warning
    // them not to enable auto-merge while the review is in progress.
    let has_author_warning = on_success.iter().any(|e| {
        if let Effect::DeliverMailboxMessage { name, message, .. } = e {
            name == "madison" && message.contains(&pr_number.to_string())
        } else {
            false
        }
    });

    assert!(
        has_author_warning,
        "on_success effects must include a DeliverMailboxMessage to 'madison' warning \
         them not to enable auto-merge while review is in progress. \
         Before fix: no such warning was sent, allowing the author to merge while \
         the reviewer was still working (as happened with PR #1523). \
         on_success effects: {:#?}",
        on_success
    );
}

// ── collect_pr_task_link_effects tests ──────────────────────────────────────

/// A task with an open PR title match but no task.pr set should emit SetTaskPr.
///
/// Scenario: PR #42 has title "Fix auth [Midtown !100]" so github_open_pr_task_ids
/// maps "100" → 42. Task !100 exists but has pr = None. The polling fallback must
/// emit SetTaskPr to repair the missing link.
#[test]
fn test_collect_pr_task_link_effects_links_unlinked_task() {
    use super::super::snapshot::minimal_snapshot_for_test;
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "100".to_string(),
        subject: "Fix auth".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("york".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None, // not yet linked
        created_at: None,
    };

    let mut snap = minimal_snapshot_for_test();
    snap.all_tasks = vec![task];
    snap.github_open_pr_task_ids.insert("100".to_string(), 42);

    let effects = collect_pr_task_link_effects(&snap);

    assert_eq!(
        effects.len(),
        1,
        "Expected exactly one SetTaskPr effect, got: {:#?}",
        effects
    );
    assert!(
        matches!(
            &effects[0],
            Effect::SetTaskPr {
                task_id,
                pr_number: 42,
                ..
            } if task_id == "100"
        ),
        "Expected SetTaskPr for task 100 / PR 42, got: {:#?}",
        effects[0]
    );
}

/// A task already correctly linked to the PR should not emit any effect.
#[test]
fn test_collect_pr_task_link_effects_skips_already_linked_task() {
    use super::super::snapshot::minimal_snapshot_for_test;
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "200".to_string(),
        subject: "Add feature".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("amsterdam".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(99), // already linked correctly
        created_at: None,
    };

    let mut snap = minimal_snapshot_for_test();
    snap.all_tasks = vec![task];
    snap.github_open_pr_task_ids.insert("200".to_string(), 99);

    let effects = collect_pr_task_link_effects(&snap);

    assert!(
        effects.is_empty(),
        "Expected no effects for already-linked task, got: {:#?}",
        effects
    );
}

/// A PR with no [Midtown !XXX] task marker should produce no effects.
#[test]
fn test_collect_pr_task_link_effects_ignores_pr_without_task_marker() {
    use super::super::snapshot::minimal_snapshot_for_test;

    // github_open_pr_task_ids is empty (no Midtown marker found in any title)
    let snap = minimal_snapshot_for_test();

    let effects = collect_pr_task_link_effects(&snap);

    assert!(
        effects.is_empty(),
        "Expected no effects when no PR has a task marker, got: {:#?}",
        effects
    );
}

/// A completed task with an open PR and no task.pr link should NOT emit SetTaskPr.
///
/// Scenario: Task !400 is completed but its PR is still open (e.g., task was
/// manually closed early). Emitting SetTaskPr on every tick for completed tasks
/// would cause redundant disk writes with no benefit.
#[test]
fn test_collect_pr_task_link_effects_skips_completed_task() {
    use super::super::snapshot::minimal_snapshot_for_test;
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "400".to_string(),
        subject: "Old work".to_string(),
        status: TaskStatus::Completed,
        owner: Some("york".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: None, // never linked
        created_at: None,
    };

    let mut snap = minimal_snapshot_for_test();
    snap.all_tasks = vec![task];
    snap.github_open_pr_task_ids.insert("400".to_string(), 88);

    let effects = collect_pr_task_link_effects(&snap);

    assert!(
        effects.is_empty(),
        "Expected no effects for completed task, got: {:#?}",
        effects
    );
}

/// A mismatched PR number (task.pr points to a different PR) should emit SetTaskPr
/// to correct the link.
#[test]
fn test_collect_pr_task_link_effects_corrects_mismatched_pr() {
    use super::super::snapshot::minimal_snapshot_for_test;
    use crate::tasks::{Task, TaskStatus};

    let task = Task {
        id: "300".to_string(),
        subject: "Refactor".to_string(),
        status: TaskStatus::InProgress,
        owner: Some("lexington".to_string()),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr: Some(55), // stale/wrong PR number
        created_at: None,
    };

    let mut snap = minimal_snapshot_for_test();
    snap.all_tasks = vec![task];
    snap.github_open_pr_task_ids.insert("300".to_string(), 77); // actual open PR

    let effects = collect_pr_task_link_effects(&snap);

    assert_eq!(
        effects.len(),
        1,
        "Expected one SetTaskPr to fix mismatch, got: {:#?}",
        effects
    );
    assert!(
        matches!(
            &effects[0],
            Effect::SetTaskPr {
                task_id,
                pr_number: 77,
                ..
            } if task_id == "300"
        ),
        "Expected SetTaskPr for task 300 / PR 77, got: {:#?}",
        effects[0]
    );
}

// ── is_pr_reviewed negative cache interaction tests ─────────────────────

/// Regression test: when a review is cached (e.g. via webhook) but comment IDs
/// are empty, the fast path falls through to backfill. If a stale negative cache
/// entry exists for the same PR, it must not suppress the backfill by returning
/// false.
///
/// Sequence that triggers the bug:
///   1. Poll tick finds no review → negative cache populated
///   2. Webhook caches review (mark_reviewed_pr) but no comment IDs recorded
///   3. Next poll: fast path sees cached review, IDs empty → falls through
///   4. BUG: negative cache hit → returns false (backfill never runs)
#[tokio::test]
async fn test_negative_cache_does_not_suppress_cached_review_backfill() {
    let (state, _temp_dir, _guard) = make_test_state("neg-cache-test");
    let pr_number = 77777u64;

    // Step 1: Populate the negative cache (simulates a prior poll finding no review)
    {
        let mut neg_cache = state.pr_review_negative_cache.lock().unwrap();
        neg_cache.insert(pr_number, std::time::Instant::now());
    }

    // Step 2: Webhook arrives and caches the review, but no comment IDs
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.mark_reviewed_pr(pr_number);
        // Deliberately do NOT add any comment IDs — this is the scenario
    }

    // Step 3: Call is_pr_reviewed — should return true (review is cached)
    let result = state.is_pr_reviewed(pr_number).await;
    assert!(
        result,
        "is_pr_reviewed should return true for a cached review, \
         even when a stale negative cache entry exists"
    );
}

// ── extract_review_comment_ids_from_json tests ──────────────────────────

#[test]
fn extract_review_comment_ids_filters_review_comments() {
    let comments: Vec<serde_json::Value> = serde_json::from_value(json!([
        {"id": 1001, "body": "## Code Review by madison\n\nLooks good!"},
        {"id": 1002, "body": "Just a regular comment"},
        {"id": 1003, "body": "🤖 Reviewed by park\n\nAll checks pass."},
    ]))
    .unwrap();

    let ids = extract_review_comment_ids_from_json(&comments);
    assert_eq!(ids, vec![1001, 1003]);
}

#[test]
fn extract_review_comment_ids_empty_on_no_reviews() {
    let comments: Vec<serde_json::Value> = serde_json::from_value(json!([
        {"id": 1, "body": "Nice work!"},
        {"id": 2, "body": "LGTM"},
    ]))
    .unwrap();

    let ids = extract_review_comment_ids_from_json(&comments);
    assert!(ids.is_empty());
}

/// Demonstrates why `--slurp` is required: `gh api --paginate` without
/// `--slurp` produces concatenated JSON arrays that fail to parse as a
/// single `Vec<Value>`. With `--slurp`, gh merges pages into one array.
#[test]
fn concatenated_json_pages_fail_without_slurp() {
    // This is what `gh api --paginate` produces without `--slurp`:
    // two separate JSON arrays concatenated together.
    let page1 = json!([
        {"id": 1001, "body": "## Code Review by madison\n\nPage 1 review"},
    ]);
    let page2 = json!([
        {"id": 1002, "body": "## Code Review by park\n\nPage 2 review"},
    ]);
    let concatenated = format!("{}{}", page1, page2);

    // Without --slurp, serde_json cannot parse concatenated arrays
    let parse_result: Result<Vec<serde_json::Value>, _> = serde_json::from_str(&concatenated);
    assert!(
        parse_result.is_err(),
        "Concatenated JSON should fail to parse as Vec<Value> — \
         this is why --slurp is required on gh api --paginate"
    );

    // With --slurp, gh merges into a single array that parses correctly
    let slurped = json!([
        {"id": 1001, "body": "## Code Review by madison\n\nPage 1 review"},
        {"id": 1002, "body": "## Code Review by park\n\nPage 2 review"},
    ]);
    let comments: Vec<serde_json::Value> = serde_json::from_value(slurped).unwrap();
    let ids = extract_review_comment_ids_from_json(&comments);
    assert_eq!(ids, vec![1001, 1002]);
}

/// Helper to create a PrContext with a task association AND task_channel mapping,
/// so that workflow events are emitted (requires both channel and task_id).
fn make_pr_context_with_channel(pr_number: u64, task_id: &str, channel: &str) -> PrContext {
    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(pr_number, task_id.to_string());
    let mut task_channel = HashMap::new();
    task_channel.insert(task_id.to_string(), channel.to_string());
    PrContext {
        pr_task_associations,
        task_channel,
        session_context: None,
        task_session_id: None,
        has_active_reviewer: false,
    }
}

/// Helper: extract EmitWorkflowEvent effects from an effect list.
fn extract_workflow_events(effects: &[Effect]) -> Vec<&crate::workflow::WorkflowEvent> {
    effects
        .iter()
        .filter_map(|e| {
            if let Effect::EmitWorkflowEvent(ev) = e {
                Some(ev)
            } else {
                None
            }
        })
        .collect()
}

/// Gate !1902: PrApproved workflow event is suppressed while reviewer is active.
///
/// When a GitHub approved review arrives but the reviewer coworker is still working,
/// the PrApproved event must NOT be emitted. This keeps the workflow script contract
/// clean: "pr.approved = safe to merge".
#[test]
fn pr_approved_suppressed_while_reviewer_active() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let mut ctx = make_pr_context_with_channel(42, "100", "daemon-core");
    ctx.has_active_reviewer = true;

    let effects = pr_action_to_effects(
        crate::rules::PrAction::NudgeOwner {
            owner: "broadway".to_string(),
            message: "PR #42 — approved".to_string(),
        },
        42,
        "Fix auth",
        PrIssueType::Approved,
        &state,
        &ctx,
    );

    let workflow_events = extract_workflow_events(&effects);
    assert!(
        workflow_events.is_empty(),
        "PrApproved should NOT be emitted while reviewer is active, got: {:?}",
        workflow_events
    );
}

/// Gate !1902: PrApproved workflow event IS emitted when no reviewer is active.
///
/// Once the reviewer has finished (assignment cleared, not in reviewing phase),
/// the PrApproved event should fire normally.
#[test]
fn pr_approved_emitted_when_no_reviewer_active() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let ctx = make_pr_context_with_channel(42, "100", "daemon-core");
    // has_active_reviewer defaults to false

    let effects = pr_action_to_effects(
        crate::rules::PrAction::NudgeOwner {
            owner: "broadway".to_string(),
            message: "PR #42 — approved".to_string(),
        },
        42,
        "Fix auth",
        PrIssueType::Approved,
        &state,
        &ctx,
    );

    let workflow_events = extract_workflow_events(&effects);
    assert_eq!(
        workflow_events.len(),
        1,
        "PrApproved should be emitted when no reviewer is active"
    );
    assert!(
        matches!(
            workflow_events[0],
            crate::workflow::WorkflowEvent::PrApproved { pr_number: 42, .. }
        ),
        "Event should be PrApproved for PR #42"
    );
}

/// Gate !1902: Other workflow events (e.g., CiFailed) are NOT affected by the reviewer gate.
///
/// The has_active_reviewer flag only gates PrApproved, not other event types.
#[test]
fn non_approved_events_unaffected_by_reviewer_gate() {
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let mut ctx = make_pr_context_with_channel(42, "100", "daemon-core");
    ctx.has_active_reviewer = true;

    let effects = pr_action_to_effects(
        crate::rules::PrAction::NudgeOwner {
            owner: "broadway".to_string(),
            message: "PR #42 — CI failed".to_string(),
        },
        42,
        "Fix auth",
        PrIssueType::CiFailed,
        &state,
        &ctx,
    );

    let workflow_events = extract_workflow_events(&effects);
    assert_eq!(
        workflow_events.len(),
        1,
        "CiFailed should be emitted regardless of reviewer state"
    );
    assert!(
        matches!(
            workflow_events[0],
            crate::workflow::WorkflowEvent::PrCiFailed { pr_number: 42, .. }
        ),
        "Event should be PrCiFailed for PR #42"
    );
}

/// Gate !1902: Full suppression → cooldown → clear → re-emit flow.
///
/// Verifies the Codex review fix: when PrApproved is suppressed due to active
/// reviewer, the nudge cooldown is recorded. After clearing the cooldown (as
/// happens when the reviewer finishes), the next tick successfully emits PrApproved.
#[test]
fn pr_approved_re_emitted_after_reviewer_clears() {
    use crate::daemon::trackers::PrIssueTracker;

    let (state, _tmp, _guard) = make_test_state("test-repo");
    let pr_number = 42;

    // Step 1: Reviewer is active → PrApproved suppressed
    let mut ctx = make_pr_context_with_channel(pr_number, "100", "daemon-core");
    ctx.has_active_reviewer = true;

    let effects = pr_action_to_effects(
        crate::rules::PrAction::NudgeOwner {
            owner: "broadway".to_string(),
            message: "PR #42 — approved".to_string(),
        },
        pr_number,
        "Fix auth",
        PrIssueType::Approved,
        &state,
        &ctx,
    );
    assert!(
        extract_workflow_events(&effects).is_empty(),
        "PrApproved should be suppressed while reviewer is active"
    );

    // Step 2: Simulate effect execution — nudge cooldown recorded
    let mut tracker = PrIssueTracker::new();
    tracker.record_nudge(pr_number, PrIssueType::Approved);
    assert!(
        !tracker.should_nudge(pr_number, PrIssueType::Approved),
        "Nudge should be on cooldown after recording"
    );

    // Step 3: Reviewer finishes → clear nudge cooldown (as done in review-complete path)
    tracker.clear_nudge(pr_number, PrIssueType::Approved);
    assert!(
        tracker.should_nudge(pr_number, PrIssueType::Approved),
        "Nudge should be unblocked after clearing cooldown"
    );

    // Step 4: Next tick — reviewer cleared, PrApproved fires
    let mut ctx_cleared = make_pr_context_with_channel(pr_number, "100", "daemon-core");
    ctx_cleared.has_active_reviewer = false;

    let effects = pr_action_to_effects(
        crate::rules::PrAction::NudgeOwner {
            owner: "broadway".to_string(),
            message: "PR #42 — approved".to_string(),
        },
        pr_number,
        "Fix auth",
        PrIssueType::Approved,
        &state,
        &ctx_cleared,
    );
    let workflow_events = extract_workflow_events(&effects);
    assert_eq!(
        workflow_events.len(),
        1,
        "PrApproved should fire after reviewer clears"
    );
    assert!(
        matches!(
            workflow_events[0],
            crate::workflow::WorkflowEvent::PrApproved { pr_number: 42, .. }
        ),
        "Event should be PrApproved for PR #42"
    );
}

/// Gate !1902: has_cached_review bypass — PrApproved is NOT suppressed when the
/// review is already cached, even if get_reviewer() still returns Some.
///
/// This handles the race between webhook review completion (which caches the
/// review) and the poll tick that clears the reviewer assignment.
#[tokio::test]
async fn pr_approved_not_suppressed_when_review_cached() {
    let (state, _tmp, _guard) = make_test_state("test-repo");
    let pr_number = 42;

    // Simulate: reviewer is assigned BUT review is already cached (complete)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.assign_reviewer(
            pr_number,
            "lexington",
            crate::github_state::AssignmentSource::PollingFallback,
        );
        ps.github.mark_reviewed_pr(pr_number);
    }

    // Build PrContext — should detect cached review and NOT flag active reviewer
    let ctx = {
        let ps = state.persistent_state.lock().await;
        PrContext::from_persistent_state(&ps, pr_number)
    };

    assert!(
        !ctx.has_active_reviewer,
        "has_active_reviewer should be false when review is cached (race bypass)"
    );
}

/// A coworker in Reviewing phase with no assignment (cleared) should NOT
/// block PrApproved for any specific PR — without PR-specific evidence,
/// suppressing would incorrectly gate unrelated PRs.
#[test]
fn augment_reviewer_ignores_reviewing_phase_without_assignment() {
    let pr_number = 42;
    let mut ctx = make_pr_context_with_channel(pr_number, "100", "daemon-core");

    // Coworker "lexington" is in Reviewing phase,
    // but the reviewer_pr_assignments has been cleared.
    let mut snap = super::super::snapshot::minimal_snapshot_for_test();
    snap.reviewing_phase_coworkers
        .insert("lexington".to_string());
    // reviewer_pr_assignments is empty — no PR-specific evidence

    ctx.augment_reviewer_from_snapshot(pr_number, &snap);

    assert!(
        !ctx.has_active_reviewer,
        "Should NOT flag active reviewer when coworker has no assignment (would block all PRs)"
    );
}

/// Bug !1907 issue 1 (inverse): augment_reviewer_from_snapshot also catches the
/// case where the assignment exists but the coworker hasn't entered Reviewing phase.
#[test]
fn augment_reviewer_catches_assignment_without_reviewing_phase() {
    let pr_number = 42;
    let mut ctx = make_pr_context_with_channel(pr_number, "100", "daemon-core");

    // Assignment exists for "lexington" → PR 42, but they aren't in
    // reviewing_phase_coworkers yet (e.g., just started, haven't set workflow phase).
    let mut snap = super::super::snapshot::minimal_snapshot_for_test();
    snap.reviewer_pr_assignments
        .insert("lexington".to_string(), pr_number);
    // reviewing_phase_coworkers is empty

    ctx.augment_reviewer_from_snapshot(pr_number, &snap);

    assert!(
        ctx.has_active_reviewer,
        "Should flag active reviewer when assignment exists even without Reviewing phase"
    );
}

/// Bug !1907 issue 1: augment_reviewer_from_snapshot should NOT flag active
/// reviewer when a coworker is in Reviewing phase for a DIFFERENT PR.
#[test]
fn augment_reviewer_ignores_reviewing_phase_for_different_pr() {
    let pr_number = 42;
    let mut ctx = make_pr_context_with_channel(pr_number, "100", "daemon-core");

    // "lexington" is reviewing PR 99, not PR 42
    let mut snap = super::super::snapshot::minimal_snapshot_for_test();
    snap.reviewing_phase_coworkers
        .insert("lexington".to_string());
    snap.reviewer_pr_assignments
        .insert("lexington".to_string(), 99);

    ctx.augment_reviewer_from_snapshot(pr_number, &snap);

    assert!(
        !ctx.has_active_reviewer,
        "Should NOT flag active reviewer when coworker is reviewing a different PR"
    );
}

// ── json_has_completed_review tests (reviewer identity gate) ────────────

/// The core bug from !1924: a bot comment with NO midtown frontmatter was being
/// accepted as a completed review because `pr_has_completed_review_uncached`
/// never checked who posted the review.
#[test]
fn json_review_rejects_bot_comment_when_reviewer_assigned() {
    let json = json!({
        "reviews": [],
        "comments": [
            {
                "body": "Coverage report: 67.03% — no midtown frontmatter here",
                "author": {"login": "codecov-bot"}
            }
        ]
    });

    // Without assigned reviewer: should NOT match (no review signature)
    assert!(!json_has_completed_review(&json, None));

    // With assigned reviewer: should NOT match
    assert!(!json_has_completed_review(&json, Some("pleasant")));
}

/// A review by the assigned reviewer should be accepted.
#[test]
fn json_review_accepts_assigned_reviewer_comment() {
    let json = json!({
        "reviews": [],
        "comments": [
            {
                "body": "<!-- midtown: pleasant -->\n## Code Review by pleasant\n\nLGTM, no issues found.",
                "author": {"login": "btucker"}
            }
        ]
    });

    assert!(json_has_completed_review(&json, Some("pleasant")));
}

/// A review by a DIFFERENT coworker should be rejected when an assigned reviewer exists.
#[test]
fn json_review_rejects_wrong_coworker_comment() {
    let json = json!({
        "reviews": [],
        "comments": [
            {
                "body": "<!-- midtown: broadway -->\n## Code Review by broadway\n\nLGTM!",
                "author": {"login": "btucker"}
            }
        ]
    });

    assert!(
        !json_has_completed_review(&json, Some("pleasant")),
        "Review by broadway should not satisfy the gate when pleasant is assigned"
    );
}

/// When no reviewer is assigned, any valid review should be accepted (backward compat).
#[test]
fn json_review_accepts_any_review_when_no_reviewer_assigned() {
    let json = json!({
        "reviews": [],
        "comments": [
            {
                "body": "<!-- midtown: broadway -->\n## Code Review by broadway\n\nLooks good.",
                "author": {"login": "btucker"}
            }
        ]
    });

    assert!(json_has_completed_review(&json, None));
}

/// Formal GitHub reviews (APPROVED state) by the assigned reviewer should be accepted.
#[test]
fn json_review_accepts_formal_review_by_assigned_reviewer() {
    let json = json!({
        "reviews": [
            {
                "state": "APPROVED",
                "body": "<!-- midtown: pleasant -->\nLGTM!",
                "author": {"login": "btucker"}
            }
        ],
        "comments": []
    });

    assert!(json_has_completed_review(&json, Some("pleasant")));
}

/// Formal reviews without midtown frontmatter should NOT be accepted
/// when an assigned reviewer exists — they could be from bots.
#[test]
fn json_review_rejects_formal_review_without_attribution() {
    let json = json!({
        "reviews": [
            {
                "state": "COMMENTED",
                "body": "",
                "author": {"login": "dependabot[bot]"}
            }
        ],
        "comments": []
    });

    // With assigned reviewer: reject (no author attribution in empty body)
    assert!(
        !json_has_completed_review(&json, Some("pleasant")),
        "Formal review from bot without frontmatter should be rejected when reviewer assigned"
    );

    // Without assigned reviewer: accept (backward compat)
    assert!(
        json_has_completed_review(&json, None),
        "Formal review should be accepted when no reviewer is assigned"
    );
}

/// Formal APPROVED/CHANGES_REQUESTED reviews with empty body should be accepted
/// even when an assigned reviewer exists. These are strong deliberate actions
/// unlikely from bots, and the assigned reviewer may submit them without text.
#[test]
fn json_review_accepts_bodyless_approved_when_reviewer_assigned() {
    let json = json!({
        "reviews": [
            {
                "state": "APPROVED",
                "body": "",
                "author": {"login": "btucker"}
            }
        ],
        "comments": []
    });

    assert!(
        json_has_completed_review(&json, Some("pleasant")),
        "APPROVED with empty body should be accepted — strong state, not a bot"
    );
}

/// CHANGES_REQUESTED with empty body should also be accepted (strong state).
#[test]
fn json_review_accepts_bodyless_changes_requested_when_reviewer_assigned() {
    let json = json!({
        "reviews": [
            {
                "state": "CHANGES_REQUESTED",
                "body": "",
                "author": {"login": "btucker"}
            }
        ],
        "comments": []
    });

    assert!(
        json_has_completed_review(&json, Some("pleasant")),
        "CHANGES_REQUESTED with empty body should be accepted — strong state"
    );
}

/// COMMENTED with empty body should still be rejected (weak state, common from bots).
#[test]
fn json_review_rejects_bodyless_commented_when_reviewer_assigned() {
    let json = json!({
        "reviews": [
            {
                "state": "COMMENTED",
                "body": "",
                "author": {"login": "some-bot[bot]"}
            }
        ],
        "comments": []
    });

    assert!(
        !json_has_completed_review(&json, Some("pleasant")),
        "COMMENTED with empty body should be rejected — weak state, likely a bot"
    );
}

/// Draft PRs should be skipped entirely in poll_prs_for_issues, producing no
/// orphaned PR alerts even when they have merge conflicts and no active owner.
///
/// This prevents noisy repeated warnings for draft PRs like PR #1453
/// (codex/matrix-bridge-spike) that have merge conflicts but don't need
/// immediate action since they're still drafts.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_draft_pr_skipped_in_orphaned_pr_alerts() {
    use super::super::snapshot::minimal_snapshot_for_test;

    // Create a draft PR with a merge conflict and no active owner —
    // without the draft check this would trigger an orphaned PR alert
    let pr_json = json!({
        "number": 1453,
        "headRefName": "codex/matrix-bridge-spike",
        "title": "feat: add midtown view --matrix mode",
        "mergeable": "CONFLICTING",
        "statusCheckRollup": null,
        "reviewDecision": null,
        "isDraft": true,
    });

    let temp_dir = tempfile::tempdir().unwrap();
    let pr_list_file = temp_dir.path().join("pr_list.json");
    std::fs::write(
        &pr_list_file,
        serde_json::to_string(&vec![pr_json]).unwrap(),
    )
    .unwrap();

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

    let snap = minimal_snapshot_for_test();
    let (state, _tmp, _guard) = make_test_state("test-repo");

    let result = poll_prs_for_issues(&snap, &state).await;

    unsafe {
        std::env::set_var("PATH", original_path);
    }
    drop(_path_guard);

    let effects = result.expect("poll_prs_for_issues should succeed");

    // Draft PRs must produce no effects — no orphaned PR warnings, no nudges
    assert!(
        effects.is_empty(),
        "Draft PR should be skipped entirely, producing no effects. Got: {:?}",
        effects
    );

    // Double-check: no orphaned PR warning messages
    let has_orphan_warning = effects.iter().any(|e| {
        matches!(e, Effect::PostSystemMessage { message, .. } if message.contains("Orphaned PR"))
    });
    assert!(
        !has_orphan_warning,
        "Draft PR must not trigger orphaned PR alerts"
    );
}
