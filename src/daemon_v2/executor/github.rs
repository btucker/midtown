#[path = "github_tests.rs"]
#[cfg(test)]
mod tests;

use serde_json::Value;

use crate::daemon_v2::events::{CiStatus, DomainEvent, ReviewState};
use crate::daemon_v2::projections::work::WorkIndex;

#[derive(Debug, Clone)]
pub struct ParsedPr {
    pub number: u64,
    pub branch: String,
    pub github_author: String,
    pub is_draft: bool,
    pub ci_passed: bool,
    pub is_approved: bool,
    pub needs_review: bool,
}

#[derive(Debug, Clone)]
pub struct ParsedMergedPr {
    pub number: u64,
    pub branch: String,
}

pub fn parse_open_prs(json: &Value) -> Vec<ParsedPr> {
    let Some(arr) = json.as_array() else {
        return vec![];
    };

    arr.iter()
        .filter_map(|pr| {
            let number = pr.get("number")?.as_u64()?;
            let branch = pr
                .get("headRefName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let github_author = pr
                .get("author")
                .and_then(|v| v.get("login"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let is_draft = pr.get("isDraft").and_then(|v| v.as_bool()).unwrap_or(false);

            let ci_passed = parse_ci_status(pr) == CiStatus::Passed;

            let review_decision = pr
                .get("reviewDecision")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let is_approved = !is_draft && review_decision == "APPROVED";
            // Spec 3.4: draft PRs never need review regardless of reviewDecision
            let needs_review = !is_draft && review_decision == "REVIEW_REQUIRED";

            Some(ParsedPr {
                number,
                branch,
                github_author,
                is_draft,
                ci_passed,
                is_approved,
                needs_review,
            })
        })
        .collect()
}

fn parse_ci_status(pr: &Value) -> CiStatus {
    let Some(checks) = pr.get("statusCheckRollup").and_then(|v| v.as_array()) else {
        // Spec 3.4: no statusCheckRollup → Passed
        return CiStatus::Passed;
    };
    if checks.is_empty() {
        // Spec 3.4: empty statusCheckRollup → Passed
        return CiStatus::Passed;
    }

    let mut has_pending = false;
    for check in checks {
        let state = check
            .get("state")
            .or_else(|| check.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let conclusion = check.get("conclusion").and_then(|v| v.as_str());

        match state {
            "FAILURE" | "ERROR" => return CiStatus::Failed,
            "PENDING" | "QUEUED" | "IN_PROGRESS" => has_pending = true,
            "SUCCESS" => {}
            _ => {
                // Check conclusion for check runs (vs status contexts)
                match conclusion {
                    Some("FAILURE") | Some("TIMED_OUT") | Some("CANCELLED") => {
                        return CiStatus::Failed;
                    }
                    Some("SUCCESS") | Some("NEUTRAL") | Some("SKIPPED") => {}
                    _ => has_pending = true,
                }
            }
        }
    }

    if has_pending {
        CiStatus::Running
    } else {
        CiStatus::Passed
    }
}

pub fn parse_merged_prs(json: &Value) -> Vec<ParsedMergedPr> {
    let Some(arr) = json.as_array() else {
        return vec![];
    };

    arr.iter()
        .filter_map(|pr| {
            let number = pr.get("number")?.as_u64()?;
            let branch = pr
                .get("headRefName")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(ParsedMergedPr { number, branch })
        })
        .collect()
}

/// Diff polled PR state against the WorkIndex projection.
/// Returns domain events for newly opened, newly merged, and newly review-requested PRs.
pub fn diff_pr_state(
    work: &WorkIndex,
    open: &[ParsedPr],
    merged: &[ParsedMergedPr],
) -> Vec<DomainEvent> {
    let mut events = Vec::new();

    // Detect newly opened PRs
    for pr in open {
        if !work.prs.contains_key(&pr.number) {
            events.push(DomainEvent::PrOpened {
                number: pr.number,
                branch: pr.branch.clone(),
                github_author: pr.github_author.clone(),
            });
            // Spec 3.2: all new non-draft PRs need review (v1 approach — don't
            // rely on reviewDecision which requires branch protection rules).
            if !pr.is_draft {
                events.push(DomainEvent::PrReviewRequested { number: pr.number });
            }
            // Spec 3.1: link PR to task by branch prefix (worktree convention)
            if let Some(task) = work.tasks.values().find(|t| {
                t.status == crate::daemon_v2::events::TaskStatus::InProgress
                    && pr.branch.starts_with(&format!("task-{}-", t.id))
            }) {
                events.push(DomainEvent::PrLinkedToTask {
                    number: pr.number,
                    task_id: task.id.clone(),
                });
            }
        }

        // Detect CI/review state changes for known PRs
        if let Some(existing) = work.prs.get(&pr.number) {
            let new_ci = if pr.ci_passed {
                CiStatus::Passed
            } else {
                CiStatus::Running
            };
            let new_review = if pr.is_approved {
                ReviewState::Approved
            } else if pr.needs_review {
                ReviewState::Pending
            } else {
                ReviewState::None
            };

            if existing.ci_status != new_ci || existing.review_state != new_review {
                events.push(DomainEvent::PrUpdated {
                    number: pr.number,
                    ci_status: new_ci,
                    review_state: new_review,
                });
            }
        }

        // Detect review requested
        if pr.needs_review && !work.needing_review.contains(&pr.number) {
            events.push(DomainEvent::PrReviewRequested { number: pr.number });
        }
    }

    // Detect newly merged PRs
    for pr in merged {
        let is_known_merged = work
            .prs
            .get(&pr.number)
            .is_some_and(|existing| existing.is_merged);
        if !is_known_merged {
            events.push(DomainEvent::PrMerged {
                number: pr.number,
                branch: pr.branch.clone(),
            });
        }
    }

    events
}

pub async fn fetch_open_prs() -> Result<Vec<ParsedPr>, String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,headRefName,isDraft,mergeable,reviewDecision,statusCheckRollup,author",
        ])
        .output()
        .await
        .map_err(|e| format!("failed to run gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr list failed: {stderr}"));
    }

    let json: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("JSON parse error: {e}"))?;

    Ok(parse_open_prs(&json))
}

pub async fn fetch_merged_prs() -> Result<Vec<ParsedMergedPr>, String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "10",
            "--json",
            "number,headRefName,title,mergedAt",
        ])
        .output()
        .await
        .map_err(|e| format!("failed to run gh: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr list failed: {stderr}"));
    }

    let json: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("JSON parse error: {e}"))?;

    Ok(parse_merged_prs(&json))
}

/// Rate limit status from GitHub API.
#[derive(Debug, Clone)]
pub struct RateLimitStatus {
    /// Remaining requests in the current window.
    pub remaining: u32,
    /// Total limit for the current window.
    pub limit: u32,
    /// Seconds until the rate limit resets.
    pub reset_in_secs: u64,
}

/// Check the GitHub API rate limit.
/// Returns None if the check fails (gh not available, etc.).
pub async fn check_rate_limit() -> Option<RateLimitStatus> {
    let output = tokio::process::Command::new("gh")
        .args(["api", "rate_limit"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let json: Value = serde_json::from_slice(&output.stdout).ok()?;
    let core = json.get("resources")?.get("core")?;

    let remaining = core.get("remaining")?.as_u64()? as u32;
    let limit = core.get("limit")?.as_u64()? as u32;
    let reset = core.get("reset")?.as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let reset_in_secs = reset.saturating_sub(now);

    Some(RateLimitStatus {
        remaining,
        limit,
        reset_in_secs,
    })
}

/// Parse rate limit JSON (for testing).
pub fn parse_rate_limit(json: &Value) -> Option<RateLimitStatus> {
    let core = json.get("resources")?.get("core")?;
    let remaining = core.get("remaining")?.as_u64()? as u32;
    let limit = core.get("limit")?.as_u64()? as u32;
    let reset = core.get("reset")?.as_u64()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let reset_in_secs = reset.saturating_sub(now);

    Some(RateLimitStatus {
        remaining,
        limit,
        reset_in_secs,
    })
}

/// Returns true if polling should be skipped due to rate limiting.
/// Threshold: skip when remaining < 10% of limit.
pub fn should_throttle(status: &RateLimitStatus) -> bool {
    let threshold = status.limit / 10; // 10% of limit
    status.remaining < threshold
}
