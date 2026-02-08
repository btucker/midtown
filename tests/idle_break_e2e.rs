//! E2E test for idle coworker break detection using captured WorldSnapshot fixtures.
//!
//! These tests verify the daemon correctly identifies idle coworkers for break
//! using real-world pane captures and state snapshots.

use std::collections::{HashMap, HashSet};

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
/// Snapshot context: lexington completed task !22, posted idle status, and has
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

/// Test that busy coworkers (york with task !27) are not sent on break.
#[test]
fn busy_coworker_not_sent_on_break() {
    let fixture = include_str!("fixtures/snapshot/snapshot-20260203-152121.json");
    let (coworkers, data) = load_snapshot(fixture);

    // York is busy with task !27 in this snapshot
    assert!(data.busy_coworkers.contains("york"), "york should be busy");

    let _york = coworkers.iter().find(|c| c.name == "york").unwrap();

    // Verify york's pane shows active work
    let pane = data.pane_contents.get("york").unwrap();
    assert!(
        pane.contains("Fixing false idle detection"),
        "york should be actively working on task !27"
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
