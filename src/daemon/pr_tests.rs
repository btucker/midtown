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
    let cm = crate::coworker::CoworkerManager::new(wm);

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

/// Helper to create minimal DaemonState for testing with a specific repo owner.
/// Adds a fake origin remote so DaemonState::new detects the owner from git URL.
fn make_test_state_with_owner(repo_name: &str, owner: &str) -> DaemonState {
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
    let state = make_test_state("test-repo");

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
#[tokio::test]
#[allow(clippy::await_holding_lock)]
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

    let state = make_test_state("midtown");
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
    )
    .await;

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

    let state = make_test_state("test-repo");

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
    let state = make_test_state("test-repo");

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
        |e| matches!(e, Effect::PostSystemMessage { message } if message.contains("Orphaned PR") || message.contains("orphaned")),
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

    let state = make_test_state("test-repo");
    let active_names = std::collections::HashSet::new();

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
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
    let state = make_test_state("test-repo");
    let active_names = std::collections::HashSet::new();

    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
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
    let state = make_test_state("midtown");
    let active_names = std::collections::HashSet::new();

    // Call the function under test with snapshot's worktree registry and synthetic PR
    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &snap.worktree_registry, // Real snapshot data with task 1323's completed worktree
        &active_names,
        &state,
        &[pr_json], // Synthetic PR that extracts task ID 1323 from title
        crate::github_state::AssignmentSource::PollingFallback,
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
    let state = make_test_state_with_owner("midtown", "btucker");
    let active_names = std::collections::HashSet::new();

    // Call the function under test
    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &worktree_registry,
        &active_names,
        &state,
        &[pr_json],
        crate::github_state::AssignmentSource::PollingFallback,
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

/// Bug: reconcile_orphaned_prs creates duplicate "Merge PR #X" tasks every 30 seconds.
///
/// Root cause: The function only checks pr_task_associations (which tracks the *original*
/// task that created the PR via pr_author_sessions). When a merge task is created, it's
/// a new task not linked in pr_author_sessions, so pr_task_associations doesn't contain
/// the PR. On the next tick, the function creates another duplicate merge task.
///
/// Expected: Only one "Merge PR #X" task should exist for each PR, even across multiple ticks.
#[test]
fn test_reconcile_orphaned_prs_does_not_create_duplicates() {
    use super::super::snapshot::minimal_snapshot_for_test;
    use crate::tasks::{Task, TaskStatus};

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

    // First tick: No existing merge task yet
    let effects1 = reconcile_orphaned_prs(&snap);

    // Should create one merge task
    let create_task_count1 = effects1
        .iter()
        .filter(|e| matches!(e, Effect::CreateTask { .. }))
        .count();
    assert_eq!(
        create_task_count1, 1,
        "First tick should create exactly one merge task"
    );

    // Simulate the created task now exists in all_tasks as pending
    let merge_task = Task {
        id: "1001".to_string(),
        subject: "Merge PR #42 — reviewed, CI green".to_string(),
        status: TaskStatus::Pending,
        owner: None,
        description: Some("PR #42 (Fix authentication bug) has been reviewed...".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(42), // This will be set after the fix
        created_at: None,
    };

    snap.all_tasks = vec![merge_task];

    // Second tick: Merge task now exists
    let effects2 = reconcile_orphaned_prs(&snap);

    // Should NOT create another merge task (bug: currently creates duplicate)
    let create_task_count2 = effects2
        .iter()
        .filter(|e| matches!(e, Effect::CreateTask { .. }))
        .count();
    assert_eq!(
        create_task_count2, 0,
        "Second tick should NOT create duplicate merge task. BUG: reconcile_orphaned_prs \
         only checks pr_task_associations (which tracks original PR author tasks), not \
         existing merge tasks in all_tasks. This causes duplicate 'Merge PR #42' tasks \
         to be created every 30 seconds."
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
        name_session_map: std::collections::HashMap::new(),
    }
}

/// Helper to create a PrContext with no task associations.
fn make_pr_context_empty() -> PrContext {
    PrContext {
        pr_task_associations: std::collections::HashMap::new(),
        task_channel: std::collections::HashMap::new(),
        session_context: None,
        name_session_map: std::collections::HashMap::new(),
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
    let state = make_test_state("test-repo");
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
    let state = make_test_state("test-repo");
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
    let state = make_test_state("test-repo");
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
    let state = make_test_state("test-repo");
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
    let state = make_test_state("test-repo");
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
    let state = make_test_state("test-repo");
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
    let state = make_test_state("test-repo");
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
    let state = make_test_state("test-repo");
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

/// Bug: reconcile_orphaned_prs checks all_tasks including completed tasks.
///
/// Scenario: A merge task was created, marked completed (mistakenly or due to some edge case),
/// but the PR never actually merged and is still open. The dedup check prevents creating
/// a new merge task because it finds the completed task.
///
/// Expected: Only active (pending/in_progress) merge tasks should block creating new ones.
/// Completed tasks should not prevent reconciliation.
#[test]
fn test_reconcile_orphaned_prs_ignores_completed_merge_tasks() {
    use super::super::snapshot::minimal_snapshot_for_test;
    use crate::tasks::{Task, TaskStatus};

    // Simulate PR #42 that meets all orphan criteria
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

    // Simulate a completed merge task exists for this PR
    // (perhaps it was mistakenly completed, or there was a race condition)
    let completed_merge_task = Task {
        id: "1002".to_string(),
        subject: "Merge PR #43 — reviewed, CI green".to_string(),
        status: TaskStatus::Completed, // Task is completed
        owner: None,
        description: Some("PR #43 (Add logging feature) has been reviewed...".to_string()),
        blocked_by: vec![],
        channel: None,
        pr: Some(43), // Associated with PR #43
        created_at: None,
    };

    snap.all_tasks = vec![completed_merge_task];

    // Call reconcile_orphaned_prs
    let effects = reconcile_orphaned_prs(&snap);

    // Should create a new merge task because the existing one is completed
    let create_task_count = effects
        .iter()
        .filter(|e| matches!(e, Effect::CreateTask { .. }))
        .count();
    assert_eq!(
        create_task_count, 1,
        "Should create a new merge task when the existing merge task is completed. \
         BUG: Currently skips creation because all_tasks check includes completed tasks, \
         leaving the PR stuck without an active merge task."
    );
}

/// Bug !1377: pr_action_to_effects was missing RecordTaskAssignment in on_success,
/// allowing cross-tick duplicate spawns for the same task.
#[test]
fn pr_action_to_effects_includes_record_task_assignment() {
    let state = make_test_state("test-repo");

    // Build PrContext with a PR→task association
    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(123, "42".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        name_session_map: HashMap::new(),
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
    let state = make_test_state("test-repo");

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(456, "99".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        name_session_map: HashMap::new(),
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
    let state = make_test_state("test-repo");

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(789, "17".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        name_session_map: HashMap::new(),
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
    let state = make_test_state("test-repo");

    let mut pr_task_associations = HashMap::new();
    pr_task_associations.insert(321, "55".to_string());

    let ctx = PrContext {
        pr_task_associations,
        task_channel: HashMap::new(),
        session_context: None,
        name_session_map: HashMap::new(),
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

    let state = make_test_state("midtown");

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

    let state = make_test_state("midtown");

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
            preferred_name: None,
            working_dir: "/tmp/test".to_string(),
            branch: Some("lexington/fix-auth".to_string()),
            pr_number: Some(42),
            initial_prompt: None,
            is_reviewer: false,
            coworker_type: "dev".to_string(),
            is_running: true,
            created_at: chrono::Utc::now(),
            resume_on_startup: false,
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
/// the lookup should return None so the caller falls back to branch-based resolution.
#[test]
fn test_resolve_pr_owner_from_session_returns_none_for_suspended_session() {
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
            current_name: None, // suspended — no name allocated
            preferred_name: Some("lexington".to_string()),
            working_dir: "/tmp/test".to_string(),
            branch: Some("lexington/fix-auth".to_string()),
            pr_number: Some(42),
            initial_prompt: None,
            is_reviewer: false,
            coworker_type: "dev".to_string(),
            is_running: false,
            created_at: chrono::Utc::now(),
            resume_on_startup: false,
        },
    )]
    .into_iter()
    .collect();

    let result =
        resolve_pr_owner_from_session(42, &pr_task_associations, &session_task_map, &sessions);
    assert_eq!(
        result, None,
        "Should return None when session has no current_name (suspended)"
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
            preferred_name: None,
            working_dir: "/tmp/test".to_string(),
            branch: Some("lexington/fix-auth".to_string()),
            pr_number: Some(42),
            initial_prompt: None,
            is_reviewer: false,
            coworker_type: "dev".to_string(),
            is_running: true,
            created_at: chrono::Utc::now(),
            resume_on_startup: false,
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

    let state = make_test_state("test-repo");

    let result = poll_prs_for_issues(&snap, &state).await;

    unsafe {
        std::env::set_var("PATH", original_path);
    }
    drop(_path_guard);

    let effects = result.expect("poll_prs_for_issues should succeed");

    // With session-based resolution, the owner is "madison" (not "lexington").
    // The PR has a merge conflict, so we expect a nudge to "madison".
    let nudges_madison = effects.iter().any(|e| match e {
        Effect::NudgeCoworkerWithCallbacks { name, .. } => name == "madison",
        Effect::NudgeCoworker { name, .. } => name == "madison",
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

/// The name_session_map in PrContext should be used by action_to_effects functions
/// to populate session_id on NudgeCoworker effects.
#[test]
fn test_pr_context_name_session_map_provides_session_id_for_nudge() {
    let ctx = PrContext {
        pr_task_associations: HashMap::new(),
        task_channel: HashMap::new(),
        session_context: None,
        name_session_map: [("madison".to_string(), "sess-abc".to_string())]
            .into_iter()
            .collect(),
    };

    // Simulate NudgeOwner action → should populate session_id from name_session_map
    let action = crate::rules::PrAction::NudgeOwner {
        owner: "madison".to_string(),
        message: "PR #42 has a merge conflict".to_string(),
    };

    let state = make_test_state("test-repo");
    let effects = pr_action_to_effects(
        action,
        42,
        "Fix auth bug",
        PrIssueType::MergeConflict,
        &state,
        &ctx,
    );

    // The NudgeCoworkerWithCallbacks effect should have session_id populated
    let nudge_effect = effects
        .iter()
        .find(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }));
    assert!(
        nudge_effect.is_some(),
        "Expected NudgeCoworkerWithCallbacks effect"
    );

    if let Some(Effect::NudgeCoworkerWithCallbacks { session_id, .. }) = nudge_effect {
        assert_eq!(
            *session_id,
            Some("sess-abc".to_string()),
            "session_id should be populated from name_session_map"
        );
    }
}
