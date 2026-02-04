//! Pure utility functions used throughout the daemon module.
//!
//! These are stateless helpers that don't depend on DaemonState or
//! external processes. They parse, format, and detect patterns.
//!
//! Functions in this module are used by both the webhook and polling paths,
//! ensuring functional equivalence for graceful degradation.

use super::constants::{COWORKER_NAMES, SYSTEM_SENDERS};
pub use super::trackers::PrIssueType;

// ---------------------------------------------------------------------------
// Text / parsing helpers
// ---------------------------------------------------------------------------

/// Truncate a string to max length with ellipsis.
pub fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Truncate a message for summary display.
pub fn truncate_message(msg: &str, max_len: usize) -> String {
    let first_line = msg.lines().next().unwrap_or(msg);
    if first_line.len() <= max_len {
        first_line.to_string()
    } else {
        format!("{}...", &first_line[..max_len])
    }
}

// ---------------------------------------------------------------------------
// Mention / sender helpers
// ---------------------------------------------------------------------------

/// Check if a message contains @all (case-insensitive, with word boundary).
pub fn contains_at_all(content: &str) -> bool {
    let content_lower = content.to_lowercase();
    if let Some(idx) = content_lower.find("@all") {
        let after_idx = idx + 4; // "@all".len()
        after_idx >= content.len()
            || !content[after_idx..]
                .chars()
                .next()
                .unwrap_or(' ')
                .is_alphanumeric()
    } else {
        false
    }
}

/// Extract valid coworker @mentions from message content.
///
/// Returns a list of coworker names that were mentioned (lowercase).
/// Uses word boundary detection to avoid false positives.
pub fn extract_mentions(content: &str) -> Vec<String> {
    let mut mentions = Vec::new();
    let content_lower = content.to_lowercase();

    // Look for @name patterns where name is a valid coworker name
    for &name in COWORKER_NAMES {
        let pattern = format!("@{}", name);
        if let Some(idx) = content_lower.find(&pattern) {
            // Check that this is at a word boundary (not part of a larger word)
            let after_idx = idx + pattern.len();
            let at_word_boundary = after_idx >= content.len()
                || !content[after_idx..]
                    .chars()
                    .next()
                    .unwrap_or(' ')
                    .is_alphanumeric();

            if at_word_boundary && !mentions.contains(&name.to_string()) {
                mentions.push(name.to_string());
            }
        }
    }

    mentions
}

/// Check if a sender is a coworker (not Lead or system).
pub fn is_coworker_sender(from: &str) -> bool {
    !SYSTEM_SENDERS.contains(&from)
}

/// Extract coworker name from branch prefix (e.g., "lexington/fix-auth" -> "lexington").
pub fn coworker_from_branch(branch: &str) -> Option<String> {
    let prefix = branch.split('/').next()?;
    COWORKER_NAMES
        .iter()
        .find(|&&name| name.eq_ignore_ascii_case(prefix))
        .map(|&s| s.to_string())
}

// ---------------------------------------------------------------------------
// PR helpers
// ---------------------------------------------------------------------------

/// Detect actionable issues for a PR.
pub fn detect_pr_issues(pr: &serde_json::Value) -> Vec<PrIssueType> {
    let mut issues = Vec::new();

    // Check for merge conflicts
    let mergeable = pr.get("mergeable").and_then(|m| m.as_str()).unwrap_or("");
    if mergeable == "CONFLICTING" {
        issues.push(PrIssueType::MergeConflict);
    }

    // Check for CI failures
    if let Some(checks) = pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
        let has_failure = checks.iter().any(|check| {
            let conclusion = check
                .get("conclusion")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            conclusion == "FAILURE"
        });
        if has_failure {
            issues.push(PrIssueType::CiFailed);
        }
    }

    // Check review decision
    let review_decision = pr
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    match review_decision {
        "CHANGES_REQUESTED" => issues.push(PrIssueType::ChangesRequested),
        "APPROVED" => issues.push(PrIssueType::Approved),
        _ => {}
    }

    issues
}

/// Check if a PR is eligible for daemon-assisted auto-merge.
///
/// A PR is auto-mergeable when:
/// - It has an `APPROVED` review decision OR a positive comment-based review
/// - It has no CI failures
/// - It has no merge conflicts (mergeable != "CONFLICTING")
/// - All status checks have completed (no pending checks)
///
/// Comment-based reviews are recognized because formal GitHub approval is
/// impossible when the same account (btucker) authors PRs and runs coworkers.
pub fn is_auto_mergeable(pr: &serde_json::Value) -> bool {
    let review_decision = pr
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .unwrap_or("");

    // Check for formal approval OR positive comment-based review
    let has_approval = review_decision == "APPROVED" || has_positive_review_comment(pr);
    if !has_approval {
        return false;
    }

    let mergeable = pr.get("mergeable").and_then(|m| m.as_str()).unwrap_or("");
    if mergeable == "CONFLICTING" {
        return false;
    }

    if let Some(checks) = pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
        let has_failure = checks.iter().any(|check| {
            let conclusion = check
                .get("conclusion")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            conclusion == "FAILURE"
        });
        if has_failure {
            return false;
        }

        // Ensure all checks have completed (no pending/in-progress checks)
        let has_pending = checks.iter().any(|check| {
            let conclusion = check
                .get("conclusion")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            conclusion.is_empty() || conclusion == "PENDING"
        });
        if has_pending {
            return false;
        }
    }

    true
}

/// Check if a PR has a positive review comment from a non-owner coworker.
///
/// A positive review comment must:
/// 1. Have a coworker review signature (frontmatter, header, or "Reviewed by")
/// 2. Have a valid coworker identity (validated against COWORKER_NAMES)
/// 3. NOT be from the PR owner (determined by branch prefix vs extracted name)
/// 4. Contain positive language ("No issues found", "LGTM") without issues
fn has_positive_review_comment(pr: &serde_json::Value) -> bool {
    let comments = match pr.get("comments").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return false,
    };

    // Get the PR owner from branch prefix (e.g., "lexington/fix-auth" -> "lexington")
    let branch = pr.get("headRefName").and_then(|h| h.as_str()).unwrap_or("");
    let pr_owner = coworker_from_branch(branch);

    for comment in comments {
        let body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");

        // Must have a coworker review signature
        if !text_contains_review_signature(body) {
            continue;
        }

        // Extract reviewer identity from all possible sources
        // Must have at least one valid coworker name to verify it's not a self-review
        let reviewer = coworker_from_frontmatter(body)
            .or_else(|| extract_reviewer_from_header(body))
            .or_else(|| extract_reviewer_from_signature(body));

        // Require valid coworker identity (reject unknown reviewers)
        let Some(reviewer_name) = reviewer else {
            continue;
        };

        // Must NOT be from the PR owner (self-review not allowed)
        if let Some(ref owner) = pr_owner
            && reviewer_name.eq_ignore_ascii_case(owner)
        {
            continue; // Skip self-reviews
        }

        // Check for positive review indicators
        if is_positive_review(body) {
            return true;
        }
    }

    false
}

/// Extract reviewer name from "## Code Review by <name>" header.
///
/// Only returns valid coworker names (validated against COWORKER_NAMES).
fn extract_reviewer_from_header(body: &str) -> Option<&'static str> {
    let marker = "## Code Review by ";
    if let Some(idx) = body.find(marker) {
        let after = &body[idx + marker.len()..];
        // Take until newline or end of string
        let name_end = after.find('\n').unwrap_or(after.len());
        let name = after[..name_end].trim();

        // Validate against COWORKER_NAMES (consistent with coworker_from_frontmatter)
        return COWORKER_NAMES
            .iter()
            .find(|&&n| n.eq_ignore_ascii_case(name))
            .copied();
    }
    None
}

/// Extract reviewer name from "Reviewed by <name>" or "🤖 Reviewed by <name>" signature.
///
/// Only returns valid coworker names (validated against COWORKER_NAMES).
fn extract_reviewer_from_signature(body: &str) -> Option<&'static str> {
    // Try emoji version first, then plain version
    for marker in ["🤖 Reviewed by ", "Reviewed by "] {
        if let Some(idx) = body.find(marker) {
            let after = &body[idx + marker.len()..];
            // Take until newline, end of string, or non-alphanumeric
            let name_end = after
                .find(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
                .unwrap_or(after.len());
            let name = after[..name_end].trim();

            // Validate against COWORKER_NAMES
            if let Some(coworker) = COWORKER_NAMES
                .iter()
                .find(|&&n| n.eq_ignore_ascii_case(name))
            {
                return Some(*coworker);
            }
        }
    }
    None
}

/// Check if a review comment body indicates positive approval.
///
/// Positive indicators: "No issues found", "LGTM", "looks good"
/// Negative indicators: "Issues Found", "### Issues" (with issues listed)
fn is_positive_review(body: &str) -> bool {
    let body_lower = body.to_lowercase();

    // Check for positive indicators
    let has_no_issues = body_lower.contains("no issues found");
    let has_lgtm = body_lower.contains("lgtm");
    let has_looks_good = body_lower.contains("looks good");

    // Check for negative indicators (review found issues)
    let has_issues_found =
        body_lower.contains("issues found") && !body_lower.contains("no issues found");
    let has_issues_section = body_lower.contains("### issues") || body_lower.contains("## issues");

    // Positive if has positive indicator and no negative indicators
    (has_no_issues || has_lgtm || has_looks_good) && !has_issues_found && !has_issues_section
}

/// Check if a PR has all CI checks passing (no failures, no pending).
///
/// Returns true if there are checks and all have completed successfully,
/// or if there are no checks at all (no CI configured).
pub fn all_ci_checks_passed(pr: &serde_json::Value) -> bool {
    if let Some(checks) = pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
        if checks.is_empty() {
            return true;
        }
        for check in checks {
            let conclusion = check
                .get("conclusion")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            if conclusion == "FAILURE" || conclusion == "CANCELLED" || conclusion == "TIMED_OUT" {
                return false;
            }
            if conclusion.is_empty() || conclusion == "PENDING" {
                return false; // Still running
            }
        }
        true
    } else {
        true // No checks configured
    }
}

/// Get action text for a PR issue type.
pub fn get_issue_action(issue_type: PrIssueType) -> &'static str {
    match issue_type {
        PrIssueType::MergeConflict => "please rebase",
        PrIssueType::CiFailed => "please investigate",
        PrIssueType::ChangesRequested => "please address feedback",
        PrIssueType::Approved => "ready to merge!",
        PrIssueType::NeedsReview => "calling in reviewer",
        PrIssueType::ReviewComment => "please address review feedback and merge if appropriate",
        PrIssueType::ReviewComplete => {
            "review is complete — please address feedback and merge if appropriate"
        }
        PrIssueType::GreenWithFeedback => "CI is green — please address review feedback and merge",
    }
}

/// Check if text contains a coworker review signature.
///
/// Coworker reviews are identified by:
/// - The "🤖 Reviewed by" or "Reviewed by" signature (legacy formal reviews)
/// - The "<!-- midtown:" frontmatter (comment-based reviews)
/// - The "## Code Review by" header (comment-based reviews)
pub fn text_contains_review_signature(text: &str) -> bool {
    text.contains("🤖 Reviewed by")
        || text.contains("Reviewed by")
        || text.contains("<!-- midtown:")
        || text.contains("## Code Review by")
}

/// Get the creation time of a PR to enforce review delay.
///
/// Returns None if the PR age couldn't be determined.
pub fn get_pr_age_secs(pr: &serde_json::Value) -> Option<u64> {
    let created_at = pr.get("createdAt").and_then(|c| c.as_str())?;
    let created = chrono::DateTime::parse_from_rfc3339(created_at).ok()?;
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(created);
    Some(duration.num_seconds().max(0) as u64)
}

/// Count non-owner comments on a PR for review notification polling.
///
/// Returns the number of comments that are NOT from the PR owner.
/// A comment is from the owner if:
/// - The GitHub username matches the PR author
/// - OR the comment contains `<!-- midtown: <owner> -->` frontmatter
///
/// This enables the polling path to detect new review feedback when webhooks
/// are degraded, matching the webhook behavior.
pub fn count_non_owner_comments(pr: &serde_json::Value, owner_coworker: Option<&str>) -> usize {
    let comments = match pr.get("comments").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return 0,
    };

    // Get the PR author's GitHub username for comparison
    let pr_author = pr
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
        .unwrap_or("");

    comments
        .iter()
        .filter(|comment| {
            let commenter_login = comment
                .get("author")
                .and_then(|a| a.get("login"))
                .and_then(|l| l.as_str())
                .unwrap_or("");
            let comment_body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");

            // Check if this comment is from the PR author (same GitHub account)
            if commenter_login == pr_author {
                // Could still be from a different coworker (frontmatter check)
                if let Some(owner) = owner_coworker {
                    // Check frontmatter: if it's from a different coworker, count it
                    if let Some(coworker) = coworker_from_frontmatter(comment_body) {
                        return coworker != owner;
                    }
                }
                // No frontmatter or matches owner - this is from the owner
                return false;
            }

            // Different GitHub account - definitely not from the owner
            true
        })
        .count()
}

/// Extract coworker name from frontmatter in body (e.g., "<!-- midtown: lexington -->")
fn coworker_from_frontmatter(body: &str) -> Option<&str> {
    let start = body.find("<!-- midtown:")?;
    let after_start = &body[start + 13..];
    let end = after_start.find("-->")?;
    let name = after_start[..end].trim();

    COWORKER_NAMES
        .iter()
        .find(|&&n| n.eq_ignore_ascii_case(name))
        .copied()
}

// ---------------------------------------------------------------------------
// GitHub CLI helpers
// ---------------------------------------------------------------------------

/// Check if gh CLI output indicates an authentication error.
///
/// Returns true for errors like:
/// - "Bad credentials" (invalid or expired token)
/// - HTTP 401 responses
/// - "authentication required" messages
pub fn is_gh_auth_error(stderr: &str) -> bool {
    let stderr_lower = stderr.to_lowercase();
    stderr_lower.contains("bad credentials")
        || stderr_lower.contains("401")
        || stderr_lower.contains("authentication required")
        || stderr_lower.contains("requires authentication")
        || stderr_lower.contains("not logged in")
}

/// Extract PR number from a message content.
///
/// Looks for patterns like "PR #42", "#42", "PR #123".
#[cfg(test)]
pub(super) fn extract_pr_number(content: &str) -> Option<u64> {
    // Look for "PR #N" pattern first
    if let Some(idx) = content.find("PR #") {
        let after = &content[idx + 4..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if let Ok(num) = num_str.parse() {
            return Some(num);
        }
    }

    // Look for " #N " pattern (standalone PR reference)
    // This handles messages like "approved PR #42" where we already caught it above
    // but also cases like "on #42:"
    for (i, _) in content.match_indices(" #") {
        let after = &content[i + 2..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num_str.is_empty()
            && let Ok(num) = num_str.parse()
        {
            return Some(num);
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    // Comment-based review approval (when formal GitHub approval not possible)
    // -------------------------------------------------------------------------

    #[test]
    fn auto_merge_accepts_comment_based_approval_no_issues_found() {
        // PR has no formal APPROVED status but has a positive review comment
        let pr = json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}],
            "reviewDecision": "", // No formal approval
            "headRefName": "lexington/fix-auth",
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": "<!-- midtown: amsterdam -->\n\n## Code Review by amsterdam\n\n**No issues found.** The code changes look good."
                }
            ]
        });

        assert!(
            is_auto_mergeable(&pr),
            "PR with positive coworker review comment should be auto-mergeable"
        );
    }

    #[test]
    fn auto_merge_accepts_comment_based_approval_lgtm() {
        let pr = json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}],
            "reviewDecision": "",
            "headRefName": "broadway/add-feature",
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": "## Code Review by york\n\nLGTM! Great work."
                }
            ]
        });

        assert!(
            is_auto_mergeable(&pr),
            "PR with LGTM review comment should be auto-mergeable"
        );
    }

    #[test]
    fn auto_merge_rejects_owner_self_review() {
        // PR owner cannot approve their own PR via comment
        let pr = json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}],
            "reviewDecision": "",
            "headRefName": "lexington/fix-auth",
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": "<!-- midtown: lexington -->\n\n## Code Review by lexington\n\nNo issues found."
                }
            ]
        });

        assert!(
            !is_auto_mergeable(&pr),
            "PR owner cannot self-approve via comment"
        );
    }

    #[test]
    fn auto_merge_rejects_review_with_issues() {
        // Review comments that flag issues are not approval
        let pr = json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}],
            "reviewDecision": "",
            "headRefName": "lexington/fix-auth",
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": "## Code Review by amsterdam\n\n### Issues Found\n\n1. Missing error handling"
                }
            ]
        });

        assert!(
            !is_auto_mergeable(&pr),
            "PR with review issues should not be auto-mergeable"
        );
    }

    #[test]
    fn auto_merge_rejects_non_review_comments() {
        // Regular comments without review signature don't count
        let pr = json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}],
            "reviewDecision": "",
            "headRefName": "lexington/fix-auth",
            "comments": [
                {
                    "author": {"login": "external"},
                    "body": "LGTM! Looks good to me."
                }
            ]
        });

        assert!(
            !is_auto_mergeable(&pr),
            "non-review comments should not trigger auto-merge"
        );
    }

    #[test]
    fn auto_merge_rejects_non_coworker_reviewer_name() {
        // "## Code Review by dependabot" should not trigger auto-merge
        // because "dependabot" is not in COWORKER_NAMES
        let pr = json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}],
            "reviewDecision": "",
            "headRefName": "lexington/fix-auth",
            "comments": [
                {
                    "author": {"login": "dependabot"},
                    "body": "## Code Review by dependabot\n\nLGTM! All dependencies look good."
                }
            ]
        });

        assert!(
            !is_auto_mergeable(&pr),
            "non-coworker reviewer names should not trigger auto-merge"
        );
    }

    #[test]
    fn auto_merge_rejects_self_review_via_reviewed_by_signature() {
        // "🤖 Reviewed by lexington" should be rejected as self-review
        let pr = json!({
            "number": 42,
            "mergeable": "MERGEABLE",
            "statusCheckRollup": [{"conclusion": "SUCCESS"}],
            "reviewDecision": "",
            "headRefName": "lexington/fix-auth",
            "comments": [
                {
                    "author": {"login": "btucker"},
                    "body": "🤖 Reviewed by lexington\n\nNo issues found."
                }
            ]
        });

        assert!(
            !is_auto_mergeable(&pr),
            "self-review via 'Reviewed by' signature should not trigger auto-merge"
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
    fn review_signature_detects_frontmatter() {
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
}
