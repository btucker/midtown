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

/// Spec 3.1: WHEN polling detects a new open PR not already tracked THEN the
/// system SHALL emit a PrOpened event as a backstop
#[test]
fn diff_detects_new_open_pr() {
    let work = WorkIndex::default();

    let open = vec![ParsedPr {
        number: 10,
        branch: "feat/new".into(),
        title: "feat: new thing".into(),
        author: "alice".into(),
        is_draft: false,
        ci_passed: false,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);
    // PrOpened + PrReviewRequested (non-draft PR)
    assert_eq!(events.len(), 2);
    assert!(matches!(
        &events[0],
        DomainEvent::PrOpened {
            number: 10,
            branch,
            author,
        } if branch == "feat/new" && author == "alice"
    ));
    assert!(matches!(
        &events[1],
        DomainEvent::PrReviewRequested { number: 10 }
    ));
}

/// Spec 3.1: WHEN polling detects a merged PR not already tracked THEN the
/// system SHALL emit a PrMerged event as a backstop
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
            midtown_author: None,
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
        title: "feat: existing".into(),
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
            midtown_author: None,
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

/// Spec 3.1: WHEN polling detects a CI or review state change not already
/// reflected THEN the system SHALL emit a PrUpdated/PrReviewRequested event
#[test]
fn diff_detects_review_requested() {
    let mut work = WorkIndex::default();
    work.prs.insert(
        10,
        PrState {
            number: 10,
            branch: "feat/review".into(),
            author: "alice".into(),
            midtown_author: None,
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
        title: "feat: review".into(),
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

/// Spec 3.1: WHEN polling detects a CI state change THEN the system SHALL
/// emit a PrUpdated event as a backstop
#[test]
fn diff_detects_ci_status_change() {
    let mut work = WorkIndex::default();
    work.prs.insert(
        10,
        PrState {
            number: 10,
            branch: "feat/ci".into(),
            author: "alice".into(),
            midtown_author: None,
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
        title: "feat: ci".into(),
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

// ── Section 15 Critical: Rate Limit Monitoring ──────────────────────────

/// parse_rate_limit extracts remaining/limit/reset from GitHub API response
#[test]
fn parse_rate_limit_extracts_fields() {
    let json = json!({
        "resources": {
            "core": {
                "limit": 5000,
                "remaining": 4999,
                "reset": 1999999999u64,
                "used": 1
            }
        }
    });
    let status = parse_rate_limit(&json).expect("should parse");
    assert_eq!(status.limit, 5000);
    assert_eq!(status.remaining, 4999);
}

/// should_throttle returns true when remaining < 10% of limit
#[test]
fn throttle_when_remaining_below_10_percent() {
    let status = RateLimitStatus {
        remaining: 400,
        limit: 5000,
        reset_in_secs: 3600,
    };
    // 10% of 5000 = 500, remaining 400 < 500 → should throttle
    assert!(
        should_throttle(&status),
        "400/5000 is below 10% threshold (500)"
    );
}

#[test]
fn no_throttle_when_remaining_above_threshold() {
    let status = RateLimitStatus {
        remaining: 4000,
        limit: 5000,
        reset_in_secs: 3600,
    };
    assert!(
        !should_throttle(&status),
        "4000/5000 is well above 10% threshold"
    );
}

#[test]
fn throttle_when_nearly_exhausted() {
    let status = RateLimitStatus {
        remaining: 10,
        limit: 5000,
        reset_in_secs: 300,
    };
    assert!(
        should_throttle(&status),
        "10/5000 is well below 10% threshold"
    );
}

// ── Spec 3.2: Polling backstop should request review for new non-draft PRs ──

/// Spec 3.2: WHEN polling detects a new non-draft PR THEN PrReviewRequested SHALL be emitted
#[test]
fn new_non_draft_pr_emits_review_requested() {
    let work = WorkIndex::default();
    let open = vec![ParsedPr {
        number: 42,
        branch: "feat/foo".into(),
        title: "feat: foo".into(),
        author: "ghost-town".into(),
        is_draft: false,
        ci_passed: false,
        is_approved: false,
        needs_review: false, // reviewDecision is null (no branch protection)
    }];

    let events = diff_pr_state(&work, &open, &[]);

    assert!(
        events
            .iter()
            .any(|e| matches!(e, DomainEvent::PrReviewRequested { number: 42 })),
        "new non-draft PR should emit PrReviewRequested even without reviewDecision, got {:?}",
        events
    );
}

/// Spec 3.2: WHEN polling detects a new draft PR THEN PrReviewRequested SHALL NOT be emitted
#[test]
fn new_draft_pr_does_not_emit_review_requested() {
    let work = WorkIndex::default();
    let open = vec![ParsedPr {
        number: 42,
        branch: "feat/foo".into(),
        title: "feat: foo".into(),
        author: "ghost-town".into(),
        is_draft: true,
        ci_passed: false,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DomainEvent::PrReviewRequested { .. })),
        "draft PR should NOT emit PrReviewRequested, got {:?}",
        events
    );
}

/// Spec 3.1: WHEN PrOpened is processed AND title contains [Midtown !N] THEN PrLinkedToTask
/// using the task ID from the title, even when the GitHub author doesn't match the agent name
#[test]
fn pr_opened_links_to_task_via_title() {
    let mut work = WorkIndex::default();
    // Create task !1 with agent named "proving-ground"
    work.apply(&DomainEvent::TaskCreated {
        id: "1".into(),
        subject: "Build feature".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: Some("proving-ground".into()),
        icon: None,
        color: None,
        parent: None,
        thread_id: None,
        message_id: None,
    });
    work.apply(&DomainEvent::TaskAssigned {
        task_id: "1".into(),
        agent_id: "agent-1".into(),
    });

    // PR opened by GitHub user "btucker" (not "proving-ground"), but title contains [Midtown !1]
    let open = vec![ParsedPr {
        number: 42,
        branch: "feat/foo".into(),
        title: "feat: add auth endpoint [Midtown !1]".into(),
        author: "btucker".into(),
        is_draft: false,
        ci_passed: false,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);

    assert!(
        events.iter().any(
            |e| matches!(e, DomainEvent::PrLinkedToTask { number: 42, task_id } if task_id == "1")
        ),
        "PR with [Midtown !1] in title should link to task 1 regardless of GitHub author, got {:?}",
        events
    );
}

/// PR title without [Midtown !N] should not link to any task
#[test]
fn pr_opened_without_midtown_title_does_not_link() {
    let mut work = WorkIndex::default();
    work.apply(&DomainEvent::TaskCreated {
        id: "1".into(),
        subject: "Build feature".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: Some("proving-ground".into()),
        icon: None,
        color: None,
        parent: None,
        thread_id: None,
        message_id: None,
    });
    work.apply(&DomainEvent::TaskAssigned {
        task_id: "1".into(),
        agent_id: "agent-1".into(),
    });

    let open = vec![ParsedPr {
        number: 42,
        branch: "feat/foo".into(),
        title: "feat: some unrelated PR".into(),
        author: "btucker".into(),
        is_draft: false,
        ci_passed: false,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DomainEvent::PrLinkedToTask { .. })),
        "PR without [Midtown !N] in title should not link to any task, got {:?}",
        events
    );
}

/// Spec 3.1: WHEN PrOpened AND branch matches task worktree THEN PrLinkedToTask (fallback)
#[test]
fn pr_opened_links_to_task_by_branch_fallback() {
    let mut work = WorkIndex::default();
    work.apply(&DomainEvent::TaskCreated {
        id: "t1".into(),
        subject: "Build feature".into(),
        channel: "main".into(),
        blocked_by: vec![],
        agent_type: None,
        agent_name: Some("ghost-town".into()),
        icon: None,
        color: None,
        parent: None,
        thread_id: None,
        message_id: None,
    });
    work.apply(&DomainEvent::TaskAssigned {
        task_id: "t1".into(),
        agent_id: "agent-1".into(),
    });

    // No [Midtown !N] in title, but branch matches worktree convention
    let open = vec![ParsedPr {
        number: 42,
        branch: "task-t1-build-feature".into(),
        title: "feat: build feature".into(),
        author: "btucker".into(),
        is_draft: false,
        ci_passed: false,
        is_approved: false,
        needs_review: false,
    }];

    let events = diff_pr_state(&work, &open, &[]);

    assert!(
        events.iter().any(
            |e| matches!(e, DomainEvent::PrLinkedToTask { number: 42, task_id } if task_id == "t1")
        ),
        "PR with task branch should emit PrLinkedToTask as fallback when title has no [Midtown !N], got {:?}",
        events
    );
}
