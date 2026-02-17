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

/// Check if a PR is authored by the lead (repo owner).
///
/// Pure function: compares the PR's author login against the pre-fetched repo owner
/// from `WorldSnapshot`. The repo owner is extracted from the git remote URL at
/// daemon startup, avoiding I/O in decision functions.
///
/// Returns true if:
/// - PR has an author.login field that matches the repo owner
/// - `repo_owner` is provided
///
/// Returns false if:
/// - PR has no author field
/// - `repo_owner` is None (could not be determined at startup)
/// - Author doesn't match repo owner
pub fn is_lead_authored_pr(pr: &serde_json::Value, repo_owner: Option<&str>) -> bool {
    let repo_owner = match repo_owner {
        Some(owner) => owner,
        None => return false,
    };

    let author_login = match pr
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(|l| l.as_str())
    {
        Some(login) => login,
        None => return false,
    };

    author_login.eq_ignore_ascii_case(repo_owner)
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
/// - The "# Code Review by" header at any heading level (case-insensitive)
///
/// Note: We do NOT check for "<!-- midtown:" frontmatter alone, as ALL coworker
/// GitHub comments include this frontmatter. Checking for it would cause false
/// positives where any coworker comment (CI fix explanations, status updates, etc.)
/// would be incorrectly detected as a code review.
pub fn text_contains_review_signature(text: &str) -> bool {
    text.contains("🤖 Reviewed by")
        || text.contains("Reviewed by")
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

#[path = "helpers_tests.rs"]
#[cfg(test)]
mod tests;
