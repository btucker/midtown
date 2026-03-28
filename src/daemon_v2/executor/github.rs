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
    pub author: String,
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
            let author = pr
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
            let is_approved = review_decision == "APPROVED";
            let needs_review = review_decision == "REVIEW_REQUIRED";

            Some(ParsedPr {
                number,
                branch,
                author,
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
        return CiStatus::Pending;
    };
    if checks.is_empty() {
        return CiStatus::Pending;
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
                author: pr.author.clone(),
            });
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
