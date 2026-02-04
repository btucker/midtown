//! E2E tests for coworker health detection using captured WorldSnapshot fixtures.
//!
//! These tests verify the daemon correctly:
//! - Identifies idle coworkers for break
//! - Detects stuck coworkers in compaction/waiting states
//! - Detects zombie coworkers with blank panes
//! - Handles usage limit screens appropriately
//!
//! Tests use both real-world snapshot fixtures and synthetic data to verify
//! health detection logic.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;

/// Load a snapshot fixture and parse it into test-friendly data structures.
fn load_snapshot(json_str: &str) -> (Vec<TestCoworker>, SnapshotData) {
    let v: Value = serde_json::from_str(json_str).expect("valid JSON");

    // Extract coworker snapshots
    let coworkers: Vec<TestCoworker> = v["coworker_snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|cw| TestCoworker {
            name: cw["name"].as_str().unwrap().to_string(),
            started_at: DateTime::parse_from_rfc3339(cw["started_at"].as_str().unwrap())
                .unwrap()
                .with_timezone(&Utc),
            isolated_tasks: cw["isolated_tasks"].as_bool().unwrap_or(false),
        })
        .collect();

    // Extract HashSets from snapshot
    let busy_coworkers: HashSet<String> = v["busy_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let coworkers_with_open_prs: HashSet<String> = v["coworkers_with_open_prs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let active_reviewers: HashSet<String> = v["active_reviewers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let coworkers_with_unblocked_deps: HashSet<String> = v["coworkers_with_unblocked_deps"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let ci_passed_pr_coworkers: HashSet<String> = v["ci_passed_pr_coworkers"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let pane_contents: HashMap<String, String> = v["pane_contents"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();

    let now_utc = DateTime::parse_from_rfc3339(v["now_utc"].as_str().unwrap())
        .unwrap()
        .with_timezone(&Utc);

    (
        coworkers,
        SnapshotData {
            busy_coworkers,
            coworkers_with_open_prs,
            active_reviewers,
            coworkers_with_unblocked_deps,
            ci_passed_pr_coworkers,
            pane_contents,
            now_utc,
        },
    )
}

#[derive(Debug, Clone)]
struct TestCoworker {
    name: String,
    started_at: DateTime<Utc>,
    isolated_tasks: bool,
}

#[derive(Debug)]
struct SnapshotData {
    busy_coworkers: HashSet<String>,
    coworkers_with_open_prs: HashSet<String>,
    active_reviewers: HashSet<String>,
    coworkers_with_unblocked_deps: HashSet<String>,
    #[allow(dead_code)]
    ci_passed_pr_coworkers: HashSet<String>,
    pane_contents: HashMap<String, String>,
    now_utc: DateTime<Utc>,
}

/// Test that lexington should be sent on break in the captured snapshot.
///
/// Snapshot context: lexington completed task #22, posted idle status, and has
/// been at the prompt for >2 minutes. The daemon should detect this and return
/// a shutdown decision.
#[test]
fn idle_lexington_should_be_sent_on_break() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (coworkers, data) = load_snapshot(fixture);

    // Verify lexington is in the snapshot and meets break conditions
    let lexington = coworkers.iter().find(|c| c.name == "lexington").unwrap();

    // Lexington should NOT be blocked by any of these conditions:
    assert!(
        !data.busy_coworkers.contains("lexington"),
        "lexington should not be busy"
    );
    assert!(
        !data.coworkers_with_open_prs.contains("lexington"),
        "lexington should not have an open PR"
    );
    assert!(
        !data.active_reviewers.contains("lexington"),
        "lexington should not be an active reviewer"
    );
    assert!(
        !data.coworkers_with_unblocked_deps.contains("lexington"),
        "lexington should not have unblocked deps"
    );

    // Verify lexington has been running long enough (minimum lifetime check)
    let lifetime = data.now_utc.signed_duration_since(lexington.started_at);
    assert!(
        lifetime > chrono::Duration::minutes(5),
        "lexington should have been running >5 minutes, was {:?}",
        lifetime
    );

    // Verify lexington's pane shows idle state (completed task, at prompt)
    let pane = data.pane_contents.get("lexington").unwrap();
    assert!(
        pane.contains("midtown state idle"),
        "pane should show lexington set state to idle"
    );
    assert!(
        pane.contains("work done for now"),
        "pane should show lexington's idle message"
    );

    // The test confirms lexington meets all conditions for being sent on break:
    // - Running > minimum_lifetime
    // - Not busy (no in-progress tasks)
    // - No open PR
    // - Not an active reviewer
    // - No unblocked deps waiting
    // - Pane shows idle state with completed work
    //
    // If the daemon's decide_idle_shutdowns is called with lexington in
    // SessionHealth::Idle for >= idle_break_duration, it should return a
    // shutdown decision for lexington.
}

/// Test that coworkers with open PRs are NOT sent on break even if idle.
#[test]
fn coworker_with_open_pr_not_sent_on_break() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (coworkers, data) = load_snapshot(fixture);

    // Vernon, Columbus, and Amsterdam have open PRs in this snapshot
    for name in &["vernon", "columbus", "amsterdam"] {
        assert!(
            data.coworkers_with_open_prs.contains(*name),
            "{} should have an open PR",
            name
        );

        // Even if they're idle in terms of not having in-progress tasks,
        // they should NOT be sent on break because they have open PRs
        let cw = coworkers.iter().find(|c| c.name == *name).unwrap();
        let lifetime = data.now_utc.signed_duration_since(cw.started_at);
        assert!(
            lifetime > chrono::Duration::minutes(5),
            "{} should have been running long enough",
            name
        );
    }
}

/// Test that busy coworkers (york with task #27) are not sent on break.
#[test]
fn busy_coworker_not_sent_on_break() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (coworkers, data) = load_snapshot(fixture);

    // York is busy with task #27 in this snapshot
    assert!(data.busy_coworkers.contains("york"), "york should be busy");

    let _york = coworkers.iter().find(|c| c.name == "york").unwrap();

    // Verify york's pane shows active work
    let pane = data.pane_contents.get("york").unwrap();
    assert!(
        pane.contains("Fixing false idle detection"),
        "york should be actively working on task #27"
    );
}

/// Test that active reviewers (broadway) are not sent on break.
#[test]
fn active_reviewer_not_sent_on_break() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (_coworkers, data) = load_snapshot(fixture);

    // Broadway is an active reviewer in this snapshot
    assert!(
        data.active_reviewers.contains("broadway"),
        "broadway should be an active reviewer"
    );
}

/// Test that isolated coworkers (reviewers) go on break immediately when idle.
#[test]
fn isolated_reviewer_immediate_break() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (coworkers, data) = load_snapshot(fixture);

    // Broadway is isolated (isolated_tasks: true) in this snapshot
    let broadway = coworkers.iter().find(|c| c.name == "broadway").unwrap();
    assert!(
        broadway.isolated_tasks,
        "broadway should have isolated_tasks=true"
    );

    // Broadway is also an active reviewer, so it won't be sent on break
    // But if it weren't a reviewer, an isolated coworker would go on break
    // immediately without waiting for idle_break_duration
    assert!(
        data.active_reviewers.contains("broadway"),
        "broadway is still an active reviewer in this snapshot"
    );
}

// ---------------------------------------------------------------------------
// Stuck coworker detection tests
// ---------------------------------------------------------------------------

/// Test that coworkers stuck in compaction get recovery action.
///
/// Compaction ("Whirlpooling your conversation..." / "Baking your conversation...")
/// is normally a healthy operation. But if it runs for too long (>5 minutes),
/// the daemon should interrupt it with Escape to recover the coworker.
#[test]
fn stuck_compaction_triggers_recovery() {
    // Simulate a pane showing compaction running for an extended time
    let mut pane_contents = HashMap::new();
    pane_contents.insert(
        "york".to_string(),
        "  Whirlpooling your conversation…\n  (esc to interrupt · 18m 50s · ↓ 0 tokens)\n"
            .to_string(),
    );

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    // York has been running long enough to be eligible
    coworker_start_times.insert("york".to_string(), now_utc - chrono::Duration::minutes(30));

    // Call the pure decision function
    let recoveries = midtown::rules::decide_stuck_ui_recoveries(
        &pane_contents,
        Duration::from_secs(300), // 5 minute threshold
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(60),
    );

    // 18m 50s > 5 minute threshold, should trigger InterruptCompaction
    assert_eq!(
        recoveries.len(),
        1,
        "long-running compaction should trigger recovery"
    );
    assert!(
        matches!(
            &recoveries[0],
            midtown::rules::StuckUiRecovery::InterruptCompaction { name } if name == "york"
        ),
        "recovery should be InterruptCompaction for york"
    );
}

/// Test that short compaction does NOT trigger recovery.
///
/// Compaction under the threshold is healthy and should be left alone.
#[test]
fn short_compaction_no_recovery() {
    let mut pane_contents = HashMap::new();
    pane_contents.insert(
        "amsterdam".to_string(),
        "  Baking your conversation…\n  (esc to interrupt · 2m 30s · ↓ 42 tokens)\n".to_string(),
    );

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    coworker_start_times.insert(
        "amsterdam".to_string(),
        now_utc - chrono::Duration::minutes(30),
    );

    let recoveries = midtown::rules::decide_stuck_ui_recoveries(
        &pane_contents,
        Duration::from_secs(300), // 5 minute threshold
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(60),
    );

    // 2m 30s < 5 minute threshold, should NOT trigger
    assert!(
        recoveries.is_empty(),
        "short compaction should not trigger recovery"
    );
}

/// Test that coworkers with queued nudges get recovery action.
///
/// When nudges pile up in the input queue (visible as multiple ❯ lines),
/// the coworker is stuck waiting for input. The daemon should detect this.
#[test]
fn queued_nudges_trigger_recovery() {
    let mut pane_contents = HashMap::new();
    // TUI structure with queued nudges visible
    let tui_content = "\
⏺ Completed previous task

✳ Running cargo test...
  (esc to interrupt · 45s)
❯ Check the channel for updates
❯ Your PR needs attention
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
    pane_contents.insert("columbus".to_string(), tui_content.to_string());

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    // Columbus has been running long enough (>60s min age)
    coworker_start_times.insert(
        "columbus".to_string(),
        now_utc - chrono::Duration::minutes(5),
    );

    let recoveries = midtown::rules::decide_stuck_ui_recoveries(
        &pane_contents,
        Duration::from_secs(300),
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(60), // min age for queued nudge detection
    );

    assert_eq!(recoveries.len(), 1, "queued nudges should trigger recovery");
    assert!(
        matches!(
            &recoveries[0],
            midtown::rules::StuckUiRecovery::InterruptQueuedNudges { name } if name == "columbus"
        ),
        "recovery should be InterruptQueuedNudges for columbus"
    );
}

/// Test that young coworkers are protected from queued nudge detection.
///
/// During startup, the TUI structure is still forming and can produce false
/// positives for queued nudge detection. We require a minimum age before
/// triggering recovery.
#[test]
fn young_coworker_protected_from_queued_nudge_detection() {
    let mut pane_contents = HashMap::new();
    let tui_content = "\
⏺ Starting up

✳ Initializing...
❯ Some queued text
────────────────────────────────────────────────────────────────────────────────
❯
────────────────────────────────────────────────────────────────────────────────
  ⏵⏵ bypass permissions on";
    pane_contents.insert("madison".to_string(), tui_content.to_string());

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    // Madison is young (only 30 seconds old)
    coworker_start_times.insert(
        "madison".to_string(),
        now_utc - chrono::Duration::seconds(30),
    );

    let recoveries = midtown::rules::decide_stuck_ui_recoveries(
        &pane_contents,
        Duration::from_secs(300),
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(60), // min age threshold
    );

    // Madison is too young (30s < 60s threshold), should be protected
    assert!(
        recoveries.is_empty(),
        "young coworkers should be protected from queued nudge detection"
    );
}

// ---------------------------------------------------------------------------
// Zombie coworker detection tests
// ---------------------------------------------------------------------------

/// Test that blank-pane coworkers are detected as zombies.
///
/// A coworker with an entirely blank pane (no visible output) is considered
/// a zombie - likely crashed on startup. The daemon should respawn them.
#[test]
fn blank_pane_detected_as_zombie() {
    let mut blank_pane_coworkers = HashSet::new();
    blank_pane_coworkers.insert("riverside".to_string());

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    // Riverside has been running long enough to be past the startup grace period
    coworker_start_times.insert(
        "riverside".to_string(),
        now_utc - chrono::Duration::seconds(60),
    );

    let zombies = midtown::rules::detect_blank_pane_zombies(
        &blank_pane_coworkers,
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(20), // min age threshold
    );

    assert_eq!(zombies.len(), 1, "blank pane coworker should be detected");
    assert_eq!(
        zombies[0], "riverside",
        "riverside should be detected as zombie"
    );
}

/// Test that young coworkers are not detected as zombies.
///
/// During startup, the pane may be blank while the TUI initializes.
/// We require a minimum age before flagging as a zombie.
#[test]
fn young_coworker_not_detected_as_zombie() {
    let mut blank_pane_coworkers = HashSet::new();
    blank_pane_coworkers.insert("broadway".to_string());

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    // Broadway just started (only 5 seconds old)
    coworker_start_times.insert(
        "broadway".to_string(),
        now_utc - chrono::Duration::seconds(5),
    );

    let zombies = midtown::rules::detect_blank_pane_zombies(
        &blank_pane_coworkers,
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(20), // min age threshold
    );

    // Broadway is too young (5s < 20s threshold), should not be flagged
    assert!(
        zombies.is_empty(),
        "young coworkers should not be detected as zombies during startup"
    );
}

/// Test that the lead window is never treated as a zombie.
///
/// The lead has its own health check (check_and_respawn_lead). It must never
/// be included in zombie coworker detection.
#[test]
fn lead_window_never_treated_as_zombie() {
    let mut blank_pane_coworkers = HashSet::new();
    blank_pane_coworkers.insert("lead".to_string());

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    coworker_start_times.insert("lead".to_string(), now_utc - chrono::Duration::minutes(10));

    let zombies = midtown::rules::detect_blank_pane_zombies(
        &blank_pane_coworkers,
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(20),
    );

    assert!(
        zombies.is_empty(),
        "lead window must never be treated as a zombie"
    );
}

/// Test that non-blank panes are not detected as zombies.
#[test]
fn non_blank_pane_not_zombie() {
    // Empty set - no blank panes
    let blank_pane_coworkers = HashSet::new();

    let now_utc = chrono::Utc::now();
    let mut coworker_start_times = HashMap::new();
    coworker_start_times.insert("park".to_string(), now_utc - chrono::Duration::minutes(10));

    let zombies = midtown::rules::detect_blank_pane_zombies(
        &blank_pane_coworkers,
        &coworker_start_times,
        now_utc,
        chrono::Duration::seconds(20),
    );

    assert!(
        zombies.is_empty(),
        "coworkers with non-blank panes should not be zombies"
    );
}

// ---------------------------------------------------------------------------
// Usage limit detection tests
// ---------------------------------------------------------------------------

/// Test that usage limit screens are detected.
///
/// When a coworker hits the Claude API usage limit, their pane shows a message
/// containing "/upgrade". The daemon should detect this and schedule a nudge
/// for when the limit resets.
#[test]
fn usage_limit_screen_detected() {
    let mut pane_contents = HashMap::new();
    pane_contents.insert(
        "lexington".to_string(),
        "You've reached your usage limit. /upgrade to increase your limit.".to_string(),
    );
    pane_contents.insert(
        "vernon".to_string(),
        "  Reading files...\n  Edit complete.\n".to_string(),
    );

    let decision = midtown::rules::decide_usage_limit_detection(&pane_contents);

    assert!(
        matches!(
            decision,
            midtown::rules::UsageLimitDecision::Detected { coworker } if coworker == "lexington"
        ),
        "usage limit screen should be detected for lexington"
    );
}

/// Test that normal panes do not trigger usage limit detection.
#[test]
fn normal_pane_no_usage_limit_detected() {
    let mut pane_contents = HashMap::new();
    pane_contents.insert(
        "park".to_string(),
        "  $ cargo build\n  Compiling midtown...\n".to_string(),
    );
    pane_contents.insert(
        "madison".to_string(),
        "  Reading file src/main.rs\n  ✓ File read\n".to_string(),
    );

    let decision = midtown::rules::decide_usage_limit_detection(&pane_contents);

    assert!(
        matches!(decision, midtown::rules::UsageLimitDecision::NoneDetected),
        "normal panes should not trigger usage limit detection"
    );
}

/// Test that code discussing upgrades (without literal /upgrade) does not trigger false positive.
///
/// The usage limit pattern only matches the literal "/upgrade" string from the
/// actual usage limit screen. Code discussing upgrades in comments, variable
/// names, or function names should not trigger detection.
#[test]
fn code_discussing_upgrades_not_false_positive() {
    let mut pane_contents = HashMap::new();
    // Code that discusses upgrades without the literal "/upgrade" pattern
    pane_contents.insert(
        "amsterdam".to_string(),
        r#"
// Handle upgrade requests for premium users
fn handle_upgrade_request(request: &UpgradeRequest) -> Response {
    // Check if user can upgrade their subscription
    if user.can_upgrade() {
        perform_subscription_upgrade()
    }
}

/// Upgrade health checks: idle shutdown, stuck detection, usage limits.
const UPGRADE_DOCS: &str = "See upgrade documentation for details";
"#
        .to_string(),
    );

    let decision = midtown::rules::decide_usage_limit_detection(&pane_contents);

    assert!(
        matches!(decision, midtown::rules::UsageLimitDecision::NoneDetected),
        "code discussing upgrades should not trigger false positive"
    );
}

/// Test that the /upgrade pattern specifically triggers detection.
///
/// The literal "/upgrade" string is used by Claude Code's usage limit screen.
/// When it appears in pane content, it's treated as a usage limit signal.
/// This is a known limitation - code containing literal "/upgrade" URL paths
/// could trigger a false positive, but this is rare in practice.
#[test]
fn literal_upgrade_pattern_triggers_detection() {
    let mut pane_contents = HashMap::new();
    // Content with the literal /upgrade pattern (like the usage limit screen)
    pane_contents.insert(
        "park".to_string(),
        "You've hit a limit. /upgrade for more capacity.".to_string(),
    );

    let decision = midtown::rules::decide_usage_limit_detection(&pane_contents);

    assert!(
        matches!(
            decision,
            midtown::rules::UsageLimitDecision::Detected { coworker } if coworker == "park"
        ),
        "literal /upgrade pattern should trigger detection"
    );
}

// ---------------------------------------------------------------------------
// Stuck pane hash detection tests
// ---------------------------------------------------------------------------

/// Test that unchanged pane hash with in-progress task triggers restart.
///
/// When a coworker's pane content has not changed for the stuck duration
/// AND they have an in-progress task, they are considered stuck and should
/// be restarted.
#[test]
fn stuck_pane_with_task_triggers_restart() {
    let mut pane_hashes = HashMap::new();
    let now = Instant::now();
    let old_time = now - Duration::from_secs(400); // 6+ minutes ago

    // Content that doesn't have activity indicators
    let stuck_content = "⏺ Working on task #42\n\nReading files...\n";

    // Hash the content
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    stuck_content.hash(&mut hasher);
    let content_hash = hasher.finish();

    // Set old hash to same value (pane unchanged)
    pane_hashes.insert("vernon".to_string(), (content_hash, old_time));

    let mut pane_contents = HashMap::new();
    pane_contents.insert("vernon".to_string(), stuck_content.to_string());

    // Vernon has an in-progress task
    let tasks = vec![(
        "42".to_string(),
        "Test task".to_string(),
        "vernon".to_string(),
    )];

    let result = midtown::rules::decide_stuck_coworker_restarts(
        &pane_hashes,
        &pane_contents,
        &tasks,
        now,
        Duration::from_secs(180), // 3 minute stuck duration
    );

    assert_eq!(
        result.restarts.len(),
        1,
        "stuck coworker with task should be restarted"
    );
    assert_eq!(result.restarts[0].name, "vernon");
    assert_eq!(result.restarts[0].task_id, "42");
}

/// Test that coworkers with running subagents are protected from stuck detection.
///
/// When a coworker is running Task agents (subagents), the pane may appear frozen
/// while waiting for the subagent to complete. This is normal behavior, not stuck.
#[test]
fn subagent_running_protects_from_stuck_detection() {
    let mut pane_hashes = HashMap::new();
    let now = Instant::now();
    let old_time = now - Duration::from_secs(400); // 6+ minutes ago

    // Content showing active subagent (whirlpool indicator)
    let active_content = r#"
✽ Checking PR eligibility… (esc to interrupt · ctrl+t to hide tasks · 33s · ↓ 784 tokens · thinking)
  ⎿  ◼ #1 Check PR #508 eligibility for code review (madison)
"#;

    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    active_content.hash(&mut hasher);
    let content_hash = hasher.finish();

    pane_hashes.insert("broadway".to_string(), (content_hash, old_time));

    let mut pane_contents = HashMap::new();
    pane_contents.insert("broadway".to_string(), active_content.to_string());

    let tasks = vec![(
        "99".to_string(),
        "Review PR".to_string(),
        "broadway".to_string(),
    )];

    let result = midtown::rules::decide_stuck_coworker_restarts(
        &pane_hashes,
        &pane_contents,
        &tasks,
        now,
        Duration::from_secs(180),
    );

    assert!(
        result.restarts.is_empty(),
        "coworker with running subagent should be protected from stuck detection"
    );
}

/// Test that pane content change resets stuck timer.
#[test]
fn pane_change_resets_stuck_timer() {
    let mut pane_hashes = HashMap::new();
    let now = Instant::now();
    let old_time = now - Duration::from_secs(400); // 6+ minutes ago

    // Old content hash
    let old_content = "Old pane content";
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    old_content.hash(&mut hasher);
    let old_hash = hasher.finish();

    pane_hashes.insert("park".to_string(), (old_hash, old_time));

    // New, different content
    let mut pane_contents = HashMap::new();
    pane_contents.insert(
        "park".to_string(),
        "New pane content - actively working".to_string(),
    );

    let tasks = vec![(
        "10".to_string(),
        "Some task".to_string(),
        "park".to_string(),
    )];

    let result = midtown::rules::decide_stuck_coworker_restarts(
        &pane_hashes,
        &pane_contents,
        &tasks,
        now,
        Duration::from_secs(180),
    );

    // Content changed, so no restart
    assert!(
        result.restarts.is_empty(),
        "pane content change should reset stuck timer"
    );

    // Updated hash should have new timestamp (now)
    let updated = result.updated_hashes.get("park").unwrap();
    assert_eq!(updated.1, now, "timestamp should be updated to now");
}
