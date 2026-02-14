use super::*;
use serde_json::json;

// -------------------------------------------------------------------------
// truncate_str / truncate_message — UTF-8 safety
// -------------------------------------------------------------------------

#[test]
fn truncate_str_ascii_within_limit() {
    assert_eq!(truncate_str("hello", 10), "hello");
}

#[test]
fn truncate_str_ascii_over_limit() {
    let long = "a".repeat(120);
    let result = truncate_str(&long, 100);
    assert!(result.len() <= 100);
    assert!(result.ends_with("..."));
}

#[test]
fn truncate_str_multibyte_boundary() {
    // 25 x 4-byte emoji = 100 bytes. Slicing at byte 97 lands mid-char.
    let emoji_str = "😀".repeat(25);
    assert_eq!(emoji_str.len(), 100);
    // Must not panic
    let result = truncate_str(&emoji_str, 50);
    assert!(result.ends_with("..."));
    // Result must be valid UTF-8 (implicit — it's a String)
    assert!(result.len() <= 50);
}

#[test]
fn truncate_str_multibyte_exactly_over() {
    // 26 x 4-byte emoji = 104 bytes, triggers truncation at max_len=100
    let emoji_str = "😀".repeat(26);
    let result = truncate_str(&emoji_str, 100);
    assert!(result.ends_with("..."));
    assert!(result.len() <= 100);
}

#[test]
fn truncate_message_multibyte() {
    // Multi-byte characters in a single line
    let msg = "日本語のテスト".repeat(20); // 7 chars * 3 bytes * 20 = 420 bytes
    let result = truncate_message(&msg, 50);
    assert!(result.ends_with("..."));
    assert!(result.len() <= 50);
}

#[test]
fn truncate_message_ascii_respects_max_len() {
    let msg = "a".repeat(120);
    let result = truncate_message(&msg, 60);
    assert!(result.ends_with("..."));
    assert!(
        result.len() <= 60,
        "truncate_message exceeded max_len: {} > 60",
        result.len()
    );
}

// =========================================================================
// Graceful degradation tests: polling path detects issues without webhooks
//
// These tests verify that when GitHub webhooks aren't delivering events,
// the polling path (via `poll_prs_for_issues`) can still detect and handle
// all PR issues. The key insight is that webhooks and polling use the SAME
// detection functions — these tests verify those functions work correctly.
// =========================================================================

// -------------------------------------------------------------------------
// detect_pr_issues — identifies actionable PR issues from JSON data
// -------------------------------------------------------------------------

#[test]
fn detect_issues_finds_merge_conflict() {
    // Polling discovers merge conflict via `gh pr list --json mergeable`
    let pr = json!({
        "number": 42,
        "mergeable": "CONFLICTING",
        "statusCheckRollup": [],
        "reviewDecision": ""
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.contains(&PrIssueType::MergeConflict),
        "polling should detect merge conflicts without webhook"
    );
}

#[test]
fn detect_issues_finds_ci_failure() {
    // Polling discovers CI failure via `gh pr list --json statusCheckRollup`
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"conclusion": "FAILURE"}
        ],
        "reviewDecision": ""
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.contains(&PrIssueType::CiFailed),
        "polling should detect CI failures without webhook"
    );
}

#[test]
fn detect_issues_finds_changes_requested() {
    // Polling discovers review state via `gh pr list --json reviewDecision`
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [],
        "reviewDecision": "CHANGES_REQUESTED"
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.contains(&PrIssueType::ChangesRequested),
        "polling should detect changes_requested without webhook"
    );
}

#[test]
fn detect_issues_finds_approval() {
    // Polling discovers approval via `gh pr list --json reviewDecision`
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [{"conclusion": "SUCCESS"}],
        "reviewDecision": "APPROVED"
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.contains(&PrIssueType::Approved),
        "polling should detect approval without webhook"
    );
}

#[test]
fn detect_issues_finds_multiple_issues() {
    // A PR can have multiple issues (e.g., CI failed AND merge conflict)
    let pr = json!({
        "number": 42,
        "mergeable": "CONFLICTING",
        "statusCheckRollup": [{"conclusion": "FAILURE"}],
        "reviewDecision": ""
    });

    let issues = detect_pr_issues(&pr);

    assert_eq!(
        issues.len(),
        2,
        "should detect both merge conflict and CI failure"
    );
    assert!(issues.contains(&PrIssueType::MergeConflict));
    assert!(issues.contains(&PrIssueType::CiFailed));
}

#[test]
fn detect_issues_returns_empty_for_healthy_pr() {
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [{"conclusion": "SUCCESS"}],
        "reviewDecision": ""
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.is_empty(),
        "healthy PR with no review yet should have no issues"
    );
}

// -------------------------------------------------------------------------
// is_auto_mergeable — identifies PRs ready for auto-merge (polling-only)
// -------------------------------------------------------------------------

#[test]
fn auto_merge_requires_approval() {
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [{"conclusion": "SUCCESS"}],
        "reviewDecision": "" // No review yet
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR without approval cannot be auto-merged"
    );
}

#[test]
fn auto_merge_requires_no_conflicts() {
    let pr = json!({
        "number": 42,
        "mergeable": "CONFLICTING",
        "statusCheckRollup": [{"conclusion": "SUCCESS"}],
        "reviewDecision": "APPROVED"
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR with merge conflicts cannot be auto-merged"
    );
}

#[test]
fn auto_merge_requires_ci_success() {
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [{"conclusion": "FAILURE"}],
        "reviewDecision": "APPROVED"
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR with CI failure cannot be auto-merged"
    );
}

#[test]
fn auto_merge_requires_ci_complete() {
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [{"conclusion": "PENDING"}],
        "reviewDecision": "APPROVED"
    });

    assert!(
        !is_auto_mergeable(&pr),
        "PR with pending CI cannot be auto-merged"
    );
}

#[test]
fn auto_merge_succeeds_when_all_conditions_met() {
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"conclusion": "SUCCESS"}
        ],
        "reviewDecision": "APPROVED"
    });

    assert!(
        is_auto_mergeable(&pr),
        "approved PR with green CI should be auto-mergeable"
    );
}

// -------------------------------------------------------------------------
// all_ci_checks_passed — verifies CI status for PR break decisions
// -------------------------------------------------------------------------

#[test]
fn ci_passed_with_all_success() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"conclusion": "SUCCESS"}
        ]
    });

    assert!(
        all_ci_checks_passed(&pr),
        "all SUCCESS conclusions = CI passed"
    );
}

#[test]
fn ci_not_passed_with_failure() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"conclusion": "FAILURE"}
        ]
    });

    assert!(!all_ci_checks_passed(&pr), "any FAILURE = CI not passed");
}

#[test]
fn ci_not_passed_with_pending() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"conclusion": "PENDING"}
        ]
    });

    assert!(
        !all_ci_checks_passed(&pr),
        "any PENDING = CI not passed yet"
    );
}

#[test]
fn ci_not_passed_with_empty_conclusion() {
    // Empty conclusion typically means check is still running
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"conclusion": ""}
        ]
    });

    assert!(
        !all_ci_checks_passed(&pr),
        "empty conclusion = still running = CI not passed"
    );
}

#[test]
fn ci_passed_with_no_checks() {
    // No CI configured = considered passing (repo choice)
    let pr = json!({
        "statusCheckRollup": []
    });

    assert!(
        all_ci_checks_passed(&pr),
        "no CI checks configured = considered passing"
    );
}

// -------------------------------------------------------------------------
// coworker_from_branch — extracts owner from PR branch name
// -------------------------------------------------------------------------

#[test]
fn coworker_from_branch_extracts_prefix() {
    assert_eq!(
        coworker_from_branch("lexington/fix-auth"),
        Some("lexington".to_string())
    );
    assert_eq!(
        coworker_from_branch("amsterdam/feature-123"),
        Some("amsterdam".to_string())
    );
}

#[test]
fn coworker_from_branch_case_insensitive() {
    assert_eq!(
        coworker_from_branch("YORK/big-feature"),
        Some("york".to_string())
    );
    assert_eq!(
        coworker_from_branch("Amsterdam/Fix"),
        Some("amsterdam".to_string())
    );
}

#[test]
fn coworker_from_branch_returns_none_for_unknown() {
    assert_eq!(
        coworker_from_branch("unknown-name/feature"),
        None,
        "unknown branch prefix should return None"
    );
    assert_eq!(
        coworker_from_branch("main"),
        None,
        "branch without slash should return None"
    );
}

// -------------------------------------------------------------------------
// text_contains_review_signature — detects Claude reviews
// -------------------------------------------------------------------------

#[test]
fn review_signature_detects_emoji_signature() {
    assert!(text_contains_review_signature(
        "## Summary\n\nLGTM!\n\n🤖 Reviewed by lexington"
    ));
}

#[test]
fn review_signature_detects_frontmatter_with_review_header() {
    // Frontmatter + review header = valid review
    assert!(text_contains_review_signature(
        "<!-- midtown:reviewer=lexington -->\n\n## Code Review"
    ));
}

#[test]
fn review_signature_detects_header() {
    assert!(text_contains_review_signature(
        "## Code Review by amsterdam\n\nLooks good!"
    ));
}

#[test]
fn review_signature_returns_false_for_normal_comment() {
    assert!(!text_contains_review_signature(
        "Thanks for the PR! I'll take a look."
    ));
}

#[test]
fn review_signature_detects_h3_lowercase_header() {
    // Bug: pleasant's review used "### Code review by" (h3, lowercase "review")
    // but the pattern only matched "## Code Review by" (h2, capital R)
    assert!(text_contains_review_signature(
        "### Code review by pleasant\n\nFound 4 issues:\n\n1. First issue..."
    ));
}

#[test]
fn review_signature_case_insensitive() {
    // Various case combinations should all match
    assert!(text_contains_review_signature("## code review by york"));
    assert!(text_contains_review_signature(
        "### CODE REVIEW BY amsterdam"
    ));
    assert!(text_contains_review_signature("# Code review by pleasant"));
}

#[test]
fn review_signature_rejects_checklist_heading() {
    // Should NOT match headings like "Code Review Checklist" to avoid false positives
    assert!(!text_contains_review_signature(
        "### Code Review Checklist\n\n- [ ] Tests added\n- [ ] Docs updated"
    ));
    assert!(!text_contains_review_signature(
        "## Code Review Process\n\nFollow these steps..."
    ));
    assert!(!text_contains_review_signature(
        "### Code Review Notes\n\nSome observations here"
    ));
}

#[test]
fn review_signature_detects_exact_code_review() {
    // Should match exact "code review" (from code-review skill default output)
    assert!(text_contains_review_signature(
        "### Code review\n\nNo issues found."
    ));
    // But not with trailing text
    assert!(!text_contains_review_signature(
        "### Code review checklist\n\nItems..."
    ));
}

#[test]
fn review_signature_requires_review_header_with_frontmatter() {
    // Bug fix: frontmatter alone should NOT be detected as a review.
    // ALL coworker GitHub comments have frontmatter, so we need to also check
    // for an actual review heading or signature.

    // This is a CI fix explanation, NOT a review:
    assert!(!text_contains_review_signature(
        "<!-- midtown: columbus -->\n\n## Fix for zellij_e2e CI failure\n\nThe test was failing because..."
    ));

    // This is a status update, NOT a review:
    assert!(!text_contains_review_signature(
        "<!-- midtown: broadway -->\n\n## Update\n\nCompleted task 1269, all tests passing."
    ));

    // But frontmatter + review heading SHOULD be detected:
    assert!(text_contains_review_signature(
        "<!-- midtown: columbus -->\n\n## Code Review by columbus\n\nFound 2 issues..."
    ));

    // And frontmatter + exact "code review" heading SHOULD be detected:
    assert!(text_contains_review_signature(
        "<!-- midtown: york -->\n\n### Code review\n\nNo issues found."
    ));
}

// -------------------------------------------------------------------------
// @mention extraction
// -------------------------------------------------------------------------

#[test]
fn extract_mentions_finds_coworker() {
    let mentions = extract_mentions("Hey @york can you look at this?");
    assert_eq!(mentions, vec!["york"]);
}

#[test]
fn extract_mentions_finds_multiple() {
    let mentions = extract_mentions("@amsterdam @broadway please review");
    assert!(mentions.contains(&"amsterdam".to_string()));
    assert!(mentions.contains(&"broadway".to_string()));
}

#[test]
fn extract_mentions_respects_word_boundary() {
    // @yorkshire should not match @york
    let mentions = extract_mentions("Contact @yorkshire for help");
    assert!(mentions.is_empty(), "@yorkshire should not match @york");
}

#[test]
fn contains_at_all_detects_broadcast() {
    assert!(contains_at_all("Hey @all, please check the channel"));
    assert!(contains_at_all("@ALL important update"));
    assert!(!contains_at_all("@alliance meeting tomorrow"));
}

// -------------------------------------------------------------------------
// count_non_owner_comments — polling fallback for review comments
// -------------------------------------------------------------------------

#[test]
fn count_non_owner_comments_excludes_pr_author() {
    let pr = json!({
        "author": {"login": "btucker"},
        "comments": [
            {"author": {"login": "btucker"}, "body": "I'll fix this"},
            {"author": {"login": "reviewer"}, "body": "LGTM!"}
        ]
    });

    assert_eq!(
        count_non_owner_comments(&pr, Some("lexington")),
        1,
        "should exclude PR author's comments"
    );
}

#[test]
fn count_non_owner_comments_excludes_owner_frontmatter() {
    // When a coworker comments (using the shared GitHub account), they
    // include <!-- midtown: name --> frontmatter. Comments from the PR
    // owner's coworker should be excluded.
    let pr = json!({
        "author": {"login": "btucker"},
        "comments": [
            {
                "author": {"login": "btucker"},
                "body": "<!-- midtown: lexington -->\n\nI'll fix this"
            },
            {
                "author": {"login": "btucker"},
                "body": "<!-- midtown: columbus -->\n\nLGTM!"
            }
        ]
    });

    // lexington is the PR owner, so their comment is excluded
    // columbus is a different coworker, so their comment counts
    assert_eq!(
        count_non_owner_comments(&pr, Some("lexington")),
        1,
        "should exclude owner's coworker comments via frontmatter"
    );
}

#[test]
fn count_non_owner_comments_counts_external_reviewers() {
    let pr = json!({
        "author": {"login": "btucker"},
        "comments": [
            {"author": {"login": "external_reviewer"}, "body": "Please add tests"},
            {"author": {"login": "another_reviewer"}, "body": "Looks good!"}
        ]
    });

    assert_eq!(
        count_non_owner_comments(&pr, Some("lexington")),
        2,
        "should count all external reviewer comments"
    );
}

#[test]
fn count_non_owner_comments_returns_zero_for_empty() {
    let pr = json!({
        "author": {"login": "btucker"},
        "comments": []
    });

    assert_eq!(
        count_non_owner_comments(&pr, Some("lexington")),
        0,
        "should return 0 for no comments"
    );
}

#[test]
fn count_non_owner_comments_handles_missing_comments_field() {
    let pr = json!({
        "author": {"login": "btucker"}
    });

    assert_eq!(
        count_non_owner_comments(&pr, Some("lexington")),
        0,
        "should return 0 when comments field is missing"
    );
}

// =========================================================================
// is_gh_auth_error tests
// =========================================================================

#[test]
fn is_gh_auth_error_detects_bad_credentials() {
    assert!(is_gh_auth_error("gh: Bad credentials (HTTP 401)"));
    assert!(is_gh_auth_error("error: bad credentials"));
    assert!(is_gh_auth_error("Bad Credentials"));
}

#[test]
fn is_gh_auth_error_detects_401() {
    assert!(is_gh_auth_error("HTTP 401 Unauthorized"));
    assert!(is_gh_auth_error("status: 401"));
}

#[test]
fn is_gh_auth_error_detects_auth_required() {
    assert!(is_gh_auth_error("authentication required"));
    assert!(is_gh_auth_error("requires authentication"));
    assert!(is_gh_auth_error("not logged in to github"));
}

#[test]
fn is_gh_auth_error_ignores_other_errors() {
    assert!(!is_gh_auth_error("network error"));
    assert!(!is_gh_auth_error("repository not found"));
    assert!(!is_gh_auth_error("rate limit exceeded"));
    assert!(!is_gh_auth_error(""));
}

// =========================================================================
// format_task_prompt tests
// =========================================================================

#[test]
fn format_task_prompt_appends_footer() {
    let result = format_task_prompt(
        "42",
        "You've been assigned task !42: Fix auth bug. Get started!",
    );
    assert_eq!(
        result,
        "You've been assigned task !42: Fix auth bug. Get started!\n\nRun `midtown task view 42` for full details."
    );
}

#[test]
fn format_task_prompt_works_with_recovery_context() {
    let result = format_task_prompt(
        "99",
        "Resume task !99: Add tests. The daemon was restarted and discovered you still running. Check your git status and continue where you left off.",
    );
    assert!(result.starts_with("Resume task !99:"));
    assert!(result.ends_with("Run `midtown task view 99` for full details."));
}

#[test]
fn format_task_prompt_works_with_nudge_context() {
    let result = format_task_prompt(
        "7",
        "You have pending task !7: Deploy service. Get started!",
    );
    assert!(result.contains("pending task !7"));
    assert!(result.contains("midtown task view 7"));
}

// =========================================================================
// format_cross_post_content tests
// =========================================================================

#[test]
fn format_cross_post_content_includes_channel_prefix() {
    let msg = crate::message::Message::for_channel(
        "auth-refactor",
        "park",
        "💡 The tower::Layer stack composes auth providers independently",
        crate::message::MessageType::Text,
    );
    let result = format_cross_post_content(&msg);
    assert_eq!(
        result,
        "#auth-refactor | 💡 The tower::Layer stack composes auth providers independently"
    );
}

#[test]
fn format_cross_post_content_omits_author_from_content() {
    // Author is omitted because the `from` field already carries it
    let msg = crate::message::Message::for_channel(
        "perf-tuning",
        "broadway",
        "💡 Connection pooling reduces latency by 40%",
        crate::message::MessageType::Text,
    );
    let result = format_cross_post_content(&msg);
    assert_eq!(
        result,
        "#perf-tuning | 💡 Connection pooling reduces latency by 40%"
    );
    // Verify author is NOT in the formatted content
    assert!(!result.contains("broadway:"));
}

// =========================================================================
// should_cross_post_insight tests
// =========================================================================

#[test]
fn should_cross_post_insight_detects_insight_in_topic_channel() {
    let msg = crate::message::Message::for_channel(
        "auth-refactor",
        "park",
        "💡 The tower::Layer stack composes auth providers independently",
        crate::message::MessageType::Text,
    );
    assert!(should_cross_post_insight(&msg, "midtown"));
}

#[test]
fn should_cross_post_insight_ignores_insight_in_main_channel() {
    let msg = crate::message::Message::for_channel(
        "midtown",
        "park",
        "💡 This insight is already in main channel",
        crate::message::MessageType::Text,
    );
    assert!(!should_cross_post_insight(&msg, "midtown"));
}

#[test]
fn should_cross_post_insight_ignores_non_insight_in_topic_channel() {
    let msg = crate::message::Message::for_channel(
        "auth-refactor",
        "park",
        "Working on the auth module",
        crate::message::MessageType::Text,
    );
    assert!(!should_cross_post_insight(&msg, "midtown"));
}

#[test]
fn should_cross_post_insight_ignores_non_insight_in_main_channel() {
    let msg = crate::message::Message::for_channel(
        "midtown",
        "park",
        "Regular message in main channel",
        crate::message::MessageType::Text,
    );
    assert!(!should_cross_post_insight(&msg, "midtown"));
}

// =========================================================================
// extract_task_id tests
// =========================================================================

#[test]
fn extract_task_id_from_task_bang_pattern() {
    assert_eq!(
        extract_task_id("Task !42 reset to pending"),
        Some("42".to_string())
    );
    assert_eq!(
        extract_task_id("task !123 completed"),
        Some("123".to_string())
    );
    assert_eq!(extract_task_id("TASK !99 assigned"), Some("99".to_string()));
}

#[test]
fn extract_task_id_from_standalone_bang() {
    assert_eq!(extract_task_id("Completed !42"), Some("42".to_string()));
    assert_eq!(extract_task_id("Working on !5"), Some("5".to_string()));
}

#[test]
fn extract_task_id_from_start_of_message() {
    assert_eq!(
        extract_task_id("!42 needs attention"),
        Some("42".to_string())
    );
}

#[test]
fn extract_task_id_returns_none_for_no_match() {
    assert_eq!(extract_task_id("No task mentioned here"), None);
    assert_eq!(extract_task_id("Just some text"), None);
}

#[test]
fn extract_task_id_takes_first_match() {
    // If multiple task refs, takes the first
    assert_eq!(
        extract_task_id("Task !10 blocks task !20"),
        Some("10".to_string())
    );
}
