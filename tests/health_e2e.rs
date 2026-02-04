//! E2E tests for daemon health check behavior using captured WorldSnapshot fixtures.
//!
//! These tests verify:
//! - Stuck coworker detection conditions
//! - Idle shutdown eligibility conditions
//! - Compaction/UI stuck conditions
//! - Usage limit detection conditions
//! - Zombie (blank pane) detection conditions
//!
//! The tests analyze captured snapshots to verify the daemon would make correct
//! health decisions. The actual decision logic is unit-tested in `rules.rs`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde_json::Value;

// ─────────────────────────────────────────────────────────────────────────────
// Test Data Structures
// ─────────────────────────────────────────────────────────────────────────────

/// Parsed coworker data from snapshot.
#[derive(Debug, Clone)]
struct TestCoworker {
    name: String,
    started_at: DateTime<Utc>,
    isolated_tasks: bool,
}

/// Parsed snapshot data for health check tests.
#[derive(Debug)]
#[allow(dead_code)] // Fields kept for completeness, used in Debug output
struct HealthSnapshot {
    coworkers: Vec<TestCoworker>,
    busy_coworkers: HashSet<String>,
    coworkers_with_open_prs: HashSet<String>,
    active_reviewers: HashSet<String>,
    coworkers_with_unblocked_deps: HashSet<String>,
    coworkers_with_running_subagents: HashSet<String>,
    ci_passed_pr_coworkers: HashSet<String>,
    blank_pane_coworkers: HashSet<String>,
    pane_contents: HashMap<String, String>,
    coworker_start_times: HashMap<String, DateTime<Utc>>,
    in_progress_tasks: Vec<(String, String, String)>,
    now_utc: DateTime<Utc>,
}

/// Load a snapshot fixture and parse into health test data structures.
fn load_health_snapshot(json_str: &str) -> HealthSnapshot {
    let v: Value = serde_json::from_str(json_str).expect("valid JSON");

    let coworkers: Vec<TestCoworker> = v["coworker_snapshots"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|cw| TestCoworker {
            name: cw["name"].as_str().unwrap_or("").to_string(),
            started_at: DateTime::parse_from_rfc3339(
                cw["started_at"].as_str().unwrap_or("2026-01-01T00:00:00Z"),
            )
            .unwrap()
            .with_timezone(&Utc),
            isolated_tasks: cw["isolated_tasks"].as_bool().unwrap_or(false),
        })
        .collect();

    let extract_string_set = |key: &str| -> HashSet<String> {
        v[key]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|s| s.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    let pane_contents: HashMap<String, String> = v["pane_contents"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();

    let coworker_start_times: HashMap<String, DateTime<Utc>> = v["coworker_start_times"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| {
                    v.as_str().and_then(|s| {
                        DateTime::parse_from_rfc3339(s)
                            .ok()
                            .map(|dt| (k.clone(), dt.with_timezone(&Utc)))
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let in_progress_tasks: Vec<(String, String, String)> = v["in_progress_tasks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|task| {
                    let arr = task.as_array()?;
                    Some((
                        arr.first()?.as_str()?.to_string(),
                        arr.get(1)?.as_str()?.to_string(),
                        arr.get(2)?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let now_utc = v["now_utc"]
        .as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    HealthSnapshot {
        coworkers,
        busy_coworkers: extract_string_set("busy_coworkers"),
        coworkers_with_open_prs: extract_string_set("coworkers_with_open_prs"),
        active_reviewers: extract_string_set("active_reviewers"),
        coworkers_with_unblocked_deps: extract_string_set("coworkers_with_unblocked_deps"),
        coworkers_with_running_subagents: extract_string_set("coworkers_with_running_subagents"),
        ci_passed_pr_coworkers: extract_string_set("ci_passed_pr_coworkers"),
        blank_pane_coworkers: extract_string_set("blank_pane_coworkers"),
        pane_contents,
        coworker_start_times,
        in_progress_tasks,
        now_utc,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Pane Content Analysis Helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Check if pane content indicates a subagent (Task tool) is running.
/// This mirrors the logic in `rules::has_running_subagent`.
///
/// Note: Claude Code TUI uses ✻ (U+273B TEARDROP-SPOKED ASTERISK) for the whirlpool.
fn pane_has_running_subagent(pane_content: &str) -> bool {
    for line in pane_content.lines() {
        let trimmed = line.trim();
        // Whirlpool indicator: ✻ followed by task description
        // (shows "Cogitated", "Worked", "Cooked" etc. during/after subagent runs)
        if trimmed.starts_with('✻') {
            return true;
        }
        // Running Task agents indicator
        if trimmed.contains("Running") && trimmed.contains("Task agent") {
            return true;
        }
    }
    false
}

/// Check if pane content indicates the coworker is stuck in compaction.
/// This mirrors the detection logic in `rules::detect_compaction_stuck`.
fn pane_shows_compaction(pane_content: &str) -> bool {
    for line in pane_content.lines() {
        let trimmed = line.trim();
        // Compaction shows whirlpool with "Compacting" text
        if trimmed.starts_with('✻') && trimmed.contains("Compacting") {
            return true;
        }
        // Also check for baking (similar to compaction)
        if trimmed.starts_with('✻') && trimmed.contains("baking") {
            return true;
        }
    }
    false
}

/// Check if pane content shows usage limit screen.
/// The daemon looks for "/upgrade" as a marker.
fn pane_shows_usage_limit(pane_content: &str) -> bool {
    pane_content.contains("/upgrade")
}

/// Check if pane content indicates queued nudges in the input box.
/// Looks for input prompt (❯) followed by text that wasn't submitted.
fn pane_has_queued_input(pane_content: &str) -> bool {
    // Look for lines starting with ❯ that have text after them
    for line in pane_content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('❯') {
            let rest = rest.trim();
            if !rest.is_empty() {
                return true;
            }
        }
    }
    false
}

// ─────────────────────────────────────────────────────────────────────────────
// Fixture-Based Health Check Tests
// ─────────────────────────────────────────────────────────────────────────────

/// Test idle coworker conditions in snapshot-20260203-152121.
///
/// Expected behavior:
/// - York is busy (has task #27) → NOT eligible for idle shutdown
/// - Lexington is idle (completed task, at prompt) → eligible for idle shutdown
/// - Coworkers with open PRs → NOT eligible for idle shutdown
/// - Broadway is active reviewer → NOT eligible for idle shutdown
#[test]
fn fixture_idle_detection_snapshot_152121() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_health_snapshot(fixture);

    // ─── York: Busy with task #27, should NOT be sent on break ───
    assert!(
        snap.busy_coworkers.contains("york"),
        "York should be busy with task #27"
    );

    // Verify york's pane shows active work (not idle)
    let york_pane = snap.pane_contents.get("york").expect("york pane exists");
    assert!(
        york_pane.contains("Fixing false idle detection"),
        "York should be working on task #27"
    );

    // ─── Lexington: Idle, should be sent on break ───
    assert!(
        !snap.busy_coworkers.contains("lexington"),
        "Lexington should not be busy"
    );
    assert!(
        !snap.coworkers_with_open_prs.contains("lexington"),
        "Lexington should not have open PR"
    );
    assert!(
        !snap.active_reviewers.contains("lexington"),
        "Lexington should not be an active reviewer"
    );

    let lexington_pane = snap
        .pane_contents
        .get("lexington")
        .expect("lexington pane exists");
    assert!(
        lexington_pane.contains("work done for now"),
        "Lexington's pane should show idle message"
    );

    // Verify lexington has been running long enough (>5 min minimum lifetime)
    let lexington = snap
        .coworkers
        .iter()
        .find(|c| c.name == "lexington")
        .unwrap();
    let lifetime = snap.now_utc.signed_duration_since(lexington.started_at);
    assert!(
        lifetime > chrono::Duration::minutes(5),
        "Lexington should have run >5 minutes to be eligible for idle break"
    );

    // ─── Coworkers with open PRs: Protected from idle shutdown ───
    for name in &["amsterdam", "columbus", "vernon"] {
        assert!(
            snap.coworkers_with_open_prs.contains(*name),
            "{} should have an open PR (protected from idle shutdown)",
            name
        );
    }

    // ─── Broadway: Active reviewer, protected ───
    assert!(
        snap.active_reviewers.contains("broadway"),
        "Broadway should be an active reviewer (protected from idle shutdown)"
    );
}

/// Test subagent detection in pane content.
///
/// When a coworker has a running subagent (Task tool), the daemon should NOT
/// send them on break even if they appear idle. The pane shows indicators like
/// "✽ Cogitated" or "Running X Task agents".
#[test]
fn subagent_detection_in_pane_content() {
    // Whirlpool indicator (subagent thinking)
    let pane_with_whirlpool = "✻ Cogitated for 2m 30s\n\n❯ ";
    assert!(
        pane_has_running_subagent(pane_with_whirlpool),
        "Should detect subagent from whirlpool indicator"
    );

    // Running Task agents indicator
    let pane_with_task_agents = "Running 3 Task agents\n\n⏺ Working...";
    assert!(
        pane_has_running_subagent(pane_with_task_agents),
        "Should detect subagent from 'Running X Task agent' text"
    );

    // Normal pane without subagent
    let normal_pane = "⏺ Working on task\n\n❯ cargo build";
    assert!(
        !pane_has_running_subagent(normal_pane),
        "Should not detect subagent in normal pane"
    );

    // Completed whirlpool (not currently running)
    let completed_whirlpool = "✻ Worked for 5m 30s\n\n⏺ Done with task";
    assert!(
        pane_has_running_subagent(completed_whirlpool),
        "Whirlpool indicator still present = subagent context (may be between steps)"
    );
}

/// Test compaction detection in pane content.
///
/// Compaction shows "✻ Compacting conversation…" and the daemon should detect
/// this to avoid false idle detection and potentially interrupt if stuck.
#[test]
fn compaction_detection_in_pane_content() {
    // Active compaction
    let compacting_pane = "✻ Compacting conversation… (5m 30s elapsed)\n\n❯ ";
    assert!(
        pane_shows_compaction(compacting_pane),
        "Should detect compaction from whirlpool + 'Compacting' text"
    );

    // Normal pane (not compacting)
    let normal_pane = "⏺ Working on task\n\n❯ ";
    assert!(
        !pane_shows_compaction(normal_pane),
        "Should not detect compaction in normal working pane"
    );

    // Pane with regular whirlpool (subagent, not compaction)
    let subagent_pane = "✻ Cogitated for 2m\n\n⏺ Analyzing code...";
    assert!(
        !pane_shows_compaction(subagent_pane),
        "Cogitation whirlpool is NOT compaction"
    );
}

/// Test usage limit detection in pane content.
///
/// Usage limits show "/upgrade" on screen. The daemon detects this and schedules
/// nudges for when the limit resets.
#[test]
fn usage_limit_detection_in_pane_content() {
    // Actual usage limit screen
    let limit_pane = "Your usage limit has been reached.\nPlease wait or /upgrade your plan.";
    assert!(
        pane_shows_usage_limit(limit_pane),
        "Should detect usage limit from /upgrade text"
    );

    // Normal pane
    let normal_pane = "⏺ Working on task #42\n\n❯ ";
    assert!(
        !pane_shows_usage_limit(normal_pane),
        "Should not detect usage limit in normal pane"
    );

    // Code with /upgrade string (known false positive limitation)
    let code_pane = r#"fn show_upgrade_prompt() {
    println!("/upgrade your account");
}"#;
    // NOTE: Current implementation will match this (false positive)
    // This test documents the known limitation
    assert!(
        pane_shows_usage_limit(code_pane),
        "Current implementation triggers on code containing /upgrade (known limitation)"
    );
}

/// Test queued input detection in pane content.
///
/// When nudges get sent but the coworker is busy (compacting, processing),
/// they queue up in the input box. The daemon detects this to auto-submit
/// daemon-sent nudges or leave alone user-typed input.
#[test]
fn queued_input_detection_in_pane_content() {
    // Queued nudges in input
    let queued_pane = r#"
⏺ Previous output

❯ github said: @york Check 'Build' passed
❯ github said: @york Check 'Test' passed
───────────────────────────────────────
"#;
    assert!(
        pane_has_queued_input(queued_pane),
        "Should detect queued input from ❯ lines with text"
    );

    // Empty input (ready for new input)
    let ready_pane = "⏺ Done with task\n\n❯ \n───────────────────────";
    assert!(
        !pane_has_queued_input(ready_pane),
        "Empty ❯ prompt should not be detected as queued"
    );
}

/// Test zombie detection conditions.
///
/// A zombie is a coworker with a completely blank pane that has been running
/// long enough (>20 seconds) that it should have rendered something.
#[test]
fn zombie_detection_conditions() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_health_snapshot(fixture);

    // This snapshot should have NO blank pane coworkers (all healthy)
    assert!(
        snap.blank_pane_coworkers.is_empty(),
        "Snapshot should have no zombie coworkers (all panes have content)"
    );

    // Verify all coworkers have pane content
    for cw in &snap.coworkers {
        let pane = snap.pane_contents.get(&cw.name);
        assert!(
            pane.is_some() && !pane.unwrap().trim().is_empty(),
            "Coworker {} should have non-blank pane content",
            cw.name
        );
    }
}

/// Test coworker age calculations for health decisions.
///
/// Health decisions have minimum age requirements:
/// - Minimum lifetime (5 min) before eligible for idle shutdown
/// - Minimum age (20 sec) before zombie detection
/// - Minimum age (60 sec) before queued nudge detection
#[test]
fn coworker_age_requirements() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_health_snapshot(fixture);

    for cw in &snap.coworkers {
        let age = snap.now_utc.signed_duration_since(cw.started_at);

        // All coworkers in this snapshot should be old enough for health checks
        assert!(
            age > chrono::Duration::seconds(20),
            "Coworker {} age ({:?}) should be > 20s for zombie detection",
            cw.name,
            age
        );

        // Most should be old enough for idle shutdown eligibility
        // (though other conditions may still protect them)
        if age < chrono::Duration::minutes(5) {
            println!(
                "Note: {} is young ({:?}), protected by minimum lifetime",
                cw.name, age
            );
        }
    }
}

/// Test that busy coworkers are correctly identified from tasks.
#[test]
fn busy_coworker_identification() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_health_snapshot(fixture);

    // Verify busy_coworkers matches in_progress_tasks owners
    for (task_id, _subject, owner) in &snap.in_progress_tasks {
        assert!(
            snap.busy_coworkers.contains(owner),
            "Task {} owner '{}' should be in busy_coworkers",
            task_id,
            owner
        );
    }

    // York has task #27
    assert!(
        snap.in_progress_tasks
            .iter()
            .any(|(id, _, owner)| id == "27" && owner == "york"),
        "York should own in-progress task #27"
    );
}

/// Test that isolated coworkers (reviewers) are correctly identified.
#[test]
fn isolated_coworker_identification() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_health_snapshot(fixture);

    // Broadway is isolated (reviewer)
    let broadway = snap
        .coworkers
        .iter()
        .find(|c| c.name == "broadway")
        .unwrap();
    assert!(
        broadway.isolated_tasks,
        "Broadway should have isolated_tasks=true (is a reviewer)"
    );

    // Other coworkers are not isolated
    for cw in &snap.coworkers {
        if cw.name != "broadway" {
            assert!(
                !cw.isolated_tasks,
                "{} should not have isolated_tasks (is a developer)",
                cw.name
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Snapshot Sanity Tests
// ─────────────────────────────────────────────────────────────────────────────

/// Verify the test snapshot loads correctly with all expected fields.
#[test]
fn snapshot_loads_correctly() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let snap = load_health_snapshot(fixture);

    assert!(!snap.coworkers.is_empty(), "Should have coworkers");
    assert!(!snap.pane_contents.is_empty(), "Should have pane contents");
    assert!(
        !snap.coworker_start_times.is_empty(),
        "Should have start times"
    );

    // Verify all coworkers have start times
    for cw in &snap.coworkers {
        assert!(
            snap.coworker_start_times.contains_key(&cw.name),
            "Coworker {} should have a start time",
            cw.name
        );
    }

    // Verify all coworkers have pane content
    for cw in &snap.coworkers {
        assert!(
            snap.pane_contents.contains_key(&cw.name),
            "Coworker {} should have pane content",
            cw.name
        );
    }
}

/// Test snapshot with multiple health scenarios.
#[test]
fn fixture_health_check_snapshot_182216() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-182216.json");
    let snap = load_health_snapshot(fixture);

    // This snapshot has 9 coworkers - verify they all loaded
    assert!(
        snap.coworkers.len() >= 9,
        "Should have at least 9 coworkers"
    );

    // Check for active reviewers
    assert!(
        !snap.active_reviewers.is_empty(),
        "Should have active reviewers"
    );

    // All coworkers should have pane content
    for cw in &snap.coworkers {
        assert!(
            snap.pane_contents.contains_key(&cw.name),
            "Coworker {} should have pane content",
            cw.name
        );
    }
}
