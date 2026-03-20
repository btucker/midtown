use super::*;
use serde_json::json;

#[test]
fn default_model_for_provider_role_uses_codex_model_for_all_roles() {
    let provider = crate::auth::AuthProvider::Codex;
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-project-lead"),
        "gpt-5-codex"
    );
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-code-author"),
        "gpt-5-codex"
    );
}

#[test]
fn default_model_for_provider_role_uses_claude_tiers() {
    let provider = crate::auth::AuthProvider::Claude;
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-project-lead"),
        "opus"
    );
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-code-author"),
        "sonnet"
    );
}

#[test]
fn normalize_model_for_provider_role_rewrites_claude_alias_for_codex() {
    let normalized = normalize_model_for_provider_role(
        "sonnet",
        crate::auth::AuthProvider::Codex,
        "midtown-channel-lead",
    );
    assert_eq!(normalized, "gpt-5-codex");
}

#[test]
fn normalize_model_for_provider_role_keeps_codex_alias_for_codex() {
    let normalized = normalize_model_for_provider_role(
        "o3",
        crate::auth::AuthProvider::Codex,
        "midtown-code-author",
    );
    assert_eq!(normalized, "o3");
}

#[test]
fn normalize_model_for_provider_role_rewrites_codex_alias_for_claude() {
    let normalized = normalize_model_for_provider_role(
        "gpt-5.3-codex",
        crate::auth::AuthProvider::Claude,
        "midtown-project-lead",
    );
    assert_eq!(normalized, "opus");
}

#[test]
fn normalize_model_for_provider_role_maps_size_aliases_for_claude() {
    let small = normalize_model_for_provider_role(
        "small",
        crate::auth::AuthProvider::Claude,
        "midtown-code-author",
    );
    let medium = normalize_model_for_provider_role(
        "medium",
        crate::auth::AuthProvider::Claude,
        "midtown-project-lead",
    );
    assert_eq!(small, "haiku");
    assert_eq!(medium, "sonnet");
}

#[test]
fn normalize_model_for_provider_role_maps_size_aliases_for_zai() {
    let small = normalize_model_for_provider_role(
        "small",
        crate::auth::AuthProvider::Zai,
        "midtown-code-author",
    );
    let medium = normalize_model_for_provider_role(
        "medium",
        crate::auth::AuthProvider::Zai,
        "midtown-project-lead",
    );
    let large = normalize_model_for_provider_role(
        "large",
        crate::auth::AuthProvider::Zai,
        "midtown-code-reviewer",
    );
    assert_eq!(small, "haiku");
    assert_eq!(medium, "sonnet");
    assert_eq!(large, "opus");
}

#[test]
fn normalize_model_for_provider_role_maps_size_aliases_for_codex() {
    let small = normalize_model_for_provider_role(
        "small",
        crate::auth::AuthProvider::Codex,
        "midtown-code-author",
    );
    let medium = normalize_model_for_provider_role(
        "medium",
        crate::auth::AuthProvider::Codex,
        "midtown-project-lead",
    );
    let large = normalize_model_for_provider_role(
        "large",
        crate::auth::AuthProvider::Codex,
        "midtown-code-reviewer",
    );
    assert_eq!(small, "gpt-5.1-codex-mini");
    assert_eq!(medium, "gpt-5.3-codex-spark");
    assert_eq!(large, "gpt-5.3-codex");
}

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
fn detect_issues_finds_ci_failure_from_commit_status() {
    // Commit statuses (e.g. Codecov) use `state` instead of `conclusion`
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"state": "failure"}
        ],
        "reviewDecision": ""
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.contains(&PrIssueType::CiFailed),
        "should detect CI failure from commit status `state` field"
    );
}

#[test]
fn detect_issues_finds_ci_error_from_commit_status() {
    // Commit statuses can also report `error`
    let pr = json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"state": "error"}
        ],
        "reviewDecision": ""
    });

    let issues = detect_pr_issues(&pr);

    assert!(
        issues.contains(&PrIssueType::CiFailed),
        "should detect CI error from commit status `state` field"
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

#[test]
fn auto_mergeable_with_commit_status_success() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"state": "success"}
        ],
        "reviewDecision": "APPROVED"
    });

    assert!(
        is_auto_mergeable(&pr),
        "approved PR with commit status state=success should be auto-mergeable"
    );
}

#[test]
fn not_auto_mergeable_with_commit_status_pending() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"state": "pending"}
        ],
        "reviewDecision": "APPROVED"
    });

    assert!(
        !is_auto_mergeable(&pr),
        "approved PR with commit status state=pending should not be auto-mergeable"
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

#[test]
fn ci_passed_with_commit_status_success() {
    // Commit statuses (like Codecov) use `state` instead of `conclusion`
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"state": "success"}
        ]
    });

    assert!(
        all_ci_checks_passed(&pr),
        "commit status with state=success should count as passed"
    );
}

#[test]
fn ci_not_passed_with_commit_status_failure() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"state": "failure"}
        ]
    });

    assert!(
        !all_ci_checks_passed(&pr),
        "commit status with state=failure should count as failed"
    );
}

#[test]
fn ci_not_passed_with_commit_status_pending() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"state": "pending"}
        ]
    });

    assert!(
        !all_ci_checks_passed(&pr),
        "commit status with state=pending should count as pending"
    );
}

#[test]
fn ci_not_passed_with_commit_status_error() {
    let pr = json!({
        "statusCheckRollup": [
            {"conclusion": "SUCCESS"},
            {"state": "error"}
        ]
    });

    assert!(
        !all_ci_checks_passed(&pr),
        "commit status with state=error should count as failed"
    );
}

// -------------------------------------------------------------------------
// text_contains_review_signature — detects Claude reviews
// -------------------------------------------------------------------------

#[test]
fn review_signature_detects_emoji_signature() {
    // Text-based emoji signatures no longer match — only type:review frontmatter does.
    assert!(!text_contains_review_signature(
        "## Summary\n\nLGTM!\n\n🤖 Reviewed by lexington"
    ));
}

#[test]
fn review_signature_detects_frontmatter_with_review_header() {
    // Only type:review frontmatter matches; old-style frontmatter does not.
    assert!(!text_contains_review_signature(
        "<!-- midtown:reviewer=lexington -->\n\n## Code Review"
    ));
    // New structured frontmatter with type:review does match.
    assert!(text_contains_review_signature(
        "<!-- midtown session:abc type:review -->\n\n## Code Review"
    ));
}

#[test]
fn review_signature_detects_header() {
    // Text-based headers no longer match without type:review frontmatter.
    assert!(!text_contains_review_signature(
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
    // Text-based headers (even h3 lowercase) no longer match without type:review frontmatter.
    assert!(!text_contains_review_signature(
        "### Code review by pleasant\n\nFound 4 issues:\n\n1. First issue..."
    ));
}

#[test]
fn review_signature_case_insensitive() {
    // Text-based case variations no longer match — only type:review frontmatter does.
    assert!(!text_contains_review_signature("## code review by york"));
    assert!(!text_contains_review_signature(
        "### CODE REVIEW BY amsterdam"
    ));
    assert!(!text_contains_review_signature("# Code review by pleasant"));
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
    // Text-based "code review" headings no longer match without type:review frontmatter.
    assert!(!text_contains_review_signature(
        "### Code review\n\nNo issues found."
    ));
    assert!(!text_contains_review_signature(
        "### Code review checklist\n\nItems..."
    ));
}

#[test]
fn review_signature_requires_review_header_with_frontmatter() {
    // Only structured type:review frontmatter is detected — text headings are not enough.

    // Old-style frontmatter without type:review is NOT a review, regardless of heading:
    assert!(!text_contains_review_signature(
        "<!-- midtown: columbus -->\n\n## Fix for zellij_e2e CI failure\n\nThe test was failing because..."
    ));
    assert!(!text_contains_review_signature(
        "<!-- midtown: broadway -->\n\n## Update\n\nCompleted task 1269, all tests passing."
    ));
    assert!(!text_contains_review_signature(
        "<!-- midtown: columbus -->\n\n## Code Review by columbus\n\nFound 2 issues..."
    ));
    assert!(!text_contains_review_signature(
        "<!-- midtown: york -->\n\n### Code review\n\nNo issues found."
    ));

    // Only new structured frontmatter with type:review matches:
    assert!(text_contains_review_signature(
        "<!-- midtown session:abc type:review -->\n\n## Code Review\n\nFound 2 issues..."
    ));
}

#[test]
fn review_signature_rejects_lowercase_reviewed_by() {
    // Lowercase "reviewed by" in prose should NOT match — the case-sensitive check
    // for "Reviewed by" (capital R) prevents false positives from natural English.
    assert!(!text_contains_review_signature(
        "This change was reviewed by the platform team last week."
    ));
    assert!(!text_contains_review_signature(
        "The code was reviewed by security before release."
    ));
}

#[test]
fn review_signature_matches_capital_reviewed_by_anywhere() {
    // "Reviewed by" text signatures no longer match — only type:review frontmatter does.
    assert!(!text_contains_review_signature(
        "Some intro text\nReviewed by lexington\nFooter"
    ));
    assert!(!text_contains_review_signature("  Reviewed by madison"));
    assert!(!text_contains_review_signature("LGTM! Reviewed by york"));
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

#[test]
fn extract_task_id_from_lead_at_mention_pattern() {
    // The canonical lead message format for task-based routing: "@name !N message"
    // This is the primary use case for task-based @mention routing in chat.rs.
    assert_eq!(
        extract_task_id("@park !42 here's the review feedback"),
        Some("42".to_string())
    );
    assert_eq!(
        extract_task_id("@lexington !1234 please fix the auth bug"),
        Some("1234".to_string())
    );
    // Works even when @name and !N are the only content
    assert_eq!(extract_task_id("@amsterdam !7"), Some("7".to_string()));
    // No task ID in plain @mention
    assert_eq!(extract_task_id("@park check this out"), None);
}

#[test]
fn resolve_model_for_role_respects_config_or_falls_back() {
    // resolve_model_for_role reads from config. When config has a value,
    // it normalizes it for the provider. When not set, it uses the hardcoded
    // default from default_model_for_provider_role.
    let repo = "nonexistent-test-repo";
    let claude = crate::auth::AuthProvider::Claude;
    let codex = crate::auth::AuthProvider::Codex;

    // The result should match what get_model_for_role returns (config-aware),
    // normalized for the provider, or the hardcoded default if no config.
    let coworker_model = resolve_model_for_role(repo, claude, "midtown-code-author");
    let lead_model = resolve_model_for_role(repo, claude, "midtown-project-lead");

    // Both should be valid Claude model names
    assert!(
        ["haiku", "sonnet", "opus"].contains(&coworker_model.as_str()),
        "coworker model '{}' should be a valid Claude model alias",
        coworker_model
    );
    assert!(
        ["haiku", "sonnet", "opus"].contains(&lead_model.as_str()),
        "lead model '{}' should be a valid Claude model alias",
        lead_model
    );

    // With config set to "large", all roles should resolve to "opus" for Claude
    if crate::config::get_model_for_role(repo, crate::config::ExecutionRole::Coworker)
        == Some(crate::config::ModelSize::Large)
    {
        assert_eq!(coworker_model, "opus");
        assert_eq!(lead_model, "opus");
    }

    // Codex should produce Codex-compatible model names
    let codex_coworker = resolve_model_for_role(repo, codex, "midtown-code-author");
    assert!(
        !codex_coworker.contains("sonnet") && !codex_coworker.contains("opus"),
        "Codex coworker model '{}' should not contain Claude aliases",
        codex_coworker
    );
}

#[test]
fn resolve_model_for_role_matches_default_when_no_config() {
    // When get_model_for_role returns None, resolve should match the hardcoded default.
    let repo = "nonexistent-test-repo";
    let claude = crate::auth::AuthProvider::Claude;

    if crate::config::get_model_for_role(repo, crate::config::ExecutionRole::Coworker).is_none() {
        assert_eq!(
            resolve_model_for_role(repo, claude, "midtown-code-author"),
            default_model_for_provider_role(claude, "midtown-code-author")
        );
        assert_eq!(
            resolve_model_for_role(repo, claude, "midtown-project-lead"),
            default_model_for_provider_role(claude, "midtown-project-lead")
        );
    }
}

// ---------------------------------------------------------------------------
// provider_for_model_alias tests
// ---------------------------------------------------------------------------

#[test]
fn provider_for_model_alias_claude_models() {
    use crate::auth::AuthProvider;
    assert_eq!(provider_for_model_alias("opus"), Some(AuthProvider::Claude));
    assert_eq!(
        provider_for_model_alias("sonnet"),
        Some(AuthProvider::Claude)
    );
    assert_eq!(
        provider_for_model_alias("haiku"),
        Some(AuthProvider::Claude)
    );
    // Full model names containing the alias
    assert_eq!(
        provider_for_model_alias("claude-sonnet-4-6"),
        Some(AuthProvider::Claude)
    );
}

#[test]
fn provider_for_model_alias_codex_models() {
    use crate::auth::AuthProvider;
    assert_eq!(
        provider_for_model_alias("gpt-5-codex"),
        Some(AuthProvider::Codex)
    );
    assert_eq!(
        provider_for_model_alias("gpt-5.1-codex-mini"),
        Some(AuthProvider::Codex)
    );
    assert_eq!(provider_for_model_alias("o3"), Some(AuthProvider::Codex));
    assert_eq!(
        provider_for_model_alias("o4-mini"),
        Some(AuthProvider::Codex)
    );
}

#[test]
fn provider_for_model_alias_size_aliases_return_none() {
    assert_eq!(provider_for_model_alias("small"), None);
    assert_eq!(provider_for_model_alias("medium"), None);
    assert_eq!(provider_for_model_alias("large"), None);
}

#[test]
fn provider_for_model_alias_unknown_returns_none() {
    assert_eq!(provider_for_model_alias(""), None);
    assert_eq!(provider_for_model_alias("some-custom-model"), None);
}

// ---------------------------------------------------------------------------
// Agent definition auth_provider resolution tests
// ---------------------------------------------------------------------------

/// Reproduces the bug: when an agent definition specifies `model: opus`,
/// the LaunchConfig should have auth_provider set to Claude. Before the fix,
/// auth_provider was left as the passed-in provider parameter regardless of
/// the agent's model, causing spawn_coworker() to silently normalize the
/// model to match the wrong provider.
#[test]
fn agent_model_override_resolves_auth_provider() {
    use crate::auth::AuthProvider;

    // Simulate the config construction from handle_coworker_spawn
    // with an agent definition that specifies model: opus
    let agent_model = Some("opus".to_string());
    let passed_in_provider = AuthProvider::Codex; // wrong provider for "opus"

    // The fix: resolve auth_provider from the agent's model
    let resolved_provider = agent_model
        .as_deref()
        .and_then(provider_for_model_alias)
        .unwrap_or(passed_in_provider);

    assert_eq!(
        resolved_provider,
        AuthProvider::Claude,
        "Agent specifying model 'opus' should resolve to Claude provider, \
         not keep the passed-in Codex provider"
    );
}

/// When agent definition specifies a size alias like "large", the provider
/// should be kept as the passed-in provider (size aliases are provider-agnostic).
#[test]
fn agent_model_size_alias_keeps_passed_provider() {
    use crate::auth::AuthProvider;

    let agent_model = Some("large".to_string());
    let passed_in_provider = AuthProvider::Codex;

    let resolved_provider = agent_model
        .as_deref()
        .and_then(provider_for_model_alias)
        .unwrap_or(passed_in_provider);

    assert_eq!(
        resolved_provider,
        AuthProvider::Codex,
        "Size alias 'large' should keep the passed-in provider"
    );
}

/// When no agent model is specified, provider stays as passed-in.
#[test]
fn no_agent_model_keeps_passed_provider() {
    use crate::auth::AuthProvider;

    let agent_model: Option<String> = None;
    let passed_in_provider = AuthProvider::Claude;

    let resolved_provider = agent_model
        .as_deref()
        .and_then(provider_for_model_alias)
        .unwrap_or(passed_in_provider);

    assert_eq!(resolved_provider, AuthProvider::Claude);
}

// ---------------------------------------------------------------------------
// Addressed-review tag tests
// ---------------------------------------------------------------------------

#[test]
fn parse_addresses_review_tag_extracts_id() {
    let body =
        "<!-- midtown: broadway -->\n<!-- addresses-review: 12345 -->\n✅ Addressed in abc1234";
    assert_eq!(parse_addresses_review_tag(body), Some(12345));
}

#[test]
fn parse_addresses_review_tag_with_whitespace() {
    let body = "<!-- addresses-review:  67890  -->\nDone";
    assert_eq!(parse_addresses_review_tag(body), Some(67890));
}

#[test]
fn parse_addresses_review_tag_no_tag() {
    let body = "<!-- midtown: broadway -->\n✅ Addressed in abc1234";
    assert_eq!(parse_addresses_review_tag(body), None);
}

#[test]
fn parse_addresses_review_tag_invalid_id() {
    let body = "<!-- addresses-review: not-a-number -->";
    assert_eq!(parse_addresses_review_tag(body), None);
}

#[test]
fn all_review_feedback_addressed_empty_review_ids() {
    let (addressed, unaddressed) = all_review_feedback_addressed(&[], &[]);
    assert!(addressed);
    assert!(unaddressed.is_empty());
}

#[test]
fn all_review_feedback_addressed_all_addressed() {
    let review_ids = vec![111, 222];
    let comments = vec![
        json!({"body": "<!-- addresses-review: 111 -->\n✅ Fixed"}),
        json!({"body": "<!-- addresses-review: 222 -->\n✅ Also fixed"}),
    ];
    let (addressed, unaddressed) = all_review_feedback_addressed(&review_ids, &comments);
    assert!(addressed);
    assert!(unaddressed.is_empty());
}

#[test]
fn all_review_feedback_addressed_some_unaddressed() {
    let review_ids = vec![111, 222, 333];
    let comments = vec![
        json!({"body": "<!-- addresses-review: 111 -->\n✅ Fixed"}),
        json!({"body": "Just a normal comment, no tag"}),
    ];
    let (addressed, unaddressed) = all_review_feedback_addressed(&review_ids, &comments);
    assert!(!addressed);
    assert_eq!(unaddressed, vec![222, 333]);
}

#[test]
fn all_review_feedback_addressed_no_comments() {
    let review_ids = vec![111];
    let comments: Vec<serde_json::Value> = vec![];
    let (addressed, unaddressed) = all_review_feedback_addressed(&review_ids, &comments);
    assert!(!addressed);
    assert_eq!(unaddressed, vec![111]);
}

/// Integration test: simulates the full Gate 3 flow as used in `handle_pr_merge`.
///
/// 1. Populate `pr_review_comment_ids` (via `add_review_comment_id`)
/// 2. Simulate PR comments with/without `addresses-review` tags
/// 3. Verify Gate 3 correctly identifies unaddressed feedback
///
/// This mirrors the Gate 3 logic in `rpc_prs.rs::handle_pr_merge`:
/// ```ignore
/// let review_comment_ids = ps.github.get_review_comment_ids(pr_number);
/// if !review_comment_ids.is_empty() {
///     let (all_addressed, unaddressed) = all_review_feedback_addressed(&ids, &comments);
///     if !all_addressed { /* reject merge */ }
/// }
/// ```
#[test]
fn gate3_blocks_merge_when_feedback_unaddressed() {
    use crate::github_state::GitHubState;

    // Step 1: Simulate webhook recording a review comment ID
    let mut github_state = GitHubState::default();
    github_state.add_review_comment_id(42, 98765);

    // Step 2: Get the review comment IDs (as handle_pr_merge does)
    let review_comment_ids = github_state.get_review_comment_ids(42).to_vec();
    assert_eq!(review_comment_ids, vec![98765]);

    // Step 3a: No addresses-review tags exist → Gate 3 should block
    let comments_no_address: Vec<serde_json::Value> = vec![
        json!({"body": "<!-- midtown: columbus -->\n✅ Addressed in abc1234"}),
        json!({"body": "Thanks for the fix!"}),
    ];
    let (all_addressed, unaddressed) =
        all_review_feedback_addressed(&review_comment_ids, &comments_no_address);
    assert!(
        !all_addressed,
        "Gate 3 should block: no addresses-review tag for comment 98765"
    );
    assert_eq!(unaddressed, vec![98765]);

    // Step 3b: Correct addresses-review tag exists → Gate 3 should pass
    let comments_addressed: Vec<serde_json::Value> = vec![
        json!({"body": "<!-- midtown: columbus -->\n<!-- addresses-review: 98765 -->\n✅ Addressed in abc1234"}),
    ];
    let (all_addressed, unaddressed) =
        all_review_feedback_addressed(&review_comment_ids, &comments_addressed);
    assert!(
        all_addressed,
        "Gate 3 should pass: addresses-review tag matches comment 98765"
    );
    assert!(unaddressed.is_empty());
}

/// Integration test: Gate 3 with multiple review comments (multiple reviewers).
#[test]
fn gate3_tracks_multiple_review_comments() {
    use crate::github_state::GitHubState;

    let mut github_state = GitHubState::default();
    // Two separate review comments on the same PR
    github_state.add_review_comment_id(42, 111);
    github_state.add_review_comment_id(42, 222);

    let review_ids = github_state.get_review_comment_ids(42).to_vec();

    // Only one addressed → should block
    let comments = vec![json!({"body": "<!-- addresses-review: 111 -->\n✅ Fixed"})];
    let (all_addressed, unaddressed) = all_review_feedback_addressed(&review_ids, &comments);
    assert!(!all_addressed);
    assert_eq!(unaddressed, vec![222]);

    // Both addressed → should pass
    let comments = vec![
        json!({"body": "<!-- addresses-review: 111 -->\n✅ Fixed"}),
        json!({"body": "<!-- addresses-review: 222 -->\n✅ Also fixed"}),
    ];
    let (all_addressed, unaddressed) = all_review_feedback_addressed(&review_ids, &comments);
    assert!(all_addressed);
    assert!(unaddressed.is_empty());
}

/// Gate 3 is a no-op when no review comment IDs are recorded
/// (e.g., for formal GitHub reviews that don't use issue comments).
#[test]
fn gate3_passes_when_no_review_comment_ids() {
    use crate::github_state::GitHubState;

    let github_state = GitHubState::default();
    let review_ids = github_state.get_review_comment_ids(42).to_vec();
    assert!(review_ids.is_empty());

    // Empty review_ids → Gate 3 is not checked (always passes)
    let (all_addressed, unaddressed) = all_review_feedback_addressed(&review_ids, &[]);
    assert!(all_addressed);
    assert!(unaddressed.is_empty());
}

// ── extract_review_author_from_body tests ───────────────────────────────

#[test]
fn extract_author_from_frontmatter() {
    let body = "<!-- midtown: pleasant -->\n## Code Review by pleasant\nLooks good!";
    assert_eq!(
        extract_review_author_from_body(body),
        Some("pleasant".to_string())
    );
}

#[test]
fn extract_author_from_frontmatter_case_insensitive() {
    let body = "<!-- midtown: Pleasant -->\nReviewed by pleasant";
    assert_eq!(
        extract_review_author_from_body(body),
        Some("pleasant".to_string())
    );
}

#[test]
fn extract_author_from_reviewed_by() {
    let body = "LGTM! Reviewed by columbus\nAll checks pass.";
    assert_eq!(
        extract_review_author_from_body(body),
        Some("columbus".to_string())
    );
}

#[test]
fn extract_author_from_emoji_reviewed_by() {
    let body = "🤖 Reviewed by lexington\nGreat code!";
    assert_eq!(
        extract_review_author_from_body(body),
        Some("lexington".to_string())
    );
}

#[test]
fn extract_author_from_code_review_header() {
    let body = "## Code Review by madison\n\nLooks clean.";
    assert_eq!(
        extract_review_author_from_body(body),
        Some("madison".to_string())
    );
}

#[test]
fn extract_author_none_for_plain_code_review() {
    // "### Code review" without "by NAME" and no frontmatter — author unknown
    let body = "### Code review\n\nNo issues found.";
    assert_eq!(extract_review_author_from_body(body), None);
}

#[test]
fn extract_author_none_for_regular_comment() {
    let body = "Just a regular comment, not a review.";
    assert_eq!(extract_review_author_from_body(body), None);
}

#[test]
fn extract_author_frontmatter_takes_priority() {
    // Frontmatter says "amsterdam" but body says "Reviewed by broadway"
    // Frontmatter should win (it's the explicit attribution)
    let body = "<!-- midtown: amsterdam -->\nReviewed by broadway";
    assert_eq!(
        extract_review_author_from_body(body),
        Some("amsterdam".to_string())
    );
}

// ── review_author_matches tests ─────────────────────────────────────────

#[test]
fn review_author_matches_no_assigned_reviewer() {
    // No assigned reviewer — accept any review
    assert!(review_author_matches("Reviewed by anyone", None, None));
}

#[test]
fn review_author_matches_correct_reviewer() {
    let body = "<!-- midtown: pleasant -->\n## Code Review by pleasant\nLGTM";
    assert!(review_author_matches(body, Some("pleasant"), None));
}

#[test]
fn review_author_matches_wrong_reviewer() {
    // Bot or different coworker posted the review
    let body = "<!-- midtown: codecov -->\nReviewed by codecov";
    assert!(
        !review_author_matches(body, Some("pleasant"), None),
        "Should reject review from wrong author"
    );
}

#[test]
fn review_author_matches_case_insensitive() {
    let body = "<!-- midtown: Pleasant -->\nReviewed by Pleasant";
    assert!(review_author_matches(body, Some("pleasant"), None));
}

#[test]
fn review_author_matches_rejects_unknown_author() {
    // Review exists but no author can be determined — conservative rejection
    // This prevents bot comments that have no midtown frontmatter from
    // being treated as the assigned reviewer's review
    let body = "### Code review\n\nNo issues found.";
    assert!(
        !review_author_matches(body, Some("pleasant"), None),
        "Unknown author should not match assigned reviewer"
    );
}

#[test]
fn review_author_matches_bot_comment_rejected() {
    // Simulates the PR #1657 bug: a bot (codecov) posts a comment that
    // happens to be detected as a review signature. The assigned reviewer
    // (pleasant) hasn't posted yet, so this should NOT mark the PR as reviewed.
    let body = "Some bot output that doesn't have midtown frontmatter";
    assert!(
        !review_author_matches(body, Some("pleasant"), None),
        "Bot comment without frontmatter should not match assigned reviewer"
    );
}

// ---------------------------------------------------------------------------
// parse_frontmatter tests
// ---------------------------------------------------------------------------

#[test]
fn parse_frontmatter_placeholder() {
    let body = "<!-- midtown task:42 type:review-placeholder -->\n## Review Status";
    let fm = parse_frontmatter(body).unwrap();
    assert_eq!(fm.task_id.as_deref(), Some("42"));
    assert!(fm.is_placeholder());
    assert!(fm.session_id.is_none());
}

#[test]
fn parse_frontmatter_review() {
    let body = "<!-- midtown session:abc123 task:42 type:review -->\n## Code Review\nLGTM";
    let fm = parse_frontmatter(body).unwrap();
    assert_eq!(fm.session_id.as_deref(), Some("abc123"));
    assert_eq!(fm.task_id.as_deref(), Some("42"));
    assert!(fm.is_review());
}

#[test]
fn parse_frontmatter_session_only() {
    let body = "<!-- midtown session:xyz789 -->\nSome comment";
    let fm = parse_frontmatter(body).unwrap();
    assert_eq!(fm.session_id.as_deref(), Some("xyz789"));
    assert!(fm.task_id.is_none());
    assert!(fm.comment_type.is_none());
}

#[test]
fn parse_frontmatter_no_tag() {
    let body = "Just a regular comment with no midtown tag";
    assert!(parse_frontmatter(body).is_none());
}

#[test]
fn parse_frontmatter_legacy_not_parsed_as_structured() {
    // Legacy `<!-- midtown: name -->` should return Some but with no fields set
    let body = "<!-- midtown: park -->\n## Code Review by park";
    let fm = parse_frontmatter(body).unwrap();
    assert!(fm.session_id.is_none());
    assert!(fm.task_id.is_none());
    assert!(fm.comment_type.is_none());
}

#[test]
fn format_placeholder_frontmatter_roundtrip() {
    let tag = format_placeholder_frontmatter("42");
    let fm = parse_frontmatter(&tag).unwrap();
    assert_eq!(fm.task_id.as_deref(), Some("42"));
    assert!(fm.is_placeholder());
}

#[test]
fn format_review_frontmatter_roundtrip() {
    let tag = format_review_frontmatter("sess-abc", "42");
    let fm = parse_frontmatter(&tag).unwrap();
    assert_eq!(fm.session_id.as_deref(), Some("sess-abc"));
    assert_eq!(fm.task_id.as_deref(), Some("42"));
    assert!(fm.is_review());
}

#[test]
fn format_session_frontmatter_roundtrip() {
    let tag = format_session_frontmatter("sess-xyz");
    let fm = parse_frontmatter(&tag).unwrap();
    assert_eq!(fm.session_id.as_deref(), Some("sess-xyz"));
    assert!(fm.task_id.is_none());
}

#[test]
fn extract_review_session_id_from_review() {
    let body = "<!-- midtown session:abc123 task:42 type:review -->\n## Code Review\nLGTM";
    assert_eq!(extract_review_session_id(body), Some("abc123".to_string()));
}

#[test]
fn extract_review_session_id_from_non_review() {
    // Session-only comment (no type:review) should return None
    let body = "<!-- midtown session:abc123 -->\nSome comment";
    assert_eq!(extract_review_session_id(body), None);
}

#[test]
fn review_author_matches_by_session_id() {
    let body = "<!-- midtown session:sess-42 task:100 type:review -->\n## Code Review\nLGTM";
    // Match by session ID even though name doesn't match
    assert!(review_author_matches(body, Some("park"), Some("sess-42")));
}

#[test]
fn review_author_matches_session_id_mismatch() {
    let body = "<!-- midtown session:sess-42 task:100 type:review -->\n## Code Review\nLGTM";
    // Wrong session ID and wrong name
    assert!(!review_author_matches(body, Some("park"), Some("sess-99")));
}

#[test]
fn text_contains_review_signature_new_format() {
    let body = "<!-- midtown session:abc task:42 type:review -->\n## Code Review\nLGTM";
    assert!(text_contains_review_signature(body));
}

#[test]
fn text_contains_review_signature_placeholder_not_review() {
    let body = "<!-- midtown task:42 type:review-placeholder -->\n## Review Status\nIn progress...";
    // Placeholder is NOT a review signature
    assert!(!text_contains_review_signature(body));
}

// ---------------------------------------------------------------------------
// json_has_completed_review with session ID tests
// ---------------------------------------------------------------------------

#[test]
fn json_review_matches_by_session_id() {
    let json = serde_json::json!({
        "reviews": [],
        "comments": [
            {
                "body": "<!-- midtown session:sess-42 task:100 type:review -->\n## Code Review\nLGTM",
                "author": {"login": "btucker"}
            }
        ]
    });

    // Name doesn't match but session ID does
    assert!(super::super::pr::json_has_completed_review(
        &json,
        Some("park"),
        Some("sess-42")
    ));
}

#[test]
fn json_review_rejects_wrong_session_id() {
    let json = serde_json::json!({
        "reviews": [],
        "comments": [
            {
                "body": "<!-- midtown session:sess-42 task:100 type:review -->\n## Code Review\nLGTM",
                "author": {"login": "btucker"}
            }
        ]
    });

    // Both name and session ID mismatch
    assert!(!super::super::pr::json_has_completed_review(
        &json,
        Some("park"),
        Some("sess-99")
    ));
}

// ---------------------------------------------------------------------------
// is_non_lead_coworker tests
// ---------------------------------------------------------------------------

#[test]
fn is_non_lead_coworker_excludes_project_lead() {
    let channel_leads = std::collections::HashSet::new();
    assert!(!is_non_lead_coworker("midtown", "midtown", &channel_leads));
    assert!(!is_non_lead_coworker("lead", "midtown", &channel_leads));
}

#[test]
fn is_non_lead_coworker_excludes_channel_leads() {
    let channel_leads: std::collections::HashSet<String> =
        ["ops".to_string()].into_iter().collect();
    assert!(!is_non_lead_coworker("ops", "midtown", &channel_leads));
}

#[test]
fn is_non_lead_coworker_includes_regular_coworkers() {
    let channel_leads: std::collections::HashSet<String> =
        ["ops".to_string()].into_iter().collect();
    assert!(is_non_lead_coworker("lexington", "midtown", &channel_leads));
    assert!(is_non_lead_coworker("madison", "midtown", &channel_leads));
}

// ---------------------------------------------------------------------------
// PrFields tests
// ---------------------------------------------------------------------------

#[test]
fn pr_fields_from_json_extracts_core_fields() {
    let pr = json!({
        "number": 42,
        "title": "Fix the auth bug",
        "headRefName": "lexington/fix-auth",
        "isDraft": false,
    });
    let pf = PrFields::from_json(&pr);
    assert_eq!(pf.number, 42);
    assert_eq!(pf.title, "Fix the auth bug");
    assert_eq!(pf.head_ref, "lexington/fix-auth");
    assert!(!pf.is_draft);
}

#[test]
fn pr_fields_from_json_defaults_missing_fields() {
    let pr = json!({});
    let pf = PrFields::from_json(&pr);
    assert_eq!(pf.number, 0);
    assert_eq!(pf.title, "");
    assert_eq!(pf.head_ref, "");
    assert!(!pf.is_draft);
}

#[test]
fn pr_fields_author_login_and_review_decision() {
    let pr = json!({
        "number": 99,
        "title": "Add feature",
        "headRefName": "york/add-feature",
        "isDraft": true,
        "author": {"login": "btucker"},
        "reviewDecision": "APPROVED",
        "mergeable": "CONFLICTING",
    });
    let pf = PrFields::from_json(&pr);
    assert!(pf.is_draft);
    assert_eq!(pf.author_login(), "btucker");
    assert_eq!(pf.review_decision(), "APPROVED");
    assert_eq!(pf.mergeable(), "CONFLICTING");
}

// ---------------------------------------------------------------------------
// get_merged_task_pr tests
// ---------------------------------------------------------------------------

/// Helper to construct a minimal Task for testing.
fn make_task(id: &str, pr: Option<u64>) -> crate::task_store::Task {
    crate::task_store::Task {
        id: id.to_string(),
        subject: "Test task".to_string(),
        status: crate::task_store::TaskStatus::InProgress,
        agent_name: String::new(),
        description: None,
        blocked_by: vec![],
        channel: None,
        pr,
        ..Default::default()
    }
}

#[test]
fn get_merged_task_pr_returns_merged_pr_number() {
    let tasks = vec![make_task("42", Some(123))];
    let merged: std::collections::HashSet<u64> = [123].into_iter().collect();
    assert_eq!(get_merged_task_pr("42", &tasks, &merged), Some(123));
}

#[test]
fn get_merged_task_pr_returns_none_for_unmerged_pr() {
    let tasks = vec![make_task("42", Some(123))];
    let merged: std::collections::HashSet<u64> = std::collections::HashSet::new();
    assert_eq!(get_merged_task_pr("42", &tasks, &merged), None);
}

#[test]
fn get_merged_task_pr_returns_none_for_task_without_pr() {
    let tasks = vec![make_task("42", None)];
    let merged: std::collections::HashSet<u64> = [123].into_iter().collect();
    assert_eq!(get_merged_task_pr("42", &tasks, &merged), None);
}

#[test]
fn get_merged_task_pr_returns_none_for_unknown_task() {
    let tasks: Vec<crate::task_store::Task> = vec![];
    let merged: std::collections::HashSet<u64> = [123].into_iter().collect();
    assert_eq!(get_merged_task_pr("42", &tasks, &merged), None);
}

// ============================================================================
// format_unexpected_exit_message tests
// ============================================================================

#[test]
fn test_unexpected_exit_message_no_stderr() {
    let msg = format_unexpected_exit_message("Coworker", "park", None);
    assert_eq!(msg, "⚠️ Coworker park session exited unexpectedly");
}

#[test]
fn test_unexpected_exit_message_empty_stderr() {
    let empty: Vec<String> = vec![];
    let msg = format_unexpected_exit_message("Coworker", "park", Some(&empty));
    assert_eq!(msg, "⚠️ Coworker park session exited unexpectedly");
}

#[test]
fn test_unexpected_exit_message_with_stderr() {
    let stderr = vec![
        "error: something went wrong".to_string(),
        "panic at line 42".to_string(),
    ];
    let msg = format_unexpected_exit_message("Coworker", "park", Some(&stderr));
    assert!(msg.contains("⚠️ Coworker park session exited unexpectedly"));
    assert!(msg.contains("Stderr (2 lines)"));
    assert!(msg.contains("error: something went wrong"));
    assert!(msg.contains("panic at line 42"));
}

#[test]
fn test_unexpected_exit_message_stderr_truncated_to_10() {
    let stderr: Vec<String> = (0..20).map(|i| format!("line {}", i)).collect();
    let msg = format_unexpected_exit_message("Coworker", "park", Some(&stderr));
    assert!(msg.contains("Stderr (20 lines)"));
    // Should contain last 10 lines (10-19), not first 10 (0-9)
    assert!(msg.contains("line 19"));
    assert!(msg.contains("line 10"));
    assert!(!msg.contains("\nline 9\n"));
}

#[test]
fn test_unexpected_exit_message_lead_role() {
    let msg = format_unexpected_exit_message("Lead", "midtown", None);
    assert_eq!(msg, "⚠️ Lead midtown session exited unexpectedly");
}
