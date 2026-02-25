//! PR data handler for the `prs.status` RPC method.
//!
//! Contains the `prs.status` RPC handler, the `PrsCache`, and all
//! GraphQL/PR-formatting logic. Extracted from `rpc_kanban.rs` as part of
//! splitting the old `kanban.data` RPC into two concerns:
//!
//! - `prs.status` (this module): GitHub PR data, cached for 60s.
//! - `coworkers.status` (rpc_coworker.rs): live local coworker state, no cache.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// PR data handler
// ============================================================================

/// Handle `prs.status` RPC method — returns PR data for the kanban board.
///
/// Returns open PRs with author, reviewer, CI status, and timestamps,
/// plus recently merged PRs for the Done column.
///
/// Runs blocking GraphQL operations in `spawn_blocking` to avoid blocking
/// the async runtime and causing RPC timeouts.
///
/// Uses a 60s TTL cache keyed by repo paths (not coworker state, since
/// coworker state is now served separately via `coworkers.status`).
pub(crate) async fn handle_prs_status(id: RequestId, state: &DaemonState) -> Response {
    let all_repo_paths = state.all_repo_paths.clone();

    // Compute a hash of all repo paths for cache keying.
    // The PR cache key depends only on repo paths — coworker state is served
    // separately via `coworkers.status` and does not affect PR data.
    let mut hasher = DefaultHasher::new();
    for path in &all_repo_paths {
        path.hash(&mut hasher);
    }
    let cache_key = hasher.finish();

    // Check cache first
    if let Some(cached) = state.prs_cache.get(cache_key) {
        debug!(
            "Returning cached PR data (TTL: {}s)",
            PRS_CACHE_TTL.as_secs()
        );
        return Response::success(id, cached);
    }

    debug!("Cache miss, fetching fresh PR data");

    // Get reviewer assignments from persistent state (best-effort via try_lock)
    let reviewer_assignments: HashMap<u64, crate::github_state::PrReviewerAssignment> = state
        .persistent_state
        .try_lock()
        .map(|ps| ps.github.active_assignments())
        .unwrap_or_default();

    let is_multi_repo = all_repo_paths.len() > 1;

    // Pre-resolve repo full names (this uses caching and is fast)
    let repo_data: Vec<(std::path::PathBuf, String, String)> = all_repo_paths
        .iter()
        .map(|repo_path| {
            let full_name = state.get_repo_full_name(repo_path);
            let label = repo_path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            (repo_path.clone(), full_name, label)
        })
        .collect();

    // Run blocking GraphQL operations in spawn_blocking
    let (prs, merged_prs, repos) = match tokio::task::spawn_blocking(move || {
        let mut prs = Vec::new();
        let mut merged_prs = Vec::new();
        let mut repos = Vec::new();

        for (repo_path, full_name, label) in repo_data {
            let repo_label = if is_multi_repo {
                repo_path.file_name().and_then(|s| s.to_str())
            } else {
                None
            };

            repos.push(serde_json::json!({
                "label": label,
                "full_name": full_name,
            }));

            let (open, merged) =
                fetch_prs_all(&reviewer_assignments, &full_name, &repo_path, repo_label);
            prs.extend(open);
            merged_prs.extend(merged);
        }

        (prs, merged_prs, repos)
    })
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("spawn_blocking panic in prs_status handler: {}", e);
            return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
        }
    };

    let response_data = serde_json::json!({
        "prs": prs,
        "merged_prs": merged_prs,
        "repos": repos,
    });

    state.prs_cache.set(response_data.clone(), cache_key);

    Response::success(id, response_data)
}

// ============================================================================
// PR helpers
// ============================================================================

/// TTL for PR data cache (60 seconds).
///
/// PR data is expensive (GraphQL round-trip) so we cache for 60s.
/// The coworker state is now served separately via `coworkers.status` at 1-2s
/// intervals, so this cache no longer needs to include coworker data.
const PRS_CACHE_TTL: Duration = Duration::from_secs(60);

/// GraphQL query that fetches both open and recently merged PRs in a single call.
///
/// This replaces two separate `gh pr list` CLI calls with one GraphQL request,
/// cutting API usage in half for the kanban board.
///
/// Query cost optimizations:
/// - contexts(first: 20) instead of 100 — CI status is enough with top 20 checks
/// - comments(first: 10) instead of 100 — kanban board only needs recent activity
///
/// These changes reduce query cost ~25x while preserving UI functionality.
const PRS_GRAPHQL_QUERY: &str = r#"
query($owner: String!, $repo: String!) {
  repository(owner: $owner, name: $repo) {
    openPrs: pullRequests(states: OPEN, first: 100, orderBy: {field: CREATED_AT, direction: DESC}) {
      nodes {
        number
        title
        author { login }
        createdAt
        body
        mergeable
        commits(last: 1) {
          nodes {
            commit {
              statusCheckRollup {
                contexts(first: 20) {
                  nodes {
                    __typename
                    ... on CheckRun {
                      status
                      conclusion
                    }
                    ... on StatusContext {
                      state
                    }
                  }
                }
              }
            }
          }
        }
        comments(first: 10) {
          nodes {
            body
            createdAt
          }
        }
      }
    }
    mergedPrs: pullRequests(states: MERGED, first: 10, orderBy: {field: UPDATED_AT, direction: DESC}) {
      nodes {
        number
        title
        mergedAt
      }
    }
  }
}
"#;

/// Fetch both open and merged PRs for a repo using a single GraphQL call.
///
/// `name_with_owner` should be `"owner/repo"` (e.g. `"anthropics/midtown"`).
/// Returns `(open_prs, merged_prs)` formatted for the kanban board.
/// Falls back to empty vectors on failure.
fn fetch_prs_all(
    reviewer_assignments: &HashMap<u64, crate::github_state::PrReviewerAssignment>,
    name_with_owner: &str,
    repo_path: &std::path::Path,
    repo_label: Option<&str>,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let parts: Vec<&str> = name_with_owner.splitn(2, '/').collect();
    if parts.len() != 2 {
        debug!("Unexpected nameWithOwner format: {}", name_with_owner);
        return (Vec::new(), Vec::new());
    }
    let (owner, repo_name) = (parts[0], parts[1]);

    // Execute the batched GraphQL query
    let graphql_output = std::process::Command::new("gh")
        .current_dir(repo_path)
        .args([
            "api",
            "graphql",
            "-F",
            &format!("owner={}", owner),
            "-F",
            &format!("repo={}", repo_name),
            "-f",
            &format!("query={}", PRS_GRAPHQL_QUERY),
        ])
        .output();

    let data = match graphql_output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            match serde_json::from_str::<serde_json::Value>(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to parse PR GraphQL response: {}", e);
                    return (Vec::new(), Vec::new());
                }
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                "GitHub API query failed for {}: {}",
                name_with_owner,
                stderr.trim()
            );
            return (Vec::new(), Vec::new());
        }
        Err(e) => {
            warn!("Failed to execute gh command: {}", e);
            return (Vec::new(), Vec::new());
        }
    };

    let repository = match data.pointer("/data/repository") {
        Some(r) => r,
        None => {
            debug!("No repository data in PR GraphQL response");
            return (Vec::new(), Vec::new());
        }
    };

    // Process open PRs
    let open_prs = repository
        .pointer("/openPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;

                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let github_author = pr
                        .pointer("/author/login")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    let body = pr.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let author = extract_coworker_from_pr_body(body).unwrap_or(github_author);

                    let created_at = pr.get("createdAt").and_then(|v| v.as_str()).unwrap_or("");

                    // Check for merge conflicts
                    let has_conflicts = pr
                        .get("mergeable")
                        .and_then(|v| v.as_str())
                        .map(|s| s == "CONFLICTING")
                        .unwrap_or(false);

                    // Extract CI status from the last commit's statusCheckRollup
                    let check_contexts: Vec<serde_json::Value> = pr
                        .pointer("/commits/nodes")
                        .and_then(|v| v.as_array())
                        .and_then(|a| a.last())
                        .and_then(|node| node.pointer("/commit/statusCheckRollup/contexts/nodes"))
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let ci_status = pr_ci_status(&check_contexts);

                    // Extract reviewer from comments
                    let comments: Vec<serde_json::Value> = pr
                        .pointer("/comments/nodes")
                        .and_then(|v| v.as_array())
                        .cloned()
                        .unwrap_or_default();
                    let (comment_reviewer, reviewed_at) =
                        extract_reviewer_from_pr_comments(&comments);

                    // Use comment reviewer, or fall back to assigned reviewer.
                    // Track whether the review was actually posted (vs just assigned).
                    let (reviewer, reviewer_assigned_at, review_posted) =
                        if let Some(reviewer) = comment_reviewer {
                            (Some(reviewer), reviewed_at, true)
                        } else if let Some(assignment) = reviewer_assignments.get(&number) {
                            (
                                Some(assignment.reviewer.clone()),
                                Some(assignment.assigned_at.to_rfc3339()),
                                false,
                            )
                        } else {
                            (None, None, false)
                        };

                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "author": author,
                        "created_at": created_at,
                        "ci_status": ci_status,
                        "reviewer": reviewer,
                        "reviewed_at": reviewer_assigned_at,
                        "review_posted": review_posted,
                        "repo": repo_label,
                        "has_conflicts": has_conflicts,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    // Process merged PRs
    let merged_prs = repository
        .pointer("/mergedPrs/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|pr| {
                    let number = pr.get("number").and_then(|v| v.as_u64())?;
                    let title = pr
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let merged_at = pr
                        .get("mergedAt")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(serde_json::json!({
                        "number": number,
                        "title": title,
                        "merged_at": merged_at,
                        "repo": repo_label,
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    (open_prs, merged_prs)
}

/// Extract coworker name from PR body frontmatter (<!-- midtown: name -->).
fn extract_coworker_from_pr_body(body: &str) -> Option<String> {
    let marker = "midtown:";
    let marker_pos = body.find(marker)?;
    let before = &body[..marker_pos];
    if !before.contains("<!--") {
        return None;
    }
    let after_marker = &body[marker_pos + marker.len()..];
    let end = after_marker.find("-->")?;
    let name = after_marker[..end].trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Extract reviewer name and timestamp from PR comments.
fn extract_reviewer_from_pr_comments(
    comments: &[serde_json::Value],
) -> (Option<String>, Option<String>) {
    for comment in comments {
        let body = comment.get("body").and_then(|v| v.as_str()).unwrap_or("");
        if !body.contains("Code Review") && !body.contains("Code review") {
            continue;
        }

        // Try frontmatter first
        let reviewer = extract_coworker_from_pr_body(body).or_else(|| {
            // Fall back to "Code Review by {name}" header
            for line in body.lines() {
                let trimmed = line.trim().trim_start_matches('#').trim();
                if let Some(rest) = trimmed
                    .strip_prefix("Code Review by ")
                    .or_else(|| trimmed.strip_prefix("Code review by "))
                {
                    let name = rest.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            None
        });

        if let Some(name) = reviewer {
            let created_at = comment
                .get("createdAt")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return (Some(name), created_at);
        }
    }
    (None, None)
}

/// Compute CI status string from statusCheckRollup array.
fn pr_ci_status(checks: &[serde_json::Value]) -> &'static str {
    if checks.is_empty() {
        return "unknown";
    }

    let mut has_running = false;
    let mut has_failed = false;
    let mut has_passed = false;

    for check in checks {
        let status = check.get("status").and_then(|v| v.as_str());
        let conclusion = check.get("conclusion").and_then(|v| v.as_str());
        let state = check.get("state").and_then(|v| v.as_str());

        if let Some(status) = status {
            match status {
                "IN_PROGRESS" | "QUEUED" | "WAITING" | "PENDING" => has_running = true,
                "COMPLETED" => match conclusion {
                    Some("SUCCESS") => has_passed = true,
                    Some("FAILURE") | Some("CANCELLED") | Some("TIMED_OUT") => has_failed = true,
                    _ => {}
                },
                _ => {}
            }
        }

        if let Some(state) = state {
            match state {
                "PENDING" => has_running = true,
                "SUCCESS" => has_passed = true,
                "FAILURE" | "ERROR" => has_failed = true,
                _ => {}
            }
        }
    }

    if has_failed {
        "failed"
    } else if has_running {
        "running"
    } else if has_passed {
        "passed"
    } else {
        "unknown"
    }
}

// ============================================================================
// PR data cache
// ============================================================================

/// Thread-safe TTL cache for PR GraphQL data.
///
/// Stores the PR response (open PRs, merged PRs, repos) keyed by a hash of
/// repo paths. Unlike the old `KanbanCache`, this cache does NOT include
/// coworker state in its key — coworker data is now served separately via
/// `coworkers.status`.
///
/// The cache expires after `PRS_CACHE_TTL` (60s) and avoids expensive GraphQL
/// queries on every RPC call.
pub(crate) struct PrsCache {
    inner: std::sync::Mutex<Option<(Instant, serde_json::Value, u64)>>,
}

impl PrsCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(None),
        }
    }

    /// Return cached value if it exists, is younger than TTL, and matches the cache_key.
    pub(crate) fn get(&self, cache_key: u64) -> Option<serde_json::Value> {
        let guard = self.inner.lock().ok()?;
        guard
            .as_ref()
            .filter(|(ts, _, key)| ts.elapsed() < PRS_CACHE_TTL && *key == cache_key)
            .map(|(_, v, _)| v.clone())
    }

    /// Store a new value with the current timestamp and cache_key.
    pub(crate) fn set(&self, value: serde_json::Value, cache_key: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some((Instant::now(), value, cache_key));
        }
    }

    /// Remove expired entries. Called by `DaemonState::cleanup_rpc_response_cache`.
    pub(crate) fn cleanup(&self) {
        if let Ok(mut guard) = self.inner.lock()
            && guard
                .as_ref()
                .is_some_and(|(ts, _, _)| ts.elapsed() >= PRS_CACHE_TTL)
        {
            *guard = None;
        }
    }
}

// ============================================================================
// PR review handler
// ============================================================================

/// Handle `pr.review` RPC method — manually trigger a reviewer spawn for a PR.
///
/// Bypasses the auto-review delay (PR age check) and webhook deference, but still
/// respects the coworker limit and review mode configuration.
///
/// Returns a message indicating:
/// - "Reviewer assigned: <name>" on success
/// - "PR #N already has a completed review" if reviewed
/// - "PR #N already assigned to reviewer <name>" if assignment exists
/// - An error if the PR is not open, not found, or no slots are available
pub(super) async fn handle_pr_review(
    id: RequestId,
    pr_number: u64,
    state: &DaemonState,
) -> Response {
    info!("Manual review requested for PR #{}", pr_number);

    // Check if already reviewed.
    if state.is_pr_reviewed(pr_number).await {
        return Response::success(
            id,
            serde_json::json!({
                "message": format!("PR #{} already has a completed review", pr_number)
            }),
        );
    }

    // Check if already assigned.
    {
        let ps = state.persistent_state.lock().await;
        if let Some(reviewer) = ps.github.get_reviewer(pr_number) {
            return Response::success(
                id,
                serde_json::json!({
                    "message": format!(
                        "PR #{} already assigned to reviewer {}",
                        pr_number, reviewer
                    )
                }),
            );
        }
    }

    // Fetch the PR from GitHub to validate it exists and is open.
    let pr_json = match fetch_pr_json_for_review(pr_number, state).await {
        Ok(pr) => pr,
        Err(msg) => return Response::error(id, RpcError::new(-32603, msg)),
    };

    // Collect a world snapshot so collect_reviewer_effects_with_source has
    // the worktree registry, active coworker names, and branch owner map.
    let snap = super::snapshot::collect_world_snapshot(state).await;

    // Call the shared reviewer selection logic.
    // We use AssignmentSource::Manual which:
    //   - bypasses the webhook-deference guard (only active for PollingFallback)
    //   - is recorded in the assignment for observability
    // We pass None for branch_owners_map to prevent orphan-skipping based on
    // branch prefix: for a manual trigger the user wants a reviewer regardless.
    let effects = super::pr::collect_reviewer_effects_with_source(
        None,
        &snap.worktree_registry,
        &snap.active_names,
        state,
        &[pr_json],
        crate::github_state::AssignmentSource::Manual,
    )
    .await;

    if effects.is_empty() {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!(
                    "Could not assign reviewer for PR #{}: no available coworker slots or review mode disabled",
                    pr_number
                ),
            ),
        );
    }

    // Extract the reviewer name from the AssignReviewer effect for the response message.
    let reviewer_name = effects
        .iter()
        .find_map(|e| {
            if let super::effects::Effect::AssignReviewer { reviewer_name, .. } = e {
                Some(reviewer_name.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".to_string());

    super::effects::execute_effects(effects, state).await;

    Response::success(
        id,
        serde_json::json!({
            "message": format!("Reviewer assigned: {} (PR #{})", reviewer_name, pr_number)
        }),
    )
}

/// Fetch a minimal PR JSON for use with `collect_reviewer_effects_with_source`.
///
/// Uses `gh pr view` to verify the PR exists and is open, then returns a JSON
/// object with the fields the reviewer selection logic needs. `createdAt` is
/// intentionally omitted so the PR age check is bypassed for manual triggers.
async fn fetch_pr_json_for_review(
    pr_number: u64,
    state: &DaemonState,
) -> Result<serde_json::Value, String> {
    let repo_path = state
        .all_repo_paths
        .first()
        .ok_or_else(|| "No repository path configured".to_string())?
        .clone();

    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("gh")
            .current_dir(&repo_path)
            .args([
                "pr",
                "view",
                &pr_number.to_string(),
                "--json",
                "number,title,headRefName,isDraft,state,author",
            ])
            .output()
    })
    .await
    .map_err(|e| format!("spawn_blocking error: {}", e))?
    .map_err(|e| format!("Failed to run gh pr view: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "PR #{} not found or not accessible: {}",
            pr_number,
            stderr.trim()
        ));
    }

    let pr: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse gh pr view output: {}", e))?;

    let state_str = pr
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    if state_str != "OPEN" {
        return Err(format!(
            "PR #{} is {} (must be OPEN to request a review)",
            pr_number, state_str
        ));
    }

    Ok(pr)
}

#[path = "rpc_prs_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "rpc_pr_review_tests.rs"]
#[cfg(test)]
mod pr_review_tests;
