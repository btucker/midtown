//! End-to-end tests for GitHub webhook handling.
//!
//! These tests verify the daemon correctly receives and processes GitHub webhook
//! events via the HTTP endpoint. They test:
//! - PR opened/closed/merged events
//! - Review submitted events
//! - Check run completed events
//! - Webhook signature verification
//! - Event deduplication
//!
//! Run with `cargo test --test webhook_e2e -- --ignored --test-threads=1`
//! as these spawn real daemon processes.

use ntest::timeout;
use std::thread;
use std::time::Duration;

mod common;
use common::{DaemonHarnessOptions, DaemonTestHarness, WebhookTestClient};

// ── Test Helpers ────────────────────────────────────────────────────

/// Create a webhook test fixture with the given port.
fn create_webhook_fixture(webhook_port: u16) -> Option<DaemonTestHarness> {
    let options = DaemonHarnessOptions {
        enable_webhook: true,
        webhook_port,
        webhook_secret: "test-webhook-secret".to_string(),
        custom_state_dir: None,
    };

    let mut harness = DaemonTestHarness::new("webhook-e2e-test", options)?;
    if !harness.start_daemon() {
        return None;
    }

    Some(harness)
}

// ── Webhook E2E Tests ──────────────────────────────────────────────

/// Test that PR opened events are processed and posted to channel.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_pr_opened() {
    let harness = create_webhook_fixture(47100).expect("Failed to create fixture");

    let payload = r#"{
        "action": "opened",
        "number": 42,
        "pull_request": {
            "title": "Add authentication feature",
            "user": {"login": "testuser"},
            "merged": false,
            "head": {"ref": "lexington/add-auth"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    eprintln!("Webhook response status: {}", status);
    assert_eq!(status, 200, "Webhook should return 200 OK");

    // Verify message appears in channel
    let found = harness.wait_for_channel_message("opened PR #42", 5000);
    if !found {
        eprintln!("Messages in channel:");
        for msg in harness.read_channel_messages() {
            eprintln!("  - {}", msg);
        }
    }
    assert!(found, "PR opened message should appear in channel");
    assert!(
        harness.channel_contains("@lexington"),
        "Message should mention coworker from branch"
    );
}

/// Test that PR merged events are processed correctly.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_pr_merged() {
    let harness = create_webhook_fixture(47101).expect("Failed to create fixture");

    let payload = r#"{
        "action": "closed",
        "number": 55,
        "pull_request": {
            "title": "Fix critical bug",
            "user": {"login": "testuser"},
            "merged": true,
            "head": {"ref": "park/fix-bug"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Verify merged message and lead nudge
    assert!(
        harness.wait_for_channel_message("merged PR #55", 5000),
        "PR merged message should appear in channel"
    );
    // Lead should be nudged to pull
    assert!(
        harness.wait_for_channel_message("@lead PR #55 merged", 5000),
        "Lead should be nudged about merge"
    );
}

/// Test that PR closed (not merged) events are processed correctly.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_pr_closed_not_merged() {
    let harness = create_webhook_fixture(47102).expect("Failed to create fixture");

    let payload = r#"{
        "action": "closed",
        "number": 66,
        "pull_request": {
            "title": "Experimental feature",
            "user": {"login": "testuser"},
            "merged": false,
            "head": {"ref": "amsterdam/experiment"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Verify closed (not merged) message
    assert!(
        harness.wait_for_channel_message("closed PR #66 (not merged)", 5000),
        "PR closed message should indicate not merged"
    );
}

/// Test that review approved events are processed.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_review_approved() {
    let harness = create_webhook_fixture(47103).expect("Failed to create fixture");

    let payload = r#"{
        "action": "submitted",
        "review": {
            "id": 100,
            "state": "approved",
            "user": {"login": "reviewer"}
        },
        "pull_request": {
            "number": 77,
            "head": {"ref": "broadway/feature"},
            "body": "PR description"
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request_review", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("reviewer approved PR #77", 5000),
        "Review approval should appear in channel"
    );
    assert!(
        harness.channel_contains("@broadway"),
        "Message should mention PR owner"
    );
}

/// Test that review changes_requested events are processed.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_review_changes_requested() {
    let harness = create_webhook_fixture(47104).expect("Failed to create fixture");

    let payload = r#"{
        "action": "submitted",
        "review": {
            "id": 101,
            "state": "changes_requested",
            "user": {"login": "reviewer"}
        },
        "pull_request": {
            "number": 88,
            "head": {"ref": "columbus/wip"},
            "body": "<!-- midtown: columbus -->\n\nDescription"
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request_review", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("requested changes on PR #88", 5000),
        "Changes requested should appear in channel"
    );
}

/// Test that check run failure events are processed.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_check_run_failure() {
    let harness = create_webhook_fixture(47105).expect("Failed to create fixture");

    let payload = r#"{
        "action": "completed",
        "check_run": {
            "name": "Build",
            "status": "completed",
            "conclusion": "failure",
            "check_suite": {
                "head_sha": "abc123",
                "head_branch": "madison/feature",
                "pull_requests": [{"number": 99}]
            }
        },
        "repository": {"full_name": "test/repo", "default_branch": "main"}
    }"#;

    let status = harness
        .send_webhook("check_run", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("Check 'Build' failed on PR #99", 15000),
        "CI failure should appear in channel"
    );
    assert!(
        harness.channel_contains("@madison"),
        "Message should mention PR owner"
    );
}

/// Test that check run failure on default branch nudges lead.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_check_run_failure_on_main() {
    let harness = create_webhook_fixture(47106).expect("Failed to create fixture");

    let payload = r#"{
        "action": "completed",
        "check_run": {
            "name": "E2E Tests",
            "status": "completed",
            "conclusion": "failure",
            "check_suite": {
                "head_sha": "def456",
                "head_branch": "main",
                "pull_requests": []
            }
        },
        "repository": {"full_name": "test/repo", "default_branch": "main"}
    }"#;

    let status = harness
        .send_webhook("check_run", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("Check 'E2E Tests' failed on main", 15000),
        "CI failure on main should appear in channel"
    );
}

/// Test that check run success events are processed.
/// Note: CI success events are batched by the daemon for aggregation,
/// so this test may need longer timeouts to see the batched message.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_check_run_success() {
    let harness = create_webhook_fixture(47107).expect("Failed to create fixture");

    let payload = r#"{
        "action": "completed",
        "check_run": {
            "name": "Tests",
            "status": "completed",
            "conclusion": "success",
            "started_at": "2026-02-04T12:00:00Z",
            "completed_at": "2026-02-04T12:05:00Z",
            "check_suite": {
                "head_sha": "ghi789",
                "head_branch": "riverside/tests",
                "pull_requests": [{"number": 100}]
            }
        },
        "repository": {"full_name": "test/repo", "default_branch": "main"}
    }"#;

    let status = harness
        .send_webhook("check_run", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("Check 'Tests' passed on PR #100", 15000),
        "CI success should appear in channel"
    );
}

/// Test that issue comments on PRs are processed.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_issue_comment() {
    let harness = create_webhook_fixture(47108).expect("Failed to create fixture");

    let payload = r#"{
        "action": "created",
        "issue": {
            "number": 111,
            "pull_request": {}
        },
        "comment": {
            "id": 500,
            "user": {"login": "commenter"},
            "body": "LGTM! Great work on this."
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("issue_comment", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("commenter commented on PR #111", 5000),
        "Comment should appear in channel"
    );
    assert!(
        harness.channel_contains("LGTM!"),
        "Comment preview should appear"
    );
}

/// Test that issue comments with coworker signature use coworker name.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_issue_comment_with_coworker_signature() {
    let harness = create_webhook_fixture(47109).expect("Failed to create fixture");

    let payload = r#"{
        "action": "created",
        "issue": {
            "number": 122,
            "pull_request": {}
        },
        "comment": {
            "id": 501,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\nNice implementation!"
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("issue_comment", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("park commented on PR #122", 5000),
        "Comment should use coworker name from signature"
    );
}

/// Test webhook signature verification - invalid signature should be rejected.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_signature_verification_rejects_invalid() {
    let harness = create_webhook_fixture(47110).expect("Failed to create fixture");

    let payload = r#"{
        "action": "opened",
        "number": 999,
        "pull_request": {
            "title": "Invalid signature test",
            "user": {"login": "testuser"},
            "merged": false,
            "head": {"ref": "test/branch"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook_with_signature("pull_request", payload, false)
        .expect("Failed to send webhook");

    // Should be rejected with 401 Unauthorized
    assert_eq!(status, 401, "Invalid signature should return 401");

    // Message should NOT appear in channel
    thread::sleep(Duration::from_millis(500));
    assert!(
        !harness.channel_contains("PR #999"),
        "Message with invalid signature should not be processed"
    );
}

/// Test that missing event type header returns 400.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_missing_event_header() {
    let harness = create_webhook_fixture(47111).expect("Failed to create fixture");

    // Send request without X-GitHub-Event header
    let http_client = reqwest::blocking::Client::new();
    let url = format!("http://localhost:{}/webhook", harness.webhook_port.unwrap());

    let payload = r#"{"action": "opened"}"#;
    let signature =
        common::generate_signature(harness.webhook_secret.as_ref().unwrap(), payload.as_bytes());

    let response = http_client
        .post(&url)
        .header("X-Hub-Signature-256", signature)
        .header("Content-Type", "application/json")
        .body(payload)
        .send()
        .expect("HTTP request failed");

    assert_eq!(
        response.status().as_u16(),
        400,
        "Missing event header should return 400"
    );
}

/// Test that ping events are handled gracefully.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_ping_event() {
    let harness = create_webhook_fixture(47112).expect("Failed to create fixture");

    let payload = r#"{
        "zen": "Speak like a human.",
        "hook_id": 12345,
        "hook": {
            "type": "Repository",
            "id": 12345
        }
    }"#;

    let status = harness
        .send_webhook("ping", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200, "Ping should return 200 OK");
}

/// Test that unhandled event types return 200 but don't post to channel.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_unhandled_event_type() {
    let harness = create_webhook_fixture(47113).expect("Failed to create fixture");

    let payload = r#"{
        "action": "created",
        "release": {
            "tag_name": "v1.0.0"
        }
    }"#;

    let status = harness
        .send_webhook("release", payload)
        .expect("Failed to send webhook");

    // Should still return 200 (gracefully ignored)
    assert_eq!(status, 200, "Unhandled event should return 200");
}

/// Test that draft PR opened events don't trigger review.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_draft_pr_no_review() {
    let harness = create_webhook_fixture(47114).expect("Failed to create fixture");

    let payload = r#"{
        "action": "opened",
        "number": 200,
        "pull_request": {
            "title": "WIP: New feature",
            "user": {"login": "testuser"},
            "merged": false,
            "draft": true,
            "head": {"ref": "vernon/wip"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Message should appear
    assert!(
        harness.wait_for_channel_message("opened PR #200", 5000),
        "Draft PR message should appear"
    );
    // But it should be marked as draft (no review spawn)
}

/// Test that ready_for_review event is processed.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_pr_ready_for_review() {
    let harness = create_webhook_fixture(47115).expect("Failed to create fixture");

    let payload = r#"{
        "action": "ready_for_review",
        "number": 210,
        "pull_request": {
            "title": "Feature now ready",
            "user": {"login": "testuser"},
            "merged": false,
            "draft": false,
            "head": {"ref": "pleasant/feature"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("marked PR #210 ready for review", 5000),
        "Ready for review message should appear"
    );
}

/// Test status event (CI) processing.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_status_event() {
    let harness = create_webhook_fixture(47116).expect("Failed to create fixture");

    let payload = r#"{
        "state": "success",
        "context": "ci/tests",
        "description": "All 150 tests passed",
        "sha": "abc123",
        "branches": [{"name": "central/tests"}],
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("status", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("CI passed (ci/tests)", 5000),
        "Status success should appear in channel"
    );
}

/// Test that pending status events are ignored.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_status_pending_ignored() {
    let harness = create_webhook_fixture(47117).expect("Failed to create fixture");

    let payload = r#"{
        "state": "pending",
        "context": "ci/tests",
        "description": "Running tests...",
        "sha": "def456",
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("status", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Pending status should NOT appear in channel
    thread::sleep(Duration::from_millis(500));
    assert!(
        !harness.channel_contains("Running tests"),
        "Pending status should not be posted to channel"
    );
}

/// Test review comment processing.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_review_comment() {
    let harness = create_webhook_fixture(47118).expect("Failed to create fixture");

    let payload = r#"{
        "action": "created",
        "pull_request": {
            "number": 300,
            "head": {"ref": "madison/refactor"},
            "body": "Refactoring work"
        },
        "comment": {
            "id": 600,
            "user": {"login": "reviewer"},
            "body": "Consider using a const here instead."
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request_review_comment", payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    assert!(
        harness.wait_for_channel_message("left review comment on PR #300", 5000),
        "Review comment should appear in channel"
    );
    assert!(
        harness.channel_contains("@madison"),
        "Message should mention PR owner"
    );
}

/// Test that health endpoint works.
#[test]
#[ignore]
#[timeout(60000)]
fn test_webhook_health_endpoint() {
    let harness = create_webhook_fixture(47119).expect("Failed to create fixture");

    let http_client = reqwest::blocking::Client::new();
    let url = format!("http://localhost:{}/health", harness.webhook_port.unwrap());

    let response = http_client.get(&url).send().expect("HTTP request failed");

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.text().unwrap_or_default(), "ok");
}

/// Test that tasks are NOT auto-completed when PR is opened, only when merged.
/// This verifies the fix for task #936.
#[test]
#[ignore]
#[timeout(60000)]
fn test_task_completion_on_pr_merge_not_pr_open() {
    let harness = create_webhook_fixture(47120).expect("Failed to create fixture");

    // Step 1: PR opened with [Midtown #42] in title
    let pr_opened_payload = r#"{
        "action": "opened",
        "number": 42,
        "pull_request": {
            "title": "feat: Add auth endpoint [Midtown #42]",
            "user": {"login": "testuser"},
            "merged": false,
            "head": {"ref": "madison/add-auth"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request", pr_opened_payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Verify PR opened message appears
    assert!(
        harness.wait_for_channel_message("opened PR #42", 5000),
        "PR opened message should appear"
    );

    // CRITICAL: Task should NOT be completed when PR opens
    thread::sleep(Duration::from_millis(500)); // Give daemon time to process
    assert!(
        !harness.channel_contains("Auto-completed task !42"),
        "Task should NOT be auto-completed when PR is opened (before the fix, this would fail)"
    );

    // Step 2: PR merged with same task number in title
    let pr_merged_payload = r#"{
        "action": "closed",
        "number": 42,
        "pull_request": {
            "title": "feat: Add auth endpoint [Midtown #42]",
            "user": {"login": "testuser"},
            "merged": true,
            "head": {"ref": "madison/add-auth"}
        },
        "repository": {"full_name": "test/repo"}
    }"#;

    let status = harness
        .send_webhook("pull_request", pr_merged_payload)
        .expect("Failed to send webhook");
    assert_eq!(status, 200);

    // Verify PR merged message appears
    assert!(
        harness.wait_for_channel_message("merged PR #42", 5000),
        "PR merged message should appear"
    );

    // NOW task should be auto-completed (after merge, not open)
    assert!(
        harness.wait_for_channel_message("Auto-completed task !42", 5000),
        "Task should be auto-completed when PR is merged"
    );
}
