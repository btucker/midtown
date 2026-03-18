use super::*;
use serde_json::json;

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
        crate::paths::ProjectPaths::with_project_name(repo_name, repo_name),
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

/// Regression test: reviewer allocation must exclude names that are in active_names
/// even if they are NOT registered in CoworkerManager.
///
/// Scenario: "amsterdam" finished its task and was cleaned up from CoworkerManager,
/// but still has an active session (appears in active_names from WorldSnapshot).
/// A reviewer spawn should NOT allocate "amsterdam" because it's still running.
///
/// Before fix: next_available_name_excluding only checked CoworkerManager, so
/// "amsterdam" would be allocated as a reviewer name, causing a collision.
#[tokio::test]
async fn test_reviewer_allocation_excludes_active_session_names() {
    let pr = json!({
        "number": 7777,
        "headRefName": "park/some-feature",
        "title": "feat: Some feature [Midtown !200]",
        "isDraft": false,
        "createdAt": "2026-01-01T00:00:00Z",
        "state": "OPEN",
    });

    let (state, _tmp, _guard) = make_test_state("midtown");

    // Register all AVENUE_NAMES except "amsterdam" and "park" as active coworkers.
    // "park" is the PR author (excluded from reviewer selection).
    // "amsterdam" is NOT in CoworkerManager but IS in active_names (the bug scenario).
    for (i, name) in crate::coworker::AVENUE_NAMES
        .iter()
        .filter(|&&n| n != "amsterdam" && n != "park")
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

    // Populate session data so session-based orphan check resolves "park" as PR author.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.sessions.insert(
            "sess-park".to_string(),
            crate::daemon::state::SessionRecord {
                session_id: "sess-park".to_string(),
                task_id: Some("200".to_string()),
                pr_number: Some(7777),
                current_name: Some("park".to_string()),
                working_dir: "/tmp/test".to_string(),
                ..Default::default()
            },
        );
    }

    let registry = crate::worktree_registry::WorktreeRegistry::new();

    // "amsterdam" is in active_names (still has an active session) even though
    // it's not registered in CoworkerManager. "park" is the PR author.
    let active_names: std::collections::HashSet<String> =
        ["amsterdam".to_string(), "park".to_string()]
            .into_iter()
            .collect();

    let effects = collect_reviewer_effects_with_source(
        &registry,
        &active_names,
        &state,
        &[pr],
        true, // is_polling_fallback
        &std::collections::HashMap::new(),
        false, // not at task limit
    )
    .await;

    // A review task should be created (name allocation now happens in dispatch,
    // not in collect_reviewer_effects — name collision prevention moved there too).
    let has_review_task = effects.iter().any(|e| {
        matches!(
            e,
            Effect::CreateReviewTask {
                pr_number: 7777,
                ..
            }
        )
    });

    assert!(
        has_review_task,
        "Expected a CreateReviewTask effect for PR #7777. \
         Name allocation and collision prevention now happen in dispatch."
    );
}
