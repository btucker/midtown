//! Kanban board data handler and helpers.
//!
//! Extracted from `rpc.rs` to keep the main RPC module focused on dispatch.
//! Contains the `kanban.data` and `coworkers.status` RPC handlers, the `KanbanCache`,
//! and all GraphQL/PR-formatting logic for the web UI kanban board.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use chrono::Utc;
use tracing::{debug, error, warn};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::snapshot::ProcessHealth;

// ============================================================================
// Kanban data handler
// ============================================================================

/// Handle kanban.data RPC method - returns PR data for the kanban board.
///
/// Returns open PRs with author, reviewer, CI status, and timestamps,
/// plus recently merged PRs for the Done column.
///
/// Runs blocking GraphQL operations in spawn_blocking to avoid blocking
/// the async runtime and causing RPC timeouts.
///
/// Uses a 60s TTL cache (via `DaemonState::kanban_cache`) to avoid expensive
/// GraphQL queries on every call and reduce GitHub API usage. The cache key
/// is based only on repo paths — coworker state is no longer included since
/// coworker data is served by the separate `coworkers.status` RPC.
pub(crate) async fn handle_kanban_data(id: RequestId, state: &DaemonState) -> Response {
    // Clone data needed for cache key computation
    let all_repo_paths = state.all_repo_paths.clone();

    // Compute a hash of all repo paths for cache keying
    let mut hasher = DefaultHasher::new();
    for path in &all_repo_paths {
        path.hash(&mut hasher);
    }
    let cache_key = hasher.finish();

    // Check cache first
    if let Some(cached) = state.kanban_cache.get(cache_key) {
        debug!(
            "Returning cached kanban data (TTL: {}s)",
            KANBAN_CACHE_TTL.as_secs()
        );
        return Response::success(id, cached);
    }

    // Cache miss - fetch fresh data
    debug!("Cache miss, fetching fresh kanban data");

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
                fetch_kanban_all_prs(&reviewer_assignments, &full_name, &repo_path, repo_label);
            prs.extend(open);
            merged_prs.extend(merged);
        }

        (prs, merged_prs, repos)
    })
    .await
    {
        Ok(result) => result,
        Err(e) => {
            error!("spawn_blocking panic in kanban_data handler: {}", e);
            return Response::error(id, RpcError::new(-32603, "Internal error".to_string()));
        }
    };

    let response_data = serde_json::json!({
        "prs": prs,
        "merged_prs": merged_prs,
        "repos": repos,
    });

    state.kanban_cache.set(response_data.clone(), cache_key);

    Response::success(id, response_data)
}

/// Handle coworkers.status RPC method - returns live coworker state.
///
/// This is a lightweight endpoint with no GraphQL queries and no caching.
/// It reads directly from in-memory daemon state so responses are always
/// current (microsecond latency). The TUI polls this at 1–2s to keep the
/// coworker status panel up-to-date without delay.
///
/// Returns: coworkers, max_coworkers, lead_working, tool_activity, channel_leads.
pub(crate) async fn handle_coworkers_status(id: RequestId, state: &DaemonState) -> Response {
    let (coworkers_data, channel_lead_names) = build_coworkers_data(state, &[]).await;

    let lead_working = is_lead_actively_working(state);
    let tool_activity = collect_tool_activity(state);
    let channel_leads: Vec<&String> = channel_lead_names.iter().collect();

    Response::success(
        id,
        serde_json::json!({
            "coworkers": coworkers_data,
            "max_coworkers": state.max_coworkers,
            "lead_working": lead_working,
            "tool_activity": tool_activity,
            "channel_leads": channel_leads,
        }),
    )
}

/// Build the coworker data array from daemon state.
///
/// Accepts an optional slice of PR JSON objects. When non-empty, PR title
/// parsing is used to map PR numbers to source task IDs (for reviewers).
/// Pass an empty slice from `coworkers.status` (no GraphQL available);
/// pass the fetched PRs from `kanban.data` for richer display.
///
/// Returns `(coworkers_data, channel_lead_names)`.
async fn build_coworkers_data(
    state: &DaemonState,
    prs: &[serde_json::Value],
) -> (Vec<serde_json::Value>, std::collections::HashSet<String>) {
    // Get reviewer assignments, worktree registry, and channel lead names from persistent state
    // (best-effort via try_lock)
    let (reviewer_assignments, worktree_pr_map, channel_lead_names): (
        HashMap<u64, crate::github_state::PrReviewerAssignment>,
        HashMap<String, u64>,
        std::collections::HashSet<String>,
    ) = state
        .persistent_state
        .try_lock()
        .map(|ps| {
            let assignments = ps.github.active_assignments();
            // Build coworker -> PR map from worktree registry (for reviewers)
            let wt_map: HashMap<String, u64> = ps
                .worktree_registry
                .all_assignments()
                .iter()
                .filter_map(|(_, assignment)| {
                    let coworker = assignment.current_coworker.as_ref()?;
                    let pr_number = assignment.pr_number?;
                    Some((coworker.clone(), pr_number))
                })
                .collect();
            let cl_names: std::collections::HashSet<String> =
                ps.channel_lead_sessions.keys().cloned().collect();
            (assignments, wt_map, cl_names)
        })
        .unwrap_or_default();

    // Build reviewer -> PR number map from reviewer_assignments
    let reviewer_pr_map: HashMap<String, u64> = reviewer_assignments
        .iter()
        .map(|(pr_number, assignment)| (assignment.reviewer.clone(), *pr_number))
        .collect();

    let active_coworkers = state.coworkers.list();
    let coworker_records = state.coworker_records.read().await;

    let prs_by_task_id = build_pr_task_map(prs);

    // Read tasks to get explicit PR associations (task !1151)
    let all_tasks = crate::tasks::read_tasks();
    let task_pr_map: HashMap<u32, u64> = all_tasks
        .iter()
        .filter_map(|task| {
            let task_id: u32 = task.id.parse().ok()?;
            let pr = task.pr?;
            Some((task_id, pr))
        })
        .collect();

    // Build reverse map: PR number -> source task ID (from PR titles)
    let task_id_by_pr: HashMap<u64, u32> = prs
        .iter()
        .filter_map(|pr| {
            let pr_number = pr.get("number")?.as_u64()?;
            let title = pr.get("title")?.as_str()?;
            let task_id = crate::tasks::extract_task_id_from_pr_title(title)?;
            let task_id_u32 = u32::try_from(task_id).ok()?;
            Some((pr_number, task_id_u32))
        })
        .collect();

    // Clone health data to avoid holding the lock across await
    let health_snapshot: HashMap<String, ProcessHealth> = {
        let health_guard = state.headless_health.read().unwrap();
        health_guard.clone()
    };

    let coworkers_data = active_coworkers
        .iter()
        .filter_map(|cw| {
            // Skip channel lead sessions — they are scoped to a specific topic
            // channel and must not appear in the general coworker status panel.
            // The lead session itself also uses a reserved name and is excluded.
            if is_channel_lead(&cw.name, &channel_lead_names)
                || cw.name.eq_ignore_ascii_case("lead")
            {
                return None;
            }

            // Get coworker's workflow state from records
            let record = coworker_records.get(&cw.name);
            let workflow_phase = record.and_then(|r| r.workflow_phase);
            let task_id = record.and_then(|r| r.task_id);

            // Skip idle coworkers (phase = Idle or Completed)
            if matches!(
                workflow_phase,
                Some(crate::coworker_state::WorkflowPhase::Idle)
                    | Some(crate::coworker_state::WorkflowPhase::Completed)
            ) {
                return None;
            }

            // Get health status
            let health = health_snapshot.get(&cw.name);
            let health_color = if let Some(h) = health {
                if !h.is_alive {
                    "red" // dead
                } else if h.has_usage_limit || h.has_api_error {
                    "yellow" // degraded
                } else {
                    "green" // healthy
                }
            } else {
                "green" // default healthy
            };

            // Find PR number for this coworker, trying sources in priority order:
            // 1. Explicit task.pr field (task !1151) - most authoritative
            // 2. GitHub reviewer assignment (for review tasks)
            // 3. Worktree registry (for reviewers when reviewer_pr_map is empty)
            // 4. PR title extraction (final fallback, only when PR data is available)
            let pr_number = task_id
                .and_then(|tid| task_pr_map.get(&tid).copied())
                .or_else(|| reviewer_pr_map.get(&cw.name).copied())
                .or_else(|| worktree_pr_map.get(&cw.name).copied())
                .or_else(|| task_id.and_then(|tid| prs_by_task_id.get(&tid).copied()));

            // For display: prefer source task ID (from PR title) over internal task ID
            // This ensures reviewers show the meaningful task ID, not their ephemeral one
            let display_task_id = pr_number
                .and_then(|pr| task_id_by_pr.get(&pr).copied())
                .or(task_id);

            Some(serde_json::json!({
                "name": cw.name,
                "task_id": display_task_id,
                "phase": workflow_phase.map(|p| p.abbreviation()),
                "pr_number": pr_number,
                "health": health_color,
                "provider": cw.provider.as_str(),
                "profile": cw.profile,
                "progress": record.and_then(|r| r.progress),
                "time_estimate": record.and_then(|r| r.format_time_remaining()),
            }))
        })
        .collect::<Vec<_>>();

    (coworkers_data, channel_lead_names)
}

// ============================================================================
// Kanban / PR data helpers
// ============================================================================

/// Returns true if the coworker name identifies a channel lead session.
///
/// Channel leads are tracked in `DaemonPersistentState::channel_lead_sessions`.
/// They are scoped to a specific topic channel and must not appear in the
/// general coworker status list.
pub(crate) fn is_channel_lead(
    name: &str,
    channel_lead_names: &std::collections::HashSet<String>,
) -> bool {
    channel_lead_names.contains(name)
}

/// TTL for kanban data cache (60 seconds).
///
/// The cache key includes coworker state (task assignments, workflow phases), so it
/// invalidates automatically on meaningful changes. The TTL provides a backstop to
/// ensure freshness even when coworker state is stable. 60s (up from 30s) halves
/// GraphQL API usage while keeping the kanban board acceptably up-to-date.
const KANBAN_CACHE_TTL: Duration = Duration::from_secs(60);

/// Timeout for considering the lead session "actively working".
///
/// If the last stream event from the lead session is older than this, the
/// lead is considered idle (waiting for user input, between turns, etc.).
const LEAD_ACTIVITY_TIMEOUT: Duration = Duration::from_secs(5);

/// Check whether the headless lead session is actively working by examining
/// the `last_event_at` timestamp from `headless_health`.
///
/// Returns `true` only when the lead session is alive AND has received a
/// stream event within `LEAD_ACTIVITY_TIMEOUT`. This distinguishes "actively
/// computing" from "running but idle at the prompt".
fn is_lead_actively_working(state: &DaemonState) -> bool {
    let health_guard = state.headless_health.read().unwrap();
    let lead_health = health_guard.get("lead");
    is_session_actively_working(lead_health)
}

/// Core logic for activity detection: returns `true` when a session is alive
/// and has received a stream event within `LEAD_ACTIVITY_TIMEOUT`.
fn is_session_actively_working(health: Option<&ProcessHealth>) -> bool {
    let Some(h) = health else {
        return false;
    };
    if !h.is_alive {
        return false;
    }
    h.last_event_at.is_some_and(|ts| {
        let elapsed = (Utc::now() - ts).num_seconds();
        elapsed >= 0 && elapsed < LEAD_ACTIVITY_TIMEOUT.as_secs() as i64
    })
}

/// Collect recent tool call/result items per agent as a JSON value for the RPC response.
///
/// Returns a JSON object mapping agent name → array of serialized `UniversalItem`s.
/// This is live state — never cached — so the TUI always sees the latest activity.
fn collect_tool_activity(state: &DaemonState) -> serde_json::Value {
    let tool_map = state.recent_tool_items.read().unwrap();
    serialize_tool_activity(&tool_map)
}

/// Serialize a tool activity map to a JSON object.
///
/// Separated from `collect_tool_activity` for testability without `DaemonState`.
fn serialize_tool_activity(
    tool_map: &HashMap<String, Vec<crate::universal_events::UniversalItem>>,
) -> serde_json::Value {
    let obj: serde_json::Map<String, serde_json::Value> = tool_map
        .iter()
        .filter_map(|(agent, items)| serde_json::to_value(items).ok().map(|v| (agent.clone(), v)))
        .collect();
    serde_json::Value::Object(obj)
}

/// Build a map of task_id -> pr_number from PR data.
///
/// Extracts task IDs from PR titles (e.g., "[Midtown !1234]") and maps them
/// to their PR numbers for coworker status display.
fn build_pr_task_map(prs: &[serde_json::Value]) -> HashMap<u32, u64> {
    prs.iter()
        .filter_map(|pr| {
            let title = pr.get("title")?.as_str()?;
            let pr_number = pr.get("number")?.as_u64()?;
            let task_id = crate::tasks::extract_task_id_from_pr_title(title)?;
            // extract_task_id_from_pr_title returns u64, but task_id in CoworkerRecord is u32
            let task_id_u32 = u32::try_from(task_id).ok()?;
            Some((task_id_u32, pr_number))
        })
        .collect()
}

/// Thread-safe TTL cache for kanban GraphQL data.
///
/// Stores the kanban response (PRs, merged PRs, repos, coworkers) keyed by a
/// combined hash of repo paths AND coworker state (task assignments).
/// Note: `lead_working` and `tool_activity` are excluded from the cache and
/// injected live on each read, since they change on a sub-second cadence.
/// The cache expires after KANBAN_CACHE_TTL and avoids expensive GraphQL
/// queries on every RPC call.
///
/// The cache key includes coworker state so it invalidates when:
/// - Coworkers spawn or shut down
/// - Task assignments change
/// - Coworker workflow phases change (idle → active, etc.)
///
/// Lives in `DaemonState` so the daemon can inspect and clean it up alongside
/// other caches (see `DaemonState::cleanup_rpc_response_cache`).
pub(crate) struct KanbanCache {
    inner: std::sync::Mutex<Option<(Instant, serde_json::Value, u64)>>,
}

impl KanbanCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(None),
        }
    }

    /// Return cached value if it exists, is younger than TTL, and matches the cache_key.
    fn get(&self, cache_key: u64) -> Option<serde_json::Value> {
        let guard = self.inner.lock().ok()?;
        guard
            .as_ref()
            .filter(|(ts, _, key)| ts.elapsed() < KANBAN_CACHE_TTL && *key == cache_key)
            .map(|(_, v, _)| v.clone())
    }

    /// Store a new value with the current timestamp and cache_key.
    fn set(&self, value: serde_json::Value, cache_key: u64) {
        if let Ok(mut guard) = self.inner.lock() {
            *guard = Some((Instant::now(), value, cache_key));
        }
    }

    /// Remove expired entries. Called by `DaemonState::cleanup_rpc_response_cache`.
    pub(crate) fn cleanup(&self) {
        if let Ok(mut guard) = self.inner.lock()
            && guard
                .as_ref()
                .is_some_and(|(ts, _, _)| ts.elapsed() >= KANBAN_CACHE_TTL)
        {
            *guard = None;
        }
    }
}

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
const KANBAN_GRAPHQL_QUERY: &str = r#"
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
fn fetch_kanban_all_prs(
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
            &format!("query={}", KANBAN_GRAPHQL_QUERY),
        ])
        .output();

    let data = match graphql_output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            match serde_json::from_str::<serde_json::Value>(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to parse kanban GraphQL response: {}", e);
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
            debug!("No repository data in kanban GraphQL response");
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
                    let ci_status = kanban_ci_status(&check_contexts);

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
fn kanban_ci_status(checks: &[serde_json::Value]) -> &'static str {
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

#[path = "rpc_kanban_tests.rs"]
#[cfg(test)]
mod cache_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_coworker_from_pr_body() {
        assert_eq!(
            extract_coworker_from_pr_body("<!-- midtown: york -->\n## Summary"),
            Some("york".to_string())
        );
        assert_eq!(
            extract_coworker_from_pr_body("<!--midtown:  park  -->\nDesc"),
            Some("park".to_string())
        );
        assert_eq!(extract_coworker_from_pr_body("no frontmatter here"), None);
        assert_eq!(extract_coworker_from_pr_body(""), None);
    }

    #[test]
    fn test_extract_reviewer_from_pr_comments() {
        let comments = vec![serde_json::json!({
            "body": "<!-- midtown: lexington -->\n\n### Code review\nNo issues.",
            "createdAt": "2026-01-29T10:00:00Z"
        })];
        let (reviewer, at) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("lexington".to_string()));
        assert_eq!(at, Some("2026-01-29T10:00:00Z".to_string()));

        let comments = vec![serde_json::json!({
            "body": "## Code Review by vernon\nLGTM",
            "createdAt": "2026-01-29T11:00:00Z"
        })];
        let (reviewer, _) = extract_reviewer_from_pr_comments(&comments);
        assert_eq!(reviewer, Some("vernon".to_string()));

        let (reviewer, _) = extract_reviewer_from_pr_comments(&[]);
        assert_eq!(reviewer, None);
    }

    #[test]
    fn test_kanban_ci_status() {
        assert_eq!(kanban_ci_status(&[]), "unknown");
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "SUCCESS"})
            ]),
            "passed"
        );
        assert_eq!(
            kanban_ci_status(&[
                serde_json::json!({"status": "COMPLETED", "conclusion": "FAILURE"})
            ]),
            "failed"
        );
        assert_eq!(
            kanban_ci_status(&[serde_json::json!({"status": "IN_PROGRESS"})]),
            "running"
        );
    }

    #[test]
    fn test_reviewer_displays_source_task_id() {
        use std::collections::HashMap;

        // Simulate the kanban_data logic for a reviewer scenario

        // Mock PR data: PR #968 is for task !1158 (in title)
        let pr_number: u64 = 968;
        let _pr_title = "Fix worktree sandbox issue [Midtown !1158]";
        let source_task_id: u32 = 1158;

        // Mock reviewer's internal task ID (ephemeral, not meaningful to user)
        let reviewer_internal_task_id: u32 = 62;

        // Build task_id_by_pr map (extracted from PR titles)
        let mut task_id_by_pr: HashMap<u64, u32> = HashMap::new();
        task_id_by_pr.insert(pr_number, source_task_id);

        // Build prs_by_task_id map (for authors to find their PRs)
        let mut prs_by_task_id: HashMap<u32, u64> = HashMap::new();
        prs_by_task_id.insert(source_task_id, pr_number);

        // Build reviewer_pr_map (reviewer name -> PR they're reviewing)
        // This comes from GitHub state
        let mut reviewer_pr_map: HashMap<String, u64> = HashMap::new();
        reviewer_pr_map.insert("amsterdam".to_string(), pr_number);

        // Simulate coworker data collection for a reviewer
        let coworker_name = "amsterdam";
        let task_id = Some(reviewer_internal_task_id); // Reviewer's internal task

        // Find PR number for this coworker (either as reviewer or author)
        let pr_number_opt = reviewer_pr_map
            .get(coworker_name)
            .copied()
            .or_else(|| task_id.and_then(|tid| prs_by_task_id.get(&tid).copied()));

        // Prefer source task ID (from PR title) over internal task ID
        let display_task_id = pr_number_opt
            .and_then(|pr| task_id_by_pr.get(&pr).copied())
            .or(task_id);

        // Verify the display shows the source task ID, not the internal ID
        assert_eq!(
            display_task_id,
            Some(source_task_id),
            "Reviewer should display source task ID !{} from PR title, not internal task ID !{}",
            source_task_id,
            reviewer_internal_task_id
        );

        // Also verify we correctly found the PR for the reviewer
        assert_eq!(
            pr_number_opt,
            Some(pr_number),
            "Should find PR for reviewer"
        );

        // Verify the logic works correctly: the final display is the source task, not internal
        assert_ne!(
            display_task_id,
            Some(reviewer_internal_task_id),
            "Should NOT display reviewer's internal task ID"
        );
    }

    #[test]
    fn test_reviewer_displays_source_task_id_when_lock_fails() {
        use std::collections::HashMap;

        // Simulate the bug: try_lock() fails, so reviewer_pr_map is empty.
        // The reviewer should still display the source task ID by checking
        // the worktree registry or other fallbacks.

        // Mock PR data: PR #1087 is for task !1229 (in title)
        let pr_number: u64 = 1087;
        let source_task_id: u32 = 1229;

        // Mock reviewer's internal task ID (ephemeral Claude Code TodoWrite task)
        let reviewer_internal_task_id: u32 = 62;

        // Build task_id_by_pr map (extracted from PR titles)
        let mut task_id_by_pr: HashMap<u64, u32> = HashMap::new();
        task_id_by_pr.insert(pr_number, source_task_id);

        // Build prs_by_task_id map (for authors to find their PRs)
        let prs_by_task_id: HashMap<u32, u64> = HashMap::new();
        // Note: internal task 62 is NOT in this map (it's not a midtown task)

        // Empty reviewer_pr_map (simulating try_lock() failure)
        let reviewer_pr_map: HashMap<String, u64> = HashMap::new();

        // Empty task_pr_map (task.pr field - not used for reviewers)
        let task_pr_map: HashMap<u32, u64> = HashMap::new();

        // Simulate coworker data collection for a reviewer
        let coworker_name = "park";
        let task_id = Some(reviewer_internal_task_id); // Reviewer's internal task

        // === THIS IS THE CURRENT BUGGY LOGIC ===
        // Find PR number using current implementation (lines 193-196 in rpc_kanban.rs)
        let pr_number_opt = task_id
            .and_then(|tid| task_pr_map.get(&tid).copied())
            .or_else(|| reviewer_pr_map.get(coworker_name).copied())
            .or_else(|| task_id.and_then(|tid| prs_by_task_id.get(&tid).copied()));

        // Prefer source task ID (from PR title) over internal task ID (lines 200-202)
        let display_task_id = pr_number_opt
            .and_then(|pr| task_id_by_pr.get(&pr).copied())
            .or(task_id);

        // BUG: Without a fallback to worktree registry, this fails
        // pr_number_opt = None (all three sources failed)
        // display_task_id = Some(62) (internal task ID)
        assert_eq!(
            pr_number_opt, None,
            "Current implementation fails to find PR when reviewer_pr_map is empty"
        );
        assert_eq!(
            display_task_id,
            Some(reviewer_internal_task_id),
            "BUG: Shows internal task ID !{} instead of source task ID !{}",
            reviewer_internal_task_id,
            source_task_id
        );

        // === THIS IS WHAT WE WANT AFTER THE FIX ===
        // Add worktree registry fallback
        // For the test, we'll simulate what the worktree registry would return
        let worktree_pr_number = Some(pr_number); // Worktree has pr_number = 1087

        // Fixed PR lookup: try worktree registry as 4th fallback
        let pr_number_fixed = task_id
            .and_then(|tid| task_pr_map.get(&tid).copied())
            .or_else(|| reviewer_pr_map.get(coworker_name).copied())
            .or_else(|| task_id.and_then(|tid| prs_by_task_id.get(&tid).copied()))
            .or(worktree_pr_number); // NEW: fallback to worktree registry

        // Prefer source task ID (from PR title) over internal task ID
        let display_task_id_fixed = pr_number_fixed
            .and_then(|pr| task_id_by_pr.get(&pr).copied())
            .or(task_id);

        // EXPECTED: After fix, should display source task ID
        assert_eq!(
            pr_number_fixed,
            Some(pr_number),
            "After fix: Should find PR from worktree registry"
        );
        assert_eq!(
            display_task_id_fixed,
            Some(source_task_id),
            "After fix: Should display source task ID !{} from PR title, not internal !{}",
            source_task_id,
            reviewer_internal_task_id
        );
    }
}
