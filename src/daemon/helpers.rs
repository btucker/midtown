//! Pure utility functions used throughout the daemon module.
//!
//! These are stateless helpers that don't depend on DaemonState or
//! external processes. They parse, format, and detect patterns.

use super::constants::{COWORKER_NAMES, SYSTEM_SENDERS};
use super::trackers::PrIssueType;

// ---------------------------------------------------------------------------
// Text / parsing helpers
// ---------------------------------------------------------------------------

/// Truncate a string to max length with ellipsis.
pub(super) fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Truncate a message for summary display.
pub(super) fn truncate_message(msg: &str, max_len: usize) -> String {
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
pub(super) fn contains_at_all(content: &str) -> bool {
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
pub(super) fn extract_mentions(content: &str) -> Vec<String> {
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
pub(super) fn is_coworker_sender(from: &str) -> bool {
    !SYSTEM_SENDERS.contains(&from)
}

/// Extract coworker name from branch prefix (e.g., "lexington/fix-auth" -> "lexington").
pub(super) fn coworker_from_branch(branch: &str) -> Option<String> {
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
pub(super) fn detect_pr_issues(pr: &serde_json::Value) -> Vec<PrIssueType> {
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
pub(super) fn is_auto_mergeable(pr: &serde_json::Value) -> bool {
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

/// Get action text for a PR issue type.
pub(super) fn get_issue_action(issue_type: PrIssueType) -> &'static str {
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
    }
}

/// Check if text contains a coworker review signature.
///
/// Coworker reviews are identified by:
/// - The "🤖 Reviewed by" or "Reviewed by" signature (legacy formal reviews)
/// - The "<!-- midtown:" frontmatter (comment-based reviews)
/// - The "## Code Review by" header (comment-based reviews)
pub(super) fn text_contains_review_signature(text: &str) -> bool {
    text.contains("🤖 Reviewed by")
        || text.contains("Reviewed by")
        || text.contains("<!-- midtown:")
        || text.contains("## Code Review by")
}

/// Get the creation time of a PR to enforce review delay.
///
/// Returns None if the PR age couldn't be determined.
pub(super) fn get_pr_age_secs(pr: &serde_json::Value) -> Option<u64> {
    let created_at = pr.get("createdAt").and_then(|c| c.as_str())?;
    let created = chrono::DateTime::parse_from_rfc3339(created_at).ok()?;
    let now = chrono::Utc::now();
    let duration = now.signed_duration_since(created);
    Some(duration.num_seconds().max(0) as u64)
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
