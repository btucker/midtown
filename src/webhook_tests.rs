use super::*;

#[test]
fn test_verify_signature() {
    let secret = "test-secret";
    let payload = b"test payload";

    // Generate valid signature
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(payload);
    let result = mac.finalize();
    let signature = format!("sha256={}", hex::encode(result.into_bytes()));

    assert!(verify_signature(secret, payload, &signature));
    assert!(!verify_signature(secret, payload, "sha256=invalid"));
    assert!(!verify_signature(secret, payload, "invalid-format"));
    assert!(!verify_signature("wrong-secret", payload, &signature));
}

#[test]
fn test_truncate_comment() {
    assert_eq!(truncate_comment("short", 10), "short");
    assert_eq!(
        truncate_comment("this is a longer comment", 10),
        "this is a ..."
    );
    assert_eq!(
        truncate_comment("first line\nsecond line", 50),
        "first line"
    );
    // Test unicode safety - should not panic on multi-byte characters
    assert_eq!(
        truncate_comment("Hello 世界! More text here", 8),
        "Hello 世界..."
    );
    assert_eq!(truncate_comment("emoji 👍 test", 7), "emoji 👍...");
}

#[test]
fn test_handle_pull_request_opened() {
    let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    // Content includes @mention prefix for coworker
    assert_eq!(
        event.message.content,
        "@lexington opened PR #42: Add auth endpoint"
    );
    // Sender is always "github"
    assert_eq!(event.message.from, "github");
    // Non-draft opened PR triggers review spawn
    assert_eq!(event.needs_review, Some(42));
}

#[test]
fn test_handle_pull_request_opened_draft_no_review() {
    let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "WIP: Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "draft": true,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.content,
        "@lexington opened PR #42: WIP: Add auth endpoint"
    );
    // Draft PRs should NOT trigger review spawn
    assert_eq!(event.needs_review, None);
}

#[test]
fn test_handle_pull_request_opened_with_frontmatter() {
    let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "feature/something"},
                "body": "<!-- midtown: park -->\n\nSome description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    // Content includes @mention from frontmatter (takes priority over branch)
    assert_eq!(
        event.message.content,
        "@park opened PR #42: Add auth endpoint"
    );
    // Sender is always "github"
    assert_eq!(event.message.from, "github");
}

#[test]
fn test_handle_pull_request_merged() {
    let payload = r#"{
            "action": "closed",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": true,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.content,
        "@lexington merged PR #42: Add auth endpoint"
    );
    assert_eq!(event.message.from, "github");
    // Merged PRs should NOT trigger review spawn
    assert_eq!(event.needs_review, None);
    // Merged PRs should flag for lead nudge
    assert_eq!(event.merged_pr, Some(42));
}

#[test]
fn test_handle_pull_request_closed_not_merged() {
    let payload = r#"{
            "action": "closed",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "lexington/add-auth"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.content,
        "@lexington closed PR #42 (not merged): Add auth endpoint"
    );
    // Closed (not merged) PRs should NOT flag for lead nudge
    assert_eq!(event.merged_pr, None);
}

#[test]
fn test_handle_pull_request_no_coworker() {
    // When branch doesn't match a coworker, no @mention prefix
    let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add auth endpoint",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "feature/something"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    // No @mention when no coworker is identified
    assert_eq!(event.message.content, "opened PR #42: Add auth endpoint");
    assert_eq!(event.message.from, "github");
    // Non-draft opened PR still triggers review even without coworker match
    assert_eq!(event.needs_review, Some(42));
}

#[test]
fn test_handle_review_approved() {
    let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 100,
                "state": "approved",
                "user": {"login": "madison"}
            },
            "pull_request": {"number": 42},
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request_review(payload.as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(event.message.content, "madison approved PR #42");
    assert_eq!(event.reviewed_pr, Some(42));
    // Should include review node for reactions
    let activity = event.pr_activity.unwrap();
    assert!(matches!(
        activity.comment_node,
        Some(CommentNode::Review {
            pull: 42,
            review_id: 100
        })
    ));
    assert_eq!(activity.repo_full_name.as_deref(), Some("org/repo"));
}

#[test]
fn test_handle_ci_status() {
    let payload = r#"{
            "state": "success",
            "context": "ci/tests",
            "description": "All tests passed",
            "sha": "abc123",
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_status(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.content,
        "CI passed (ci/tests): All tests passed"
    );
}

#[test]
fn test_ignores_pending_status() {
    let payload = r#"{
            "state": "pending",
            "context": "ci/tests",
            "description": "Running",
            "sha": "abc123",
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_status(payload.as_bytes()).unwrap();
    assert!(event.is_none());
}

#[test]
fn test_handle_review_with_branch_attribution() {
    let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 101,
                "state": "approved",
                "user": {"login": "btucker"}
            },
            "pull_request": {
                "number": 42,
                "head": {"ref": "amsterdam/fix-bug"},
                "body": "Some PR description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request_review(payload.as_bytes())
        .unwrap()
        .unwrap();
    // Content includes @mention prefix for coworker from branch
    assert_eq!(event.message.content, "@amsterdam btucker approved PR #42");
    // Sender is always "github"
    assert_eq!(event.message.from, "github");
}

#[test]
fn test_handle_review_with_frontmatter_attribution() {
    let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 102,
                "state": "changes_requested",
                "user": {"login": "reviewer"}
            },
            "pull_request": {
                "number": 55,
                "head": {"ref": "feature/unrelated"},
                "body": "<!-- midtown: columbus -->\n\nSome description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request_review(payload.as_bytes())
        .unwrap()
        .unwrap();
    // Frontmatter takes priority for @mention
    assert_eq!(
        event.message.content,
        "@columbus reviewer requested changes on PR #55"
    );
    // Sender is always "github"
    assert_eq!(event.message.from, "github");
}

#[test]
fn test_handle_ci_status_with_branch_attribution() {
    let payload = r#"{
            "state": "failure",
            "context": "ci/tests",
            "description": "Tests failed",
            "sha": "abc123",
            "branches": [{"name": "riverside/add-feature"}],
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_status(payload.as_bytes()).unwrap().unwrap();
    // Content includes @mention prefix for coworker from branch
    assert_eq!(
        event.message.content,
        "@riverside CI failed (ci/tests): Tests failed"
    );
    // Sender is always "github"
    assert_eq!(event.message.from, "github");
}

#[test]
fn test_handle_check_run_with_branch_attribution() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    // Content includes @mention prefix for coworker from branch
    assert_eq!(
        event.message.content,
        "@park Check 'build' passed on PR #99"
    );
    // Sender is always "github"
    assert_eq!(event.message.from, "github");
}

#[test]
fn test_handle_check_run_on_main_branch() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    // No PR, so shows branch name instead
    assert_eq!(event.message.content, "Check 'build' passed on main");
    assert_eq!(event.message.from, "github");
}

#[test]
fn test_handle_check_run_failure_on_default_branch_nudges_lead() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(event.message.content, "Check 'build' failed on main");
    assert_eq!(
        event.ci_failed_on_default_branch.as_deref(),
        Some("@lead CI check 'build' failed on main — investigate ASAP")
    );
}

#[test]
fn test_handle_check_run_failure_on_pr_branch_no_nudge() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.content,
        "@park Check 'build' failed on PR #99"
    );
    assert!(event.ci_failed_on_default_branch.is_none());
}

#[test]
fn test_handle_check_run_success_on_default_branch_no_nudge() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(event.message.content, "Check 'build' passed on main");
    assert!(event.ci_failed_on_default_branch.is_none());
}

#[test]
fn test_handle_check_run_timed_out_on_default_branch_nudges_lead() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "E2E Tests",
                "status": "completed",
                "conclusion": "timed_out",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "master",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "master"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.content,
        "Check 'E2E Tests' timed out on master"
    );
    assert_eq!(
        event.ci_failed_on_default_branch.as_deref(),
        Some("@lead CI check 'E2E Tests' failed on master — investigate ASAP")
    );
}

#[test]
fn test_handle_check_run_failure_on_non_default_branch_no_pr_no_nudge() {
    // A branch that's not the default and has no PR — no nudge
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "feature/experiment",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.content,
        "Check 'build' failed on feature/experiment"
    );
    assert!(event.ci_failed_on_default_branch.is_none());
}

#[test]
fn test_handle_review_comment_with_branch_attribution() {
    let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "PR body here"
            },
            "comment": {
                "id": 200,
                "user": {"login": "reviewer"},
                "body": "Nice work!"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
    // Content includes @mention prefix for coworker from branch
    assert_eq!(
        event.message.content,
        "@madison reviewer left review comment on PR #77: Nice work!"
    );
    // Sender is always "github"
    assert_eq!(event.message.from, "github");
    // PR activity should identify madison as owner
    let activity = event.pr_activity.unwrap();
    assert_eq!(activity.pr_number, 77);
    assert_eq!(activity.owner_coworker.as_deref(), Some("madison"));
    assert_eq!(activity.actor, "reviewer");
    // Should include comment node for reactions
    assert!(matches!(
        activity.comment_node,
        Some(CommentNode::ReviewComment(200))
    ));
    assert_eq!(activity.repo_full_name.as_deref(), Some("org/repo"));
}

#[test]
fn test_handle_issue_comment_with_coworker_signature() {
    // When a coworker posts a comment with <!-- midtown: name --> signature,
    // use the coworker name instead of GitHub username
    let payload = r#"{
            "action": "created",
            "issue": {
                "number": 42,
                "pull_request": {}
            },
            "comment": {
                "id": 201,
                "user": {"login": "btucker"},
                "body": "<!-- midtown: columbus -->\n\nLGTM! Nice fix."
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_issue_comment(payload.as_bytes()).unwrap().unwrap();
    // Should use coworker name from signature, not GitHub username
    assert_eq!(
        event.message.content,
        "columbus commented on PR #42: LGTM! Nice fix."
    );
    assert_eq!(event.message.from, "github");
    // PR activity should identify commenter
    let activity = event.pr_activity.unwrap();
    assert_eq!(activity.pr_number, 42);
    assert_eq!(activity.actor, "columbus");
    // Should include comment node for reactions
    assert!(matches!(
        activity.comment_node,
        Some(CommentNode::IssueComment(201))
    ));
    assert_eq!(activity.repo_full_name.as_deref(), Some("org/repo"));
}

#[test]
fn test_handle_issue_comment_without_signature() {
    // When no coworker signature, use the GitHub username as before
    let payload = r#"{
            "action": "created",
            "issue": {
                "number": 42,
                "pull_request": {}
            },
            "comment": {
                "id": 202,
                "user": {"login": "btucker"},
                "body": "Regular comment without signature"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_issue_comment(payload.as_bytes()).unwrap().unwrap();
    // Should use GitHub username when no signature
    assert_eq!(
        event.message.content,
        "btucker commented on PR #42: Regular comment without signature"
    );
    assert_eq!(event.message.from, "github");
}

#[test]
fn test_issue_comment_review_sets_review_comment_id() {
    // When an issue comment IS a code review, review_comment_id should be set
    let payload = r#"{
            "action": "created",
            "issue": {
                "number": 42,
                "pull_request": {}
            },
            "comment": {
                "id": 98765,
                "user": {"login": "btucker"},
                "body": "<!-- midtown: columbus -->\n\n## Code Review by columbus\n\nFound 2 issues:\n1. Bug here\n2. Bug there"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_issue_comment(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(event.reviewed_pr, Some(42));
    assert_eq!(
        event.review_comment_id,
        Some(98765),
        "review_comment_id should be the database ID of the review comment"
    );
}

#[test]
fn test_issue_comment_non_review_no_review_comment_id() {
    // When an issue comment is NOT a code review, review_comment_id should be None
    let payload = r#"{
            "action": "created",
            "issue": {
                "number": 42,
                "pull_request": {}
            },
            "comment": {
                "id": 12345,
                "user": {"login": "btucker"},
                "body": "<!-- midtown: columbus -->\n\nLGTM! Nice fix."
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_issue_comment(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(event.reviewed_pr, None);
    assert_eq!(
        event.review_comment_id, None,
        "non-review comments should not set review_comment_id"
    );
}

#[test]
fn test_handle_issue_comment_edited_placeholder_to_review() {
    // Reviewer posts a placeholder, then edits it with the full review.
    // The 'edited' event should be processed (non-review → review transition).
    let payload = serde_json::json!({
        "action": "edited",
        "issue": {
            "number": 55,
            "pull_request": {}
        },
        "comment": {
            "id": 77777,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\n## Code Review by park\n\nLGTM - no issues found."
        },
        "changes": {
            "body": {
                "from": "## Review Status\n\nReview in progress by park..."
            }
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let event = handle_issue_comment(&payload).unwrap().unwrap();
    assert_eq!(event.reviewed_pr, Some(55));
    assert_eq!(event.review_comment_id, Some(77777));
    let activity = event.pr_activity.unwrap();
    assert_eq!(activity.pr_number, 55);
    assert_eq!(activity.actor, "park");
}

#[test]
fn test_handle_issue_comment_edited_review_typo_fix_ignored() {
    // An edit to an already-posted review (e.g. fixing a typo) should
    // be ignored — the review was already detected on creation/first edit.
    let payload = serde_json::json!({
        "action": "edited",
        "issue": {
            "number": 55,
            "pull_request": {}
        },
        "comment": {
            "id": 77777,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\n## Code Review by park\n\nLGTM - no issues found. Fixed typo."
        },
        "changes": {
            "body": {
                "from": "<!-- midtown: park -->\n\n## Code Review by park\n\nLGTM - no isues found."
            }
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let result = handle_issue_comment(&payload).unwrap();
    assert!(
        result.is_none(),
        "edits to existing reviews should be ignored"
    );
}

#[test]
fn test_handle_issue_comment_edited_without_review_signature() {
    // An 'edited' event on a non-review comment should be ignored
    let payload = serde_json::json!({
        "action": "edited",
        "issue": {
            "number": 55,
            "pull_request": {}
        },
        "comment": {
            "id": 88888,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\nFixed a typo in my earlier comment."
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let result = handle_issue_comment(&payload).unwrap();
    assert!(
        result.is_none(),
        "edited non-review comments should be ignored"
    );
}

#[test]
fn test_handle_issue_comment_edited_no_changes_field() {
    // An 'edited' event without a `changes` field (edge case) should
    // still process a non-review → review transition since we can't
    // know the previous body.
    let payload = serde_json::json!({
        "action": "edited",
        "issue": {
            "number": 55,
            "pull_request": {}
        },
        "comment": {
            "id": 99999,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\n## Code Review by park\n\nLooks good!"
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let event = handle_issue_comment(&payload).unwrap().unwrap();
    assert_eq!(event.reviewed_pr, Some(55));
    assert_eq!(event.review_comment_id, Some(99999));
}

#[test]
fn test_formal_review_no_review_comment_id() {
    // Formal GitHub reviews set reviewed_pr but NOT review_comment_id
    // (only issue comment reviews populate review_comment_id for Gate 3)
    let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 100,
                "state": "approved",
                "user": {"login": "madison"}
            },
            "pull_request": {"number": 42},
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request_review(payload.as_bytes())
        .unwrap()
        .unwrap();
    assert_eq!(event.reviewed_pr, Some(42));
    assert_eq!(
        event.review_comment_id, None,
        "formal reviews should not set review_comment_id (they don't have issue comment IDs)"
    );
}

#[test]
fn test_handle_review_comment_with_coworker_signature() {
    // When a coworker posts a review comment with signature,
    // use the coworker name instead of GitHub username
    let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "PR body here"
            },
            "comment": {
                "id": 203,
                "user": {"login": "btucker"},
                "body": "<!-- midtown: lexington -->\n\nConsider using a match here."
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
    // Should use coworker name from comment signature
    // Note: @mention still uses PR attribution (madison), but commenter is lexington
    assert_eq!(
        event.message.content,
        "@madison lexington left review comment on PR #77: Consider using a match here."
    );
    assert_eq!(event.message.from, "github");
    // PR activity should identify madison as owner and lexington as actor
    let activity = event.pr_activity.unwrap();
    assert_eq!(activity.pr_number, 77);
    assert_eq!(activity.owner_coworker.as_deref(), Some("madison"));
    assert_eq!(activity.actor, "lexington");
}

#[test]
fn test_handle_review_comment_without_signature_uses_branch() {
    // When no coworker signature in the comment, but the PR branch
    // maps to a coworker and the commenter is the repo owner (shared account),
    // use the branch-derived coworker name
    let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "<!-- midtown: madison -->\n\nPR body"
            },
            "comment": {
                "id": 204,
                "user": {"login": "btucker"},
                "body": "Good point, I'll fix that."
            },
            "repository": {"full_name": "btucker/midtown"}
        }"#;

    let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
    // Should use coworker name from PR branch/body, not GitHub username
    assert_eq!(
        event.message.content,
        "@madison madison left review comment on PR #77: Good point, I'll fix that."
    );
    // Actor should be the coworker, not the GitHub username
    let activity = event.pr_activity.unwrap();
    assert_eq!(activity.actor, "madison");
}

#[test]
fn test_handle_review_comment_external_user_keeps_username() {
    // When the commenter is NOT the repo owner (e.g., an external reviewer),
    // keep the GitHub username even without frontmatter
    let payload = r#"{
            "action": "created",
            "pull_request": {
                "number": 77,
                "head": {"ref": "madison/refactor"},
                "body": "PR body here"
            },
            "comment": {
                "id": 205,
                "user": {"login": "external_reviewer"},
                "body": "Nice work!"
            },
            "repository": {"full_name": "btucker/midtown"}
        }"#;

    let event = handle_review_comment(payload.as_bytes()).unwrap().unwrap();
    // Should keep the external user's GitHub username
    assert_eq!(
        event.message.content,
        "@madison external_reviewer left review comment on PR #77: Nice work!"
    );
    let activity = event.pr_activity.unwrap();
    assert_eq!(activity.actor, "external_reviewer");
}

#[test]
fn test_review_approved_produces_state_change() {
    let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 200,
                "state": "approved",
                "user": {"login": "reviewer_bot"}
            },
            "pull_request": {
                "number": 42,
                "head": {"ref": "broadway/fix-bug"},
                "body": "Some PR description"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request_review(payload.as_bytes())
        .unwrap()
        .unwrap();
    let change = event.review_state_change.unwrap();
    assert_eq!(change.pr_number, 42);
    assert_eq!(change.owner_coworker.as_deref(), Some("broadway"));
    assert_eq!(change.reviewer, "reviewer_bot");
    assert_eq!(change.state, ReviewState::Approved);
    // CI failure should be None for review events
    assert!(event.pr_ci_failure.is_none());
}

#[test]
fn test_review_changes_requested_produces_state_change() {
    let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 201,
                "state": "changes_requested",
                "user": {"login": "reviewer_bot"}
            },
            "pull_request": {
                "number": 55,
                "head": {"ref": "columbus/add-feature"},
                "body": "<!-- midtown: columbus -->\n\nDescription"
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request_review(payload.as_bytes())
        .unwrap()
        .unwrap();
    let change = event.review_state_change.unwrap();
    assert_eq!(change.pr_number, 55);
    assert_eq!(change.owner_coworker.as_deref(), Some("columbus"));
    assert_eq!(change.reviewer, "reviewer_bot");
    assert_eq!(change.state, ReviewState::ChangesRequested);
}

#[test]
fn test_review_commented_no_state_change() {
    let payload = r#"{
            "action": "submitted",
            "review": {
                "id": 202,
                "state": "commented",
                "user": {"login": "reviewer_bot"}
            },
            "pull_request": {
                "number": 42,
                "head": {"ref": "broadway/fix-bug"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request_review(payload.as_bytes())
        .unwrap()
        .unwrap();
    // "commented" reviews should NOT produce a state change
    assert!(event.review_state_change.is_none());
    // But it is still a completed formal review submission.
    assert_eq!(event.reviewed_pr, Some(42));
}

#[test]
fn test_check_run_failure_on_pr_produces_ci_failure() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "Build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    let failure = event.pr_ci_failure.unwrap();
    assert_eq!(failure.pr_number, 99);
    assert_eq!(failure.owner_coworker.as_deref(), Some("park"));
    assert_eq!(failure.check_name, "Build");
    // Should NOT flag as default-branch CI failure
    assert!(event.ci_failed_on_default_branch.is_none());
}

#[test]
fn test_check_run_success_on_pr_no_ci_failure() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "Build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "park/implement-thing",
                    "pull_requests": [{"number": 99}]
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    assert!(event.pr_ci_failure.is_none());
}

#[test]
fn test_check_run_failure_on_main_no_pr_ci_failure() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "Build",
                "status": "completed",
                "conclusion": "failure",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo", "default_branch": "main"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    // No PR associated → no pr_ci_failure
    assert!(event.pr_ci_failure.is_none());
    // But it should flag as default-branch CI failure
    assert!(event.ci_failed_on_default_branch.is_some());
}

#[test]
fn test_pull_request_opened_no_review_state_change() {
    let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add feature",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "broadway/add-feature"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    // PR opened events should not produce review state changes or CI failures
    assert!(event.review_state_change.is_none());
    assert!(event.pr_ci_failure.is_none());
}

// -------------------------------------------------------------------------
// is_review_comment — detects completed review comments from webhook payloads
// -------------------------------------------------------------------------

#[test]
fn test_is_review_comment_detects_midtown_marker_with_review_header() {
    // Frontmatter alone is not sufficient; review signature/header is required.
    assert!(is_review_comment(
        "<!-- midtown: park -->\n\n## Code Review"
    ));
}

#[test]
fn test_is_review_comment_rejects_midtown_marker_only() {
    assert!(!is_review_comment(
        "<!-- midtown:reviewer=lexington -->\n\nLGTM!"
    ));
}

#[test]
fn test_is_review_comment_detects_emoji_signature() {
    assert!(is_review_comment(
        "## Summary\n\nLGTM!\n\n🤖 Reviewed by amsterdam"
    ));
}

#[test]
fn test_is_review_comment_detects_code_review_header() {
    assert!(is_review_comment(
        "## Code Review by columbus\n\nLooks good!"
    ));
}

#[test]
fn test_is_review_comment_detects_exact_code_review_header() {
    assert!(is_review_comment("### Code review\n\nNo issues found."));
}

#[test]
fn test_is_review_comment_returns_false_for_normal_comment() {
    assert!(!is_review_comment("Thanks for the PR! I'll take a look."));
    assert!(!is_review_comment("Can you add some tests for this?"));
}

#[test]
fn test_compute_check_duration_valid_timestamps() {
    let result = compute_check_duration(
        Some("2026-02-04T12:00:00Z"),
        Some("2026-02-04T12:05:30Z"),
        "Test",
    );
    let duration = result.expect("should return duration for valid timestamps");
    assert_eq!(duration.check_name, "Test");
    assert_eq!(duration.duration_secs, 330); // 5 minutes 30 seconds
}

#[test]
fn test_compute_check_duration_missing_started_at() {
    let result = compute_check_duration(None, Some("2026-02-04T12:05:30Z"), "Test");
    assert!(
        result.is_none(),
        "should return None when started_at is missing"
    );
}

#[test]
fn test_compute_check_duration_missing_completed_at() {
    let result = compute_check_duration(Some("2026-02-04T12:00:00Z"), None, "Test");
    assert!(
        result.is_none(),
        "should return None when completed_at is missing"
    );
}

#[test]
fn test_compute_check_duration_invalid_started_at() {
    let result = compute_check_duration(
        Some("not-a-timestamp"),
        Some("2026-02-04T12:05:30Z"),
        "Test",
    );
    assert!(
        result.is_none(),
        "should return None for invalid started_at"
    );
}

#[test]
fn test_compute_check_duration_invalid_completed_at() {
    let result = compute_check_duration(Some("2026-02-04T12:00:00Z"), Some("invalid"), "Test");
    assert!(
        result.is_none(),
        "should return None for invalid completed_at"
    );
}

#[test]
fn test_compute_check_duration_reversed_timestamps_clamps_to_zero() {
    // completed_at before started_at - should clamp to 0
    let result = compute_check_duration(
        Some("2026-02-04T12:10:00Z"),
        Some("2026-02-04T12:00:00Z"),
        "Test",
    );
    let duration = result.expect("should return duration even for reversed timestamps");
    assert_eq!(
        duration.duration_secs, 0,
        "negative duration should clamp to 0"
    );
}

#[test]
fn test_compute_check_duration_over_24_hours_returns_none() {
    // 25 hours = 90000 seconds, exceeds 86400 limit
    let result = compute_check_duration(
        Some("2026-02-03T11:00:00Z"),
        Some("2026-02-04T12:00:00Z"),
        "Test",
    );
    assert!(
        result.is_none(),
        "should return None for durations over 24 hours"
    );
}

#[test]
fn test_compute_check_duration_exactly_24_hours_returns_none() {
    // exactly 86400 seconds - should be rejected (> check is strict)
    let result = compute_check_duration(
        Some("2026-02-03T12:00:00Z"),
        Some("2026-02-04T12:00:01Z"),
        "Test",
    );
    assert!(result.is_none(), "should return None for duration > 86400s");
}

#[test]
fn test_compute_check_duration_just_under_24_hours_is_valid() {
    // 86399 seconds - should be accepted
    let result = compute_check_duration(
        Some("2026-02-03T12:00:01Z"),
        Some("2026-02-04T12:00:00Z"),
        "Test",
    );
    let duration = result.expect("should accept duration just under 24 hours");
    assert_eq!(duration.duration_secs, 86399);
}

// -------------------------------------------------------------------------
// GitHub webhook messages route to #ops channel
// -------------------------------------------------------------------------

#[test]
fn test_github_message_routes_to_ops_channel() {
    let event = WebhookEvent::github("some message");
    assert_eq!(
        event.message.channel.as_deref(),
        Some("ops"),
        "GitHub webhook messages must route to #ops channel"
    );
}

#[test]
fn test_pull_request_event_routes_to_ops_channel() {
    let payload = r#"{
            "action": "opened",
            "number": 42,
            "pull_request": {
                "title": "Add feature",
                "user": {"login": "btucker"},
                "merged": false,
                "head": {"ref": "lexington/add-feature"}
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_pull_request(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.channel.as_deref(),
        Some("ops"),
        "PR webhook messages must route to #ops channel"
    );
}

#[test]
fn test_handle_review_comment_edited_placeholder_to_review() {
    // Reviewer posts a placeholder inline comment, then edits it with a review.
    // The 'edited' event should be processed (non-review → review transition).
    let payload = serde_json::json!({
        "action": "edited",
        "pull_request": {
            "number": 77,
            "head": {"ref": "madison/refactor"},
            "body": "PR body here"
        },
        "comment": {
            "id": 300,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\n## Code Review by park\n\nLGTM - no issues found."
        },
        "changes": {
            "body": {
                "from": "## Review Status\n\nReview in progress by park..."
            }
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let event = handle_review_comment(&payload).unwrap().unwrap();
    assert_eq!(event.reviewed_pr, Some(77));
    assert_eq!(event.review_comment_id, Some(300));
    let activity = event.pr_activity.unwrap();
    assert_eq!(activity.pr_number, 77);
    assert_eq!(activity.actor, "park");
}

#[test]
fn test_handle_review_comment_edited_review_typo_fix_ignored() {
    // An edit to an already-posted review (e.g. fixing a typo) should be ignored.
    let payload = serde_json::json!({
        "action": "edited",
        "pull_request": {
            "number": 77,
            "head": {"ref": "madison/refactor"},
            "body": "PR body here"
        },
        "comment": {
            "id": 301,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\n## Code Review by park\n\nLGTM - no issues found. Fixed typo."
        },
        "changes": {
            "body": {
                "from": "<!-- midtown: park -->\n\n## Code Review by park\n\nLGTM - no isues found."
            }
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let result = handle_review_comment(&payload).unwrap();
    assert!(
        result.is_none(),
        "edits to existing review comments should be ignored"
    );
}

#[test]
fn test_handle_review_comment_edited_without_review_signature() {
    // An 'edited' event on a non-review comment should be ignored.
    let payload = serde_json::json!({
        "action": "edited",
        "pull_request": {
            "number": 77,
            "head": {"ref": "madison/refactor"},
            "body": "PR body here"
        },
        "comment": {
            "id": 302,
            "user": {"login": "btucker"},
            "body": "Updated my earlier suggestion — use a match here instead."
        },
        "changes": {
            "body": {
                "from": "Consider using a match here."
            }
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let result = handle_review_comment(&payload).unwrap();
    assert!(
        result.is_none(),
        "edited non-review comments should be ignored"
    );
}

#[test]
fn test_handle_review_comment_edited_no_changes_field() {
    // An 'edited' event without a `changes` field should still process
    // a non-review → review transition since we can't know the previous body.
    let payload = serde_json::json!({
        "action": "edited",
        "pull_request": {
            "number": 77,
            "head": {"ref": "madison/refactor"},
            "body": "PR body here"
        },
        "comment": {
            "id": 303,
            "user": {"login": "btucker"},
            "body": "<!-- midtown: park -->\n\n## Code Review by park\n\nLooks good!"
        },
        "repository": {"full_name": "org/repo"}
    });
    let payload = serde_json::to_vec(&payload).unwrap();

    let event = handle_review_comment(&payload).unwrap().unwrap();
    assert_eq!(event.reviewed_pr, Some(77));
    assert_eq!(event.review_comment_id, Some(303));
}

#[test]
fn test_check_run_event_routes_to_ops_channel() {
    let payload = r#"{
            "action": "completed",
            "check_run": {
                "name": "build",
                "status": "completed",
                "conclusion": "success",
                "check_suite": {
                    "head_sha": "abc123",
                    "head_branch": "main",
                    "pull_requests": []
                }
            },
            "repository": {"full_name": "org/repo"}
        }"#;

    let event = handle_check_run(payload.as_bytes()).unwrap().unwrap();
    assert_eq!(
        event.message.channel.as_deref(),
        Some("ops"),
        "Check run webhook messages must route to #ops channel"
    );
}
