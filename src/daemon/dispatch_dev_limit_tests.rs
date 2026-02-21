//! Tests for dev limit enforcement in task dispatch.
//!
//! These tests verify that the dev spawn cap equals max_coworkers and that
//! REVIEW_HEADROOM allows reviewers to exceed max_coworkers (not reduce dev slots).

use std::collections::HashMap;

use crate::daemon::constants::REVIEW_HEADROOM;

#[test]
fn test_review_headroom_semantics() {
    // REVIEW_HEADROOM allows reviewers to EXCEED max_coworkers — it does NOT reduce dev slots.
    // With max_coworkers=8 and REVIEW_HEADROOM=2:
    //   - dev_cap = max_coworkers = 8  (no subtraction)
    //   - reviewer_cap = max_coworkers + REVIEW_HEADROOM = 10
    let max_coworkers: usize = 8;
    let dev_cap = max_coworkers; // new semantics: no subtraction
    let reviewer_cap = max_coworkers + REVIEW_HEADROOM;

    assert_eq!(
        dev_cap, 8,
        "Dev cap should equal max_coworkers (REVIEW_HEADROOM no longer reduces dev slots)"
    );
    assert_eq!(
        reviewer_cap, 10,
        "Reviewer cap should be max_coworkers + REVIEW_HEADROOM"
    );
    assert_eq!(REVIEW_HEADROOM, 2, "REVIEW_HEADROOM should be 2");
}

#[test]
fn test_spawn_count_within_tick() {
    // This test documents the expected behavior for spawn limiting within a tick.
    //
    // Scenario: 7 active dev coworkers, 3 pending unowned tasks.
    // With max_coworkers=8, dev_cap=8 (no REVIEW_HEADROOM subtraction).
    // - Snapshot shows is_at_dev_limit = false (7 < 8)
    // - Loop processes tasks one by one
    //
    // Without per-spawn counter: Loop checks is_at_dev_limit ONCE, spawns all 3 tasks.
    // Result: 7 + 3 = 10 coworkers, exceeding dev cap of 8.
    //
    // With per-spawn counter: after each spawn decision, re-check:
    //   - spawns_this_tick = 0
    //   - Process task 1: spawns_this_tick = 1, total = 8 (at cap, STOP)
    //   - Tasks 2 and 3 deferred to next tick

    let active_count = 7;
    let pending_count = 3;
    let dev_cap = 8; // = max_coworkers (no REVIEW_HEADROOM subtraction)

    // Without per-spawn counter: all tasks spawn
    let spawned_without_counter = pending_count; // 3
    let total_without_counter = active_count + spawned_without_counter; // 10
    assert!(
        total_without_counter > dev_cap,
        "Bug: spawning exceeds dev cap"
    );

    // With per-spawn counter: only spawn until cap
    let spawned_with_counter = (dev_cap - active_count).min(pending_count); // 1
    let total_with_counter = active_count + spawned_with_counter; // 8
    assert_eq!(
        total_with_counter, dev_cap,
        "Fix: spawning stops at dev cap"
    );
}

#[test]
fn test_spawn_limit_edge_cases() {
    // dev_cap = max_coworkers (8), no REVIEW_HEADROOM subtraction
    let dev_cap = 8;

    // Edge case 1: Already at cap
    let active = 8;
    let allowed = (dev_cap - active).max(0);
    assert_eq!(allowed, 0, "No spawns allowed when at cap");

    // Edge case 2: One below cap
    let active = 7;
    let allowed = (dev_cap - active).max(0);
    assert_eq!(allowed, 1, "Exactly 1 spawn allowed when 1 below cap");

    // Edge case 3: Empty (no active coworkers)
    let active = 0;
    let allowed = (dev_cap - active).max(0);
    assert_eq!(
        allowed, 8,
        "Up to dev_cap spawns allowed when starting from 0"
    );
}

/// Build a minimal running Coworker for testing.
fn make_running_coworker(name: &str) -> crate::coworker::Coworker {
    crate::coworker::Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        status: crate::coworker::CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: "default".to_string(),
    }
}

/// Build a minimal pending task for testing.
fn make_pending_task(id: &str) -> crate::tasks::Task {
    crate::tasks::Task {
        id: id.to_string(),
        subject: format!("Task {}", id),
        status: crate::tasks::TaskStatus::Pending,
        owner: None,
        blocked_by: vec![],
        description: None,
        channel: None,
        pr: None,
        created_at: Some(std::time::SystemTime::now()),
    }
}

/// Build a minimal WorldSnapshot for dev limit tests.
fn make_dev_limit_snapshot(
    running: Vec<crate::coworker::Coworker>,
    pending_tasks: Vec<crate::tasks::Task>,
    is_at_dev_limit: bool,
) -> crate::daemon::snapshot::WorldSnapshot {
    let active_names: std::collections::HashSet<String> =
        running.iter().map(|cw| cw.name.to_lowercase()).collect();
    crate::daemon::snapshot::WorldSnapshot {
        running_coworkers: running,
        is_at_dev_limit,
        active_names,
        pending_tasks_without_owners: pending_tasks,
        active_coworkers: vec![],
        coworker_snapshots: vec![],
        active_session_ids: std::collections::HashSet::new(),
        session_name: "midtown-test".to_string(),
        coworker_start_times: std::collections::HashMap::new(),
        coworker_stop_times: std::collections::HashMap::new(),
        headless_process_health: std::collections::HashMap::new(),
        attached_coworkers: std::collections::HashMap::new(),
        in_progress_tasks: vec![],
        busy_coworkers: std::collections::HashSet::new(),
        coworker_task_assignments: std::collections::HashMap::new(),
        all_tasks: vec![],
        pending_tasks_with_owners: vec![],
        task_channel: std::collections::HashMap::new(),
        task_model_map: std::collections::HashMap::new(),
        task_plan_map: std::collections::HashMap::new(),
        task_execution_skill_map: std::collections::HashMap::new(),
        channel_lead_sessions: std::collections::HashMap::new(),
        coworkers_with_open_prs: std::collections::HashSet::new(),
        coworkers_with_merged_prs: std::collections::HashSet::new(),
        merged_pr_numbers: std::collections::HashSet::new(),
        ci_passed_pr_coworkers: std::collections::HashSet::new(),
        review_feedback_pr_coworkers: std::collections::HashSet::new(),
        open_prs_data: vec![],
        github_open_pr_task_ids: std::collections::HashMap::new(),
        pending_task_owners: std::collections::HashSet::new(),
        tasks_with_open_prs: std::collections::HashMap::new(),
        pr_task_associations: std::collections::HashMap::new(),
        active_reviewers: std::collections::HashSet::new(),
        reviewer_pr_assignments: std::collections::HashMap::new(),
        reviewer_in_progress_comment_ids: std::collections::HashMap::new(),
        reviewed_prs: std::collections::HashSet::new(),
        prs_needing_review: 0,
        reviewer_restart_counts: std::collections::HashMap::new(),
        reviewer_escalations_posted: std::collections::HashSet::new(),
        orphaned_pr_lead_nudges_sent: std::collections::HashSet::new(),
        coworkers_with_unblocked_deps: std::collections::HashSet::new(),
        usage_limit_nudge_scheduled: false,
        usage_limit_nudge_at: None,
        usage_limited_coworkers: std::collections::HashSet::new(),
        api_error_coworkers: std::collections::HashSet::new(),
        auth_error_coworkers: std::collections::HashSet::new(),
        tool_name_conflict_coworkers: std::collections::HashSet::new(),
        channel_messages: vec![],
        archived_channels: std::collections::HashSet::new(),
        daemon_logs: vec![],
        lead_session_refresh_interval_secs: 5400,
        is_at_coworker_limit: false,
        tasks_with_worktrees: std::collections::HashSet::new(),
        task_worktree_map: std::collections::HashMap::new(),
        worktree_branch_owners: std::collections::HashMap::new(),
        merged_pr_branches: std::collections::HashMap::new(),
        worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
        now_utc: chrono::Utc::now(),
        repo_name: "test-repo".to_string(),
        default_channel: "test-repo".to_string(),
        repo_owner: None,
        github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
        freshly_fetched_rate_limit: None,
        sessions: HashMap::new(),
        session_task_map: HashMap::new(),
        session_name_map: HashMap::new(),
        name_session_map: HashMap::new(),
        orphan_spawn_cooldown_active: false,
        session_dispatch_cooldown_active: false,
        spawn_failure_cooldown_names: std::collections::HashSet::new(),
    }
}

/// Make a test DaemonState with the given max_coworkers setting.
fn make_test_state_with_max(max_coworkers: usize) -> crate::daemon::DaemonState {
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

    let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf()).expect("wm");
    let cm = crate::coworker::CoworkerManager::new(wm);

    let base_dir = temp_dir.path().to_path_buf();
    std::mem::forget(temp_dir);

    let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(1);
    crate::daemon::DaemonState::new(
        "/tmp/test.sock".into(),
        cm,
        "test-repo".to_string(),
        vec![base_dir.clone()],
        channel_router,
        None,
        max_coworkers,
        None,
        "main".to_string(),
        shutdown_tx,
    )
    .expect("daemon state")
}

/// When a headless lead is running, it should NOT consume a dev slot.
///
/// Scenario: max_coworkers=8 → dev_cap=8 (no REVIEW_HEADROOM subtraction).
/// Lead (headless) + 7 real coworkers = 8 in running_coworkers.
/// Bug: running_coworkers.len()=8 ≥ dev_cap=8 → no task effects dispatched.
/// Fix: lead excluded → effective_count=7 < 8 → task effect IS emitted.
#[test]
fn test_lead_does_not_count_against_dev_cap() {
    let state = make_test_state_with_max(8);

    // Register 7 real coworkers in CoworkerManager so next_available_name()
    // skips them and returns the 8th available name for the new task.
    // This ensures the dispatch loop hits the "fresh spawn" code path.
    for name in &[
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
    ] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    // 7 real coworkers + 1 headless lead = 8 total in running_coworkers
    // The lead session name is the repo name ("test-repo"), not "lead"
    let running = vec![
        make_running_coworker("test-repo"),
        make_running_coworker("lexington"),
        make_running_coworker("park"),
        make_running_coworker("madison"),
        make_running_coworker("broadway"),
        make_running_coworker("amsterdam"),
        make_running_coworker("columbus"),
        make_running_coworker("riverside"),
    ];

    let pending = vec![make_pending_task("99")];

    // is_at_dev_limit=false because lead doesn't count (7 real coworkers < dev_cap=8)
    let snap = make_dev_limit_snapshot(running, pending, false);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    // With the bug: current_coworker_count=8 (includes lead) ≥ dev_cap=8 → no effects.
    // After the fix: lead excluded → effective_count=7 < 8 → AssignAndSpawn IS emitted.
    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        has_task_effect,
        "Expected a task dispatch effect but got none. The lead should not count against \
         the dev cap. Effects: {:?}",
        effects
    );
}

/// Channel leads should not count against the dev limit.
///
/// Scenario: max_coworkers=3, 3 running sessions: "lexington" (dev), "web" (channel lead),
/// "features" (channel lead). Only lexington should count toward the dev cap.
#[test]
fn test_channel_leads_excluded_from_dev_limit() {
    let state = make_test_state_with_max(3);

    // Register all 3 running sessions
    for name in &["lexington", "web", "features"] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let channel_lead_names: std::collections::HashSet<String> =
        ["web".to_string(), "features".to_string()]
            .into_iter()
            .collect();

    // Only 1 real dev coworker (lexington) < max_coworkers=3 → not at dev limit
    assert!(
        !state.is_at_dev_limit(&channel_lead_names),
        "Channel leads should not count toward dev limit"
    );

    // Without channel_lead_names, all 3 would count → at dev limit (3 >= 3)
    let empty: std::collections::HashSet<String> = std::collections::HashSet::new();
    assert!(
        state.is_at_dev_limit(&empty),
        "Without exclusion, all 3 running sessions count toward dev limit"
    );
}

/// Channel leads should not count against the absolute coworker limit.
///
/// Scenario: max_coworkers=2, REVIEW_HEADROOM=2, absolute cap = 4.
/// 4 running sessions: "lexington", "park" (devs), "web", "auth" (channel leads).
/// Only 2 devs < 4 absolute cap → not at coworker limit.
#[test]
fn test_channel_leads_excluded_from_coworker_limit() {
    let state = make_test_state_with_max(2);

    for name in &["lexington", "park", "web", "auth"] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let channel_lead_names: std::collections::HashSet<String> =
        ["web".to_string(), "auth".to_string()]
            .into_iter()
            .collect();

    // 2 devs < max_coworkers(2) + REVIEW_HEADROOM(2) = 4 → not at coworker limit
    assert!(
        !state.is_at_coworker_limit(&channel_lead_names),
        "Channel leads should not count toward coworker limit"
    );
}

/// has_available_coworker_slot should respect channel lead exclusion.
#[test]
fn test_has_available_slot_excludes_channel_leads() {
    let state = make_test_state_with_max(3);

    // Register 3 sessions: 1 dev + 2 channel leads
    for name in &["lexington", "web", "features"] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let channel_lead_names: std::collections::HashSet<String> =
        ["web".to_string(), "features".to_string()]
            .into_iter()
            .collect();

    // Only 1 dev coworker < absolute cap, and names are available
    assert!(
        state.has_available_coworker_slot(&channel_lead_names),
        "Should have available slot when channel leads are excluded"
    );
}

/// Both lead AND channel leads should be excluded simultaneously.
#[test]
fn test_lead_and_channel_leads_both_excluded() {
    let state = make_test_state_with_max(3);

    // 5 running sessions: lead (named after repo) + 2 devs + 2 channel leads
    // The lead session name is the repo name ("test-repo"), not "lead"
    for name in &["test-repo", "lexington", "park", "web", "auth"] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    let channel_lead_names: std::collections::HashSet<String> =
        ["web".to_string(), "auth".to_string()]
            .into_iter()
            .collect();

    // Only 2 real dev coworkers (lexington, park) < max_coworkers=3 → not at dev limit
    assert!(
        !state.is_at_dev_limit(&channel_lead_names),
        "Lead and channel leads should both be excluded from dev limit"
    );
}

/// When the lead is NOT in running_coworkers, dev cap (= max_coworkers) behaves normally.
#[test]
fn test_dev_cap_without_lead_unaffected() {
    let state = make_test_state_with_max(8);

    // Register 8 real coworkers — all slots at dev_cap=8 are consumed.
    for name in &[
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
        "york",
    ] {
        state
            .coworkers
            .insert_for_testing(make_running_coworker(name));
    }

    // 8 real coworkers, no lead → at dev_cap=8
    let running = vec![
        make_running_coworker("lexington"),
        make_running_coworker("park"),
        make_running_coworker("madison"),
        make_running_coworker("broadway"),
        make_running_coworker("amsterdam"),
        make_running_coworker("columbus"),
        make_running_coworker("riverside"),
        make_running_coworker("york"),
    ];

    let pending = vec![make_pending_task("99")];

    // is_at_dev_limit=true: 8 real coworkers ≥ dev_cap=8
    let snap = make_dev_limit_snapshot(running, pending, true);

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    // No task dispatch effects: truly at cap with no lead to discount.
    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        !has_task_effect,
        "Expected no task dispatch when truly at dev cap (8 real coworkers, no lead). \
         Effects: {:?}",
        effects
    );
}

/// Channel leads should not consume dev slots in the dispatch coworker count.
///
/// Scenario: max_coworkers=2, 3 running sessions: "lexington" (dev), "auth" (channel lead),
/// "web" (channel lead). The dispatch loop's `current_coworker_count` must only count
/// lexington → 1 dev < dev_cap=2 → task dispatch IS emitted.
#[test]
fn test_dispatch_excludes_channel_leads_from_dev_count() {
    let state = make_test_state_with_max(2);

    // Register the dev coworker in CoworkerManager so next_available_name()
    // skips "lexington" and returns another name for the new task.
    state
        .coworkers
        .insert_for_testing(make_running_coworker("lexington"));

    // 3 running sessions: 1 dev + 2 channel leads
    let running = vec![
        make_running_coworker("lexington"),
        make_running_coworker("auth"),
        make_running_coworker("web"),
    ];

    let pending = vec![make_pending_task("99")];

    // Build snapshot with channel_lead_sessions populated so dispatch excludes them
    let mut snap = make_dev_limit_snapshot(running, pending, false);
    snap.channel_lead_sessions
        .insert("auth".to_string(), "session-auth-123".to_string());
    snap.channel_lead_sessions
        .insert("web".to_string(), "session-web-456".to_string());

    let effects = crate::daemon::dispatch::spawn_for_pending_tasks(&snap, &state);

    // Only 1 dev coworker (lexington) < dev_cap=2 → task dispatch IS emitted.
    // Without the fix, channel leads would inflate the count to 3 ≥ 2 → no dispatch.
    let has_task_effect = effects.iter().any(|e| {
        matches!(
            e,
            crate::daemon::effects::Effect::AssignAndSpawn { .. }
                | crate::daemon::effects::Effect::SpawnCoworkerWithCallbacks { .. }
                | crate::daemon::effects::Effect::NudgeSessionWithCallbacks { .. }
        )
    });
    assert!(
        has_task_effect,
        "Expected task dispatch effect: channel leads must not count toward dev cap. \
         Effects: {:?}",
        effects
    );
}
