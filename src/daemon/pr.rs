//! PR management — polling, reviewer spawning, comment nudging.
//!
//! This module runs in the background to:
//! - Poll open PRs for merge conflicts, CI failures, and review status
//! - Nudge PR authors when approved (author-driven merge decisions)
//! - Spawn reviewer coworkers for unreviewed PRs
//! - Process pending review spawns from webhook-triggered delays
//! - Nudge PR owners when their PR receives comments

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::{config, daemon_messages};

use super::DaemonState;
use super::constants::*;
use super::effects::Effect;
use super::helpers::is_lead_branch;
use super::helpers::*;
use super::snapshot::WorldSnapshot;
use super::trackers::{PrIssueType, StuckConditionType};

/// Get list of coworker names who have open PRs.
///
/// A coworker is considered to have an open PR if the PR's branch name
/// starts with the coworker's name (e.g., "lexington/fix-auth").
/// Coworkers with open PRs should NEVER be sent on a break.
/// Get coworker names that have open PRs (branch name starts with coworker name).
///
/// Uses cached data from the latest `poll_prs_for_issues` call when available,
/// avoiding a separate `gh pr list` API call.
pub(super) fn get_coworkers_with_open_prs(state: &DaemonState) -> Vec<String> {
    let cache = state.pr_coworker_cache.read().unwrap();
    if !cache.open_pr_owners.is_empty() {
        return cache.open_pr_owners.iter().cloned().collect();
    }
    drop(cache);

    // Fallback to API call if cache is empty (e.g., first tick before poll runs)
    let output = std::process::Command::new("gh")
        .args(["pr", "list", "--json", "headRefName"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                return prs
                    .iter()
                    .filter_map(|pr| {
                        pr.get("headRefName")
                            .and_then(|r| r.as_str())
                            .and_then(coworker_from_branch)
                    })
                    .collect();
            }
            Vec::new()
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "Failed to get PRs from gh CLI for idle check: {}",
                stderr.trim()
            );
            Vec::new()
        }
        Err(e) => {
            warn!("Failed to execute gh pr list for idle check: {}", e);
            Vec::new()
        }
    }
}

/// Data extracted from persistent state for PR decision-making.
///
/// Bundles channel routing and session context extraction into a single
/// lock acquisition. Callers acquire `persistent_state.lock().await` once,
/// build this context, then pass it into pure `*_to_effects` functions.
/// This avoids `blocking_lock()` on the tokio::Mutex (which deadlocks).
struct PrContext {
    /// Channel routing: PR number → task ID → channel name
    pr_task_associations: HashMap<u64, String>,
    task_channel: HashMap<String, String>,
    /// Session context for the target PR (if the PR has a stored author session)
    session_context: Option<crate::rules::PrSessionContext>,
}

impl PrContext {
    /// Extract all PR decision context from persistent state for a given PR.
    ///
    /// Caller must hold `persistent_state.lock().await`. This method reads
    /// channel routing data (shared across all PRs) and session context
    /// (specific to `pr_number`) in a single pass.
    fn from_persistent_state(ps: &super::state::DaemonPersistentState, pr_number: u64) -> Self {
        let pr_task_associations: HashMap<u64, String> = ps
            .github
            .pr_author_sessions
            .iter()
            .filter_map(|(pr_num, session)| {
                session
                    .task_id
                    .as_ref()
                    .map(|task_id| (*pr_num, task_id.clone()))
            })
            .collect();

        let session_context =
            ps.github
                .get_pr_author_session(pr_number)
                .map(|s| crate::rules::PrSessionContext {
                    session_id: s.session_id.clone(),
                    branch: s.branch.clone(),
                    original_author: s.original_author.clone(),
                    pr_number,
                });

        Self {
            pr_task_associations,
            task_channel: ps.task_channel.clone(),
            session_context,
        }
    }

    /// Extract only channel routing data (when session context isn't needed).
    fn routing_only(ps: &super::state::DaemonPersistentState) -> Self {
        let pr_task_associations: HashMap<u64, String> = ps
            .github
            .pr_author_sessions
            .iter()
            .filter_map(|(pr_num, session)| {
                session
                    .task_id
                    .as_ref()
                    .map(|task_id| (*pr_num, task_id.clone()))
            })
            .collect();

        Self {
            pr_task_associations,
            task_channel: ps.task_channel.clone(),
            session_context: None,
        }
    }

    /// Look up the topic channel for a PR based on its associated task.
    fn get_channel(&self, pr_number: u64) -> Option<String> {
        let task_id = self.pr_task_associations.get(&pr_number)?;
        self.task_channel.get(task_id).cloned()
    }
}

/// Add RecordTaskAssignment to on_success for cross-tick spawn deduplication.
///
/// When spawning a coworker for a PR that's associated with a task, we must include
/// RecordTaskAssignment in the on_success callback so mark_in_flight_spawns_from_effects()
/// can track the spawn and prevent task dispatch from double-spawning the same task
/// in the next tick. See bug !1377.
fn add_task_assignment_to_on_success(
    on_success: &mut Vec<Effect>,
    pr_number: u64,
    coworker: &str,
    ctx: &PrContext,
) {
    if let Some(task_id) = ctx.pr_task_associations.get(&pr_number) {
        on_success.push(Effect::RecordTaskAssignment {
            coworker: coworker.to_string(),
            task_id: task_id.clone(),
        });
    }
}

/// How often to re-fetch merged PRs (5 minutes). Merges aren't urgent so
/// polling less frequently saves significant API calls.
const MERGED_PRS_FETCH_INTERVAL_SECS: u64 = 300;

/// Get coworker names that have recently merged PRs (branch name starts with coworker name).
///
/// Uses a time-based cache to reduce API calls. Merged PR status is only refreshed
/// every 5 minutes since merge events aren't time-critical.
pub(super) fn get_coworkers_with_merged_prs(state: &DaemonState) -> HashSet<String> {
    // Check if we need to refresh (uses CooldownTracker instead of standalone timestamp)
    let needs_refresh = {
        let cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.check(
            "merged_pr_fetch",
            "global",
            Duration::from_secs(MERGED_PRS_FETCH_INTERVAL_SECS),
        )
    };

    if !needs_refresh {
        let cache = state.pr_coworker_cache.read().unwrap();
        return cache.merged_pr_owners.clone();
    }

    // Fetch from API (include title and mergedAt for RPC cache)
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "10", // Limit merged PR fetches to last 10 PRs
            "--json",
            "headRefName,number,title,mergedAt",
        ])
        .output();

    let (coworker_names, branch_names, pr_numbers, merged_prs_data): (
        HashSet<String>,
        HashSet<String>,
        HashSet<u64>,
        Vec<serde_json::Value>,
    ) = match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                let branches: HashSet<String> = prs
                    .iter()
                    .filter_map(|pr| {
                        pr.get("headRefName")
                            .and_then(|r| r.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                let coworkers: HashSet<String> = branches
                    .iter()
                    .filter_map(|b| coworker_from_branch(b))
                    .collect();
                let numbers: HashSet<u64> = prs
                    .iter()
                    .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
                    .collect();
                // Store full PR data for RPC cache (includes number, headRefName, title, mergedAt)
                (coworkers, branches, numbers, prs)
            } else {
                (HashSet::new(), HashSet::new(), HashSet::new(), Vec::new())
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to get merged PRs from gh CLI: {}", stderr.trim());
            (HashSet::new(), HashSet::new(), HashSet::new(), Vec::new())
        }
        Err(e) => {
            warn!("Failed to execute gh pr list (merged): {}", e);
            (HashSet::new(), HashSet::new(), HashSet::new(), Vec::new())
        }
    };

    // Update cache
    {
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.merged_pr_owners = coworker_names.clone();
        cache.merged_pr_branches = branch_names;
        cache.merged_pr_numbers = pr_numbers;
        cache.merged_prs_data = merged_prs_data;
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("merged_pr_fetch", "global");
    }

    coworker_names
}

/// Get PR numbers of recently merged PRs from cache.
///
/// Used by task dispatch to skip tasks referencing merged PRs.
/// Data is populated by `get_coworkers_with_merged_prs()` as a side effect.
pub(super) fn get_merged_pr_numbers(state: &DaemonState) -> HashSet<u64> {
    let cache = state.pr_coworker_cache.read().unwrap();
    cache.merged_pr_numbers.clone()
}

/// Compute a time-aware hash of PR data for caching purposes.
///
/// Includes a time bucket (current time divided by `bucket_secs`) so the hash changes
/// periodically even when the data is unchanged. This ensures time-based decisions
/// (like PR age eligibility for reviewer spawn) are re-evaluated.
///
/// # Arguments
/// * `data` - The PR data string to hash
/// * `bucket_secs` - The time bucket size in seconds (hash changes every this many seconds)
fn compute_time_aware_hash(data: &str, bucket_secs: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    compute_time_aware_hash_at(data, bucket_secs, now_secs)
}

/// Internal function for computing time-aware hash with explicit timestamp.
/// Used by `compute_time_aware_hash` and tests.
#[cfg(test)]
fn compute_time_aware_hash_at(data: &str, bucket_secs: u64, timestamp_secs: u64) -> u64 {
    let time_bucket = timestamp_secs / bucket_secs;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    time_bucket.hash(&mut hasher);
    hasher.finish()
}

#[cfg(not(test))]
fn compute_time_aware_hash_at(data: &str, bucket_secs: u64, timestamp_secs: u64) -> u64 {
    let time_bucket = timestamp_secs / bucket_secs;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    time_bucket.hash(&mut hasher);
    hasher.finish()
}

/// Detect tasks linked to abandoned PRs (closed without merge) and return reset effects.
///
/// Pure decision function that takes snapshot data and returns effects for tasks
/// whose PRs were closed without merging. Merged PRs are handled separately by
/// build_task_completion_effects. Only resets tasks that are still in_progress.
///
/// Called from `poll_prs_for_issues` after fetching open PR list from GitHub.
pub(super) fn detect_abandoned_pr_tasks(
    snap: &WorldSnapshot,
    open_pr_numbers: &[u64],
    repo_name: &str,
) -> Vec<Effect> {
    let open_set: HashSet<u64> = open_pr_numbers.iter().copied().collect();
    let mut effects = Vec::new();

    // Check each PR with an associated task ID
    for (pr_number, task_id) in &snap.pr_task_associations {
        // PR is closed if it's not in the open set and wasn't merged
        let is_closed = !open_set.contains(pr_number);
        let is_merged = snap.merged_pr_numbers.contains(pr_number);

        if is_closed && !is_merged {
            // Check if the task is still in_progress (not already completed)
            let is_in_progress = snap
                .in_progress_tasks
                .iter()
                .any(|(tid, _, _)| tid == task_id);

            if is_in_progress {
                // Before resetting, check if the work was already completed by a DIFFERENT PR.
                // This prevents resetting tasks when a duplicate PR is closed but a sibling
                // PR for the same task was already merged.
                let work_already_landed = {
                    // Find the task once and reuse it
                    let task = snap.all_tasks.iter().find(|t| t.id == *task_id);

                    // Check if task status is completed
                    let task_completed = task
                        .map(|t| matches!(t.status, crate::tasks::TaskStatus::Completed))
                        .unwrap_or(false);

                    // Check if any other PR associated with this task was merged
                    let has_merged_sibling =
                        snap.pr_task_associations
                            .iter()
                            .any(|(other_pr, other_task_id)| {
                                other_task_id == task_id
                                    && other_pr != pr_number
                                    && snap.merged_pr_numbers.contains(other_pr)
                            });

                    // Check if task.pr field points to a merged PR
                    let task_pr_merged = task
                        .and_then(|t| t.pr)
                        .map(|pr| snap.merged_pr_numbers.contains(&pr))
                        .unwrap_or(false);

                    task_completed || has_merged_sibling || task_pr_merged
                };

                if !work_already_landed {
                    effects.push(Effect::ResetAbandonedTask {
                        task_id: task_id.clone(),
                        pr_number: *pr_number,
                        repo_name: repo_name.to_string(),
                    });
                }
            }
        }
    }

    effects
}

// ============================================================================

/// Poll all open PRs and return effects for actionable issues.
///
/// Fetches PR data from GitHub, reads tracker state to avoid duplicate nudges,
/// and returns a list of effects to execute. The caller is responsible for
/// executing the returned effects via `execute_effects()`.
///
/// Called from `evaluate_tick(PrPollTick)` in the main event loop.
pub(super) async fn poll_prs_for_issues(
    snap: &WorldSnapshot,
    state: &DaemonState,
) -> Result<Vec<Effect>, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Polling PRs for actionable issues...");

    let mut effects: Vec<Effect> = Vec::new();

    // Get list of active coworkers from snapshot (consistent with other tick handlers)
    let active_coworkers: Vec<String> = snap
        .active_coworkers
        .iter()
        .map(|c| c.name.clone())
        .collect();

    // Get running coworkers for cleanup_expired_preserving, which removes timed-out
    // reviewer assignments but preserves those for still-running reviewers (i.e., reviews
    // that are taking longer than the timeout but the reviewer is still actively working).
    // Only include coworkers that own a review worktree branch — if a reviewer name was
    // reused for dev work after restart, the stale assignment should expire naturally
    // rather than being preserved by the dev coworker's presence.
    // Also exclude usage-limited coworkers: they can't complete reviews.
    // Normalize to lowercase for consistent matching — worktree_branch_owners
    // comes from WorktreeAssignment.current_coworker (external input), while
    // running_coworkers uses names from AVENUE_NAMES (always lowercase).
    let review_branch_owners: HashSet<String> = snap
        .worktree_branch_owners
        .iter()
        .filter(|(branch, _)| branch.starts_with("review-pr-"))
        .map(|(_, owner)| owner.to_lowercase())
        .collect();
    let running_coworker_names: HashSet<String> = snap
        .running_coworkers
        .iter()
        .map(|c| c.name.clone())
        .filter(|name| {
            review_branch_owners.contains(&name.to_lowercase())
                && !snap.usage_limited_coworkers.contains(&name.to_lowercase())
        })
        .collect();
    // Build session ID set for same reviewer-subset — enables session-based matching
    // in cleanup_expired_preserving when assignments carry a reviewer_session_id.
    let running_reviewer_session_ids: HashSet<String> = snap
        .running_coworkers
        .iter()
        .filter(|c| {
            review_branch_owners.contains(&c.name.to_lowercase())
                && !snap
                    .usage_limited_coworkers
                    .contains(&c.name.to_lowercase())
        })
        .filter_map(|c| c.session_id.clone())
        .collect();

    // Get list of idle coworkers for handoff decisions
    let idle_coworkers: Vec<String> = {
        let records = state.coworker_records.read().await;
        records
            .iter()
            .filter(|(name, record)| {
                // Must be an active coworker
                active_coworkers.contains(name)
                    // Must have reported Idle phase
                    && record.workflow_phase
                        == Some(crate::coworker_state::WorkflowPhase::Idle)
            })
            .map(|(name, _)| name.clone())
            .collect()
    };

    // Run gh pr list command (include createdAt and isDraft for review filtering)
    // Include state field to filter out merged/closed PRs after restart
    // NOTE: comments are fetched on-demand in collect_comment_notification_effects
    // to reduce GraphQL cost (bulk poll runs every 30s for ALL PRs, but comment detection
    // only needs to check PRs owned by coworkers/lead). Author is included for RPC cache.
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state,author",
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr list failed: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Hash the response to detect changes. If the PR data hasn't changed since the last poll,
    // skip the expensive lock acquisition, issue detection, and nudge logic.
    //
    // IMPORTANT: Include a time bucket so hash changes every PR_REVIEW_DELAY_SECS. This ensures
    // time-based decisions (like PR age eligibility for reviewer spawn) are re-evaluated even
    // when PR data is unchanged. Without this, a PR that was "too new" on one poll would never
    // be re-checked if the response hash stayed the same.
    let response_hash = compute_time_aware_hash(&stdout, PR_REVIEW_DELAY_SECS);
    {
        let mut last_hash = state.last_pr_poll_hash.lock().await;
        if *last_hash == response_hash && response_hash != 0 {
            debug!("PR poll: data unchanged, skipping processing");
            return Ok(effects);
        }
        *last_hash = response_hash;
    }

    let prs: Vec<serde_json::Value> = serde_json::from_str(&stdout)?;

    // Cleanup old tracking entries, but preserve assignments for RUNNING coworkers
    // so reviewers don't lose their PR tracking while actively reviewing.
    // Using running_coworkers (not active_coworkers) ensures that idle/stopped
    // reviewers have their assignments cleaned up, freeing slots for new reviews.
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.cleanup();
    }
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_expired_preserving(
            &running_coworker_names,
            Some(&running_reviewer_session_ids),
        );
        // Backfill reviewer_session_id for assignments created before the session
        // started (optimistic assignment pattern: assign before spawn completes).
        let reviewer_session_map: HashMap<String, String> = snap
            .running_coworkers
            .iter()
            .filter(|c| review_branch_owners.contains(&c.name.to_lowercase()))
            .filter_map(|c| {
                c.session_id
                    .as_ref()
                    .map(|sid| (c.name.clone(), sid.clone()))
            })
            .collect();
        ps.github
            .backfill_reviewer_session_ids(&reviewer_session_map);
        ps.github.cleanup_stale_webhook_events();
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.cleanup(Duration::from_secs(7200)); // 2 hours
    }
    state.cleanup_rpc_response_cache().await;

    // Filter to only open PRs (defense-in-depth: gh pr list --state open should only return
    // open PRs, but verify via the state field to guard against stale/cached results)
    let prs: Vec<serde_json::Value> = prs
        .into_iter()
        .filter(|pr| {
            let state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("OPEN");
            state == "OPEN"
        })
        .collect();

    // Cache open PR owners for reuse by get_coworkers_with_open_prs
    {
        let owners: HashSet<String> = prs
            .iter()
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(|branch| {
                        coworker_from_branch_with_map(branch, Some(&snap.worktree_branch_owners))
                    })
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.open_pr_owners = owners;
    }

    // Cache full open PR data for RPC responses (avoids gh CLI calls in handle_status).
    // Format the PR data similarly to get_open_prs() in rpc.rs, including task enrichment.
    {
        let tasks = &snap.all_tasks;
        let task_map: std::collections::HashMap<u64, String> = tasks
            .iter()
            .filter_map(|t| {
                let id = t.id.parse::<u64>().ok()?;
                Some((id, t.subject.clone()))
            })
            .collect();

        let formatted_prs: Vec<serde_json::Value> = prs
            .iter()
            .map(|pr| {
                let status = format_pr_status_for_rpc(pr);
                let title = pr.get("title").and_then(|t| t.as_str()).unwrap_or("");
                let task_id = crate::tasks::extract_task_id_from_pr_title(title);
                let task_name = task_id.and_then(|id| task_map.get(&id).cloned());
                serde_json::json!({
                    "number": pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0),
                    "title": title,
                    "author": pr.get("author").and_then(|a| a.get("login")).and_then(|l| l.as_str()).unwrap_or("unknown"),
                    "headRefName": pr.get("headRefName").and_then(|r| r.as_str()),
                    "isDraft": pr.get("isDraft").and_then(|d| d.as_bool()),
                    "status": status,
                    "task_id": task_id,
                    "task_name": task_name,
                })
            })
            .collect();

        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.open_prs_data = formatted_prs;
    }

    // Cache coworker names whose PRs have all CI checks passing (for PR break decisions)
    {
        let ci_passed: HashSet<String> = prs
            .iter()
            .filter(|pr| all_ci_checks_passed(pr))
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(|branch| {
                        coworker_from_branch_with_map(branch, Some(&snap.worktree_branch_owners))
                    })
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.ci_passed_pr_owners = ci_passed;
        // Mark PR poll as initialized so orphan detection knows we have PR data.
        // This prevents false positive orphan warnings during daemon startup when
        // orphan checks run before the first PR poll completes.
        cache.pr_poll_initialized = true;
    }

    // Cleanup saved PR break sessions for coworkers whose PRs are no longer open
    {
        let active_pr_coworkers: HashSet<String> = prs
            .iter()
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(|branch| {
                        coworker_from_branch_with_map(branch, Some(&snap.worktree_branch_owners))
                    })
            })
            .collect();
        let mut sessions = state.pr_break_sessions.write().unwrap();
        let before = sessions.len();
        sessions.retain(|name, _| active_pr_coworkers.contains(name));
        let removed = before - sessions.len();
        if removed > 0 {
            info!(
                "Cleaned up {} stale PR break session(s) (PR closed/merged)",
                removed
            );
        }
    }

    // Detect abandoned PRs (closed without merge) and reset associated tasks.
    // This uses pure decision logic that takes only snapshot data and returns effects.
    let open_pr_numbers: Vec<u64> = prs
        .iter()
        .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
        .collect();
    let abandoned_pr_effects = detect_abandoned_pr_tasks(snap, &open_pr_numbers, &state.repo_name);
    effects.extend(abandoned_pr_effects);

    // Clean up persistent reviewer assignments for PRs that are no longer open.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_closed_prs(&open_pr_numbers);
        ps.github.cleanup_expired_preserving(
            &running_coworker_names,
            Some(&running_reviewer_session_ids),
        );
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            warn!("Failed to save daemon-state.json after cleanup: {}", e);
        }
    }

    for pr in &prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Try to map branch to a coworker owner
        let owner_opt = coworker_from_branch_with_map(head_ref, Some(&snap.worktree_branch_owners));

        // Check for actionable issues
        let issues = detect_pr_issues(pr);

        // Handle PRs whose owner is not currently active (on break, never spawned, etc.)
        // coworker_from_branch_with_map returns Some("york") for "york/fix-auth" even if
        // york has no worktree, so we need to check if the owner is actually active.
        if let Some(ref owner) = owner_opt {
            // Check if this owner has an active worktree (i.e., is actually working)
            let has_active_worktree = snap.worktree_branch_owners.values().any(|o| o == owner);

            // If the owner has no active worktree, treat this as an orphaned PR
            if !has_active_worktree && !issues.is_empty() {
                for issue_type in &issues {
                    // Only handle critical issues for orphaned PRs (merge conflicts, CI failures)
                    // Skip workflow issues like approval status that require active ownership
                    match issue_type {
                        PrIssueType::MergeConflict | PrIssueType::CiFailed => {
                            // Check if we should nudge for this issue
                            let should_nudge = {
                                let tracker = state.pr_issue_tracker.lock().await;
                                tracker.should_nudge(pr_number, *issue_type)
                            };

                            if should_nudge {
                                // Post a system message warning about the orphaned PR issue
                                let warning = format!(
                                    "@lead Orphaned PR #{} ({}) - {}: {} (owner: {}, branch: {})",
                                    pr_number,
                                    truncate_str(title, 40),
                                    issue_type,
                                    get_issue_action(*issue_type),
                                    owner,
                                    head_ref
                                );
                                effects.push(Effect::PostSystemMessage {
                                    message: format!("⚠️ {}", warning),
                                });
                                // Record the nudge to prevent repeated warnings on subsequent ticks
                                effects.push(Effect::RecordPrNudge {
                                    pr_number,
                                    issue_type: *issue_type,
                                });
                            }
                        }
                        _ => {
                            // Skip non-critical issues for orphaned PRs
                        }
                    }
                }
                // Continue to skip the normal PR processing for this orphaned PR
                continue;
            }
        }

        // Handle PRs with no determinable owner (not in worktree_branch_owners and
        // doesn't match coworker/branch pattern) that have critical issues
        if owner_opt.is_none() && !issues.is_empty() {
            for issue_type in &issues {
                // Only handle critical issues for PRs with no owner (merge conflicts, CI failures)
                // Skip workflow issues like approval status that require active ownership
                match issue_type {
                    PrIssueType::MergeConflict | PrIssueType::CiFailed => {
                        // Check if we should nudge for this issue
                        let should_nudge = {
                            let tracker = state.pr_issue_tracker.lock().await;
                            tracker.should_nudge(pr_number, *issue_type)
                        };

                        if should_nudge {
                            // Post a system message warning about the fully orphaned PR issue
                            // (no extractable owner at all, not even from branch name)
                            let warning = format!(
                                "@lead Orphaned PR #{} ({}) - {}: {} (no owner, branch: {})",
                                pr_number,
                                truncate_str(title, 40),
                                issue_type,
                                get_issue_action(*issue_type),
                                head_ref
                            );
                            effects.push(Effect::PostSystemMessage {
                                message: format!("⚠️ {}", warning),
                            });
                            effects.push(Effect::RecordPrNudge {
                                pr_number,
                                issue_type: *issue_type,
                            });
                        }
                    }
                    _ => {
                        // Skip non-critical issues for PRs with no owner
                    }
                }
            }
            // Continue to skip normal PR processing for this fully orphaned PR
            continue;
        }

        // Skip PRs that don't have a coworker owner (e.g., dependabot, feature branches)
        let owner = match owner_opt {
            Some(o) => o,
            None => continue,
        };

        for issue_type in issues {
            // Check if we should nudge for this issue
            let should_nudge = {
                let tracker = state.pr_issue_tracker.lock().await;
                tracker.should_nudge(pr_number, issue_type)
            };

            if !should_nudge {
                continue;
            }

            // Author-driven merge decisions: Instead of auto-merging approved PRs,
            // nudge the author so THEY can decide to merge. This keeps merge decisions
            // with the agent who has full context of the PR and review feedback.
            use crate::rules::decide_pr_issue_action_with_handoff;

            // Format the nudge message
            let message = format!(
                "PR #{} ({}) - {}: {}",
                pr_number,
                truncate_str(title, 40),
                issue_type,
                get_issue_action(issue_type)
            );

            // Extract all decision context from persistent state in one lock
            let pr_ctx = {
                let ps = state.persistent_state.lock().await;
                PrContext::from_persistent_state(&ps, pr_number)
            };

            // Decide action using pure decision function with handoff support
            let action = decide_pr_issue_action_with_handoff(
                &owner,
                &active_coworkers,
                &idle_coworkers,
                state.is_at_dev_limit(),
                pr_ctx.session_context.as_ref(),
                &message,
            );

            effects.extend(pr_action_to_effects(
                action, pr_number, title, issue_type, state, &pr_ctx,
            ));
        }
    }

    // Polling fallback for review comment notifications (when webhooks are degraded)
    effects.extend(
        collect_comment_notification_effects(snap, state, &prs, &active_coworkers, &idle_coworkers)
            .await,
    );

    // Auto-spawn reviewers for PRs that need review
    effects.extend(collect_reviewer_effects(snap, state, &prs).await);

    // Pre-collect review status for all PRs before stuck detection (pure decision logic
    // should not make async API calls). Coworkers can't submit formal GitHub reviews
    // since they share the same user as PR authors, so we check for comment-based reviews.
    let reviewed_prs: HashSet<u64> = {
        let mut reviewed = HashSet::new();
        for pr in &prs {
            if let Some(pr_number) = pr.get("number").and_then(|n| n.as_u64())
                && state.is_pr_reviewed(pr_number).await
            {
                reviewed.insert(pr_number);
            }
        }
        reviewed
    };

    // Compute prs_needing_review and update cache (must happen here, not in effect
    // collection functions which should be pure). This value is used by task dispatch
    // to prioritize PR reviews over new task pickup.
    let prs_needing_review: usize = prs
        .iter()
        .filter(|pr| {
            let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
            let review_decision = pr
                .get("reviewDecision")
                .and_then(|r| r.as_str())
                .unwrap_or("");
            let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
            // PR needs review if it's not a draft, has no formal review, and no Claude comment review
            pr_number != 0
                && !is_draft
                && review_decision.is_empty()
                && !reviewed_prs.contains(&pr_number)
        })
        .count();
    // Cache coworker names whose PRs have CI passed + review feedback (for idle shutdown protection).
    // This mirrors the criteria in collect_green_with_feedback_effects: CI green, reviewed, not approved.
    {
        let review_feedback: HashSet<String> = prs
            .iter()
            .filter(|pr| {
                let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                let review_decision = pr
                    .get("reviewDecision")
                    .and_then(|r| r.as_str())
                    .unwrap_or("");
                all_ci_checks_passed(pr)
                    && reviewed_prs.contains(&pr_number)
                    && review_decision != "APPROVED"
            })
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(coworker_from_branch)
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.prs_needing_review = prs_needing_review;
        cache.review_feedback_pr_owners = review_feedback;
    }

    // Nudge PR owners when CI turns green and they have review feedback to address.
    // This covers the case where a coworker is waiting for CI while feedback awaits.
    effects.extend(
        collect_green_with_feedback_effects(
            snap,
            state,
            &prs,
            &reviewed_prs,
            &active_coworkers,
            &idle_coworkers,
        )
        .await,
    );

    // Check for stuck conditions and nudge lead if self-healing has failed
    effects.extend(collect_stuck_condition_effects(state, &prs, &reviewed_prs).await);

    // Detect stale CI checks and trigger re-runs
    effects.extend(collect_stale_check_effects(state, &prs).await);

    Ok(effects)
}

/// Collect effects for PRs that are green (all CI passed) and have review feedback.
///
/// When a coworker's PR has all CI checks passing and has received a code review,
/// nudge them to address any feedback and merge. This covers the case where
/// a coworker is waiting for CI to pass while feedback awaits.
async fn collect_green_with_feedback_effects(
    snap: &WorldSnapshot,
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
    active_coworkers: &[String],
    idle_coworkers: &[String],
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for pr in prs {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) => n,
            None => continue,
        };

        // Only process PRs that have been reviewed
        if !reviewed_prs.contains(&pr_number) {
            continue;
        }

        // Only process PRs where all CI checks have passed
        if !all_ci_checks_passed(pr) {
            continue;
        }

        // Skip if already approved (will be auto-merged or nudged via Approved issue type)
        let review_decision = pr
            .get("reviewDecision")
            .and_then(|r| r.as_str())
            .unwrap_or("");
        if review_decision == "APPROVED" {
            continue;
        }

        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Only process coworker-owned PRs (validates branch prefix against known names)
        let owner =
            match coworker_from_branch_with_map(head_ref, Some(&snap.worktree_branch_owners)) {
                Some(o) => o,
                None => continue, // Not a coworker PR (e.g., dependabot, btucker/*)
            };

        // Bug fix (!1067): Clear cooldown if owner is not active (died or went idle).
        // Without this, if a coworker is spawned to address review feedback but dies
        // (e.g., API error), the cooldown blocks retries and work is silently dropped.
        // This MUST run before the should_nudge check below, so the cleared cooldown
        // allows the PR to be re-evaluated in the same tick.
        if !active_coworkers.contains(&owner) {
            let mut tracker = state.pr_issue_tracker.lock().await;
            // Only clear if there WAS a prior nudge — don't touch untracked PRs
            if tracker.has_nudge(pr_number, PrIssueType::GreenWithFeedback) {
                debug!(
                    "PR #{} owner '{}' is not active — clearing GreenWithFeedback cooldown to allow retry",
                    pr_number, owner
                );
                tracker.clear_nudge(pr_number, PrIssueType::GreenWithFeedback);
            }
        }

        // Check cooldown to avoid spamming
        let should_nudge = {
            let tracker = state.pr_issue_tracker.lock().await;
            tracker.should_nudge(pr_number, PrIssueType::GreenWithFeedback)
        };
        if !should_nudge {
            continue;
        }

        let message = format!(
            "PR #{} ({}) - {}: {}",
            pr_number,
            truncate_str(title, 40),
            PrIssueType::GreenWithFeedback,
            get_issue_action(PrIssueType::GreenWithFeedback)
        );

        // Extract all decision context from persistent state in one lock
        let pr_ctx = {
            let ps = state.persistent_state.lock().await;
            PrContext::from_persistent_state(&ps, pr_number)
        };

        // Decide action using handoff-aware decision function (matches webhook path)
        let action = crate::rules::decide_pr_issue_action_with_handoff(
            &owner,
            active_coworkers,
            idle_coworkers,
            state.is_at_dev_limit(),
            pr_ctx.session_context.as_ref(),
            &message,
        );

        effects.extend(pr_action_to_effects(
            action,
            pr_number,
            title,
            PrIssueType::GreenWithFeedback,
            state,
            &pr_ctx,
        ));
    }

    effects
}

/// Convert a `PrAction` decision into a list of `Effect`s to execute.
///
/// Translates the pure decision from `rules::decide_pr_issue_action_with_handoff` (or similar)
/// into concrete effects. Uses `SpawnCoworkerWithCallbacks` for spawn actions so
/// that follow-up effects (broadcast update, channel message, session cleanup)
/// only happen on success, with a fallback message on failure.
fn pr_action_to_effects(
    action: crate::rules::PrAction,
    pr_number: u64,
    title: &str,
    issue_type: PrIssueType,
    state: &DaemonState,
    ctx: &PrContext,
) -> Vec<Effect> {
    use crate::rules::PrAction;

    // Look up topic channel for this PR's task (falls back to main if not found)
    let channel = ctx.get_channel(pr_number);

    match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::NudgeCoworkerWithCallbacks {
                name: owner,
                message,
                session_id: None,
                on_success: vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            }]
        }
        PrAction::SpawnOwner { owner, message } => {
            // Pure decision: should we resume with saved session or fresh?
            let resume_mode = {
                let sessions = state.pr_break_sessions.read().unwrap();
                crate::rules::decide_pr_owner_resume_mode(&owner, &sessions)
            };
            let has_saved_session = matches!(
                resume_mode,
                crate::rules::PrOwnerResumeMode::WithSavedSession(_)
            );
            let session_mode = match resume_mode {
                crate::rules::PrOwnerResumeMode::WithSavedSession(sid) => {
                    crate::launch::SessionMode::ResumeSession(sid)
                }
                crate::rules::PrOwnerResumeMode::WithoutSavedSession => {
                    crate::launch::SessionMode::Resume
                }
            };
            let config = crate::launch::LaunchConfig::coworker(
                owner.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(message),
            );

            let mut on_success = vec![
                Effect::BroadcastCoworkerUpdate {
                    name: owner.clone(),
                    status: "running".to_string(),
                    current_task: Some(format!("working on PR #{}", pr_number)),
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: daemon_messages::called_in_pr_issue(
                        &owner,
                        &issue_type.to_string(),
                        pr_number,
                        config::get_personality(),
                    ),
                    channel: channel.clone(),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            add_task_assignment_to_on_success(&mut on_success, pr_number, &owner, ctx);

            if has_saved_session {
                on_success.push(Effect::ClearPrBreakSession {
                    name: owner.clone(),
                });
            }

            let on_failure = vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "PR #{} ({}) owned by {} - {}: {} (call-in failed)",
                        pr_number,
                        truncate_str(title, 40),
                        owner,
                        issue_type,
                        get_issue_action(issue_type)
                    ),
                    channel: channel.clone(),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            }]
        }
        PrAction::HandoffToCoworker {
            assignee,
            original_author,
            pr_number,
            branch,
            session_id,
            message,
        } => handoff_to_coworker_effects(
            &assignee,
            &original_author,
            pr_number,
            &branch,
            session_id,
            &message,
            "resuming their session for full context",
            title,
            issue_type,
            state,
            ctx,
        ),
        PrAction::PostToChannel { message } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message,
                    channel: channel.clone(),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::Skip { reason } => {
            debug!("{}", reason);
            vec![]
        }
    }
}

/// Check for stuck conditions and return effects to nudge the lead.
///
/// This function runs during each PR poll cycle and checks for:
/// 1. PRs open with no review for too long
/// 2. PRs with unresolved feedback for too long
/// 3. PRs that are approved + CI green but not merging
/// 4. Coworkers who are silent (no channel activity) for too long
/// 5. Review backlog (more PRs need review than slots available)
///
/// Returns effects (NudgeCoworker, PostSystemMessage) instead of executing
/// side effects inline. Each condition has a cooldown tracked via the
/// stuck_tracker to avoid spamming. For stuck conditions that @mention the lead,
/// the channel's chat monitor handles routing the nudge.
///
/// The `reviewed_prs` parameter contains PR numbers that have Claude reviews
/// (comment-based or formal), pre-collected before this function to keep
/// decision logic free of async API calls.
///
async fn collect_stuck_condition_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();
    let mut tracker = state.stuck_tracker.lock().await;
    tracker.cleanup();

    let now = Instant::now();

    // Track how many nudges we send this cycle (for logging)
    let mut nudge_count = 0;

    // --- Scenario 1: PR open with no review for N minutes ---
    for pr in prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
        if is_draft {
            continue;
        }

        let review_decision = pr
            .get("reviewDecision")
            .and_then(|r| r.as_str())
            .unwrap_or("");

        let age_secs = get_pr_age_secs(pr).unwrap_or(0);
        let pr_id = pr_number.to_string();

        // Check for comment-based Claude reviews (coworkers can't submit formal reviews
        // since they share the same GitHub user as the PR author). Uses pre-collected
        // data to keep decision logic free of async API calls.
        let has_claude_review = reviewed_prs.contains(&pr_number);

        // No review decision at all, no Claude review comment, and PR is old enough
        if review_decision.is_empty()
            && !has_claude_review
            && age_secs >= STUCK_NO_REVIEW_DURATION.as_secs()
        {
            // Check if a reviewer is assigned (daemon tried to self-heal)
            let is_assigned = {
                let ps = state.persistent_state.lock().await;
                ps.github.is_assigned(pr_number)
            };

            tracker.track(&pr_id, StuckConditionType::NoReview);
            if tracker.should_nudge(&pr_id, StuckConditionType::NoReview) {
                let prior_nudges = tracker.nudge_count(&pr_id, StuckConditionType::NoReview);
                let has_available_slots = state.has_available_coworker_slot();

                let nudge = if should_escalate(prior_nudges) {
                    // Escalation: this has persisted too long, suggest investigation
                    let context = if is_assigned && has_available_slots {
                        "A reviewer was assigned but hasn't posted a review, and coworker slots are available. This looks like a daemon bug."
                    } else if !is_assigned && has_available_slots {
                        "Coworker slots are available but no reviewer was assigned. This looks like a daemon bug."
                    } else if is_assigned {
                        "A reviewer was assigned but hasn't posted a review."
                    } else {
                        "No reviewer could be assigned (all slots may be in use)."
                    };
                    format!(
                        "@lead PR #{} ({}) has been stuck for {} minutes with no review — {} Consider running `midtown e2e capture` to debug.",
                        pr_number,
                        truncate_str(title, 40),
                        age_secs / 60,
                        context,
                    )
                } else {
                    // Normal warning
                    let context = if is_assigned {
                        "I assigned a reviewer but no review has been posted yet"
                    } else {
                        "I couldn't assign a reviewer"
                    };
                    format!(
                        "@lead PR #{} ({}) has been open for {} minutes with no review — {}",
                        pr_number,
                        truncate_str(title, 40),
                        age_secs / 60,
                        context,
                    )
                };
                effects.extend(stuck_nudge_effects(&nudge));
                tracker.record_nudge(&pr_id, StuckConditionType::NoReview);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::NoReview);
        }

        // --- Scenario 2: Unresolved feedback (changes requested) for N minutes ---
        if review_decision == "CHANGES_REQUESTED" {
            let first_detected = tracker.track(&pr_id, StuckConditionType::UnresolvedFeedback);
            let stuck_duration = now.duration_since(first_detected);

            if stuck_duration >= STUCK_UNRESOLVED_FEEDBACK_DURATION
                && tracker.should_nudge(&pr_id, StuckConditionType::UnresolvedFeedback)
            {
                let prior_nudges =
                    tracker.nudge_count(&pr_id, StuckConditionType::UnresolvedFeedback);

                let nudge = if should_escalate(prior_nudges) {
                    format!(
                        "@lead PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't responded despite repeated nudges. The coworker may be stuck or the task may need reassignment.",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                } else {
                    format!(
                        "@lead PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't pushed new changes",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                };
                effects.extend(stuck_nudge_effects(&nudge));
                tracker.record_nudge(&pr_id, StuckConditionType::UnresolvedFeedback);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::UnresolvedFeedback);
        }

        // --- Scenario 3: Approved + CI green but not merging ---
        if is_auto_mergeable(pr) {
            let first_detected = tracker.track(&pr_id, StuckConditionType::MergeReady);
            let stuck_duration = now.duration_since(first_detected);

            if stuck_duration >= STUCK_MERGE_READY_DURATION
                && tracker.should_nudge(&pr_id, StuckConditionType::MergeReady)
            {
                let prior_nudges = tracker.nudge_count(&pr_id, StuckConditionType::MergeReady);

                let nudge = if should_escalate(prior_nudges) {
                    format!(
                        "@lead PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — the author isn't responding to merge nudges. Consider merging manually or investigating the coworker.",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                } else {
                    format!(
                        "@lead PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — author may need a nudge to merge",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                };
                effects.extend(stuck_nudge_effects(&nudge));
                tracker.record_nudge(&pr_id, StuckConditionType::MergeReady);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::MergeReady);
        }
    }

    // --- Scenario 4: Silent coworker (claimed task, no channel activity) ---
    {
        let busy_coworkers = state.get_all_busy_coworkers();
        let records = state.coworker_records.read().await;

        for name in &busy_coworkers {
            let last_activity: Option<Instant> =
                records.get(name.as_str()).and_then(|r| r.last_activity);
            let is_silent = match last_activity {
                Some(last) => last.elapsed() >= STUCK_SILENT_COWORKER_DURATION,
                // No activity recorded — coworker hasn't posted to channel yet.
                // They're still initializing (loading plugins, restoring session, etc.).
                // Only start the silence clock after their first channel message.
                None => false,
            };

            if is_silent {
                tracker.track(name, StuckConditionType::SilentCoworker);
                if tracker.should_nudge(name, StuckConditionType::SilentCoworker) {
                    let task_info = crate::tasks::get_in_progress_tasks_with_subjects()
                        .into_iter()
                        .find(|(_, _, owner)| owner.eq_ignore_ascii_case(name))
                        .map(|(id, subject, _)| {
                            format!("task !{} ({})", id, truncate_str(&subject, 30))
                        })
                        .unwrap_or_else(|| "their task".to_string());

                    let prior_nudges =
                        tracker.nudge_count(name, StuckConditionType::SilentCoworker);

                    if prior_nudges == 0 {
                        // First nudge: ask the coworker directly before escalating
                        let nudge_msg = format!(
                            "Status check — you've been quiet on {} for over {} minutes. \
                             Are you stuck or still working?",
                            task_info,
                            STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                        );
                        effects.push(Effect::NudgeCoworker {
                            name: name.clone(),
                            message: nudge_msg,
                            session_id: None,
                        });
                        // Post to channel so it's visible
                        effects.push(Effect::PostSystemMessage {
                            message: format!(
                                "⚠️ Nudging {} — silent on {} for over {} minutes",
                                name,
                                task_info,
                                STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                            ),
                        });
                    } else {
                        // Escalation: coworker didn't respond, notify lead
                        let nudge = format!(
                            "@lead {} has been silent on {} for over {} minutes \
                             (nudged {} previously with no response)",
                            name,
                            task_info,
                            STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                            name,
                        );
                        effects.extend(stuck_nudge_effects(&nudge));
                    }
                    tracker.record_nudge(name, StuckConditionType::SilentCoworker);
                    nudge_count += 1;
                }
            } else {
                tracker.clear(name, StuckConditionType::SilentCoworker);
            }
        }
    }

    if nudge_count > 0 {
        info!(
            "Stuck condition check: nudged lead about {} issue(s)",
            nudge_count
        );
    }

    effects
}

/// Determine if a stuck condition should escalate based on nudge count.
///
/// Returns true if this nudge (including the current one) meets or exceeds
/// the escalation threshold. Since `prior_nudges` is the count *before* the
/// current nudge is recorded, we add 1 to get "this nudge number".
///
/// With STUCK_ESCALATION_NUDGE_COUNT = 2:
/// - First nudge (prior=0): 0+1=1 < 2, no escalation
/// - Second nudge (prior=1): 1+1=2 >= 2, escalation
fn should_escalate(prior_nudges: u32) -> bool {
    prior_nudges + 1 >= STUCK_ESCALATION_NUDGE_COUNT
}

/// Convert a stuck condition nudge message into effects (system message only).
///
/// The message should contain "@lead" which the chat monitor will detect and
/// route to the lead via headed intercom. We don't return NudgeLead here because
/// that would cause double delivery (the channel @mention routing already
/// handles it).
fn stuck_nudge_effects(message: &str) -> Vec<Effect> {
    vec![Effect::PostSystemMessage {
        message: format!("⚠️ {}", message),
    }]
}

/// Fetch PR details with comments and author for a specific PR.
///
/// This is used by `collect_comment_notification_effects` to fetch comment data
/// on-demand, avoiding the GraphQL cost of fetching comments for ALL open PRs
/// in the bulk poll.
async fn fetch_pr_comments(
    pr_number: u64,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "comments,author",
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr view failed for PR #{}: {}", pr_number, stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pr_data: serde_json::Value = serde_json::from_str(&stdout)?;
    Ok(pr_data)
}

/// Polling fallback for review comment notifications.
///
/// When webhooks are degraded, this detects new review comments by comparing
/// comment counts with tracked state. Uses the same cooldown as webhooks
/// (`PrIssueType::ReviewComment`) to avoid duplicate notifications.
///
/// For each coworker-owned PR:
/// 1. Fetch comments on-demand (not included in bulk poll to reduce GraphQL cost)
/// 2. Count non-owner comments (excludes PR author and coworker's own comments)
/// 3. If count increased since last poll, nudge/spawn the owner AND create a review
///    feedback task for consistent "task !X" formatting
///
/// This enables the polling path to fill the gap identified in graceful degradation:
/// webhooks handle real-time notifications, polling handles the fallback case.
/// Both paths create tasks so the Lead sees consistent formatting, while preserving
/// handoff-to-idle-coworker and session resume capabilities.
async fn collect_comment_notification_effects(
    snap: &WorldSnapshot,
    state: &DaemonState,
    prs: &[serde_json::Value],
    active_coworkers: &[String],
    idle_coworkers: &[String],
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();

    // Get open PR numbers for tracker cleanup
    let open_pr_numbers: Vec<u64> = prs
        .iter()
        .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
        .collect();

    // Clean up tracker entries for closed PRs
    {
        let mut tracker = state.comment_tracker.lock().await;
        tracker.cleanup(&open_pr_numbers);
    }

    for pr in prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }

        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Check for lead/* branches first, before filtering by coworker ownership
        if is_lead_branch(head_ref) {
            // Fetch PR details with comments on-demand
            let pr_with_comments = match fetch_pr_comments(pr_number).await {
                Ok(data) => data,
                Err(e) => {
                    debug!("Failed to fetch comments for PR #{}: {}", pr_number, e);
                    continue;
                }
            };

            // Count all comments for lead PRs
            let non_owner_count = count_non_owner_comments(&pr_with_comments, None);

            // Check if there are new comments since last poll
            let has_new = {
                let tracker = state.comment_tracker.lock().await;
                tracker.has_new_comments(pr_number, non_owner_count)
            };

            if has_new {
                // Check cooldown before nudging
                let should_nudge = {
                    let tracker = state.pr_issue_tracker.lock().await;
                    tracker.should_nudge(pr_number, PrIssueType::ReviewComment)
                };

                // Update comment tracker regardless of cooldown
                {
                    let mut tracker = state.comment_tracker.lock().await;
                    tracker.record(pr_number, non_owner_count);
                }

                if should_nudge {
                    let lead_nudge_msg = format!(
                        "Your PR #{} ({}) has new review comments — please address feedback.",
                        pr_number,
                        truncate_str(title, 40)
                    );
                    debug!(
                        "Polling detected new review comments on lead PR #{}, nudging lead",
                        pr_number
                    );
                    effects.push(Effect::NudgeLead {
                        message: lead_nudge_msg,
                    });
                }
            } else {
                // No new comments, just update tracker
                let mut tracker = state.comment_tracker.lock().await;
                tracker.record(pr_number, non_owner_count);
            }

            continue; // Lead PR handled, move to next PR
        }

        // Only check coworker-owned PRs beyond this point
        let owner =
            match coworker_from_branch_with_map(head_ref, Some(&snap.worktree_branch_owners)) {
                Some(o) => o,
                None => continue, // Not a coworker PR
            };

        // Fetch PR details with comments on-demand
        let pr_with_comments = match fetch_pr_comments(pr_number).await {
            Ok(data) => data,
            Err(e) => {
                debug!("Failed to fetch comments for PR #{}: {}", pr_number, e);
                continue;
            }
        };

        // Count non-owner comments
        let non_owner_count = count_non_owner_comments(&pr_with_comments, Some(&owner));

        // Check if there are new comments since last poll
        let has_new = {
            let tracker = state.comment_tracker.lock().await;
            tracker.has_new_comments(pr_number, non_owner_count)
        };

        if !has_new {
            // Update tracker and continue
            let mut tracker = state.comment_tracker.lock().await;
            tracker.record(pr_number, non_owner_count);
            continue;
        }

        // New comments detected — check cooldown before nudging
        let should_nudge = {
            let tracker = state.pr_issue_tracker.lock().await;
            tracker.should_nudge(pr_number, PrIssueType::ReviewComment)
        };

        // Update comment tracker regardless of cooldown
        {
            let mut tracker = state.comment_tracker.lock().await;
            tracker.record(pr_number, non_owner_count);
        }

        if !should_nudge {
            debug!(
                "PR #{} has new comments but nudge is on cooldown",
                pr_number
            );
            continue;
        }

        let nudge_msg = format!(
            "Your PR #{} ({}) has new review comments — please address feedback.",
            pr_number,
            truncate_str(title, 40)
        );

        debug!(
            "Polling detected new review comments on PR #{}, nudging {} and creating task",
            pr_number, owner
        );

        // Extract all decision context from persistent state in one lock
        let pr_ctx = {
            let ps = state.persistent_state.lock().await;
            PrContext::from_persistent_state(&ps, pr_number)
        };

        // Decide action using handoff-aware decision function (preserves session
        // resume and idle-coworker handoff capabilities)
        let action = crate::rules::decide_pr_comment_action_with_handoff(
            &owner,
            "reviewer", // Generic actor since we don't know the specific commenter from polling
            active_coworkers,
            idle_coworkers,
            state.is_at_dev_limit(),
            pr_ctx.session_context.as_ref(),
            &nudge_msg,
        );

        effects.extend(comment_action_to_effects(
            action, pr_number, title, state, &pr_ctx,
        ));

        // If this is a lead/* branch, also nudge the lead so they see review feedback
        if is_lead_branch(head_ref) {
            let lead_nudge_msg = format!(
                "Your PR #{} ({}) has new review comments — please address feedback.",
                pr_number,
                truncate_str(title, 40)
            );
            effects.push(Effect::NudgeLead {
                message: lead_nudge_msg,
            });
        }
    }

    effects
}

/// Convert a comment notification `PrAction` into effects.
///
/// Similar to `pr_action_to_effects` but uses the comment-specific cooldown,
/// messages, and `called_in_review_feedback` channel message.
fn comment_action_to_effects(
    action: crate::rules::PrAction,
    pr_number: u64,
    title: &str,
    state: &DaemonState,
    ctx: &PrContext,
) -> Vec<Effect> {
    use crate::rules::PrAction;
    let issue_type = PrIssueType::ReviewComment;

    // Look up topic channel for this PR's task (falls back to main if not found)
    let channel = ctx.get_channel(pr_number);

    match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::NudgeCoworkerWithCallbacks {
                name: owner,
                message,
                session_id: None,
                on_success: vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            }]
        }
        PrAction::SpawnOwner { owner, message } => {
            // Pure decision: should we resume with saved session or fresh?
            let resume_mode = {
                let sessions = state.pr_break_sessions.read().unwrap();
                crate::rules::decide_pr_owner_resume_mode(&owner, &sessions)
            };
            let has_saved_session = matches!(
                resume_mode,
                crate::rules::PrOwnerResumeMode::WithSavedSession(_)
            );
            let session_mode = match resume_mode {
                crate::rules::PrOwnerResumeMode::WithSavedSession(sid) => {
                    crate::launch::SessionMode::ResumeSession(sid)
                }
                crate::rules::PrOwnerResumeMode::WithoutSavedSession => {
                    crate::launch::SessionMode::Resume
                }
            };
            let mut config = crate::launch::LaunchConfig::coworker(
                owner.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(message),
            );
            // Use Opus for review feedback responses (higher quality needed to understand feedback)
            config.model = "opus".to_string();

            let mut on_success = vec![
                Effect::BroadcastCoworkerUpdate {
                    name: owner.clone(),
                    status: "running".to_string(),
                    current_task: Some(format!("responding to feedback on PR #{}", pr_number)),
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: crate::daemon_messages::called_in_review_feedback(
                        &owner,
                        pr_number,
                        crate::config::get_personality(),
                    ),
                    channel: channel.clone(),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            add_task_assignment_to_on_success(&mut on_success, pr_number, &owner, ctx);

            if has_saved_session {
                on_success.push(Effect::ClearPrBreakSession {
                    name: owner.clone(),
                });
            }

            let on_failure = vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "PR #{} ({}) owned by {} - review comment: {} (call-in failed)",
                        pr_number,
                        truncate_str(title, 40),
                        owner,
                        get_issue_action(PrIssueType::ReviewComment)
                    ),
                    channel,
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            }]
        }
        PrAction::HandoffToCoworker {
            assignee,
            original_author,
            pr_number,
            branch,
            session_id,
            message,
        } => handoff_to_coworker_effects(
            &assignee,
            &original_author,
            pr_number,
            &branch,
            session_id,
            &message,
            "to address review feedback",
            title,
            issue_type,
            state,
            ctx,
        ),
        PrAction::PostToChannel { message } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message,
                    channel: channel.clone(),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::Skip { reason } => {
            debug!("Polling comment notification skipped: {}", reason);
            vec![]
        }
    }
}

/// Build effects for handing off a PR to a different coworker.
///
/// Shared helper that consolidates the HandoffToCoworker effect-building logic
/// used across `pr_action_to_effects`, `comment_action_to_effects`, and
/// `review_complete_action_to_effects`. The only variation is the `context_suffix`
/// that describes why the handoff is happening (e.g., "resuming their session for
/// full context" or "to address review feedback").
#[allow(clippy::too_many_arguments)]
fn handoff_to_coworker_effects(
    assignee: &str,
    original_author: &str,
    pr_number: u64,
    branch: &str,
    session_id: String,
    message: &str,
    context_suffix: &str,
    title: &str,
    issue_type: PrIssueType,
    state: &DaemonState,
    ctx: &PrContext,
) -> Vec<Effect> {
    // Look up topic channel for this PR's task (falls back to main if not found)
    let channel = ctx.get_channel(pr_number);

    let config = crate::launch::LaunchConfig::pr_handoff(
        assignee.to_string(),
        state.repo_name.clone(),
        session_id,
        pr_number,
        branch,
        original_author,
    );

    let mut on_success = vec![
        Effect::BroadcastCoworkerUpdate {
            name: assignee.to_string(),
            status: "running".to_string(),
            current_task: Some(format!("working on PR #{}", pr_number)),
        },
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "{} is taking over PR #{} from {} ({})",
                assignee, pr_number, original_author, context_suffix
            ),
            channel: channel.clone(),
        },
        Effect::RecordPrNudge {
            pr_number,
            issue_type,
        },
    ];

    add_task_assignment_to_on_success(&mut on_success, pr_number, assignee, ctx);

    let on_failure = vec![
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "Failed to hand off PR #{} ({}) to {} - {}",
                pr_number,
                truncate_str(title, 40),
                assignee,
                message
            ),
            channel,
        },
        Effect::RecordPrNudge {
            pr_number,
            issue_type,
        },
    ];

    vec![Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success,
        on_failure,
    }]
}

/// Collect effects for spawning reviewers for PRs that need code review.
///
/// Identifies PRs that need review (not drafts, old enough, no Claude review,
/// not already assigned) and returns effects to spawn reviewer coworkers.
/// Uses `SpawnCoworkerWithCallbacks` so that reviewer assignment and channel
/// messages only happen on successful spawn.
async fn collect_reviewer_effects(
    snap: &WorldSnapshot,
    state: &DaemonState,
    prs: &[serde_json::Value],
) -> Vec<Effect> {
    collect_reviewer_effects_with_source(
        Some(&snap.worktree_branch_owners),
        &snap.worktree_registry,
        state,
        prs,
        crate::github_state::AssignmentSource::PollingFallback,
    )
    .await
}

pub(crate) async fn collect_reviewer_effects_with_source(
    branch_owners_map: Option<&std::collections::HashMap<String, String>>,
    worktree_registry: &crate::worktree_registry::WorktreeRegistry,
    state: &DaemonState,
    prs: &[serde_json::Value],
    source: crate::github_state::AssignmentSource,
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();

    for pr in prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }

        // Skip draft PRs
        let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
        if is_draft {
            debug!("PR #{} is a draft, skipping auto-review", pr_number);
            continue;
        }

        // Check if PR is old enough (enforce review delay)
        if let Some(age_secs) = get_pr_age_secs(pr)
            && age_secs < PR_REVIEW_DELAY_SECS
        {
            debug!(
                "PR #{} is too new ({}s < {}s), skipping auto-review",
                pr_number, age_secs, PR_REVIEW_DELAY_SECS
            );
            continue;
        }

        // When polling, defer to webhooks if one recently handled this PR.
        // This prevents polling from spawning a duplicate reviewer when the
        // webhook path already queued a pending spawn for the same PR.
        if source == crate::github_state::AssignmentSource::PollingFallback {
            let ps = state.persistent_state.lock().await;
            if ps
                .github
                .webhook_recently_handled(pr_number, PR_REVIEW_DELAY_SECS as i64 * 2)
            {
                debug!(
                    "PR #{} was recently handled by webhook, polling defers",
                    pr_number
                );
                continue;
            }
        }

        // Check if PR already has a Claude review.
        if state.is_pr_reviewed(pr_number).await {
            debug!("PR #{} already has a Claude review", pr_number);

            // Clear the reviewer assignment now that the review is complete.
            // This allows the reviewer to be sent on break, freeing up coworker slots.
            // Previously we only cleared when the reviewer had shut down, but that left
            // idle reviewers stuck with assignments preventing break dispatch.
            {
                let mut ps = state.persistent_state.lock().await;
                if ps.github.is_assigned(pr_number) {
                    debug!(
                        "PR #{} review completed, freeing reviewer assignment",
                        pr_number
                    );
                    ps.github.remove_assignment(pr_number);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!("Failed to save daemon-state.json: {}", e);
                    }
                }
            }

            // Nudge the PR author — review is complete but PR is still open
            let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
            let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

            // Only nudge coworker-owned PRs (validates branch prefix against known names)
            if let Some(owner) = coworker_from_branch_with_map(head_ref, branch_owners_map) {
                let should_nudge = {
                    let tracker = state.pr_issue_tracker.lock().await;
                    tracker.should_nudge(pr_number, PrIssueType::ReviewComplete)
                };

                if should_nudge {
                    let nudge_msg = format!(
                        "Your PR #{} ({}) has a completed review — please address any feedback and merge if appropriate.",
                        pr_number,
                        truncate_str(title, 40)
                    );

                    let active_coworkers: Vec<String> = state
                        .coworkers
                        .list()
                        .iter()
                        .map(|c| c.name.clone())
                        .collect();
                    let busy_coworkers = state.get_all_busy_coworkers();
                    let idle_coworkers: Vec<String> = active_coworkers
                        .iter()
                        .filter(|c| !busy_coworkers.contains(*c))
                        .cloned()
                        .collect();

                    let action = crate::rules::decide_review_complete_action(
                        &owner,
                        &active_coworkers,
                        &idle_coworkers,
                        state.is_at_dev_limit(),
                        &nudge_msg,
                    );

                    let pr_ctx = {
                        let ps = state.persistent_state.lock().await;
                        PrContext::routing_only(&ps)
                    };

                    effects.extend(review_complete_action_to_effects(
                        action, pr_number, title, state, &pr_ctx,
                    ));
                }
            }

            continue;
        }

        // Check if already assigned for review.
        // Stale assignments are cleaned up by cleanup_expired_preserving() during
        // the PR poll cycle, so any remaining assignment here is still valid.
        {
            let ps = state.persistent_state.lock().await;
            if ps.github.is_assigned(pr_number) {
                if let Some(reviewer_name) = ps.github.get_reviewer(pr_number) {
                    debug!(
                        "PR #{} already assigned to active reviewer {}",
                        pr_number, reviewer_name
                    );
                } else {
                    debug!("PR #{} has assignment but no reviewer name", pr_number);
                }
                continue;
            }
        }

        // Skip orphaned PRs (PRs whose author has no active worktree or can't be determined).
        // These should not get auto-review spawned since the author can't address feedback.
        // The main PR loop already posts warnings for orphaned PRs with critical issues.
        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Check if this PR has an active worktree in the registry.
        // After daemon restart, worktree bindings may have changed (current_coworker
        // may differ from the PR branch owner), so we can't rely on matching the
        // owner name. Instead, check if a worktree exists for this PR's task.
        //
        // Try multiple strategies to find the worktree:
        // 1. Look up by PR number (if the PR was linked to a worktree via webhook)
        // 2. Look up by task ID extracted from PR title (most reliable for task-based PRs)
        // 3. Fall back to branch name lookup (for non-task PRs or legacy workflows)
        let worktree = worktree_registry
            .get_by_pr(pr_number)
            .or_else(|| {
                // Extract task ID from title and look up by task
                crate::tasks::extract_task_id_from_pr_title(title).and_then(|task_id| {
                    let task_id_str = task_id.to_string();
                    worktree_registry
                        .all_assignments()
                        .values()
                        .find(|a| a.task_id.as_ref() == Some(&task_id_str))
                })
            })
            .or_else(|| worktree_registry.get_by_branch(head_ref));

        let is_orphaned = match worktree {
            Some(assignment) if assignment.completed_at.is_none() => {
                // Has active worktree - not orphaned
                false
            }
            Some(_assignment) => {
                // Worktree exists but is completed.
                // IMPORTANT: If the PR is still open (which it is, since it's in the `prs` list),
                // the author can still address review feedback by pushing to the branch.
                // Therefore, completed worktrees with open PRs are NOT orphaned.
                //
                // Only mark as orphaned if the PR is merged/closed (which would exclude it
                // from the `prs` list in the first place).
                //
                // After daemon restart, if a task was marked complete but the PR is still
                // open awaiting review, polling reconciliation should spawn a reviewer.
                false
            }
            None => {
                // No worktree found - check if this is a lead PR
                // Lead PRs are never orphaned because the lead's main worktree is always
                // available to address review feedback, even if the PR's specific branch
                // doesn't have a dedicated worktree entry.
                if is_lead_branch(head_ref) {
                    debug!(
                        "PR #{} is a lead PR (branch: {}), not orphaned",
                        pr_number, head_ref
                    );
                    false
                } else if let Some(owner) =
                    coworker_from_branch_with_map(head_ref, branch_owners_map)
                {
                    // The branch identifies a coworker owner. Only treat as orphaned if
                    // the coworker is NOT currently running — an active coworker can always
                    // address review feedback regardless of whether a worktree is registered.
                    let is_active = state
                        .coworkers
                        .get(&owner)
                        .is_some_and(|cw| cw.status == crate::coworker::CoworkerStatus::Running);
                    if is_active {
                        debug!(
                            "PR #{} has no worktree for owner {} but coworker is active, not orphaned",
                            pr_number, owner
                        );
                        false
                    } else {
                        debug!(
                            "PR #{} is orphaned (no worktree found for owner {}, branch: {}, coworker not running), skipping auto-review",
                            pr_number, owner, head_ref
                        );
                        true
                    }
                } else if super::helpers::is_lead_authored_pr(pr, state.repo_owner.as_deref()) {
                    // PR is authored by the lead (repo owner) but doesn't follow lead/* naming.
                    // The lead can still address feedback from their main worktree.
                    debug!(
                        "PR #{} is authored by lead (branch: {}), not orphaned",
                        pr_number, head_ref
                    );
                    false
                } else {
                    debug!(
                        "PR #{} is orphaned (no determinable owner or worktree, branch: {}), skipping auto-review",
                        pr_number, head_ref
                    );
                    true
                }
            }
        };

        if is_orphaned {
            continue;
        }

        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        debug!(
            "Spawning isolated coworker to review PR #{}: {}",
            pr_number,
            truncate_str(title, 40)
        );

        // Check max coworkers limit before spawning
        if state.is_at_coworker_limit() {
            debug!(
                "Max coworkers limit ({}) reached, cannot spawn reviewer for PR #{}",
                state.max_coworkers, pr_number
            );
            continue;
        }

        let reviewer_name = match state.coworkers.next_available_name() {
            Some(name) => name,
            None => {
                warn!("No available coworker slots for reviewer");
                continue;
            }
        };

        // Compute worktree details for reviewer worktree
        let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);
        let wt_path = crate::paths::worktrees_dir_for_repo(&state.repo_name).join(&worktree_id);

        // reviewer() now takes the PR number and generates both the system prompt
        // (with merged reviewer.md instructions) and the launch prompt internally
        let mut config = crate::launch::LaunchConfig::reviewer(reviewer_name.clone(), pr_number);
        config.working_dir = Some(wt_path.clone());

        // Ensure the worktree exists BEFORE spawning (fixes effect ordering bug)
        // Extract channel routing data (async-safe)
        let pr_ctx = {
            let ps = state.persistent_state.lock().await;
            PrContext::routing_only(&ps)
        };

        // Look up topic channel for this PR's task (falls back to main if not found)
        let channel = pr_ctx.get_channel(pr_number);

        effects.push(Effect::EnsureWorktree {
            worktree_id: worktree_id.clone(),
            path: wt_path.clone(),
        });

        // Reserve the reviewer assignment BEFORE spawning to prevent race conditions.
        // If multiple events (webhooks, polling) trigger spawns for the same PR
        // before any spawn completes, they would all pass the is_assigned() check.
        // By assigning immediately, subsequent calls see the reservation and skip spawning.
        effects.push(Effect::AssignReviewer {
            pr_number,
            reviewer_name: reviewer_name.clone(),
            source,
            restart_count: 0,
            reviewer_session_id: None,
        });

        let on_success = vec![
            // Register the review worktree assignment
            Effect::RegisterWorktreeAssignment {
                assignment: crate::worktree_registry::WorktreeAssignment {
                    worktree_id: worktree_id.clone(),
                    branch_name: worktree_id.clone(), // Branch name matches worktree_id for review worktrees
                    task_id: None,                    // Reviewers are not tied to tasks
                    current_coworker: None,           // Will be set by BindCoworkerToWorktree
                    pr_number: Some(pr_number),
                    created_at: chrono::Utc::now(),
                    completed_at: None, // Will be set when PR is reviewed and merged
                },
            },
            // Bind the reviewer to the worktree
            Effect::BindCoworkerToWorktree {
                worktree_id: worktree_id.clone(),
                coworker: reviewer_name.clone(),
            },
            Effect::BroadcastCoworkerUpdate {
                name: reviewer_name.clone(),
                status: "running".to_string(),
                current_task: Some(format!("reviewing PR #{}", pr_number)),
            },
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: daemon_messages::called_in_reviewer(
                    &reviewer_name,
                    pr_number,
                    config::get_personality(),
                ),
                channel: channel.clone(),
            },
        ];

        let on_failure = vec![
            // Clean up the optimistic assignment we made before spawning
            Effect::RemoveReviewerAssignment { pr_number },
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "⚠️ Failed to spawn reviewer for PR #{} ({})",
                    pr_number,
                    truncate_str(title, 40),
                ),
                channel,
            },
        ];

        effects.push(Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure,
        });
    }

    effects
}

/// Convert a review-complete `PrAction` into effects.
///
/// Similar to `pr_action_to_effects` but uses `called_in_review_feedback`
/// for the spawn message instead of `called_in_pr_issue`.
fn review_complete_action_to_effects(
    action: crate::rules::PrAction,
    pr_number: u64,
    title: &str,
    state: &DaemonState,
    ctx: &PrContext,
) -> Vec<Effect> {
    use crate::rules::PrAction;
    let issue_type = PrIssueType::ReviewComplete;

    // Look up topic channel for this PR's task (falls back to main if not found)
    let channel = ctx.get_channel(pr_number);

    match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::NudgeCoworkerWithCallbacks {
                name: owner,
                message,
                session_id: None,
                on_success: vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            }]
        }
        PrAction::SpawnOwner { owner, message } => {
            // Pure decision: should we resume with saved session or fresh?
            let resume_mode = {
                let sessions = state.pr_break_sessions.read().unwrap();
                crate::rules::decide_pr_owner_resume_mode(&owner, &sessions)
            };
            let has_saved_session = matches!(
                resume_mode,
                crate::rules::PrOwnerResumeMode::WithSavedSession(_)
            );
            let session_mode = match resume_mode {
                crate::rules::PrOwnerResumeMode::WithSavedSession(sid) => {
                    crate::launch::SessionMode::ResumeSession(sid)
                }
                crate::rules::PrOwnerResumeMode::WithoutSavedSession => {
                    crate::launch::SessionMode::Resume
                }
            };
            let mut config = crate::launch::LaunchConfig::coworker(
                owner.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(message),
            );
            // Use Opus for review feedback responses (higher quality needed to understand feedback)
            config.model = "opus".to_string();

            let mut on_success = vec![
                Effect::BroadcastCoworkerUpdate {
                    name: owner.clone(),
                    status: "running".to_string(),
                    current_task: Some(format!("responding to feedback on PR #{}", pr_number)),
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: daemon_messages::called_in_review_feedback(
                        &owner,
                        pr_number,
                        config::get_personality(),
                    ),
                    channel: channel.clone(),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            add_task_assignment_to_on_success(&mut on_success, pr_number, &owner, ctx);

            if has_saved_session {
                on_success.push(Effect::ClearPrBreakSession {
                    name: owner.clone(),
                });
            }

            let on_failure = vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "PR #{} ({}) owned by {} - review complete: {} (call-in failed)",
                        pr_number,
                        truncate_str(title, 40),
                        owner,
                        get_issue_action(PrIssueType::ReviewComplete)
                    ),
                    channel,
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            }]
        }
        PrAction::HandoffToCoworker {
            assignee,
            original_author,
            pr_number,
            branch,
            session_id,
            message,
        } => handoff_to_coworker_effects(
            &assignee,
            &original_author,
            pr_number,
            &branch,
            session_id,
            &message,
            "to address review feedback",
            title,
            issue_type,
            state,
            ctx,
        ),
        PrAction::PostToChannel { message } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message,
                    channel: channel.clone(),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::Skip { reason } => {
            debug!("{}", reason);
            vec![]
        }
    }
}

/// Process pending webhook-triggered reviewer spawns whose delay has expired.
///
/// Drains ready entries from the persisted `pending_review_spawns` queue,
/// fetches each PR's current data, and returns effects for eligible spawns.
/// Unlike the previous `tokio::time::sleep` approach, these survive daemon restarts.
///
/// Returns effects to be executed by the caller (following the evaluate-execute pattern).
pub(super) async fn process_pending_review_spawns(
    snap: &WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    let mut all_effects = Vec::new();

    // Build branch → coworker map from the worktree registry for task-based branch lookup
    let branch_owners: std::collections::HashMap<String, String> = {
        let ps = state.persistent_state.lock().await;
        ps.worktree_registry
            .all_assignments()
            .iter()
            .filter_map(|(_, a)| {
                a.current_coworker
                    .as_ref()
                    .map(|coworker| (a.branch_name.clone(), coworker.clone()))
            })
            .collect()
    };

    // Drain ready spawns from persistent state
    let ready_prs = {
        let mut ps = state.persistent_state.lock().await;
        let ready = ps.github.drain_ready_review_spawns();
        if !ready.is_empty()
            && let Err(e) = ps.save_for_repo(&state.repo_name)
        {
            warn!("Failed to persist review spawn drain: {}", e);
        }
        ready
    };

    if ready_prs.is_empty() {
        return all_effects;
    }

    for pr_number in ready_prs {
        info!("Processing pending review spawn for PR #{}", pr_number);

        // Fetch this specific PR's data
        let output = match tokio::process::Command::new("gh")
            .args([
                "pr",
                "view",
                &pr_number.to_string(),
                "--json",
                "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state",
            ])
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => {
                warn!(
                    "Webhook: Failed to fetch PR #{} for review spawn: {}",
                    pr_number, e
                );
                continue;
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Webhook: gh pr view #{} failed: {}", pr_number, stderr);
            continue;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let pr: serde_json::Value = match serde_json::from_str(&stdout) {
            Ok(pr) => pr,
            Err(e) => {
                warn!("Webhook: Failed to parse PR #{} JSON: {}", pr_number, e);
                continue;
            }
        };

        // Check the PR is still open
        let pr_state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("");
        if pr_state != "OPEN" {
            debug!(
                "Webhook: PR #{} is no longer open (state={}), skipping review",
                pr_number, pr_state
            );
            continue;
        }

        // Reuse the existing spawn logic (handles draft check, assignment dedup, etc.)
        // Use Webhook source since this was triggered by a webhook event.
        let effects = collect_reviewer_effects_with_source(
            Some(&branch_owners),
            &snap.worktree_registry,
            state,
            &[pr],
            crate::github_state::AssignmentSource::Webhook,
        )
        .await;
        all_effects.extend(effects);
    }

    all_effects
}

/// Handle CI completion for reviewer spawn retry.
///
/// When CI passes on a PR, check if the PR needs a reviewer spawned.
/// This handles the case where the initial pending spawn (45s after PR opened)
/// was skipped for any reason (coworker limit, CI pending, etc.), and now that
/// CI is green, we should retry the spawn.
///
/// Triggered by webhook `ci_check_passed` events.
pub(super) async fn handle_ci_completion_for_review_spawn(
    state: &DaemonState,
    ci_check: &crate::webhook::CiCheckPassed,
) {
    // Extract PR number from target (format: "PR #123")
    let pr_number = match ci_check.target.strip_prefix("PR #") {
        Some(num_str) => match num_str.parse::<u64>() {
            Ok(num) => num,
            Err(_) => {
                debug!(
                    "CI check target '{}' is not a PR reference, skipping review spawn check",
                    ci_check.target
                );
                return;
            }
        },
        None => {
            // Not a PR (e.g., "main" branch) - no review needed
            debug!(
                "CI check target '{}' is not a PR, skipping review spawn check",
                ci_check.target
            );
            return;
        }
    };

    debug!(
        "CI passed for PR #{} - checking if reviewer spawn is needed",
        pr_number
    );

    // Build branch → coworker map for task-based branch lookup and get worktree registry
    let (branch_owners, worktree_registry) = {
        let ps = state.persistent_state.lock().await;
        let branch_owners: std::collections::HashMap<String, String> = ps
            .worktree_registry
            .all_assignments()
            .iter()
            .filter_map(|(_, a)| {
                a.current_coworker
                    .as_ref()
                    .map(|coworker| (a.branch_name.clone(), coworker.clone()))
            })
            .collect();
        (branch_owners, ps.worktree_registry.clone())
    };

    // Fetch PR data to check if it needs review
    let output = match tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state",
        ])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            warn!(
                "CI completion: Failed to fetch PR #{} for review spawn check: {}",
                pr_number, e
            );
            return;
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "CI completion: gh pr view #{} failed: {}",
            pr_number, stderr
        );
        return;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pr: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(pr) => pr,
        Err(e) => {
            warn!(
                "CI completion: Failed to parse PR #{} JSON: {}",
                pr_number, e
            );
            return;
        }
    };

    // Check if PR is still open
    let pr_state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("");
    if pr_state != "OPEN" {
        debug!(
            "CI completion: PR #{} is no longer open (state={}), skipping review spawn",
            pr_number, pr_state
        );
        return;
    }

    // Use the existing spawn logic (handles all conditions: draft, age, review status, assignment, etc.)
    // Use Webhook source since this was triggered by a webhook event (CI completion).
    let effects = collect_reviewer_effects_with_source(
        Some(&branch_owners),
        &worktree_registry,
        state,
        &[pr],
        crate::github_state::AssignmentSource::Webhook,
    )
    .await;

    if !effects.is_empty() {
        info!(
            "CI completion triggered reviewer spawn for PR #{} ({} effects)",
            pr_number,
            effects.len()
        );
        crate::daemon::effects::execute_effects(effects, state).await;
    } else {
        debug!(
            "CI completion: PR #{} does not need reviewer spawn (already assigned or reviewed)",
            pr_number
        );
    }
}

/// Uncached check for Claude review on a PR (makes GitHub API calls).
///
/// Fetches both reviews and comments in a single API call to reduce GitHub API usage.
pub(super) fn pr_has_claude_review_uncached(pr_number: u64) -> bool {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "reviews,comments",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let json: serde_json::Value = match serde_json::from_str(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to parse review JSON for PR #{}: {}", pr_number, e);
                    return false;
                }
            };

            // Check formal reviews
            if let Some(reviews) = json.get("reviews").and_then(|v| v.as_array()) {
                for review in reviews {
                    if let Some(body) = review.get("body").and_then(|b| b.as_str())
                        && text_contains_review_signature(body)
                    {
                        return true;
                    }
                }
            }

            // Check comments (where coworkers post their reviews)
            if let Some(comments) = json.get("comments").and_then(|v| v.as_array()) {
                for comment in comments {
                    if let Some(body) = comment.get("body").and_then(|b| b.as_str())
                        && text_contains_review_signature(body)
                    {
                        return true;
                    }
                }
            }

            false
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "Failed to fetch reviews/comments for PR #{}: {}",
                pr_number,
                stderr.trim()
            );
            false
        }
        Err(e) => {
            warn!("Failed to execute gh pr view for PR #{}: {}", pr_number, e);
            false
        }
    }
}
// Auto-nudge helpers for PR activity
// ============================================================================

/// Add an eyes reaction to a GitHub comment to indicate it was received.
///
/// Uses the GitHub Reactions API via `gh api` to add a 👀 reaction to the
/// comment that triggered a coworker nudge or spawn.
async fn add_eyes_reaction(repo_full_name: &str, comment_node: &crate::webhook::CommentNode) {
    let endpoint = match comment_node {
        crate::webhook::CommentNode::IssueComment(id) => {
            format!("/repos/{}/issues/comments/{}/reactions", repo_full_name, id)
        }
        crate::webhook::CommentNode::ReviewComment(id) => {
            format!("/repos/{}/pulls/comments/{}/reactions", repo_full_name, id)
        }
        crate::webhook::CommentNode::Review { .. } => {
            // GitHub API does not support reactions on pull request reviews
            // (only on issue comments and review comments).
            debug!("Skipping eyes reaction: GitHub API does not support reactions on reviews");
            return;
        }
    };

    let result = tokio::process::Command::new("gh")
        .args(["api", &endpoint, "-f", "content=eyes", "--silent"])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            debug!("Added eyes reaction to {}", endpoint);
        }
        Ok(output) => {
            debug!(
                "Failed to add eyes reaction to {}: {}",
                endpoint,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            debug!("Failed to run gh api for eyes reaction: {}", e);
        }
    }
}

/// Async version of `get_pr_owner_coworker` that doesn't block the Tokio runtime.
async fn get_pr_owner_coworker_async(pr_number: u64) -> Option<String> {
    let branch = get_pr_branch_async(pr_number).await?;
    coworker_from_branch(&branch)
}

/// Fetch the branch name (headRefName) for a PR using the GitHub CLI.
async fn get_pr_branch_async(pr_number: u64) -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "headRefName",
            "-q",
            ".headRefName",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Handle nudging a PR owner when a comment/review is posted on their PR.
///
/// This is called from the webhook event loop when a `PrActivity` is present.
/// It resolves the PR owner (from webhook data or async lookup), checks cooldowns,
/// and either nudges an active coworker or spawns an inactive one.
pub(super) async fn handle_pr_comment_nudge(
    state: &DaemonState,
    activity: crate::webhook::PrActivity,
) {
    let pr_number = activity.pr_number;

    // Check for lead/* branches first, before filtering by coworker ownership
    let branch = match activity.branch {
        Some(ref b) => Some(b.clone()),
        None => get_pr_branch_async(pr_number).await,
    };

    if let Some(ref branch) = branch
        && is_lead_branch(branch)
    {
        // Check cooldown before nudging
        {
            let tracker = state.pr_issue_tracker.lock().await;
            if !tracker.should_nudge(pr_number, PrIssueType::ReviewComment) {
                debug!(
                    "PR #{} review comment nudge on cooldown (lead PR), skipping",
                    pr_number
                );
                return;
            }
        }

        let lead_nudge_msg = format!(
            "Your PR #{} has new review comments — please address feedback.",
            pr_number
        );
        debug!(
            "Webhook detected review comment on lead PR #{}, nudging lead",
            pr_number
        );

        let effect = Effect::NudgeLead {
            message: lead_nudge_msg,
        };
        crate::daemon::effects::execute_effects(vec![effect], state).await;
        return;
    }

    // Only check coworker-owned PRs beyond this point
    let owner = match activity.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(mut owner) = owner else {
        debug!("PR #{} has no coworker owner, skipping nudge", pr_number);
        return;
    };

    // Check if this PR is linked to a task with an active owner.
    // If so, route the review feedback to the task owner instead of the PR owner.
    // This handles cases where a task was reassigned (e.g., via orphan recovery)
    // and the PR metadata still shows the original author.
    if let Some(task_id) = {
        let ps = state.persistent_state.lock().await;
        ps.github
            .pr_author_sessions
            .get(&pr_number)
            .and_then(|session| session.task_id.as_ref())
            .cloned()
    } {
        // Check if the task has an active owner in_progress
        if let Some(task) = crate::tasks::read_task(&task_id)
            && task.status == crate::tasks::TaskStatus::InProgress
            && let Some(task_owner) = task.owner
        {
            // Check if the task owner is active
            let task_owner_active = state
                .coworkers
                .list()
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(&task_owner));

            if task_owner_active {
                debug!(
                    "PR #{} linked to task !{} with active owner {} — routing review feedback to task owner instead of PR owner {}",
                    pr_number, task_id, task_owner, owner
                );
                owner = task_owner;
            }
        }
    }

    // Author posted a comment on their own PR — notify the reviewer
    // (e.g., author is asking a follow-up question about review feedback)
    if activity
        .owner_coworker
        .as_ref()
        .is_some_and(|o| o == &activity.actor)
    {
        debug!(
            "PR #{} comment is from author {} — checking for reviewer to notify",
            pr_number, activity.actor
        );

        // Look up the reviewer assignment from persistent state
        let reviewer_info = {
            let ps = state.persistent_state.lock().await;
            ps.github.pr_reviewers.get(&pr_number).cloned()
        };

        let Some(assignment) = reviewer_info else {
            debug!("PR #{} has no reviewer assignment, skipping", pr_number);
            return;
        };

        let reviewer_name = assignment.reviewer;
        let reviewer_session_id = assignment.reviewer_session_id;
        let nudge_msg = format!(
            "PR #{} author {} posted a follow-up comment. Please review and respond.",
            pr_number, activity.actor
        );

        // Check if the reviewer is currently active
        let is_active = state
            .coworkers
            .list()
            .iter()
            .any(|c| c.name == reviewer_name);

        let effects = if is_active {
            vec![Effect::NudgeCoworker {
                name: reviewer_name.clone(),
                message: nudge_msg,
                session_id: reviewer_session_id,
            }]
        } else if let Some(session_id) = reviewer_session_id {
            // Reviewer stopped — resume their session with the follow-up context
            let config = crate::launch::LaunchConfig::coworker(
                reviewer_name.clone(),
                state.repo_name.clone(),
                crate::launch::SessionMode::ResumeSession(session_id.clone()),
                Some(nudge_msg),
            );
            vec![Effect::ResumeCoworker {
                name: reviewer_name.clone(),
                session_id,
                config,
            }]
        } else {
            debug!(
                "PR #{} reviewer {} has no session ID and is inactive, cannot resume",
                pr_number, reviewer_name
            );
            return;
        };

        super::effects::execute_effects(effects, state).await;
        return;
    }

    // Check cooldown to avoid spamming
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, PrIssueType::ReviewComment) {
            debug!(
                "PR #{} review comment nudge on cooldown, skipping",
                pr_number
            );
            return;
        }
    }

    let nudge_msg = format!(
        "Your PR #{} has review feedback from {}. Please address it and merge if appropriate.",
        pr_number, activity.actor
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let pr_ctx = {
        let ps = state.persistent_state.lock().await;
        PrContext::from_persistent_state(&ps, pr_number)
    };

    // Decide action using pure decision function with handoff support
    let action = crate::rules::decide_pr_comment_action_with_handoff(
        &owner,
        &activity.actor,
        &active_coworkers,
        &idle_coworkers,
        state.is_at_dev_limit(),
        pr_ctx.session_context.as_ref(),
        &nudge_msg,
    );

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let is_actionable = !matches!(action, crate::rules::PrAction::Skip { .. });
    let mut effects = comment_action_to_effects(action, pr_number, "", state, &pr_ctx);

    // If this is a lead/* branch, also nudge the lead so they see review feedback
    if let Some(branch) = get_pr_branch_async(pr_number).await
        && is_lead_branch(&branch)
    {
        let lead_nudge_msg = format!(
            "Your PR #{} has review feedback from {}. Please address it and merge if appropriate.",
            pr_number, activity.actor
        );
        effects.push(Effect::NudgeLead {
            message: lead_nudge_msg,
        });
    }

    super::effects::execute_effects(effects, state).await;

    // Add eyes reaction to the comment to provide visual feedback that it was received
    if is_actionable
        && let (Some(ref node), Some(ref repo)) = (activity.comment_node, activity.repo_full_name)
    {
        add_eyes_reaction(repo, node).await;
    }
}

/// Handle a formal review state change (approved / changes_requested) from a webhook.
///
/// This provides immediate nudging when a reviewer submits a formal review,
/// instead of waiting for the next polling cycle to detect the state change.
/// The `PrIssueTracker` cooldown prevents duplicate nudges if polling also fires.
pub(super) async fn handle_webhook_review_state_change(
    state: &DaemonState,
    change: crate::webhook::PrReviewStateChange,
) {
    let pr_number = change.pr_number;
    let issue_type = match change.state {
        crate::webhook::ReviewState::Approved => PrIssueType::Approved,
        crate::webhook::ReviewState::ChangesRequested => PrIssueType::ChangesRequested,
    };

    // Check cooldown — polling may have already nudged for this issue
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, issue_type) {
            debug!(
                "PR #{} {} nudge on cooldown (already handled), skipping webhook nudge",
                pr_number, issue_type
            );
            return;
        }
    }

    // Resolve owner: use webhook data if available, otherwise look up async
    let owner = match change.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(owner) = owner else {
        debug!(
            "PR #{} has no coworker owner, skipping webhook {} nudge",
            pr_number, issue_type
        );
        return;
    };

    let nudge_msg = format!(
        "PR #{} — {}: {}",
        pr_number,
        issue_type,
        get_issue_action(issue_type)
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let pr_ctx = {
        let ps = state.persistent_state.lock().await;
        PrContext::from_persistent_state(&ps, pr_number)
    };

    let action = crate::rules::decide_pr_issue_action_with_handoff(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        state.is_at_dev_limit(),
        pr_ctx.session_context.as_ref(),
        &nudge_msg,
    );

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects = pr_action_to_effects(action, pr_number, "", issue_type, state, &pr_ctx);
    super::effects::execute_effects(effects, state).await;
}

/// Handle a CI check failure on a PR branch from a webhook.
///
/// This provides immediate nudging when CI fails on a PR, instead of waiting
/// for the next polling cycle. The `PrIssueTracker` cooldown prevents duplicate
/// nudges if polling also fires.
pub(super) async fn handle_webhook_ci_failure(
    state: &DaemonState,
    failure: crate::webhook::PrCiFailure,
) {
    let pr_number = failure.pr_number;

    // Check cooldown
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, PrIssueType::CiFailed) {
            debug!(
                "PR #{} CI failure nudge on cooldown, skipping webhook nudge",
                pr_number
            );
            return;
        }
    }

    // Resolve owner
    let owner = match failure.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(owner) = owner else {
        debug!(
            "PR #{} has no coworker owner, skipping webhook CI failure nudge",
            pr_number
        );
        return;
    };

    let nudge_msg = format!(
        "PR #{} — CI check '{}' failed: please investigate",
        pr_number, failure.check_name
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let pr_ctx = {
        let ps = state.persistent_state.lock().await;
        PrContext::from_persistent_state(&ps, pr_number)
    };

    let action = crate::rules::decide_pr_issue_action_with_handoff(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        state.is_at_dev_limit(),
        pr_ctx.session_context.as_ref(),
        &nudge_msg,
    );

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects =
        pr_action_to_effects(action, pr_number, "", PrIssueType::CiFailed, state, &pr_ctx);
    super::effects::execute_effects(effects, state).await;
}

/// Detect stale CI checks and collect re-run effects.
///
/// Examines `statusCheckRollup` for each PR to find stuck checks in two passes:
/// - **Pass 1**: IN_PROGRESS checks running > 4x typical duration.
/// - **Pass 2**: QUEUED/PENDING/WAITING checks that never started when all sibling checks
///   have completed (2x typical duration with a 30-minute minimum floor).
///
/// Returns effects to re-run the affected workflows. Uses historical check durations
/// from `CiCheckStats` to determine "typical" time.
async fn collect_stale_check_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
) -> Vec<Effect> {
    use chrono::Utc;

    // Get CI stats for duration comparisons
    let ci_stats = {
        let ps = state.persistent_state.lock().await;
        ps.ci_stats.clone()
    };

    collect_stale_check_effects_with_time(&ci_stats, prs, Utc::now())
}

/// Format PR status for RPC responses.
///
/// Matches the format used by `format_pr_status` in rpc.rs so that cached
/// PR data has the same shape as freshly-fetched data.
fn format_pr_status_for_rpc(pr: &serde_json::Value) -> String {
    let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
    if is_draft {
        return "draft".to_string();
    }

    let review_decision = pr
        .get("reviewDecision")
        .and_then(|r| r.as_str())
        .unwrap_or("");

    match review_decision {
        "APPROVED" => "approved".to_string(),
        "CHANGES_REQUESTED" => "changes requested".to_string(),
        "REVIEW_REQUIRED" => "awaiting review".to_string(),
        _ => "open".to_string(),
    }
}

/// Pure helper for `collect_stale_check_effects` that accepts a reference time.
///
/// This allows deterministic testing by passing a fixed timestamp.
fn collect_stale_check_effects_with_time(
    ci_stats: &crate::ci_stats::CiCheckStats,
    prs: &[serde_json::Value],
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Effect> {
    use crate::ci_stats::extract_run_id_from_url;
    use chrono::DateTime;

    let mut effects = Vec::new();

    for pr in prs {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) => n,
            None => continue,
        };

        let checks = match pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => continue,
        };

        // --- Pass 1: Detect IN_PROGRESS checks running too long (4x typical) ---
        for check in checks {
            let status = check.get("status").and_then(|s| s.as_str()).unwrap_or("");

            // Only consider checks that are in progress
            if status != "IN_PROGRESS" {
                continue;
            }

            let check_name = match check.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let started_at_str = match check.get("startedAt").and_then(|s| s.as_str()) {
                Some(s) => s,
                None => continue,
            };

            // Parse the started_at timestamp
            let started_at: DateTime<chrono::Utc> = match started_at_str.parse() {
                Ok(dt) => dt,
                Err(_) => continue,
            };

            // Calculate how long the check has been running
            let running_duration =
                now.signed_duration_since(started_at).num_seconds().max(0) as u64;

            // Check if it exceeds the stale threshold (4x typical)
            if !ci_stats.is_stale(check_name, running_duration) {
                continue;
            }

            // Extract run ID from the details URL
            let details_url = match check.get("detailsUrl").and_then(|u| u.as_str()) {
                Some(u) => u,
                None => continue,
            };

            let run_id = match extract_run_id_from_url(details_url) {
                Some(id) => id,
                None => continue,
            };

            // Check cooldown to prevent re-running the same workflow repeatedly
            if !ci_stats.can_rerun(run_id) {
                debug!(
                    "Skipping re-run of workflow {} for '{}' on PR #{} (on cooldown)",
                    run_id, check_name, pr_number
                );
                continue;
            }

            let typical_duration = ci_stats.typical_duration_or_default(check_name);
            info!(
                "Detected stale CI check '{}' on PR #{}: running {}s (typical: {}s, threshold: {}s)",
                check_name,
                pr_number,
                running_duration,
                typical_duration,
                (typical_duration as f64 * crate::ci_stats::STALE_THRESHOLD_MULTIPLIER) as u64
            );

            effects.push(Effect::RerunWorkflow {
                run_id,
                check_name: check_name.to_string(),
                pr_number,
            });
        }

        // --- Pass 2: Detect PENDING/QUEUED checks that never started ---
        // A check stuck in pending while all siblings completed indicates a
        // GitHub Actions scheduling failure. Use the earliest sibling startedAt
        // as a time reference (since pending checks lack their own startedAt).

        // Classify checks into pending vs non-pending
        let pending_checks: Vec<&serde_json::Value> = checks
            .iter()
            .filter(|c| {
                let status = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
                matches!(status, "QUEUED" | "PENDING" | "WAITING")
            })
            .collect();

        if pending_checks.is_empty() {
            continue;
        }

        // All non-pending checks must be completed
        let non_pending: Vec<&serde_json::Value> = checks
            .iter()
            .filter(|c| {
                let status = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
                !matches!(status, "QUEUED" | "PENDING" | "WAITING")
            })
            .collect();

        if non_pending.is_empty() {
            continue; // No sibling checks to compare against
        }

        let all_siblings_completed = non_pending.iter().all(|c| {
            let status = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
            status == "COMPLETED"
        });

        if !all_siblings_completed {
            continue; // Some siblings still running — not yet a clear signal
        }

        // Find earliest sibling startedAt as time reference
        let earliest_sibling_start: Option<DateTime<chrono::Utc>> = non_pending
            .iter()
            .filter_map(|c| {
                c.get("startedAt")
                    .and_then(|s| s.as_str())
                    .and_then(|s| s.parse::<DateTime<chrono::Utc>>().ok())
            })
            .min();

        let earliest_start = match earliest_sibling_start {
            Some(t) => t,
            None => continue, // Can't determine timing without sibling timestamps
        };

        let time_since_start = now
            .signed_duration_since(earliest_start)
            .num_seconds()
            .max(0) as u64;

        for check in &pending_checks {
            let check_name = match check.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => continue,
            };

            if !ci_stats.is_pending_stale(check_name, time_since_start) {
                continue;
            }

            let details_url = match check.get("detailsUrl").and_then(|u| u.as_str()) {
                Some(u) => u,
                None => continue,
            };

            let run_id = match extract_run_id_from_url(details_url) {
                Some(id) => id,
                None => continue,
            };

            if !ci_stats.can_rerun(run_id) {
                debug!(
                    "Skipping re-run of workflow {} for pending '{}' on PR #{} (on cooldown)",
                    run_id, check_name, pr_number
                );
                continue;
            }

            let typical_duration = ci_stats.typical_duration_or_default(check_name);
            let threshold =
                (typical_duration as f64 * crate::ci_stats::PENDING_STALE_MULTIPLIER) as u64;
            let effective_threshold = threshold.max(crate::ci_stats::MIN_PENDING_STALE_SECS);
            info!(
                "Detected stale PENDING check '{}' on PR #{}: pending {}s since siblings started (threshold: {}s)",
                check_name, pr_number, time_since_start, effective_threshold
            );

            effects.push(Effect::RerunWorkflow {
                run_id,
                check_name: check_name.to_string(),
                pr_number,
            });
        }
    }

    effects
}

/// Generate cleanup effects for recently merged PRs.
///
/// Uses the pre-computed `merged_pr_branches` map from WorldSnapshot to avoid I/O.
/// Reconciles orphaned PRs: creates tasks for PRs that are reviewed + CI green
/// but have no associated in_progress task.
///
/// This handles the case where a PR was opened under the old lifecycle (task completed
/// on PR open), leaving the PR orphaned with no one to merge it even after review + CI green.
///
/// A PR is considered orphaned if:
/// 1. It has a coworker or task branch prefix (e.g., "lexington/feature" or "task-123-fix")
/// 2. It has a Claude review comment (in `reviewed_prs`)
/// 3. All CI checks are passing (`all_ci_checks_passed`)
/// 4. There's no in_progress task linked to it (not in `tasks_with_open_prs`)
///
/// For each orphaned PR, creates a task: "Merge PR #X — reviewed, CI green"
/// Normal task dispatch picks it up from there.
///
/// This is the PR equivalent of orphan task recovery. Pure decision function that
/// returns effects, following the same pattern as `reconcile_tasks_in_review()`.
pub fn reconcile_orphaned_prs(snap: &WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Iterate over open PRs from the snapshot (pre-collected during collect_world_snapshot)
    for pr in &snap.open_prs_data {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) => n,
            None => continue,
        };

        // Only consider PRs with coworker or task branch prefixes
        let branch = match pr.get("headRefName").and_then(|r| r.as_str()) {
            Some(b) => b,
            None => continue,
        };

        // Check if it's a coworker branch or task branch
        let has_valid_prefix = coworker_from_branch(branch).is_some()
            || branch.starts_with("task-")
            || is_lead_branch(branch);

        if !has_valid_prefix {
            continue;
        }

        // Skip if there's already an in_progress task linked to this PR
        if snap.pr_task_associations.contains_key(&pr_number) {
            continue;
        }

        // Skip if an active merge task already exists for this PR (prevents duplicates)
        // Only check pending/in_progress tasks — completed tasks shouldn't block reconciliation
        // (in case a task was mistakenly completed before the PR actually merged)
        if snap.all_tasks.iter().any(|task| {
            task.pr == Some(pr_number)
                && !matches!(task.status, crate::tasks::TaskStatus::Completed)
        }) {
            continue;
        }

        // Check if PR has been reviewed (Claude review comment exists)
        if !snap.reviewed_prs.contains(&pr_number) {
            continue;
        }

        // Check if all CI checks are passing
        if !all_ci_checks_passed(pr) {
            continue;
        }

        // Skip draft PRs
        if pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false) {
            continue;
        }

        // This PR is orphaned: reviewed + CI green but no active task
        let title = pr
            .get("title")
            .and_then(|t| t.as_str())
            .unwrap_or("(no title)");

        debug!(
            "Found orphaned PR #{} ({}) - reviewed, CI green, no active task",
            pr_number, title
        );

        // Create a task to handle merging this PR
        effects.push(Effect::CreateTask {
            repo_name: snap.repo_name.clone(),
            subject: format!("Merge PR #{} — reviewed, CI green", pr_number),
            description: format!(
                "PR #{} ({}) has been reviewed and has passing CI, but the original task was \
                 completed before the new lifecycle. Review the PR and merge if appropriate.\n\n\
                 Branch: {}",
                pr_number, title, branch
            ),
            pr: Some(pr_number),
        });
    }

    effects
}

/// Generates CleanupMergedWorktree effects to remove the worktree directory and
/// registry entry after the PR is merged.
///
/// Called during polling ticks to clean up task-based worktrees after
/// their PRs are merged.
pub fn collect_merged_pr_cleanup_effects(snap: &WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Use pre-computed PR → branch mapping from snapshot
    for &pr_num in &snap.merged_pr_numbers {
        if let Some(branch) = snap.merged_pr_branches.get(&pr_num) {
            debug!(
                "PR #{} merged, scheduling worktree cleanup for branch {}",
                pr_num, branch
            );

            // Build a descriptive channel message with task ID when available
            let assignment = snap.worktree_registry.get_by_pr(pr_num);
            let message = if let Some(task_id) = assignment.and_then(|a| a.task_id.as_deref()) {
                format!(
                    "🧹 Cleaned up worktree for PR #{} (task !{})",
                    pr_num, task_id
                )
            } else {
                format!("🧹 Cleaned up worktree for PR #{}", pr_num)
            };

            effects.push(Effect::CleanupMergedWorktree {
                pr_number: pr_num,
                branch: branch.clone(),
            });
            effects.push(Effect::PostSystemMessage { message });
        }
    }

    effects
}

#[path = "pr_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "pr_review_feedback_tests.rs"]
#[cfg(test)]
mod review_feedback_tests;

#[path = "pr_ci_retry_tests.rs"]
#[cfg(test)]
mod ci_retry_tests;
