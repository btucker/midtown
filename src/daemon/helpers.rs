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
/// Uses `floor_char_boundary` to avoid panicking on multi-byte UTF-8 characters.
pub fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max_len.saturating_sub(3));
        format!("{}...", &s[..end])
    }
}

/// Truncate a message for summary display.
/// Uses `floor_char_boundary` to avoid panicking on multi-byte UTF-8 characters.
pub fn truncate_message(msg: &str, max_len: usize) -> String {
    let first_line = msg.lines().next().unwrap_or(msg);
    if first_line.len() <= max_len {
        first_line.to_string()
    } else {
        let end = first_line.floor_char_boundary(max_len.saturating_sub(3));
        format!("{}...", &first_line[..end])
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

/// Check if a message contains @user (case-insensitive, with word boundary).
///
/// Word boundary means @user is not part of a larger word:
/// - After: must be end of string or non-alphanumeric character
/// - Before: must be start of string or non-alphanumeric character (to avoid email addresses)
pub fn contains_at_user(content: &str) -> bool {
    let content_lower = content.to_lowercase();
    if let Some(idx) = content_lower.find("@user") {
        // Check before @user - must be start of string or non-alphanumeric
        let before_ok = idx == 0
            || !content[..idx]
                .chars()
                .last()
                .unwrap_or(' ')
                .is_alphanumeric();

        // Check after @user - must be end of string or non-alphanumeric
        let after_idx = idx + 5; // "@user".len()
        let after_ok = after_idx >= content.len()
            || !content[after_idx..]
                .chars()
                .next()
                .unwrap_or(' ')
                .is_alphanumeric();

        before_ok && after_ok
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

/// Determine if message content should override task channel routing and go to main.
///
/// Returns true if the message contains any of these patterns that warrant
/// routing to the main channel even when the sender has a task channel assignment:
///
/// - @user mentions (important cross-cutting communication)
/// - @lead mentions (task requests, questions)
/// - Task lifecycle events (created, completed)
/// - Error messages and warnings
/// - Escalation keywords (blocked, help)
///
/// Insights are handled separately via cross-posting and not included here.
pub fn should_route_to_main_channel(content: &str) -> bool {
    let content_lower = content.to_lowercase();

    // @user mentions - always go to main for visibility
    if contains_at_user(content) {
        return true;
    }

    // @lead mentions - task requests, questions, escalations
    if content_lower.contains("@lead") {
        return true;
    }

    // Task lifecycle events
    // Pattern: "task !N completed", "created task !N", "📋 Created task"
    if (content_lower.contains("task") && content_lower.contains("completed"))
        || (content_lower.contains("task") && content_lower.contains("created"))
        || content.contains("📋")
    {
        return true;
    }

    // Error and warning messages
    // Match specific error reporting patterns; avoid discussions about errors
    // Only match when error/failed appears in a reporting context, not as a topic
    if content_lower.contains("error:")
        || content.contains("⚠️")
        || content.contains("❌")
        || content_lower.starts_with("failed ")
        || content_lower.contains(" failed ")
        || content_lower.contains("failed to")
    {
        return true;
    }

    // Escalation keywords indicating the coworker needs attention
    if content_lower.contains("blocked on")
        || content_lower.contains("i'm blocked")
        || content_lower.contains("help needed")
        || content_lower.contains("need help")
    {
        return true;
    }

    false
}

/// Check if a sender is a coworker (not Lead or system).
pub fn is_coworker_sender(from: &str) -> bool {
    !SYSTEM_SENDERS.contains(&from)
}

/// Extract coworker name from branch prefix (e.g., "lexington/fix-auth" -> "lexington").
///
/// Supports two branch naming conventions:
/// - Legacy: `<coworker>/<description>` (e.g., "lexington/fix-auth")
/// - Task-based: `task-<id>-<slug>` or `review-pr-<number>` (requires branch_owners map)
///
/// For task-based branches, this function only works when called with the optional
/// `branch_owners` map from the WorldSnapshot. Without it, task-based branches return None.
pub fn coworker_from_branch(branch: &str) -> Option<String> {
    coworker_from_branch_with_map(branch, None)
}

/// Extract coworker name from branch with optional registry lookup.
///
/// When `branch_owners` is provided (from WorldSnapshot.worktree_branch_owners),
/// this can resolve task-based branch names like "task-42-fix-auth" or "review-pr-123".
pub fn coworker_from_branch_with_map(
    branch: &str,
    branch_owners: Option<&std::collections::HashMap<String, String>>,
) -> Option<String> {
    // Try legacy coworker-prefixed branches first (lexington/fix-auth)
    if let Some(prefix) = branch.split('/').next()
        && let Some(&name) = COWORKER_NAMES
            .iter()
            .find(|&&name| name.eq_ignore_ascii_case(prefix))
    {
        return Some(name.to_string());
    }

    // Fall back to task-based branch lookup (task-42-fix-auth, review-pr-123)
    // This requires the branch_owners map from the worktree registry.
    branch_owners.and_then(|map| map.get(branch).cloned())
}

/// Check if a branch is a lead branch (starts with "lead/").
///
/// Lead branches (e.g., "lead/fix-bug") indicate the PR is authored by the Lead,
/// not a coworker. When review feedback is posted on these PRs, the Lead should
/// be nudged in addition to or instead of any coworker who opened the PR.
pub fn is_lead_branch(branch: &str) -> bool {
    branch.starts_with("lead/")
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
/// - It has an `APPROVED` review decision
/// - It has no CI failures
/// - It has no merge conflicts (mergeable != "CONFLICTING")
/// - All status checks have completed (no pending checks)
pub fn is_auto_mergeable(pr: &serde_json::Value) -> bool {
    let review_decision = pr
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .unwrap_or("");
    if review_decision != "APPROVED" {
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
        PrIssueType::Approved => {
            "approved with CI green — please merge (use --auto if checks pending)"
        }
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
/// - The "# Code Review by" header at any heading level (case-insensitive)
pub fn text_contains_review_signature(text: &str) -> bool {
    text.contains("🤖 Reviewed by")
        || text.contains("Reviewed by")
        || text.contains("<!-- midtown:")
        || text_has_code_review_header(text)
}

/// Check if text contains a "Code Review" header at any markdown heading level.
///
/// Matches patterns like:
/// - "## Code Review by madison"  (with attribution)
/// - "### code review"             (exact match, without attribution - from code-review skill)
/// - "# CODE REVIEW BY york"       (any case)
///
/// Rationale: The code-review skill template uses "### Code review" without
/// the "by {name}" part. Coworkers are supposed to add <!-- midtown: name -->
/// frontmatter, but if they forget, we should still detect it as a review.
/// We require either an exact "code review" match OR "code review by" to avoid
/// false positives from headings like "Code Review Checklist".
fn text_has_code_review_header(text: &str) -> bool {
    let text_lower = text.to_lowercase();
    // Look for markdown heading followed by "code review" (exact) or "code review by"
    for line in text_lower.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            // Strip heading markers and check content
            let content = trimmed.trim_start_matches('#').trim();
            // Exact match OR attributed form
            if content == "code review" || content.starts_with("code review by") {
                return true;
            }
        }
    }
    false
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

// ---------------------------------------------------------------------------
// Insight cross-posting helpers
// ---------------------------------------------------------------------------

/// Format the content of an insight message for cross-posting to the main channel.
///
/// Produces the format: `#channel-name | insight content`.
/// The author is omitted because all display surfaces (CLI, TUI, web UI)
/// already render the `from` field as a sender label.
pub(super) fn format_cross_post_content(message: &crate::message::Message) -> String {
    format!("#{} | {}", message.channel_name(), message.content)
}

/// Check if a message should be cross-posted to the main channel as an insight.
///
/// Returns true if:
/// - The message contains the 💡 emoji (insight marker)
/// - The message is being sent to a topic channel (not the main channel)
pub(super) fn should_cross_post_insight(
    message: &crate::message::Message,
    main_channel_name: &str,
) -> bool {
    // Check if message contains insight marker (💡 emoji)
    let has_insight_marker = message.content.contains('💡');

    // Check if message is being sent to a topic channel (not main)
    let is_topic_channel = message.channel_name() != main_channel_name;

    has_insight_marker && is_topic_channel
}

/// Extract task ID from a message content.
///
/// Looks for patterns like "Task !42", "task !123", "!99".
/// Returns the numeric task ID if found.
pub fn extract_task_id(content: &str) -> Option<String> {
    // Look for "Task !N" or "task !N" pattern first (case insensitive)
    let content_lower = content.to_lowercase();
    if let Some(idx) = content_lower.find("task !") {
        let after = &content[idx + 6..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num_str.is_empty() {
            return Some(num_str);
        }
    }

    // Look for standalone "!N" pattern (e.g., "Completed task !42")
    for (i, _) in content.match_indices(" !") {
        let after = &content[i + 2..];
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num_str.is_empty() {
            return Some(num_str);
        }
    }

    // Also check if the message starts with "!N"
    if let Some(after) = content.strip_prefix('!') {
        let num_str: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num_str.is_empty() {
            return Some(num_str);
        }
    }

    None
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
// Task prompt helpers
// ---------------------------------------------------------------------------

/// Format a task prompt with the standard `midtown task view` footer.
///
/// Appends `\n\nRun midtown task view {id} for full details.` to the given
/// context message, ensuring consistent formatting across all task assignment
/// and nudge paths (dispatch, health, recovery).
pub fn format_task_prompt(task_id: &str, context_message: &str) -> String {
    format!(
        "{}\n\nRun `midtown task view {}` for full details.",
        context_message, task_id
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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

    // =========================================================================
    // Content-based channel filtering tests
    //
    // Verify that messages with certain content patterns (@user mentions,
    // task lifecycle events, errors) are routed to the main channel even
    // when the sender has a task channel assignment.
    // =========================================================================

    #[test]
    fn at_user_mention_routes_to_main() {
        assert!(should_route_to_main_channel("@user can you help?"));
        assert!(should_route_to_main_channel("hey @user, looking at this"));
        assert!(should_route_to_main_channel("@user I found a bug"));
    }

    #[test]
    fn at_user_case_insensitive() {
        assert!(should_route_to_main_channel("@USER check this"));
        assert!(should_route_to_main_channel("@UsEr something weird"));
    }

    #[test]
    fn task_lifecycle_events_route_to_main() {
        // Task request (already uses @lead which routes to main)
        assert!(should_route_to_main_channel(
            "@lead [Task Request] from park: \"Add validation\""
        ));

        // Task completion messages
        assert!(should_route_to_main_channel("task !42 completed"));
        assert!(should_route_to_main_channel("completed task !123"));
        assert!(should_route_to_main_channel("Task !999 is completed"));
    }

    #[test]
    fn error_messages_route_to_main() {
        assert!(should_route_to_main_channel("Error: connection failed"));
        assert!(should_route_to_main_channel("⚠️ Warning: API rate limit"));
        assert!(should_route_to_main_channel("❌ Tests failed"));
        assert!(should_route_to_main_channel("Failed to build"));
    }

    #[test]
    fn escalation_keywords_route_to_main() {
        assert!(should_route_to_main_channel("blocked on dependency"));
        assert!(should_route_to_main_channel(
            "I'm blocked waiting for approval"
        ));
        assert!(should_route_to_main_channel("help needed with this issue"));
        assert!(should_route_to_main_channel("Need help understanding this"));
    }

    #[test]
    fn regular_messages_do_not_route_to_main() {
        // Regular /me actions
        assert!(!should_route_to_main_channel("working on auth module"));
        assert!(!should_route_to_main_channel("refactoring the validator"));
        assert!(!should_route_to_main_channel("running tests"));

        // Regular text
        assert!(!should_route_to_main_channel("Added validation logic"));
        assert!(!should_route_to_main_channel("Fixed the bug"));
    }

    #[test]
    fn word_boundaries_for_user_mention() {
        // Should match
        assert!(contains_at_user("@user test"));
        assert!(contains_at_user("Hey @user!"));
        assert!(contains_at_user("@user."));

        // Should NOT match (part of larger word)
        assert!(!contains_at_user("unusual@user.com"));
        assert!(!contains_at_user("@username"));
        assert!(!contains_at_user("@users"));
    }

    #[test]
    fn task_created_routes_to_main() {
        // System messages about task creation should go to main
        assert!(should_route_to_main_channel(
            "📋 Created task !42: Add auth"
        ));
        assert!(should_route_to_main_channel("task !123 created"));
    }

    #[test]
    fn false_positives_do_not_trigger() {
        // "error" in different contexts should not trigger
        assert!(!should_route_to_main_channel("error handling is tricky"));
        assert!(!should_route_to_main_channel("the error rate decreased"));

        // "blocked" in different contexts
        assert!(!should_route_to_main_channel(
            "the request was blocked by CORS"
        ));
        assert!(!should_route_to_main_channel("blocked requests counter"));

        // "task" without lifecycle indicators
        assert!(!should_route_to_main_channel("task looks straightforward"));
        assert!(!should_route_to_main_channel("working on the task"));
    }
}
