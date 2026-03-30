use serde_json::json;

use super::*;
use crate::daemon_v2::events::{CiStatus, DomainEvent, ReviewState};
use crate::daemon_v2::projections::work::{PrState, WorkIndex};

// ── Section 3.4: CI Status Parsing ──────────────────────────────────────────

/// Spec 3.4: WHEN statusCheckRollup contains any FAILURE/TIMED_OUT/CANCELLED
/// THEN CI status SHALL be Failed
#[test]
fn ci_status_failure_on_failure_state() {
    let pr = json!({
        "statusCheckRollup": [
            { "state": "SUCCESS" },
            { "state": "FAILURE" }
        ]
    });
    assert_eq!(parse_ci_status(&pr), CiStatus::Failed);
}

#[test]
fn ci_status_failure_on_timed_out_conclusion() {
    let pr = json!({
        "statusCheckRollup": [
            { "state": "COMPLETED", "conclusion": "TIMED_OUT" }
        ]
    });
    assert_eq!(parse_ci_status(&pr), CiStatus::Failed);
}

#[test]
fn ci_status_failure_on_cancelled_conclusion() {
    let pr = json!({
        "statusCheckRollup": [
            { "state": "COMPLETED", "conclusion": "CANCELLED" }
        ]
    });
    assert_eq!(parse_ci_status(&pr), CiStatus::Failed);
}

/// Spec 3.4: WHEN statusCheckRollup contains PENDING/QUEUED/IN_PROGRESS
/// THEN CI status SHALL be Running
#[test]
fn ci_status_running_on_pending() {
    let pr = json!({
        "statusCheckRollup": [
            { "state": "SUCCESS" },
            { "state": "PENDING" }
        ]
    });
    assert_eq!(parse_ci_status(&pr), CiStatus::Running);
}

#[test]
fn ci_status_running_on_in_progress() {
    let pr = json!({
        "statusCheckRollup": [
            { "state": "IN_PROGRESS" }
        ]
    });
    assert_eq!(parse_ci_status(&pr), CiStatus::Running);
}

/// Spec 3.4: WHEN statusCheckRollup is empty or all SUCCESS THEN CI status
/// SHALL be Passed
#[test]
fn ci_status_passed_when_empty() {
    let pr = json!({
        "statusCheckRollup": []
    });
    assert_eq!(parse_ci_status(&pr), CiStatus::Passed);
}

#[test]
fn ci_status_passed_when_all_success() {
    let pr = json!({
        "statusCheckRollup": [
            { "state": "SUCCESS" },
            { "state": "SUCCESS" }
        ]
    });
    assert_eq!(parse_ci_status(&pr), CiStatus::Passed);
}

/// Spec 3.4: WHEN a PR is draft THEN needs_review SHALL be false regardless
/// of reviewDecision
#[test]
fn draft_pr_needs_review_false() {
    let json = json!([{
        "number": 99,
        "headRefName": "feat/wip",
        "isDraft": true,
        "reviewDecision": "REVIEW_REQUIRED",
        "statusCheckRollup": [{ "state": "SUCCESS" }],
        "author": { "login": "dev" }
    }]);
    let prs = parse_open_prs(&json);
    assert_eq!(prs.len(), 1);
    assert!(
        !prs[0].needs_review,
        "draft PR should have needs_review=false, got true"
    );
}

#[test]
fn parse_open_prs_extracts_fields() {
    let json = json!([
        {
            "number": 42,
            "headRefName": "feat/login",
            "isDraft": false,
            "mergeable": "MERGEABLE",
            "reviewDecision": "APPROVED",
            "statusCheckRollup": [
                { "state": "SUCCESS" }
            ],
            "author": { "login": "alice" }
        },
        {
            "number": 43,
            "headRefName": "fix/typo",
            "isDraft": true,
            "mergeable": "MERGEABLE",
            "reviewDecision": "REVIEW_REQUIRED",
            "statusCheckRollup": [],
            "author": { "login": "bob" }
        }
    ]);

    let prs = parse_open_prs(&json);
    assert_eq!(prs.len(), 2);

    assert_eq!(prs[0].number, 42);
    assert_eq!(prs[0].branch, "feat/login");
    assert_eq!(prs[0].author, "alice");
    assert!(!prs[0].is_draft);
    assert!(prs[0].ci_passed);
    assert!(prs[0].is_approved);
    assert!(!prs[0].needs_review);

    assert_eq!(prs[1].number, 43);
    assert!(prs[1].is_draft);
    // Spec 3.4: empty statusCheckRollup → Passed
    assert!(prs[1].ci_passed);
    // Spec 3.4: draft PRs → needs_review false, is_approved false
    assert!(!prs[1].is_approved);
    assert!(!prs[1].needs_review);
}

#[test]
fn parse_open_prs_handles_empty_array() {
    let json = json!([]);
    let prs = parse_open_prs(&json);
    assert!(prs.is_empty());
}

#[test]
fn parse_open_prs_handles_non_array() {
    let json = json!({"error": "not found"});
    let prs = parse_open_prs(&json);
    assert!(prs.is_empty());
}

#[test]
fn parse_merged_prs_extracts_fields() {
    let json = json!([
        {
            "number": 40,
            "headRefName": "feat/old",
            "title": "Old feature",
            "mergedAt": "2024-01-01T00:00:00Z"
        },
        {
            "number": 41,
            "headRefName": "fix/bug",
            "title": "Bug fix",
            "mergedAt": "2024-01-02T00:00:00Z"
        }
    ]);

    let prs = parse_merged_prs(&json);
    assert_eq!(prs.len(), 2);
    assert_eq!(prs[0].number, 40);
    assert_eq!(prs[0].branch, "feat/old");
    assert_eq!(prs[1].number, 41);
    assert_eq!(prs[1].branch, "fix/bug");
}

#[test]
fn parse_merged_prs_handles_empty() {
    let json = json!([]);
    assert!(parse_merged_prs(&json).is_empty());
}

#[test]
fn diff_detects_new_open_pr() {
    let work = WorkIndex::default();

    let open = vec![ParsedPr {
        number: 10,
        branch: "feat/new".into(),
        author: "alice".into(),
        is_draft: false,
        ci_passed: false,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        DomainEvent::PrOpened {
            number: 10,
            branch,
            author,
        } if branch == "feat/new" && author == "alice"
    ));
}

#[test]
fn diff_detects_merged_pr() {
    let work = WorkIndex::default();

    let merged = vec![ParsedMergedPr {
        number: 5,
        branch: "feat/done".into(),
    }];

    let events = diff_pr_state(&work, &[], &merged);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        DomainEvent::PrMerged { number: 5, branch } if branch == "feat/done"
    ));
}

#[test]
fn diff_skips_already_known_open_pr() {
    let mut work = WorkIndex::default();
    work.prs.insert(
        10,
        PrState {
            number: 10,
            branch: "feat/existing".into(),
            author: "alice".into(),
            ci_status: CiStatus::Running,
            review_state: ReviewState::None,
            is_merged: false,
            is_closed: false,
            needs_review: false,
        },
    );

    let open = vec![ParsedPr {
        number: 10,
        branch: "feat/existing".into(),
        author: "alice".into(),
        is_draft: false,
        ci_passed: false,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);
    assert!(events.is_empty());
}

#[test]
fn diff_skips_already_merged_pr() {
    let mut work = WorkIndex::default();
    work.prs.insert(
        5,
        PrState {
            number: 5,
            branch: "feat/done".into(),
            author: "alice".into(),
            ci_status: CiStatus::Passed,
            review_state: ReviewState::Approved,
            is_merged: true,
            is_closed: true,
            needs_review: false,
        },
    );

    let merged = vec![ParsedMergedPr {
        number: 5,
        branch: "feat/done".into(),
    }];

    let events = diff_pr_state(&work, &[], &merged);
    assert!(events.is_empty());
}

#[test]
fn diff_detects_review_requested() {
    let mut work = WorkIndex::default();
    work.prs.insert(
        10,
        PrState {
            number: 10,
            branch: "feat/review".into(),
            author: "alice".into(),
            ci_status: CiStatus::Passed,
            review_state: ReviewState::None,
            is_merged: false,
            is_closed: false,
            needs_review: false,
        },
    );

    let open = vec![ParsedPr {
        number: 10,
        branch: "feat/review".into(),
        author: "alice".into(),
        is_draft: false,
        ci_passed: true,
        is_approved: false,
        needs_review: true,
    }];

    let events = diff_pr_state(&work, &open, &[]);
    // Should get PrUpdated (review state changed) and PrReviewRequested
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DomainEvent::PrReviewRequested { number: 10 }))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        DomainEvent::PrUpdated {
            number: 10,
            review_state: ReviewState::Pending,
            ..
        }
    )));
}

#[test]
fn diff_detects_ci_status_change() {
    let mut work = WorkIndex::default();
    work.prs.insert(
        10,
        PrState {
            number: 10,
            branch: "feat/ci".into(),
            author: "alice".into(),
            ci_status: CiStatus::Running,
            review_state: ReviewState::None,
            is_merged: false,
            is_closed: false,
            needs_review: false,
        },
    );

    let open = vec![ParsedPr {
        number: 10,
        branch: "feat/ci".into(),
        author: "alice".into(),
        is_draft: false,
        ci_passed: true,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);
    assert_eq!(events.len(), 1);
    assert!(matches!(
        &events[0],
        DomainEvent::PrUpdated {
            number: 10,
            ci_status: CiStatus::Passed,
            ..
        }
    ));
}
