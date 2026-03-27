//! PR management — polling, reviewer spawning, comment nudging.
//!
//! This module runs in the background to:
//! - Poll open PRs for merge conflicts, CI failures, and review status
//! - Nudge PR authors when approved (author-driven merge decisions)
//! - Spawn reviewer coworkers for unreviewed PRs (via polling backstop)
//! - Nudge PR owners when their PR receives comments

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use super::DaemonState;
use super::constants::*;
use super::effects::Effect;
use super::helpers::is_lead_branch;
use super::helpers::*;
use super::trackers::{PrIssueType, StuckConditionType};

/// Resolve a PR's owner via the session-centric path:
/// PR number → task_id → session_id → session.name.
///
/// Returns `Some(name)` if a session record exists with a non-empty name,
/// or `None` if any link in the chain is missing (no task association, no session,
/// or session has an empty name).
///
/// This gives session-based routing priority over branch-based lookup. When a
/// coworker is reassigned to a different name on restart, the session record
/// tracks the current name, so PRs route to the correct coworker.
fn resolve_pr_owner_from_session(
    pr_number: u64,
    pr_task_associations: &HashMap<u64, String>,
    session_task_map: &HashMap<String, String>,
    sessions: &HashMap<String, super::state::SessionRecord>,
) -> Option<String> {
    let task_id = pr_task_associations.get(&pr_number)?;
    let session_id = session_task_map.get(task_id)?;
    let session = sessions.get(session_id)?;
    if session.name.is_empty() {
        None
    } else {
        Some(session.name.clone())
    }
}

/// Build task_id → channel mapping from TaskStore.
fn task_channel_map_from_store(
    task_store: &crate::task_store::TaskStore,
) -> HashMap<String, String> {
    task_store
        .load_all()
        .into_iter()
        .filter_map(|t| t.channel.map(|ch| (t.id, ch)))
        .collect()
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
    /// Whether this PR has an active reviewer (assigned or in reviewing phase).
    /// Used to suppress both `PrApproved` workflow events AND inline nudge effects
    /// while a reviewer is still working, so the contract remains:
    /// "pr.approved = safe to merge".
    has_active_reviewer: bool,
    /// Channel→workflow assignments for checking if a channel has a workflow.
    channel_workflows: HashMap<String, String>,
    /// Channels operating in lead-driven mode. When set, the daemon relays
    /// events as @mentions to the channel lead instead of executing built-in
    /// behavior (auto-dispatch, reviewer spawning, PR nudges).
    lead_driven_channels: std::collections::HashSet<String>,
}

impl PrContext {
    /// Extract all PR decision context from persistent state for a given PR.
    ///
    /// Caller must hold `persistent_state.lock().await`. This method reads
    /// channel routing data (shared across all PRs) and session context
    /// (specific to `pr_number`) in a single pass.
    fn from_persistent_state(
        ps: &super::state::DaemonPersistentState,
        pr_number: u64,
        task_channel: HashMap<String, String>,
    ) -> Self {
        let pr_task_associations = super::state::pr_to_task_map_from_sessions(&ps.sessions);

        // Gate check: active reviewer span exists for this PR.
        //
        // Bypass: if the review is already cached (complete), don't suppress
        // PrApproved even if the span hasn't been closed yet. This handles
        // the race between webhook review completion and span closure.
        let has_active_reviewer = ps.active_reviewer_for_pr(pr_number).is_some()
            && !ps.github.has_cached_review(pr_number);

        Self {
            pr_task_associations,
            task_channel,
            has_active_reviewer,
            channel_workflows: ps.channel_workflows.clone(),
            lead_driven_channels: ps.lead_driven_channels.clone(),
        }
    }

    /// Extract only channel routing data (when session context isn't needed).
    ///
    /// Note: `has_active_reviewer` defaults to `false` because this constructor
    /// is only used for `ReviewComplete` contexts where the reviewer has already
    /// finished. Do NOT use this for `PrIssueType::Approved` code paths.
    fn routing_only(
        ps: &super::state::DaemonPersistentState,
        task_channel: HashMap<String, String>,
    ) -> Self {
        Self {
            pr_task_associations: super::state::pr_to_task_map_from_sessions(&ps.sessions),
            task_channel,
            has_active_reviewer: false,
            channel_workflows: ps.channel_workflows.clone(),
            lead_driven_channels: ps.lead_driven_channels.clone(),
        }
    }

    /// Look up the topic channel for a PR based on its associated task.
    fn get_channel(&self, pr_number: u64) -> Option<String> {
        let task_id = self.pr_task_associations.get(&pr_number)?;
        self.task_channel.get(task_id).cloned()
    }

    /// Returns true if the PR's task channel is in lead-driven mode.
    fn is_lead_driven(&self, pr_number: u64) -> bool {
        self.get_channel(pr_number)
            .is_some_and(|ch| self.lead_driven_channels.contains(&ch))
    }
}

/// Get coworker names that have sessions with open PRs.
///
/// Derived from `SessionRecord.pr_number` cross-referenced with `tick_open_prs`.
fn sessions_with_open_prs(ps: &super::state::DaemonPersistentState) -> HashSet<String> {
    let open_pr_numbers: HashSet<u64> = ps
        .tick_open_prs
        .iter()
        .filter_map(|pr| pr["number"].as_u64())
        .collect();

    ps.sessions
        .values()
        .filter(|s| s.pr_number.is_some_and(|pr| open_pr_numbers.contains(&pr)))
        .filter_map(|s| {
            if s.name.is_empty() {
                None
            } else {
                Some(s.name.clone())
            }
        })
        .collect()
}

/// Get coworker names that have sessions with recently merged PRs.
///
/// Derived from `SessionRecord.pr_number` cross-referenced with `tick_merged_pr_numbers`.
fn sessions_with_merged_prs(ps: &super::state::DaemonPersistentState) -> HashSet<String> {
    ps.sessions
        .values()
        .filter(|s| {
            s.pr_number
                .is_some_and(|pr| ps.tick_merged_pr_numbers.contains(&pr))
        })
        .filter_map(|s| {
            if s.name.is_empty() {
                None
            } else {
                Some(s.name.clone())
            }
        })
        .collect()
}

/// How often to re-fetch merged PRs (5 minutes). Merges aren't urgent so
/// polling less frequently saves significant API calls.
const MERGED_PRS_FETCH_INTERVAL_SECS: u64 = 300;

/// Fetch recently merged PRs from GitHub and cache the raw data.
///
/// Uses a time-based cooldown to reduce API calls. Merged PR status is only refreshed
/// every 5 minutes since merge events aren't time-critical.
///
/// Returns `(merged_pr_numbers, merged_prs_data)` — raw GitHub data without
/// coworker ownership derivation. Ownership is resolved at snapshot time via
/// `SessionRecord.pr_number`.
pub(super) fn fetch_merged_pr_data(state: &DaemonState) -> (HashSet<u64>, Vec<serde_json::Value>) {
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
        let cache = state.pr_poll_data.read().unwrap();
        return (
            cache.merged_pr_numbers.clone(),
            cache.merged_prs_data.clone(),
        );
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

    let (pr_numbers, merged_prs_data): (HashSet<u64>, Vec<serde_json::Value>) = match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                let numbers: HashSet<u64> = prs
                    .iter()
                    .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
                    .collect();
                (numbers, prs)
            } else {
                (HashSet::new(), Vec::new())
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to get merged PRs from gh CLI: {}", stderr.trim());
            (HashSet::new(), Vec::new())
        }
        Err(e) => {
            warn!("Failed to execute gh pr list (merged): {}", e);
            (HashSet::new(), Vec::new())
        }
    };

    // Update cache
    {
        let mut cache = state.pr_poll_data.write().unwrap();
        cache.merged_pr_numbers = pr_numbers.clone();
        cache.merged_prs_data = merged_prs_data.clone();
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("merged_pr_fetch", "global");
    }

    (pr_numbers, merged_prs_data)
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
fn compute_time_aware_hash_at(data: &str, bucket_secs: u64, timestamp_secs: u64) -> u64 {
    let time_bucket = timestamp_secs / bucket_secs;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    time_bucket.hash(&mut hasher);
    hasher.finish()
}

// NOTE: build_pr_opened_author_warning_effect was removed — the auto-merge
// warning is now sent by the workflow script's pr.opened handler (policy).
// See sdk/python/midtown/default_workflow.py.

/// Detect tasks linked to abandoned PRs (closed without merge) and return reset effects.
///
/// Pure decision function that takes snapshot data and returns effects for tasks
/// whose PRs were closed without merging. Merged PRs are handled separately by
/// build_task_completion_effects. Only resets tasks that are still in_progress.
///
/// Called from `poll_prs_for_issues` after fetching open PR list from GitHub.
fn detect_abandoned_pr_tasks(
    tick: &PrPollTickState,
    tasks: &[crate::task_store::Task],
    open_pr_numbers: &[u64],
    dir_key: &str,
) -> Vec<Effect> {
    let open_set: HashSet<u64> = open_pr_numbers.iter().copied().collect();
    let mut effects = Vec::new();

    // Check each PR with an associated task ID
    for (pr_number, task_id) in tick.pr_task_index.pr_task_pairs() {
        // PR is closed if it's not in the open set and wasn't merged
        let is_closed = !open_set.contains(&pr_number);
        let is_merged = tick.merged_pr_numbers.contains(&pr_number);

        if is_closed && !is_merged {
            // Check if the task is still in_progress (not already completed)
            let is_in_progress = tick
                .in_progress_tasks
                .iter()
                .any(|(tid, _, _)| tid == task_id);

            if is_in_progress {
                // Before resetting, check if the work was already completed by a DIFFERENT PR.
                // This prevents resetting tasks when a duplicate PR is closed but a sibling
                // PR for the same task was already merged.
                let work_already_landed = {
                    // Find the task once and reuse it
                    let task = tasks.iter().find(|t| t.id == task_id);

                    // Check if task status is completed
                    let task_completed = task
                        .map(|t| matches!(t.status, crate::task_store::TaskStatus::Completed))
                        .unwrap_or(false);

                    // Check if any other PR associated with this task was merged
                    let has_merged_sibling =
                        tick.pr_task_index
                            .pr_task_pairs()
                            .any(|(other_pr, other_task_id)| {
                                other_task_id == task_id
                                    && other_pr != pr_number
                                    && tick.merged_pr_numbers.contains(&other_pr)
                            });

                    // Check if task.pr field points to a merged PR
                    let task_pr_merged = task
                        .and_then(|t| t.pr)
                        .map(|pr| tick.merged_pr_numbers.contains(&pr))
                        .unwrap_or(false);

                    task_completed || has_merged_sibling || task_pr_merged
                };

                if !work_already_landed {
                    effects.push(Effect::ResetAbandonedTask {
                        task_id: task_id.to_string(),
                        pr_number,
                        dir_key: dir_key.to_string(),
                    });
                }
            }
        }
    }

    effects
}

/// Detect review tasks for PRs that have closed and auto-complete them.
///
/// Unlike `detect_abandoned_pr_tasks` (which relies on `pr_task_index` built from
/// session records), this scans the task store directly. Review tasks that are
/// still Pending (never spawned a session) won't appear in `pr_task_index`,
/// so they'd never be cleaned up when their PR closes — causing an infinite
/// respawn loop (!2511).
fn detect_abandoned_review_tasks(
    tasks: &[crate::task_store::Task],
    open_pr_numbers: &[u64],
    dir_key: &str,
) -> Vec<Effect> {
    let open_set: HashSet<u64> = open_pr_numbers.iter().copied().collect();
    let mut effects = Vec::new();

    for task in tasks {
        // Only review tasks
        if task.agent_type != "midtown-code-reviewer" {
            continue;
        }
        // Only tasks with a PR association
        let Some(pr_number) = task.pr else {
            continue;
        };
        // Only pending or in-progress (don't re-complete already-completed tasks)
        if matches!(task.status, crate::task_store::TaskStatus::Completed) {
            continue;
        }
        // PR is closed
        if !open_set.contains(&pr_number) {
            effects.push(Effect::CompleteTask {
                task_id: task.id.clone(),
                dir_key: dir_key.to_string(),
            });
        }
    }

    effects
}

/// Resolve the owner of a PR from snapshot data.
///
/// Uses session-based resolution only: PR# → task → session → name.
/// Returns `None` if no session owns the PR.
fn resolve_pr_owner(pf: &PrFields<'_>, tick: &PrPollTickState) -> Option<String> {
    resolve_pr_owner_from_session(
        pf.number,
        tick.pr_task_index.pr_to_task_map(),
        &tick.session_task_map,
        &tick.sessions,
    )
}

/// Resolve PR owner from persistent state (used by webhook handlers).
///
/// Locks persistent_state once, uses session-based resolution only.
async fn resolve_pr_owner_from_state(state: &DaemonState, pr_number: u64) -> Option<String> {
    let ps = state.persistent_state.lock().await;
    let pr_task_associations = super::state::pr_to_task_map_from_sessions(&ps.sessions);

    let session_task_map: HashMap<String, String> = ps
        .sessions
        .iter()
        .filter(|(_, record)| !record.is_fork_session())
        .filter_map(|(session_id, record)| {
            record
                .task_id
                .as_ref()
                .map(|task_id| (task_id.clone(), session_id.clone()))
        })
        .collect();

    resolve_pr_owner_from_session(
        pr_number,
        &pr_task_associations,
        &session_task_map,
        &ps.sessions,
    )
}

// ============================================================================

/// Collect warning effects for an orphaned PR with critical issues.
///
/// Called for two cases:
/// - Owner known but has no active worktree (`owner = Some(name)`)
/// - Owner completely unresolvable (`owner = None`)
///
/// Only emits effects for `MergeConflict` and `CiFailed` — skips approval/review
/// workflow issues that require active ownership to resolve.
async fn collect_orphaned_pr_effects(
    pr_number: u64,
    title: &str,
    head_ref: &str,
    owner: Option<&str>,
    issues: &[PrIssueType],
    state: &DaemonState,
) -> Vec<Effect> {
    let owner_desc = match owner {
        Some(o) => format!("owner: {}, branch: {}", o, head_ref),
        None => format!("no owner, branch: {}", head_ref),
    };
    let mut effects = Vec::new();
    for issue_type in issues {
        match issue_type {
            PrIssueType::MergeConflict | PrIssueType::CiFailed => {
                let should_nudge = {
                    let tracker = state.pr_issue_tracker.lock().await;
                    tracker.should_nudge_with_cooldown(
                        pr_number,
                        *issue_type,
                        ORPHANED_PR_NUDGE_COOLDOWN_SECS,
                    )
                };
                if should_nudge {
                    let warning = format!(
                        "@ops Orphaned PR #{} ({}) - {}: {} ({})",
                        pr_number,
                        truncate_str(title, 40),
                        issue_type,
                        get_issue_action(*issue_type),
                        owner_desc
                    );
                    effects.push(Effect::PostSystemMessage {
                        message: format!("⚠️ {}", warning),
                        channel: Some(OPS_CHANNEL.to_string()),
                    });
                    effects.push(Effect::RecordPrNudge {
                        pr_number,
                        issue_type: *issue_type,
                    });
                }
            }
            _ => {}
        }
    }
    effects
}

/// Cleanup expired tracking entries and stale state.
///
/// Cleans up: PR issue tracker, persistent state (stale webhook events),
/// cooldowns, and RPC response cache.
async fn cleanup_pr_tracking_state(state: &DaemonState) {
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.cleanup();
    }
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_stale_webhook_events();
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.cleanup(Duration::from_secs(7200)); // 2 hours
    }
    state.cleanup_rpc_response_cache().await;
}

/// Update PR-related caches and detect abandoned PRs.
///
/// Updates: open PR owner cache, formatted PR data for RPC, CI-passed owner cache,
/// PR break sessions. Also detects abandoned PRs (closed without merge) and cleans
/// up persistent reviewer assignments for closed PRs.
async fn update_pr_caches(
    state: &DaemonState,
    tick: &PrPollTickState,
    tasks: &[crate::task_store::Task],
    prs: &[serde_json::Value],
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Cache full open PR data for RPC responses (avoids gh CLI calls in handle_status).
    {
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
                let pf = PrFields::from_json(pr);
                let status = format_pr_status_for_rpc(pr);
                let task_id = crate::task_store::extract_task_id_from_pr_title(pf.title);
                let task_name = task_id.and_then(|id| task_map.get(&id).cloned());
                serde_json::json!({
                    "number": pf.number,
                    "title": pf.title,
                    "author": pf.author_login(),
                    "headRefName": pf.head_ref,
                    "isDraft": pf.is_draft,
                    "status": status,
                    "task_id": task_id,
                    "task_name": task_name,
                })
            })
            .collect();

        let mut cache = state.pr_poll_data.write().unwrap();
        cache.open_prs_data = formatted_prs;
        cache.pr_poll_initialized = true;
    }

    // Detect abandoned PRs (closed without merge) and reset associated tasks.
    // This uses pure decision logic that takes only snapshot data and returns effects.
    let open_pr_numbers: Vec<u64> = prs
        .iter()
        .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
        .collect();
    effects.extend(detect_abandoned_pr_tasks(
        tick,
        tasks,
        &open_pr_numbers,
        state.paths.dir_key(),
    ));

    // Auto-complete review tasks for closed PRs. This catches review tasks
    // that have no session record yet (Pending), which detect_abandoned_pr_tasks
    // misses because it only checks pr_task_index (session-derived).
    effects.extend(detect_abandoned_review_tasks(
        tasks,
        &open_pr_numbers,
        state.paths.dir_key(),
    ));

    // Clean up persistent reviewer assignments for PRs that are no longer open.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_closed_prs(&open_pr_numbers);
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!("Failed to save daemon-state.json after cleanup: {}", e);
        }
    }

    effects
}

#[allow(clippy::too_many_arguments)]
/// Shared pipeline for deciding and building effects for a single PR issue.
///
/// Both `process_pr_issue_nudges` and `collect_green_with_feedback_effects`
/// follow the same 7-step pipeline once they've identified a PR + issue to act on.
/// This helper encapsulates that shared logic: build message, get PrContext,
/// augment reviewer, decide action, convert to effects, and log.
///
/// `review_content` should be pre-fetched by the caller (no I/O here beyond
/// the mutex locks on persistent state).
async fn decide_and_build_pr_issue_effects(
    owner: &str,
    pr_number: u64,
    title: &str,
    issue_type: PrIssueType,
    review_content: Option<&str>,
    at_task_limit: bool,
    reviewer_pr_assignments: &HashMap<String, u64>,
    state: &DaemonState,
    active_coworkers: &[String],
    idle_coworkers: &[String],
) -> Vec<Effect> {
    use crate::rules::{PrActionContext, decide_pr_action};

    let message = format!(
        "PR #{} ({}) - {}: {}{}",
        pr_number,
        truncate_str(title, 40),
        issue_type,
        get_issue_action(issue_type),
        review_content.unwrap_or("")
    );

    // Extract all decision context from persistent state in one lock
    let task_channel_map: HashMap<String, String> = state
        .task_store
        .load_all()
        .into_iter()
        .filter_map(|t| t.channel.map(|ch| (t.id, ch)))
        .collect();
    let mut pr_ctx = {
        let ps = state.persistent_state.lock().await;
        PrContext::from_persistent_state(&ps, pr_number, task_channel_map)
    };

    // Defense-in-depth: check reviewer_pr_assignments from tick state.
    if !pr_ctx.has_active_reviewer {
        let has_assignment = reviewer_pr_assignments
            .iter()
            .any(|(_, &assigned_pr)| assigned_pr == pr_number);
        pr_ctx.has_active_reviewer = has_assignment;
    }

    // Decide action using handoff-aware decision function (matches webhook path)
    let action = decide_pr_action(
        owner,
        active_coworkers,
        idle_coworkers,
        at_task_limit,
        &message,
        PrActionContext::PrIssue,
    );

    let action_name = pr_action_name(&action);

    let new_effects = action_to_effects(action, pr_number, title, issue_type, state, &pr_ctx);

    log_pr_decision(&PrDecisionEntry {
        repo_name: state.paths.dir_key(),
        pr_number,
        title,
        owner,
        issue_type,
        action_name,
        effects: &new_effects,
        ctx: &pr_ctx,
        owner_is_active: active_coworkers.iter().any(|s| s == owner),
        owner_is_idle: idle_coworkers.iter().any(|s| s == owner),
        at_task_limit,
        source: "polling",
    });

    new_effects
}

#[allow(clippy::too_many_arguments)]
/// Shared single-issue pipeline gate: cooldown check + decision/effect build.
///
/// `process_pr_issue_nudges` and `collect_green_with_feedback_effects` both
/// run this same sequence once they identify an owner + issue for a PR.
async fn maybe_decide_pr_issue_effects(
    owner: &str,
    pf: &PrFields<'_>,
    issue_type: PrIssueType,
    review_content: Option<&str>,
    at_task_limit: bool,
    reviewer_pr_assignments: &HashMap<String, u64>,
    state: &DaemonState,
    active_coworkers: &[String],
    idle_coworkers: &[String],
) -> Vec<Effect> {
    let should_nudge = {
        let tracker = state.pr_issue_tracker.lock().await;
        tracker.should_nudge(pf.number, issue_type)
    };

    if !should_nudge {
        return Vec::new();
    }

    decide_and_build_pr_issue_effects(
        owner,
        pf.number,
        pf.title,
        issue_type,
        review_content,
        at_task_limit,
        reviewer_pr_assignments,
        state,
        active_coworkers,
        idle_coworkers,
    )
    .await
}

/// Process per-PR issue detection and generate nudge effects.
///
/// For each non-draft PR: resolves the owner, detects actionable issues (merge
/// conflicts, CI failures, review status), handles orphaned PRs, and generates
/// nudge effects using the author-driven merge decision model.
async fn process_pr_issue_nudges(
    tick: &PrPollTickState,
    state: &DaemonState,
    prs: &[serde_json::Value],
    active_coworkers: &[String],
    idle_coworkers: &[String],
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for pr in prs {
        let pf = PrFields::from_json(pr);

        if pf.is_draft {
            continue;
        }

        // Session-based resolution: PR# → task → session → name.
        let owner_opt = resolve_pr_owner(&pf, tick);
        let issues = detect_pr_issues(pr);

        // Handle PRs whose owner is not currently active (on break, never spawned, etc.)
        if let Some(ref owner) = owner_opt {
            let is_active = tick.active_session_names.contains(&owner.to_lowercase());

            if !is_active && !issues.is_empty() {
                effects.extend(
                    collect_orphaned_pr_effects(
                        pf.number,
                        pf.title,
                        pf.head_ref,
                        Some(owner),
                        &issues,
                        state,
                    )
                    .await,
                );
                continue;
            }
        }

        // Handle PRs with no determinable owner that have critical issues
        if owner_opt.is_none() && !issues.is_empty() {
            effects.extend(
                collect_orphaned_pr_effects(pf.number, pf.title, pf.head_ref, None, &issues, state)
                    .await,
            );
            continue;
        }

        let owner = match owner_opt {
            Some(o) => o,
            None => continue,
        };

        for issue_type in issues {
            let review_content = match issue_type {
                PrIssueType::ChangesRequested | PrIssueType::Approved => {
                    fetch_review_content(pf.number).await
                }
                _ => None,
            };

            effects.extend(
                maybe_decide_pr_issue_effects(
                    &owner,
                    &pf,
                    issue_type,
                    review_content.as_deref(),
                    tick.is_at_task_limit,
                    &tick.reviewer_pr_assignments,
                    state,
                    active_coworkers,
                    idle_coworkers,
                )
                .await,
            );
        }
    }

    effects
}

/// Update review status caches after processing PR issues.
///
/// Computes prs_needing_review count and caches it in PrPollData.
fn update_review_status_cache(
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
) {
    let prs_needing_review: usize = prs
        .iter()
        .filter(|pr| {
            let pf = PrFields::from_json(pr);
            pf.number != 0
                && !pf.is_draft
                && pf.review_decision().is_empty()
                && !reviewed_prs.contains(&pf.number)
        })
        .count();

    let mut cache = state.pr_poll_data.write().unwrap();
    cache.prs_needing_review = prs_needing_review;
}

/// Detect external/fork PRs from polling data and record them in persistent state.
///
/// Compares each PR's `headRepositoryOwner` against the base repo owner.
/// For newly detected external PRs, generates a channel notification effect
/// directed at the user (not agents).
async fn detect_and_block_external_prs(
    state: &DaemonState,
    repo_owner: Option<&str>,
    default_channel: &str,
    prs: &[serde_json::Value],
) -> Vec<Effect> {
    let mut effects = Vec::new();
    let repo_owner = match repo_owner {
        Some(owner) => owner,
        None => return effects, // Can't detect forks without knowing our repo owner
    };

    let mut ps = state.persistent_state.lock().await;
    for pr in prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }

        // headRepositoryOwner is an object with a "login" field
        let head_owner = pr
            .get("headRepositoryOwner")
            .and_then(|o| o.get("login"))
            .and_then(|l| l.as_str());

        let head_owner = match head_owner {
            Some(owner) => owner,
            None => continue, // Field missing — assume same-repo PR
        };

        if head_owner.eq_ignore_ascii_case(repo_owner) {
            continue; // Same owner — not a fork
        }

        let title = pr.get("title").and_then(|t| t.as_str()).unwrap_or("");
        // gh pr list only gives us the head owner login, not the full repo name.
        // Use "owner/fork" as a placeholder — the webhook path has the real full_name.
        let source_repo = format!("{}/fork", head_owner);

        let is_new = ps.github.record_external_pr(pr_number, &source_repo, title);

        // Only notify if newly detected AND not already allowed
        if is_new && !ps.github.is_blocked_external_pr(pr_number) {
            // PR was just recorded but already allowed — no notification needed
            continue;
        }

        if is_new {
            info!(
                "Detected external PR #{} from fork '{}': {}",
                pr_number, source_repo, title
            );
            ps.github.mark_external_pr_notified(pr_number);

            effects.push(Effect::PostSystemMessage {
                message: format!(
                    "⚠️ PR #{} from fork `{}` is from an external repository. \
                     External PRs are not processed automatically. \
                     To allow it, run: `midtown pr allow {}`",
                    pr_number, source_repo, pr_number
                ),
                channel: if default_channel.is_empty() {
                    None
                } else {
                    Some(default_channel.to_string())
                },
            });
        }
    }

    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
        warn!("Failed to persist external PR state: {}", e);
    }

    effects
}

/// Poll all open PRs and return effects for actionable issues.
///
/// Fetches PR data from GitHub, reads tracker state to avoid duplicate nudges,
/// and returns a list of effects to execute. The caller is responsible for
/// executing the returned effects via `execute_effects()`.
///
/// Called from `evaluate_tick(PrPollTick)` in the main event loop.
/// Snapshot of tick fields needed by PR polling.
///
/// Extracted from `DaemonPersistentState` under a single lock, then passed
/// to sub-functions. This avoids holding the persistent_state lock while
/// calling async functions that also need to lock it.
#[derive(Default)]
pub(super) struct PrPollTickState {
    pub(super) active_coworkers: Vec<crate::coworker::Coworker>,
    pub(super) active_session_names: HashSet<String>,
    pub(super) is_at_task_limit: bool,
    pub(super) reviewer_pr_assignments: HashMap<String, u64>,
    pub(super) repo_owner: Option<String>,
    pub(super) default_channel: String,
    pub(super) pr_task_index: super::snapshot::PrTaskIndex,
    pub(super) session_task_map: HashMap<String, String>,
    pub(super) sessions: HashMap<String, super::state::SessionRecord>,
    pub(super) worktree_registry: crate::worktree_registry::WorktreeRegistry,
    pub(super) merged_pr_numbers: HashSet<u64>,
    pub(super) in_progress_tasks: Vec<(String, String, String)>,
}

impl PrPollTickState {
    fn from_persistent_state(ps: &super::state::DaemonPersistentState) -> Self {
        Self {
            active_coworkers: ps.tick_active_coworkers.clone(),
            active_session_names: ps.tick_active_session_names.clone(),
            is_at_task_limit: ps.tick_is_at_task_limit,
            reviewer_pr_assignments: ps.tick_reviewer_pr_assignments.clone(),
            repo_owner: ps.tick_repo_owner.clone(),
            default_channel: ps.tick_default_channel.clone(),
            pr_task_index: ps.tick_pr_task_index.clone(),
            session_task_map: ps.tick_session_task_map.clone(),
            sessions: ps.sessions.clone(),
            worktree_registry: ps.worktree_registry.clone(),
            merged_pr_numbers: ps.tick_merged_pr_numbers.clone(),
            in_progress_tasks: ps.tick_in_progress_tasks.clone(),
        }
    }
}

pub(super) async fn poll_prs_for_issues(
    state: &DaemonState,
) -> Result<Vec<Effect>, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Polling PRs for actionable issues...");

    let mut effects: Vec<Effect> = Vec::new();

    // Extract tick state under a single lock, then drop before async calls.
    let (tick, tasks) = {
        let ps = state.persistent_state.lock().await;
        let tasks = state.task_store.load_all();
        (PrPollTickState::from_persistent_state(&ps), tasks)
    };

    // Get list of active coworkers from tick state
    let active_coworkers: Vec<String> = tick
        .active_coworkers
        .iter()
        .map(|c| c.name.clone())
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
                        == Some(crate::workflow_phase::WorkflowPhase::Idle)
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
            "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state,author,body,headRepositoryOwner",
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

    cleanup_pr_tracking_state(state).await;

    // Filter to only open PRs (defense-in-depth: gh pr list --state open should only return
    // open PRs, but verify via the state field to guard against stale/cached results)
    let prs: Vec<serde_json::Value> = prs
        .into_iter()
        .filter(|pr| {
            let state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("OPEN");
            state == "OPEN"
        })
        .collect();

    // Detect and block external/fork PRs from daemon processing.
    // External PRs are detected by comparing headRepositoryOwner against the base repo owner.
    // Blocked PRs generate a one-time channel notification and are excluded from all
    // downstream processing (reviewer spawning, nudges, task linking, etc.).
    effects.extend(
        detect_and_block_external_prs(
            state,
            tick.repo_owner.as_deref(),
            &tick.default_channel,
            &prs,
        )
        .await,
    );

    // Collect ALL open PR numbers before filtering, so cleanup_closed_external_prs
    // sees the full set and doesn't purge still-open blocked external PRs.
    let all_open_pr_numbers: Vec<u64> = prs
        .iter()
        .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
        .collect();

    let prs: Vec<serde_json::Value> = {
        let ps_lock = state.persistent_state.lock().await;
        prs.into_iter()
            .filter(|pr| {
                let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                !ps_lock.github.is_blocked_external_pr(pr_number)
            })
            .collect()
    };

    effects.extend(update_pr_caches(state, &tick, &tasks, &prs).await);

    // Clean up external PR tracking for truly closed PRs, using the unfiltered
    // open PR list so blocked-but-still-open external PRs are preserved.
    {
        let mut ps_lock = state.persistent_state.lock().await;
        ps_lock
            .github
            .cleanup_closed_external_prs(&all_open_pr_numbers);
        if let Err(e) = ps_lock.save_for_repo(state.paths.dir_key()) {
            warn!(
                "Failed to save daemon-state.json after external PR cleanup: {}",
                e
            );
        }
    }

    effects.extend(
        process_pr_issue_nudges(&tick, state, &prs, &active_coworkers, &idle_coworkers).await,
    );

    // Polling fallback for review comment notifications (when webhooks are degraded)
    effects.extend(
        collect_comment_notification_effects(
            &tick,
            state,
            &prs,
            &active_coworkers,
            &idle_coworkers,
        )
        .await,
    );

    // Pre-collect review status for all PRs before decision functions (pure decision logic
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

    // Pre-fetch review content for all reviewed PRs. This keeps subprocess I/O here
    // (the polling entry point) instead of inside decision functions — CLAUDE.md:
    // "Decision functions are pure: must not perform I/O."
    let pre_fetched_review_content = pre_fetch_review_content_for_prs(&prs, &reviewed_prs).await;

    // Auto-spawn reviewers for PRs that need review
    effects.extend(collect_reviewer_effects(&tick, state, &prs, &pre_fetched_review_content).await);

    update_review_status_cache(state, &prs, &reviewed_prs);

    // Nudge PR owners when CI turns green and they have review feedback to address.
    // This covers the case where a coworker is waiting for CI while feedback awaits.
    effects.extend(
        collect_green_with_feedback_effects(
            &tick,
            state,
            &prs,
            &reviewed_prs,
            &active_coworkers,
            &idle_coworkers,
            &pre_fetched_review_content,
        )
        .await,
    );

    // Check for stuck conditions and nudge lead if self-healing has failed
    let review_mode = crate::config::get_review_mode_for_repo(state.paths.dir_key());
    effects.extend(
        collect_stuck_condition_effects(
            state,
            &prs,
            &reviewed_prs,
            review_mode,
            tick.is_at_task_limit,
        )
        .await,
    );

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
    tick: &PrPollTickState,
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    pre_fetched_review_content: &HashMap<u64, String>,
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
        let pf = PrFields::from_json(pr);
        if pf.review_decision() == "APPROVED" {
            continue;
        }

        // Only process coworker-owned PRs — session-first, branch fallback.
        let owner = match resolve_pr_owner(&pf, tick) {
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

        // Use pre-fetched review content (fetched at the top of poll_prs_for_issues
        // to keep this function free of I/O — CLAUDE.md: "Decision functions are pure").
        let review_content = pre_fetched_review_content
            .get(&pr_number)
            .map(|s| s.as_str());

        effects.extend(
            maybe_decide_pr_issue_effects(
                &owner,
                &pf,
                PrIssueType::GreenWithFeedback,
                review_content,
                tick.is_at_task_limit,
                &tick.reviewer_pr_assignments,
                state,
                active_coworkers,
                idle_coworkers,
            )
            .await,
        );
    }

    effects
}

/// Convert a `PrAction` decision into a list of `Effect`s to execute.
///
/// For the 5 PR lifecycle events with workflow script counterparts (approved,
/// changes_requested, ci_failed, ci_passed, conflict), the workflow script is
/// **authoritative** when a channel + task association exists AND a workflow
/// script is configured. Only cooldown tracking (`RecordPrNudge`) and the
/// workflow event are emitted — the script handles nudging via
/// `rpc.nudge_coworker()`. This makes PR lifecycle behavior fully customizable
/// through project or channel `workflow.py` overrides.
///
/// When no workflow script exists, the original inline effects fire alongside
/// the workflow event (preserving pre-script behavior). When a script is added,
/// inline effects are removed and the script takes over cleanly.
///
/// Build the workflow event for a PR issue type (if task-linked with a channel).
///
/// Returns the appropriate `WorkflowEvent` variant for the issue type, or `None`
/// for issue types without workflow event counterparts (ReviewComment).
/// `ReviewComplete` maps to `ReviewerComplete` when called from review-complete context.
fn build_workflow_event(
    issue_type: PrIssueType,
    channel: &Option<String>,
    pr_number: u64,
    ctx: &PrContext,
) -> Option<crate::workflow::WorkflowEvent> {
    let (channel_name, task_id) = match (channel, ctx.pr_task_associations.get(&pr_number)) {
        (Some(ch), Some(tid)) => (ch, tid),
        _ => return None,
    };

    match issue_type {
        PrIssueType::Approved => Some(crate::workflow::WorkflowEvent::PrApproved {
            channel: channel_name.clone(),
            task_id: task_id.clone(),
            pr_number,
        }),
        PrIssueType::ChangesRequested => Some(crate::workflow::WorkflowEvent::PrChangesRequested {
            channel: channel_name.clone(),
            task_id: task_id.clone(),
            pr_number,
        }),
        PrIssueType::MergeConflict => Some(crate::workflow::WorkflowEvent::PrConflict {
            channel: channel_name.clone(),
            task_id: task_id.clone(),
            pr_number,
        }),
        PrIssueType::CiFailed => Some(crate::workflow::WorkflowEvent::PrCiFailed {
            channel: channel_name.clone(),
            task_id: task_id.clone(),
            pr_number,
            check_name: None,
        }),
        PrIssueType::GreenWithFeedback => Some(crate::workflow::WorkflowEvent::PrCiPassed {
            channel: channel_name.clone(),
            task_id: task_id.clone(),
            pr_number,
        }),
        PrIssueType::ReviewComplete => Some(crate::workflow::WorkflowEvent::ReviewerComplete {
            channel: channel_name.clone(),
            task_id: task_id.clone(),
            pr_number,
        }),
        // These issue types don't have workflow event counterparts.
        PrIssueType::ReviewComment => None,
    }
}

/// Unified converter from `PrAction` → `Vec<Effect>`.
///
/// Replaces the former `pr_action_to_effects`, `comment_action_to_effects`, and
/// `review_complete_action_to_effects` with a single function. For task-linked PRs,
/// `NudgeOwner` and `SpawnOwner` are collapsed into `Effect::TaskPrompt` — the
/// `deliver_task_prompt` function handles nudge-if-running / resume-if-stopped
/// internally. For task-less PRs, `NudgeOwner` produces
/// `NudgeCoworker` and `SpawnOwner` produces
/// `SpawnCoworkerWithCallbacks` — only `PrAction::PostToChannel` maps to
/// `Effect::PostToChannel`.
///
/// Gates `PrApproved` events: when `ctx.has_active_reviewer` is true, both the
/// workflow event and inline effects are suppressed. The Approved cooldown is
/// cleared when the reviewer finishes (see `collect_reviewer_effects`),
/// allowing re-evaluation on the next tick. See !1902.
///
/// Cooldown tracking (`RecordPrNudge`) and workflow event emission are preserved
/// at call sites — `TaskPrompt` is a pure delivery mechanism.
fn action_to_effects(
    action: crate::rules::PrAction,
    pr_number: u64,
    title: &str,
    issue_type: PrIssueType,
    _state: &DaemonState,
    ctx: &PrContext,
) -> Vec<Effect> {
    use crate::rules::PrAction;

    // Look up topic channel for this PR's task (falls back to main if not found)
    let channel = ctx.get_channel(pr_number);
    let task_id = ctx.pr_task_associations.get(&pr_number);

    // Suppress PrApproved while a reviewer is still active.
    if issue_type == PrIssueType::Approved && ctx.has_active_reviewer {
        debug!(
            "PR #{}: suppressing PrApproved — reviewer still active",
            pr_number
        );
        return vec![];
    }

    let workflow_event = build_workflow_event(issue_type, &channel, pr_number, ctx);

    // When a workflow script or lead-driven mode exists AND we have a workflow
    // event, the script/lead is authoritative: emit only cooldown tracking +
    // the event. The script handles nudging via rpc.nudge_coworker(); in
    // lead-driven mode, the EmitWorkflowEvent handler relays to the channel lead.
    if let Some(ref event) = workflow_event {
        let has_workflow = channel
            .as_ref()
            .is_some_and(|ch| ctx.channel_workflows.contains_key(ch));
        let is_lead_driven = ctx.is_lead_driven(pr_number);

        if is_lead_driven || has_workflow {
            // Authoritative — emit cooldown tracking + event only.
            // This fires even for Skip actions so the state machine stays in sync.
            return vec![
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
                Effect::EmitWorkflowEvent(event.clone()),
            ];
        }
    }

    // Skip actions: no inline effects. Still emit the workflow event if one was
    // built so the workflow's state machine stays in sync (the event is a no-op
    // if no script is configured).
    if let PrAction::Skip { reason } = &action {
        debug!("{}", reason);
        let mut effects = Vec::new();
        if let Some(event) = workflow_event {
            effects.push(Effect::EmitWorkflowEvent(event));
        }
        return effects;
    }

    // Model override: use Opus for review feedback responses (higher quality
    // needed to understand nuanced review feedback).
    let model = match issue_type {
        PrIssueType::ReviewComment
        | PrIssueType::ReviewComplete
        | PrIssueType::GreenWithFeedback => Some("opus".to_string()),
        _ => None,
    };

    let mut effects = match action {
        // Task-linked PRs: use TaskPrompt for both nudge and spawn.
        // deliver_task_prompt handles nudge-if-running / resume-if-stopped internally.
        PrAction::NudgeOwner { message, .. } | PrAction::SpawnOwner { message, .. }
            if task_id.is_some() =>
        {
            vec![
                Effect::TaskPrompt {
                    task_id: task_id.unwrap().clone(),
                    message,
                    model,
                    pr_context: Some(super::effects::TaskPromptPrContext {
                        pr_number,
                        issue_type,
                    }),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        // Task-less PRs: post to ops for manual investigation.
        // All coworker PRs should have tasks; reaching here indicates a
        // data gap (e.g., daemon restart lost session records).
        // Bug !2377: Use RecordPermanentPrNudge (one-shot) instead of RecordPrNudge
        // (cooldown-based). No coworker exists to fix the issue, so cooldown expiry
        // would just re-fire the same message every 10 minutes indefinitely.
        PrAction::NudgeOwner { owner, message: _ } | PrAction::SpawnOwner { owner, message: _ } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "PR #{} ({}) owned by {} \u{2014} {}: {} (no task linked, posting for manual review)",
                        pr_number,
                        truncate_str(title, 40),
                        owner,
                        issue_type,
                        get_issue_action(issue_type),
                    ),
                    channel: Some(OPS_CHANNEL.to_string()),
                    auto_output: false,
                    message_type: None,
                    nudge_type: None,
                    tool_data: None,
                    provider: None,
                    tool_use_id: None,
                    parent_tool_use_id: None,
                },
                Effect::RecordPermanentPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::PostToChannel { message } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message,
                    channel: channel.clone(),
                    auto_output: false,
                    message_type: None,
                    nudge_type: None,
                    tool_data: None,
                    provider: None,
                    tool_use_id: None,
                    parent_tool_use_id: None,
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::Skip { .. } => unreachable!(), // handled above
    };

    // Append workflow event alongside inline effects (no-op if no script exists,
    // but keeps event emission consistent for observability and future script setup).
    if let Some(event) = workflow_event {
        effects.push(Effect::EmitWorkflowEvent(event));
    }

    effects
}

/// Pre-fetched data for stuck condition evaluation.
///
/// Bundles async state lookups done once before the per-PR loop, so individual
/// scenario functions can be synchronous.
struct StuckEvalContext<'a> {
    review_mode: crate::config::ReviewMode,
    /// PR number → current coworker name, derived from session data.
    pr_session_names: HashMap<u64, String>,
    channel_lead_names: HashSet<String>,
    has_available_slots: bool,
    running_coworkers: Vec<crate::coworker::Coworker>,
    project_name: &'a str,
    /// PR numbers that have a reviewer assigned in persistent state.
    assigned_prs: HashSet<u64>,
    /// PR numbers with an active reviewer who hasn't finished their cached review.
    active_reviewer_prs: HashSet<u64>,
    /// PR number → task ID mapping for workflow event routing.
    pr_task_associations: HashMap<u64, String>,
    /// Task ID → channel name mapping for workflow event routing.
    task_channel: HashMap<String, String>,
    /// Channel→workflow assignments for checking if a channel has a workflow.
    channel_workflows: HashMap<String, String>,
    /// Channels in lead-driven mode (skip stuck-condition nudges).
    lead_driven_channels: std::collections::HashSet<String>,
    /// PR numbers that have a pending review task awaiting dispatch.
    pending_review_prs: HashSet<u64>,
}

impl StuckEvalContext<'_> {
    /// Returns true if a PR's task channel is in lead-driven mode.
    fn is_lead_driven(&self, pr_number: u64) -> bool {
        self.pr_task_associations
            .get(&pr_number)
            .and_then(|tid| self.task_channel.get(tid))
            .is_some_and(|ch| self.lead_driven_channels.contains(ch))
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
/// The `reviewed_prs` parameter contains PR numbers that have completed reviews
/// (comment-based or formal), pre-collected before this function to keep
/// decision logic free of async API calls.
async fn collect_stuck_condition_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
    review_mode: crate::config::ReviewMode,
    at_task_limit: bool,
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();
    let mut tracker = state.stuck_tracker.lock().await;
    tracker.cleanup();

    let now = Instant::now();
    let mut nudge_count = 0;

    // Pre-fetch async data so per-PR scenario functions can be synchronous.
    let ctx = {
        let ps = state.persistent_state.lock().await;
        let assigned: HashSet<u64> = prs
            .iter()
            .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
            .filter(|&n| ps.active_reviewer_for_pr(n).is_some())
            .collect();
        let active_reviewers: HashSet<u64> = prs
            .iter()
            .filter_map(|pr| {
                let n = pr.get("number").and_then(|n| n.as_u64())?;
                if ps.active_reviewer_for_pr(n).is_some() && !ps.github.has_cached_review(n) {
                    Some(n)
                } else {
                    None
                }
            })
            .collect();
        let channel_lead_names = ps.channel_lead_names();
        // With task-based naming, there is always a name available.
        // The only constraint is the in-progress task limit.
        let has_available_slots = !at_task_limit;
        let pr_task_associations = super::state::pr_to_task_map_from_sessions(&ps.sessions);
        let pr_session_names: HashMap<u64, String> = {
            let task_to_name: HashMap<&str, &str> = ps
                .sessions
                .values()
                .filter_map(|s| {
                    let name = if s.name.is_empty() {
                        return None;
                    } else {
                        s.name.as_str()
                    };
                    Some((s.task_id.as_deref()?, name))
                })
                .collect();
            pr_task_associations
                .iter()
                .filter_map(|(&pr_num, task_id)| {
                    task_to_name
                        .get(task_id.as_str())
                        .map(|name| (pr_num, name.to_string()))
                })
                .collect()
        };
        let all_tasks = state.task_store.load_all();
        let task_channel: HashMap<String, String> = all_tasks
            .iter()
            .filter_map(|t| t.channel.as_ref().map(|ch| (t.id.clone(), ch.clone())))
            .collect();
        let pending_review_prs: HashSet<u64> = all_tasks
            .iter()
            .filter(|t| {
                t.pr.is_some()
                    && t.subject.starts_with("Review PR #")
                    && t.status == crate::task_store::TaskStatus::Pending
            })
            .filter_map(|t| t.pr)
            .collect();
        let channel_workflows = ps.channel_workflows.clone();
        let lead_driven_channels = ps.lead_driven_channels.clone();
        StuckEvalContext {
            review_mode,
            pr_session_names,
            channel_lead_names,
            has_available_slots,
            running_coworkers: state.coworkers.list_running(),
            project_name: &state.project_name,
            assigned_prs: assigned,
            active_reviewer_prs: active_reviewers,
            pr_task_associations,
            task_channel,
            channel_workflows,
            lead_driven_channels,
            pending_review_prs,
        }
    };

    for pr in prs {
        let pf = PrFields::from_json(pr);
        if pf.number == 0 || pf.is_draft {
            continue;
        }

        // Skip PRs in lead-driven channels — the lead handles stuck-condition triage.
        if ctx.is_lead_driven(pf.number) {
            debug!(
                "PR #{}: skipping stuck-condition check — channel is lead-driven",
                pf.number
            );
            continue;
        }

        let review_decision = pf.review_decision();
        let age_secs = get_pr_age_secs(pr).unwrap_or(0);
        let pr_id = pf.number.to_string();
        let has_completed_review = reviewed_prs.contains(&pf.number);

        nudge_count += no_review_scenario(
            &mut effects,
            &mut tracker,
            &pf,
            review_decision,
            age_secs,
            has_completed_review,
            &ctx,
        );

        nudge_count += unresolved_feedback_scenario(
            &mut effects,
            &mut tracker,
            &pf,
            &pr_id,
            review_decision,
            now,
        );

        nudge_count += merge_ready_scenario(
            &mut effects,
            &mut tracker,
            &pf,
            &pr_id,
            pr,
            ctx.active_reviewer_prs.contains(&pf.number),
            now,
        );
    }

    nudge_count += silent_coworker_scenario(&mut effects, &mut tracker, state).await;

    if nudge_count > 0 {
        info!(
            "Stuck condition check: notified ops about {} issue(s)",
            nudge_count
        );
    }

    // When a workflow or lead-driven mode is active, replace AutoMergePr with
    // EmitWorkflowEvent(PrAutoMerge) so the workflow/lead controls auto-merge.
    effects = effects
        .into_iter()
        .map(|effect| {
            if let Effect::AutoMergePr { pr_number, .. } = &effect {
                let pr_number = *pr_number;
                // Look up PR → task → channel
                if let Some(task_id) = ctx.pr_task_associations.get(&pr_number)
                    && let Some(channel) = ctx.task_channel.get(task_id)
                {
                    let has_workflow = ctx.channel_workflows.contains_key(channel);
                    let is_lead_driven = ctx.lead_driven_channels.contains(channel);
                    if has_workflow || is_lead_driven {
                        return Effect::EmitWorkflowEvent(
                            crate::workflow::WorkflowEvent::PrAutoMerge {
                                channel: channel.clone(),
                                task_id: task_id.clone(),
                                pr_number,
                            },
                        );
                    }
                }
            }
            effect
        })
        .collect();

    effects
}

/// Scenario 1: PR open with no review for N minutes.
///
/// Tracks PRs that have no formal review decision and no completed comment-based
/// review. After STUCK_NO_REVIEW_DURATION, nudges ops with context about whether
/// a reviewer was assigned and whether coworker slots are available.
fn no_review_scenario(
    effects: &mut Vec<Effect>,
    tracker: &mut super::trackers::StuckConditionTracker,
    pf: &PrFields,
    review_decision: &str,
    age_secs: u64,
    has_completed_review: bool,
    ctx: &StuckEvalContext,
) -> u32 {
    let pr_id = pf.number.to_string();

    if !review_decision.is_empty()
        || has_completed_review
        || age_secs < STUCK_NO_REVIEW_DURATION.as_secs()
    {
        tracker.clear(&pr_id, StuckConditionType::NoReview);
        return 0;
    }

    tracker.track(&pr_id, StuckConditionType::NoReview);
    if !tracker.should_nudge(&pr_id, StuckConditionType::NoReview) {
        return 0;
    }

    let prior_nudges = tracker.nudge_count(&pr_id, StuckConditionType::NoReview);

    let nudge = if ctx.review_mode == crate::config::ReviewMode::GithubApp {
        no_review_nudge_github_app(pf, age_secs, prior_nudges)
    } else {
        no_review_nudge_self_review(pf, age_secs, prior_nudges, ctx)
    };

    effects.extend(stuck_nudge_effects(&nudge));
    tracker.record_nudge(&pr_id, StuckConditionType::NoReview);
    1
}

fn no_review_nudge_github_app(pf: &PrFields, age_secs: u64, prior_nudges: u32) -> String {
    if should_escalate(prior_nudges) {
        format!(
            "@ops PR #{} ({}) has been open for {} minutes with no review while execution.review_mode=github_app. Check GitHub App review delivery/config.",
            pf.number,
            truncate_str(pf.title, 40),
            age_secs / 60,
        )
    } else {
        format!(
            "@ops PR #{} ({}) has been open for {} minutes and is still waiting for GitHub App review (execution.review_mode=github_app).",
            pf.number,
            truncate_str(pf.title, 40),
            age_secs / 60,
        )
    }
}

fn no_review_nudge_self_review(
    pf: &PrFields,
    age_secs: u64,
    prior_nudges: u32,
    ctx: &StuckEvalContext,
) -> String {
    let is_assigned = ctx.assigned_prs.contains(&pf.number);

    let build_busy_reason = || {
        let pr_author = ctx.pr_session_names.get(&pf.number).cloned();
        let mut busy: Vec<String> = ctx
            .running_coworkers
            .iter()
            .filter(|cw| is_non_lead_coworker(&cw.name, ctx.project_name, &ctx.channel_lead_names))
            .map(|cw| cw.name.clone())
            .collect();
        busy.sort();
        format_no_reviewer_reason(&busy, pr_author.as_deref())
    };

    let has_pending_review = ctx.pending_review_prs.contains(&pf.number);

    if should_escalate(prior_nudges) {
        let context = if is_assigned && ctx.has_available_slots {
            "A reviewer was assigned but hasn't posted a review, and coworker slots are available. This looks like a daemon bug.".to_string()
        } else if !is_assigned && ctx.has_available_slots {
            "Coworker slots are available but no reviewer was assigned. This looks like a daemon bug.".to_string()
        } else if is_assigned {
            "A reviewer was assigned but hasn't posted a review.".to_string()
        } else if has_pending_review {
            "Review dispatch deferred: at task limit. A review task exists but cannot be assigned until a slot opens.".to_string()
        } else {
            format!("No reviewer could be assigned — {}", build_busy_reason())
        };
        format!(
            "@ops PR #{} ({}) has been stuck for {} minutes with no review — {} Consider running `midtown e2e capture` to debug.",
            pf.number,
            truncate_str(pf.title, 40),
            age_secs / 60,
            context,
        )
    } else {
        let context = if is_assigned {
            "I assigned a reviewer but no review has been posted yet".to_string()
        } else if has_pending_review {
            "review dispatch deferred — at task limit, review task queued".to_string()
        } else {
            format!("I couldn't assign a reviewer — {}", build_busy_reason())
        };
        format!(
            "@ops PR #{} ({}) has been open for {} minutes with no review — {}",
            pf.number,
            truncate_str(pf.title, 40),
            age_secs / 60,
            context,
        )
    }
}

/// Scenario 2: Unresolved feedback (changes requested) for N minutes.
///
/// Tracks PRs with CHANGES_REQUESTED review decision. After
/// STUCK_UNRESOLVED_FEEDBACK_DURATION, nudges ops that the author hasn't
/// pushed changes in response to review feedback.
fn unresolved_feedback_scenario(
    effects: &mut Vec<Effect>,
    tracker: &mut super::trackers::StuckConditionTracker,
    pf: &PrFields,
    pr_id: &str,
    review_decision: &str,
    now: Instant,
) -> u32 {
    if review_decision != "CHANGES_REQUESTED" {
        tracker.clear(pr_id, StuckConditionType::UnresolvedFeedback);
        return 0;
    }

    let first_detected = tracker.track(pr_id, StuckConditionType::UnresolvedFeedback);
    let stuck_duration = now.duration_since(first_detected);

    if stuck_duration < STUCK_UNRESOLVED_FEEDBACK_DURATION
        || !tracker.should_nudge(pr_id, StuckConditionType::UnresolvedFeedback)
    {
        return 0;
    }

    let prior_nudges = tracker.nudge_count(pr_id, StuckConditionType::UnresolvedFeedback);

    let nudge = if should_escalate(prior_nudges) {
        format!(
            "@ops PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't responded despite repeated nudges. The coworker may be stuck or the task may need reassignment.",
            pf.number,
            truncate_str(pf.title, 40),
            stuck_duration.as_secs() / 60,
        )
    } else {
        format!(
            "@ops PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't pushed new changes",
            pf.number,
            truncate_str(pf.title, 40),
            stuck_duration.as_secs() / 60,
        )
    };

    effects.extend(stuck_nudge_effects(&nudge));
    tracker.record_nudge(pr_id, StuckConditionType::UnresolvedFeedback);
    1
}

/// Scenario 3: Approved + CI green but not merging.
///
/// When a PR is auto-mergeable (approved + CI green), enables GitHub auto-merge
/// on first detection. If the PR still hasn't merged after STUCK_MERGE_READY_DURATION,
/// nudges ops. Gates auto-merge behind active reviewer check to prevent merging
/// while a review is in progress.
fn merge_ready_scenario(
    effects: &mut Vec<Effect>,
    tracker: &mut super::trackers::StuckConditionTracker,
    pf: &PrFields,
    pr_id: &str,
    pr: &serde_json::Value,
    has_active_reviewer: bool,
    now: Instant,
) -> u32 {
    if !is_auto_mergeable(pr) {
        tracker.clear(pr_id, StuckConditionType::MergeReady);
        tracker.clear(pr_id, StuckConditionType::AutoMerge);
        return 0;
    }

    // Gate: don't auto-merge while a daemon-assigned reviewer is still working.
    // Mirrors the pre-gate in handle_pr_merge (rpc_prs.rs) that prevents the
    // PR #1624 incident. Uses get_reviewer() (raw presence, no timeout) with
    // a bypass when the review is already cached as complete.
    if !has_active_reviewer {
        tracker.track(pr_id, StuckConditionType::AutoMerge);
        if tracker.should_nudge(pr_id, StuckConditionType::AutoMerge) {
            effects.push(Effect::AutoMergePr {
                pr_number: pf.number,
                title: pf.title.to_string(),
            });
            tracker.record_nudge(pr_id, StuckConditionType::AutoMerge);
        }
    }

    let first_detected = tracker.track(pr_id, StuckConditionType::MergeReady);
    let stuck_duration = now.duration_since(first_detected);

    if stuck_duration < STUCK_MERGE_READY_DURATION
        || !tracker.should_nudge(pr_id, StuckConditionType::MergeReady)
    {
        return 0;
    }

    let prior_nudges = tracker.nudge_count(pr_id, StuckConditionType::MergeReady);

    let nudge = if should_escalate(prior_nudges) {
        format!(
            "@ops PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — the author isn't responding to merge nudges. Consider merging manually or investigating the coworker.",
            pf.number,
            truncate_str(pf.title, 40),
            stuck_duration.as_secs() / 60,
        )
    } else {
        format!(
            "@ops PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — author may need a nudge to merge",
            pf.number,
            truncate_str(pf.title, 40),
            stuck_duration.as_secs() / 60,
        )
    };

    effects.extend(stuck_nudge_effects(&nudge));
    tracker.record_nudge(pr_id, StuckConditionType::MergeReady);
    1
}

/// Scenario 4: Silent coworker (claimed task, no channel activity).
///
/// Checks busy coworkers for extended silence. First nudge asks the coworker
/// directly; subsequent nudges escalate to ops. Only starts the silence clock
/// after a coworker's first channel message (to avoid false positives during
/// initialization).
async fn silent_coworker_scenario(
    effects: &mut Vec<Effect>,
    tracker: &mut super::trackers::StuckConditionTracker,
    state: &DaemonState,
) -> u32 {
    let busy_coworkers = state.get_busy_session_names().await;
    let records = state.coworker_records.read().await;
    let mut nudge_count = 0;

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

        if !is_silent {
            tracker.clear(name, StuckConditionType::SilentCoworker);
            continue;
        }

        tracker.track(name, StuckConditionType::SilentCoworker);
        if !tracker.should_nudge(name, StuckConditionType::SilentCoworker) {
            continue;
        }

        let task_info = state
            .task_store
            .load_all()
            .into_iter()
            .filter(|t| t.status == crate::task_store::TaskStatus::InProgress)
            .find(|t| t.agent_name.eq_ignore_ascii_case(name))
            .map(|t| format!("task !{} ({})", t.id, truncate_str(&t.subject, 30)))
            .unwrap_or_else(|| "their task".to_string());

        let prior_nudges = tracker.nudge_count(name, StuckConditionType::SilentCoworker);

        if prior_nudges == 0 {
            // First nudge: ask the coworker directly before escalating
            let nudge_msg = format!(
                "Status check — you've been quiet on {} for over {} minutes. \
                 Are you stuck or still working?",
                task_info,
                STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
            );
            effects.push(Effect::nudge_session(
                state.session_id_for_name(name).await,
                nudge_msg,
            ));
            effects.push(Effect::PostSystemMessage {
                message: format!(
                    "⚠️ Nudging {} — silent on {} for over {} minutes",
                    name,
                    task_info,
                    STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                ),
                channel: Some(OPS_CHANNEL.to_string()),
            });
        } else {
            // Escalation: coworker didn't respond, notify ops
            let nudge = format!(
                "@ops {} has been silent on {} for over {} minutes \
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

    nudge_count
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

/// Format a diagnostic string explaining why a reviewer couldn't be assigned.
///
/// Returns a string like "no eligible reviewers (busy: [madison, york], excluded-author: york)"
/// that helps ops triage the issue without reading daemon logs.
///
/// Parameters:
/// - `busy_names`: coworker names that are currently running (non-lead, non-channel-lead)
/// - `pr_author`: the PR author's coworker name, if determinable from the branch prefix
pub(super) fn format_no_reviewer_reason(busy_names: &[String], pr_author: Option<&str>) -> String {
    let mut parts = Vec::new();
    if !busy_names.is_empty() {
        parts.push(format!("busy: [{}]", busy_names.join(", ")));
    } else {
        parts.push("no coworkers running".to_string());
    }
    if let Some(author) = pr_author {
        parts.push(format!("excluded-author: {}", author));
    }
    format!("no eligible reviewers ({})", parts.join(", "))
}

/// Convert a stuck condition nudge message into effects (system message only).
///
/// The message should contain "@ops" which the PostSystemMessage handler in
/// effects.rs will detect and route to the ops channel lead. We don't return
/// a separate nudge effect here because that would cause double delivery
/// (the @ops routing in the PostSystemMessage handler already handles it).
fn stuck_nudge_effects(message: &str) -> Vec<Effect> {
    vec![Effect::PostSystemMessage {
        message: format!("⚠️ {}", message),
        channel: Some(OPS_CHANNEL.to_string()),
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

/// Fetch and format all review content for a PR.
///
/// Retrieves both formal GitHub reviews (e.g., Codex formal reviews) and
/// Midtown coworker reviews (posted as issue comments with review signatures).
/// Returns a formatted string to append to nudge messages so coworkers see
/// all feedback without needing to run extra `gh` commands.
///
/// Returns `None` if the fetch fails or no review content is found.
async fn fetch_review_content(pr_number: u64) -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "reviews,comments",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        debug!(
            "fetch_review_content: gh pr view failed for PR #{}",
            pr_number
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let data: serde_json::Value = serde_json::from_str(&stdout).ok()?;
    format_review_content(&data)
}

/// Pre-fetch review content for all reviewed PRs in one batch.
///
/// Called at the top of `poll_prs_for_issues` to collect all review content
/// upfront, keeping subprocess I/O out of inner decision functions.
/// Returns a map of PR number → formatted review content string.
async fn pre_fetch_review_content_for_prs(
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
) -> HashMap<u64, String> {
    let mut result = HashMap::new();

    for pr in prs {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) if reviewed_prs.contains(&n) => n,
            _ => continue,
        };

        if let Some(content) = fetch_review_content(pr_number).await {
            result.insert(pr_number, content);
        }
    }

    result
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
    tick: &PrPollTickState,
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
        let pf = PrFields::from_json(pr);
        let pr_number = pf.number;
        if pr_number == 0 {
            continue;
        }

        let head_ref = pf.head_ref;
        let title = pf.title;

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
                    effects.push(Effect::nudge_channel_lead(
                        &tick.default_channel,
                        lead_nudge_msg,
                    ));
                }
            } else {
                // No new comments, just update tracker
                let mut tracker = state.comment_tracker.lock().await;
                tracker.record(pr_number, non_owner_count);
            }

            continue; // Lead PR handled, move to next PR
        }

        // Only check coworker-owned PRs beyond this point
        let owner = match tick
            .pr_task_index
            .task_for_pr(pr_number)
            .and_then(|task_id| {
                tick.session_task_map
                    .get(task_id)
                    .and_then(|sid| tick.sessions.get(sid))
            })
            .map(|s| s.name.clone())
            .filter(|n| !n.is_empty())
        {
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

        // Embed review content so the coworker sees all feedback inline
        let review_content = fetch_review_content(pr_number).await;
        let nudge_msg = format!(
            "Your PR #{} ({}) has new review comments — please address feedback.{}",
            pr_number,
            truncate_str(title, 40),
            review_content.as_deref().unwrap_or("")
        );

        debug!(
            "Polling detected new review comments on PR #{}, nudging {} and creating task",
            pr_number, owner
        );

        // Extract all decision context from persistent state in one lock
        let pr_ctx = {
            let tc = task_channel_map_from_store(&state.task_store);
            let ps = state.persistent_state.lock().await;
            PrContext::from_persistent_state(&ps, pr_number, tc)
        };

        // If the linked task is completed, create a follow-up task rather than
        // trying to spawn/resume the original coworker with stale session context.
        if let Some(task_id) = pr_ctx.pr_task_associations.get(&pr_number)
            && let Some(task) = state.task_store.load(task_id).ok()
            && crate::rules::review_comment_creates_followup(&task.status)
        {
            let subject = format!("Address review feedback on PR #{}", pr_number);
            let description = format!(
                "PR #{} ({}) received review feedback after task !{} was completed. Please check the PR and address the feedback.",
                pr_number,
                truncate_str(title, 40),
                task_id
            );
            debug!(
                "Polling: PR #{} linked to completed task !{} — creating follow-up task",
                pr_number, task_id
            );
            effects.push(Effect::CreateTask {
                dir_key: state.paths.dir_key().to_string(),
                subject,
                description,
                pr: Some(pr_number),
            });
            effects.push(Effect::RecordPrNudge {
                pr_number,
                issue_type: PrIssueType::ReviewComment,
            });
            continue;
        }

        // Decide action using handoff-aware decision function
        let action = crate::rules::decide_pr_action(
            &owner,
            active_coworkers,
            idle_coworkers,
            tick.is_at_task_limit,
            &nudge_msg,
            crate::rules::PrActionContext::PrComment {
                actor: "reviewer".to_string(), // Generic actor since we don't know the specific commenter from polling
            },
        );

        effects.extend(action_to_effects(
            action,
            pr_number,
            title,
            PrIssueType::ReviewComment,
            state,
            &pr_ctx,
        ));

        // If this is a lead/* branch, also nudge the lead so they see review feedback
        if is_lead_branch(head_ref) {
            let lead_nudge_msg = format!(
                "Your PR #{} ({}) has new review comments — please address feedback.",
                pr_number,
                truncate_str(title, 40)
            );
            effects.push(Effect::nudge_channel_lead(
                &tick.default_channel,
                lead_nudge_msg,
            ));
        }
    }

    effects
}

/// Collect effects for spawning reviewers for PRs that need code review.
///
/// Identifies PRs that need review (not drafts, old enough, no completed review,
/// not already assigned) and returns effects to spawn reviewer coworkers.
/// Uses `SpawnCoworkerWithCallbacks` so that reviewer assignment and channel
/// messages only happen on successful spawn.
async fn collect_reviewer_effects(
    tick: &PrPollTickState,
    state: &DaemonState,
    prs: &[serde_json::Value],
    pre_fetched_review_content: &HashMap<u64, String>,
) -> Vec<Effect> {
    collect_reviewer_effects_with_source(
        &tick.worktree_registry,
        &tick.active_session_names,
        state,
        prs,
        true, // is_polling_fallback
        pre_fetched_review_content,
        tick.is_at_task_limit,
    )
    .await
}

/// Handle the review-complete path for a single PR.
///
/// Returns `Some(effects)` if the PR has a completed review (effects may be empty
/// if the nudge was already sent), or `None` if the PR is NOT reviewed and the
/// caller should proceed to reviewer spawning.
#[allow(clippy::too_many_arguments)]
async fn collect_review_complete_effects(
    pr_number: u64,
    pr: &serde_json::Value,
    state: &DaemonState,
    pr_task_associations: &HashMap<u64, String>,
    session_task_map: &HashMap<String, String>,
    sessions: &HashMap<String, super::state::SessionRecord>,
    is_at_task_limit: bool,
    active_coworkers: &[String],
    idle_coworkers: &[String],
    pre_fetched_review_content: &HashMap<u64, String>,
    pr_ctx: &PrContext,
) -> Option<Vec<Effect>> {
    if !state.is_pr_reviewed(pr_number).await {
        return None;
    }

    let pf = PrFields::from_json(pr);
    let title = pf.title;
    let mut effects = Vec::new();

    debug!("PR #{} already has a completed review", pr_number);

    // Clear the reviewer assignment now that the review is complete.
    // This allows the reviewer to be sent on break, freeing up coworker slots.
    // Previously we only cleared when the reviewer had shut down, but that left
    // idle reviewers stuck with assignments preventing break dispatch.
    {
        let mut ps = state.persistent_state.lock().await;
        if ps.active_reviewer_for_pr(pr_number).is_some() {
            debug!(
                "PR #{} review completed, marking reviewer sessions as stopped",
                pr_number
            );
            // Find session IDs of active reviewers for this PR
            let session_ids: Vec<String> = ps
                .active_reviewer_sessions()
                .iter()
                .filter(|s| s.pr_number == Some(pr_number))
                .map(|s| s.session_id.clone())
                .collect();
            // Mark them as stopped so pr_has_active_reviewer returns false
            for sid in &session_ids {
                if let Some(record) = ps.sessions.get_mut(sid) {
                    record.is_running = false;
                    record.resume_on_startup = false;
                }
            }
            if !session_ids.is_empty()
                && let Err(e) = ps.save_for_repo(state.paths.dir_key())
            {
                warn!("Failed to save daemon-state.json: {}", e);
            }
        }
    }

    // Clear any Approved nudge cooldown so the next tick re-evaluates PrApproved.
    // When the reviewer was active, pr_action_to_effects suppressed both the
    // workflow event AND inline effects (!1902, !2003). If a prior approval
    // recorded a cooldown before the reviewer started, clear it here so the
    // workflow script's PrApproved event can fire on the next tick.
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        if tracker.has_nudge(pr_number, PrIssueType::Approved) {
            debug!(
                "PR #{} reviewer cleared, resetting Approved nudge cooldown for PrApproved re-evaluation",
                pr_number
            );
            tracker.clear_nudge(pr_number, PrIssueType::Approved);
        }
    }

    // Bug !2124 + !2137: One-shot nudging for all review-complete PRs.
    // For lead branches, skip coworker owner resolution (the user must act).
    // For coworker branches, try owner resolution first (nudge vs spawn).
    let already_nudged = {
        let tracker = state.pr_issue_tracker.lock().await;
        tracker.has_nudge(pr_number, PrIssueType::ReviewComplete)
    };
    if already_nudged {
        return Some(effects);
    }

    let nudge_msg = build_review_complete_nudge_msg(pr_number, title, pre_fetched_review_content);

    // For coworker PRs, try to resolve an owner and nudge/spawn them.
    if !is_lead_branch(pf.head_ref) {
        let owner = resolve_pr_owner_from_session(
            pr_number,
            pr_task_associations,
            session_task_map,
            sessions,
        );

        if let Some(owner) = owner {
            let action = crate::rules::decide_pr_action(
                &owner,
                active_coworkers,
                idle_coworkers,
                is_at_task_limit,
                &nudge_msg,
                crate::rules::PrActionContext::ReviewComplete,
            );

            effects.extend(action_to_effects(
                action,
                pr_number,
                title,
                PrIssueType::ReviewComplete,
                state,
                pr_ctx,
            ));
            effects.push(Effect::RecordPermanentPrNudge {
                pr_number,
                issue_type: PrIssueType::ReviewComplete,
            });
            return Some(effects);
        }
    }

    // Lead branch or no coworker owner found — notify the user.
    effects.extend(user_review_complete_effects(pr_number, &nudge_msg, pr_ctx));

    Some(effects)
}

/// Build the nudge message for a completed review.
fn build_review_complete_nudge_msg(
    pr_number: u64,
    title: &str,
    pre_fetched_review_content: &HashMap<u64, String>,
) -> String {
    let review_suffix = pre_fetched_review_content
        .get(&pr_number)
        .map(|s| s.as_str())
        .unwrap_or("");
    format!(
        "Your PR #{} ({}) has a completed review — please address any feedback and merge if appropriate.{}",
        pr_number,
        truncate_str(title, 40),
        review_suffix
    )
}

/// Emit PostToChannel + RecordPermanentPrNudge effects for user-facing review-complete notification.
fn user_review_complete_effects(
    pr_number: u64,
    nudge_msg: &str,
    pr_ctx: &PrContext,
) -> Vec<Effect> {
    let channel = pr_ctx.get_channel(pr_number);
    let user_msg = format!("@user {}", nudge_msg);
    vec![
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: user_msg,
            channel,
            auto_output: false,
            message_type: None,
            nudge_type: None,
            tool_data: None,
            provider: None,
            tool_use_id: None,
            parent_tool_use_id: None,
        },
        Effect::RecordPermanentPrNudge {
            pr_number,
            issue_type: PrIssueType::ReviewComplete,
        },
    ]
}

// ---------------------------------------------------------------------------
// Reviewer-spawn guard helpers
// ---------------------------------------------------------------------------

/// Reason a PR was skipped during reviewer-spawn evaluation.
///
/// Used by `should_skip_pr_for_review` to provide structured skip reasons that
/// are useful for debug logging and make the guard cascade easier to reason about.
#[derive(Debug)]
enum ReviewSkipReason {
    /// PR number is zero (malformed JSON).
    InvalidPr,
    /// PR is a draft.
    Draft,
    /// PR belongs to a lead-driven channel.
    LeadDriven,
    /// PR is too new (hasn't passed the review delay).
    TooNew {
        #[allow(dead_code)]
        age_secs: u64,
        #[allow(dead_code)]
        delay_secs: u64,
    },
    /// A webhook recently handled this PR (polling defers).
    WebhookDeferred,
}

/// Pre-review-complete guard checks: determines whether a PR should be skipped
/// before we even check if it already has a completed review.
///
/// Returns `Some(reason)` if the PR should be skipped, `None` if processing
/// should continue.
fn should_skip_pr_for_review(
    pf: &PrFields<'_>,
    pr_ctx: &PrContext,
    is_polling_fallback: bool,
    review_delay: u64,
    pr: &serde_json::Value,
    pr_last_webhook_event: &HashMap<u64, chrono::DateTime<chrono::Utc>>,
) -> Option<ReviewSkipReason> {
    if pf.number == 0 {
        return Some(ReviewSkipReason::InvalidPr);
    }

    if pf.is_draft {
        return Some(ReviewSkipReason::Draft);
    }

    if pr_ctx.is_lead_driven(pf.number) {
        return Some(ReviewSkipReason::LeadDriven);
    }

    if let Some(age_secs) = get_pr_age_secs(pr)
        && age_secs < review_delay
    {
        return Some(ReviewSkipReason::TooNew {
            age_secs,
            delay_secs: review_delay,
        });
    }

    if is_polling_fallback {
        let window_secs = review_delay as i64 * 2;
        let webhook_recently_handled = pr_last_webhook_event.get(&pf.number).is_some_and(|ts| {
            let elapsed = chrono::Utc::now().signed_duration_since(*ts);
            elapsed < chrono::Duration::seconds(window_secs)
        });
        if webhook_recently_handled {
            return Some(ReviewSkipReason::WebhookDeferred);
        }
    }

    None
}

/// Context for orphan detection, grouping the shared lookup tables needed
/// to determine whether a PR's author is reachable.
struct OrphanCheckCtx<'a> {
    worktree_registry: &'a crate::worktree_registry::WorktreeRegistry,
    pr_task_associations: &'a HashMap<u64, String>,
    session_task_map: &'a HashMap<String, String>,
    sessions: &'a HashMap<String, super::state::SessionRecord>,
    active_names: &'a HashSet<String>,
    repo_owner: Option<&'a str>,
}

/// Determines whether a PR is orphaned (no active author who can address feedback).
///
/// A PR is orphaned when we can't identify an active worktree or running coworker
/// that owns it. Orphaned PRs should not get auto-review spawned since the author
/// can't address feedback.
///
/// Resolution strategy (in order):
/// 1. Look up worktree by PR number (webhook-linked)
/// 2. Look up by task ID extracted from PR title
/// 3. Fall back to branch name lookup
/// 4. If no worktree: check if it's a lead branch, session-owned, or lead-authored
fn is_pr_orphaned(
    pr_number: u64,
    head_ref: &str,
    title: &str,
    pr: &serde_json::Value,
    ctx: &OrphanCheckCtx<'_>,
) -> bool {
    let worktree = ctx
        .worktree_registry
        .get_by_pr(pr_number)
        .or_else(|| {
            crate::task_store::extract_task_id_from_pr_title(title).and_then(|task_id| {
                let task_id_str = task_id.to_string();
                ctx.worktree_registry
                    .all_assignments()
                    .values()
                    .find(|a| a.task_id.as_ref() == Some(&task_id_str))
            })
        })
        .or_else(|| ctx.worktree_registry.get_by_branch(head_ref));

    match worktree {
        Some(assignment) if assignment.completed_at.is_none() => {
            // Has active worktree — not orphaned.
            false
        }
        Some(_) => {
            // Worktree exists but is completed. If the PR is still open (which it
            // is, since it's in the `prs` list), the author can still push to the
            // branch, so completed worktrees with open PRs are NOT orphaned.
            false
        }
        None => {
            // No worktree found — check alternative ownership signals.
            if is_lead_branch(head_ref) {
                debug!(
                    "PR #{} is a lead PR (branch: {}), not orphaned",
                    pr_number, head_ref
                );
                false
            } else if let Some(owner) = ctx
                .pr_task_associations
                .get(&pr_number)
                .and_then(|task_id| ctx.session_task_map.get(task_id.as_str()))
                .and_then(|session_id| ctx.sessions.get(session_id))
                .map(|record| record.name.clone())
                .filter(|n| !n.is_empty())
            {
                let is_active = ctx.active_names.contains(&owner.to_lowercase());
                if is_active {
                    debug!(
                        "PR #{} has no worktree for owner {} but coworker is active, not orphaned",
                        pr_number, owner
                    );
                    false
                } else {
                    debug!(
                        "PR #{} is orphaned (no worktree, owner {} not active, branch: {})",
                        pr_number, owner, head_ref
                    );
                    true
                }
            } else if super::helpers::is_lead_authored_pr(pr, ctx.repo_owner) {
                debug!(
                    "PR #{} is authored by lead (branch: {}), not orphaned",
                    pr_number, head_ref
                );
                false
            } else {
                debug!(
                    "PR #{} is orphaned (no determinable owner or worktree, branch: {})",
                    pr_number, head_ref
                );
                true
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn collect_reviewer_effects_with_source(
    worktree_registry: &crate::worktree_registry::WorktreeRegistry,
    active_names: &std::collections::HashSet<String>,
    state: &DaemonState,
    prs: &[serde_json::Value],
    is_polling_fallback: bool,
    pre_fetched_review_content: &HashMap<u64, String>,
    at_task_limit: bool,
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();
    let review_mode = crate::config::get_review_mode_for_repo(state.paths.dir_key());
    let spawn_local_reviewers = matches!(
        review_mode,
        crate::config::ReviewMode::Local | crate::config::ReviewMode::Both
    );

    // Build all shared context once to avoid lock churn and repeated map
    // allocation inside each PR loop.
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_busy_session_names().await;
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(c.as_str()))
        .cloned()
        .collect();

    let (
        pr_ctx,
        all_tasks,
        pr_task_associations,
        session_task_map,
        sessions,
        is_at_task_limit,
        task_channel,
        channel_workflow_channels,
        pr_last_webhook_event,
    ) = {
        let ps = state.persistent_state.lock().await;
        let all_tasks = state.task_store.load_all();
        let pr_task_associations = super::state::pr_to_task_map_from_sessions(&ps.sessions);
        let session_task_map: HashMap<String, String> = ps
            .sessions
            .iter()
            .filter_map(|(session_id, record)| {
                record
                    .task_id
                    .as_ref()
                    .map(|task_id| (task_id.clone(), session_id.clone()))
            })
            .collect();
        let sessions = ps.sessions.clone();
        let task_channel: HashMap<String, String> = state
            .task_store
            .load_all()
            .into_iter()
            .filter_map(|t| t.channel.map(|ch| (t.id, ch)))
            .collect();
        let pr_ctx = PrContext::routing_only(&ps, task_channel.clone());
        let is_at_task_limit = at_task_limit;
        let channel_workflow_channels: std::collections::HashSet<String> =
            ps.channel_workflows.keys().cloned().collect();
        let pr_last_webhook_event = ps.github.pr_last_webhook_event.clone();

        (
            pr_ctx,
            all_tasks,
            pr_task_associations,
            session_task_map,
            sessions,
            is_at_task_limit,
            task_channel,
            channel_workflow_channels,
            pr_last_webhook_event,
        )
    };

    for pr in prs {
        let pf = PrFields::from_json(pr);
        let pr_number = pf.number;

        // --- Pre-review-complete guards ---
        // Compute the review delay for this PR (longer for workflow-enabled channels
        // when polling, since the workflow handles real-time spawning).
        let review_delay = if is_polling_fallback {
            let has_workflow = pr_task_associations
                .get(&pr_number)
                .and_then(|task_id| task_channel.get(task_id))
                .is_some_and(|channel| channel_workflow_channels.contains(channel));

            if has_workflow {
                PR_REVIEW_DELAY_SCRIPT_SECS
            } else {
                PR_REVIEW_DELAY_SECS
            }
        } else {
            PR_REVIEW_DELAY_SECS
        };

        if let Some(reason) = should_skip_pr_for_review(
            &pf,
            &pr_ctx,
            is_polling_fallback,
            review_delay,
            pr,
            &pr_last_webhook_event,
        ) {
            debug!("PR #{}: skipping reviewer spawn ({:?})", pr_number, reason);
            continue;
        }

        // --- Check if PR already has a completed review ---
        if let Some(review_effects) = collect_review_complete_effects(
            pr_number,
            pr,
            state,
            &pr_task_associations,
            &session_task_map,
            &sessions,
            is_at_task_limit,
            &active_coworkers,
            &idle_coworkers,
            pre_fetched_review_content,
            &pr_ctx,
        )
        .await
        {
            effects.extend(review_effects);
            continue;
        }

        // --- Post-review-complete guards ---
        if !spawn_local_reviewers {
            debug!(
                "PR #{} review pending but local reviewer spawn disabled (execution.review_mode={:?})",
                pr_number, review_mode
            );
            continue;
        }

        // Guard: skip if a CreateReviewTask effect is already in-flight for
        // this PR from a previous tick (!2511).
        if state.is_review_pr_in_flight(pr_number) {
            debug!(
                "PR #{}: skipping reviewer spawn — CreateReviewTask in-flight",
                pr_number
            );
            continue;
        }

        // Check if a review task already exists for this PR (task-based dedup).
        // Include completed tasks: once a reviewer has been spawned for a PR,
        // don't auto-spawn another. Without this, completed review tasks are
        // invisible to the guard, causing an infinite spawn loop on each tick.
        {
            let has_review_task = all_tasks
                .iter()
                .any(|t| t.pr == Some(pr_number) && t.subject.starts_with("Review PR #"));
            if has_review_task {
                debug!(
                    "PR #{} already has a review task (pending, in-progress, or completed)",
                    pr_number
                );
                continue;
            }
        }

        let orphan_ctx = OrphanCheckCtx {
            worktree_registry,
            pr_task_associations: &pr_task_associations,
            session_task_map: &session_task_map,
            sessions: &sessions,
            active_names,
            repo_owner: state.repo_owner.as_deref(),
        };
        if is_pr_orphaned(pr_number, pf.head_ref, pf.title, pr, &orphan_ctx) {
            continue;
        }

        // --- Spawn reviewer ---
        let channel = pr_ctx.get_channel(pr_number);
        let parent_task_id = pr_task_associations.get(&pr_number).cloned();

        debug!(
            "Creating review task for PR #{}: {}",
            pr_number,
            truncate_str(pf.title, 40)
        );

        effects.push(Effect::CreateReviewTask {
            pr_number,
            parent_task_id,
            channel,
        });
    }

    effects
}

// NOTE: process_pending_review_spawns and handle_ci_completion_for_review_spawn
// were removed — reviewer spawning from webhooks is now driven by the workflow
// script's pr.opened and pr.ci_passed handlers calling rpc.spawn_reviewer().
// The polling backstop (collect_reviewer_effects) still runs during poll ticks.

/// Uncached check for whether a PR has at least one completed review.
///
/// A review can be either:
/// - A formal GitHub review submission (APPROVED / CHANGES_REQUESTED / COMMENTED / DISMISSED)
/// - A comment-based coworker review detected via review signature.
///
/// When `assigned_reviewer` is provided, only reviews authored by that reviewer
/// are considered complete. This prevents bot comments or other coworkers' comments
/// from prematurely marking a PR as reviewed.
///
/// Fetches both reviews and comments in a single API call to reduce GitHub API usage.
pub(super) fn pr_has_completed_review_uncached(
    pr_number: u64,
    assigned_reviewer: Option<&str>,
    assigned_session_id: Option<&str>,
) -> bool {
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

            json_has_completed_review(&json, assigned_reviewer, assigned_session_id)
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

/// Pure logic for checking if parsed review JSON contains a completed review.
///
/// Extracted from `pr_has_completed_review_uncached` for testability (no subprocess).
///
/// When `assigned_reviewer` is `Some`, only reviews authored by that reviewer
/// are accepted. When `None`, any valid review is accepted (backward-compatible).
pub(super) fn json_has_completed_review(
    json: &serde_json::Value,
    assigned_reviewer: Option<&str>,
    assigned_session_id: Option<&str>,
) -> bool {
    // Check formal reviews first (Codex / GitHub-native review flow).
    if let Some(reviews) = json.get("reviews").and_then(|v| v.as_array()) {
        for review in reviews {
            let state_upper = review
                .get("state")
                .and_then(|s| s.as_str())
                .map(|s| s.to_ascii_uppercase());

            // Only strong review states count as completed reviews.
            // COMMENTED and DISMISSED are too weak — Codex and other tools
            // submit COMMENTED reviews automatically, causing false positives
            // that prevent midtown reviewer spawning.
            let is_strong_state = state_upper
                .as_deref()
                .is_some_and(|s| matches!(s, "APPROVED" | "CHANGES_REQUESTED"));

            let has_review_body = review
                .get("body")
                .and_then(|b| b.as_str())
                .is_some_and(text_contains_review_signature);

            if is_strong_state || has_review_body {
                let body = review.get("body").and_then(|b| b.as_str()).unwrap_or("");

                if review_author_matches(body, assigned_reviewer, assigned_session_id)
                    || (is_strong_state && assigned_reviewer.is_some())
                {
                    return true;
                }
            }
        }
    }

    // Check comments (where coworkers post comment-based reviews).
    if let Some(comments) = json.get("comments").and_then(|v| v.as_array()) {
        for comment in comments {
            if let Some(body) = comment.get("body").and_then(|b| b.as_str())
                && text_contains_review_signature(body)
                && review_author_matches(body, assigned_reviewer, assigned_session_id)
            {
                return true;
            }
        }
    }

    false
}

/// Extract review comment IDs from a JSON array of GitHub issue comments.
///
/// Filters for comments containing a review signature and returns their
/// numeric database IDs. This is the pure parsing core shared by
/// `fetch_review_comment_ids` (which handles the `gh api` subprocess call).
pub(super) fn extract_review_comment_ids_from_json(comments: &[serde_json::Value]) -> Vec<u64> {
    comments
        .iter()
        .filter_map(|comment| {
            let body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");
            if text_contains_review_signature(body) {
                comment.get("id").and_then(|id| id.as_u64())
            } else {
                None
            }
        })
        .collect()
}

/// Fetch the database IDs of review comments on a PR via the GitHub REST API.
///
/// Uses `gh api repos/{full_name}/issues/{pr}/comments` to get comments with
/// their numeric database IDs, then filters for comments that contain a review
/// signature (per `text_contains_review_signature`).
///
/// This is the polling-path counterpart to the webhook path which gets the
/// comment ID directly from the webhook payload.
pub(super) fn fetch_review_comment_ids(repo_full_name: &str, pr_number: u64) -> Vec<u64> {
    let endpoint = format!("repos/{}/issues/{}/comments", repo_full_name, pr_number);
    let output = std::process::Command::new("gh")
        .args(["api", "--paginate", "--slurp", &endpoint])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let comments: Vec<serde_json::Value> = match serde_json::from_str(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    warn!(
                        "Failed to parse REST API comments for PR #{}: {}",
                        pr_number, e
                    );
                    return vec![];
                }
            };

            extract_review_comment_ids_from_json(&comments)
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "Failed to fetch REST API comments for PR #{}: {}",
                pr_number,
                stderr.trim()
            );
            vec![]
        }
        Err(e) => {
            warn!(
                "Failed to execute gh api for PR #{} comments: {}",
                pr_number, e
            );
            vec![]
        }
    }
}

/// Check whether a PR has an unupdated "Review in progress" placeholder comment.
///
/// Returns the comment database ID if found, or None if no placeholder exists
/// or if the review has already been completed.
///
/// The placeholder is identified by structured frontmatter `type:review-placeholder`.
pub(super) fn pr_in_progress_placeholder_comment_id(pr_number: u64) -> Option<u64> {
    let output = std::process::Command::new("gh")
        .args(["pr", "view", &pr_number.to_string(), "--json", "comments"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout).ok()?;

    extract_placeholder_comment_id(&json)
}

/// Extract the placeholder comment ID from parsed PR JSON.
///
/// Separated from `pr_in_progress_placeholder_comment_id` for testability.
fn extract_placeholder_comment_id(json: &serde_json::Value) -> Option<u64> {
    let comments = json.get("comments")?.as_array()?;

    // Find the last placeholder comment via structured frontmatter
    for comment in comments.iter().rev() {
        let body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");
        let is_placeholder =
            crate::daemon::helpers::parse_frontmatter(body).is_some_and(|fm| fm.is_placeholder());
        if is_placeholder
            && let Some(id) = comment
                .get("url")
                .and_then(|u| u.as_str())
                .and_then(|url| url.split("issuecomment-").nth(1))
                .and_then(|id_str| id_str.parse::<u64>().ok())
        {
            return Some(id);
        }
    }
    None
}

/// Extract all placeholder comment IDs from parsed PR JSON.
///
/// Unlike `extract_placeholder_comment_id` (which returns only the most recent),
/// this returns all placeholder comment IDs for bulk deletion.
fn extract_all_placeholder_comment_ids(json: &serde_json::Value) -> Vec<u64> {
    let Some(comments) = json.get("comments").and_then(|c| c.as_array()) else {
        return vec![];
    };

    comments
        .iter()
        .filter(|comment| {
            let body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");
            crate::daemon::helpers::parse_frontmatter(body).is_some_and(|fm| fm.is_placeholder())
        })
        .filter_map(|comment| {
            comment
                .get("url")
                .and_then(|u| u.as_str())
                .and_then(|url| url.split("issuecomment-").nth(1))
                .and_then(|id_str| id_str.parse::<u64>().ok())
        })
        .collect()
}

/// Backstop: delete all review placeholder comments on a PR.
///
/// Called when a `type:review` comment is detected via webhook. This handles
/// the case where the final review was posted directly (e.g., via `gh pr comment`)
/// instead of through `midtown pr review post` which would have PATCH'd the
/// placeholder in place.
pub(super) async fn cleanup_review_placeholders(pr_number: u64, repo_full_name: &str) {
    let output = tokio::process::Command::new("gh")
        .args(["pr", "view", &pr_number.to_string(), "--json", "comments"])
        .output()
        .await;

    let output = match output {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            warn!(
                "Failed to list comments for placeholder cleanup on PR #{}: {}",
                pr_number,
                stderr.trim()
            );
            return;
        }
        Err(e) => {
            warn!(
                "Failed to run gh for placeholder cleanup on PR #{}: {}",
                pr_number, e
            );
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = match serde_json::from_str(&stdout) {
        Ok(v) => v,
        Err(_) => return,
    };

    let placeholder_ids = extract_all_placeholder_comment_ids(&json);
    if placeholder_ids.is_empty() {
        return;
    }

    info!(
        "Cleaning up {} review placeholder comment(s) on PR #{}",
        placeholder_ids.len(),
        pr_number
    );

    for comment_id in placeholder_ids {
        let endpoint = format!("/repos/{}/issues/comments/{}", repo_full_name, comment_id);
        let result = tokio::process::Command::new("gh")
            .args(["api", "--method", "DELETE", &endpoint])
            .output()
            .await;
        match result {
            Ok(o) if o.status.success() => {
                info!(
                    "Deleted review placeholder comment {} on PR #{}",
                    comment_id, pr_number
                );
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                warn!(
                    "Failed to delete placeholder comment {}: {}",
                    comment_id,
                    stderr.trim()
                );
            }
            Err(e) => {
                warn!(
                    "Failed to run gh api for placeholder delete {}: {}",
                    comment_id, e
                );
            }
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

    // Skip nudges for blocked external PRs — comment/review webhooks don't
    // carry fork_repo, so the webhook-level gate doesn't catch these.
    {
        let ps = state.persistent_state.lock().await;
        if ps.github.is_blocked_external_pr(pr_number) {
            debug!(
                "PR #{} is a blocked external PR, skipping comment nudge",
                pr_number
            );
            return;
        }
    }

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

        let effect =
            Effect::nudge_channel_lead(state.channel_router.default_channel_name(), lead_nudge_msg);
        crate::daemon::effects::execute_effects(vec![effect], state).await;
        return;
    }

    // Resolve owner via session/task/branch data.
    let owner = resolve_pr_owner_from_state(state, pr_number).await;

    let Some(mut owner) = owner else {
        debug!("PR #{} has no coworker owner, skipping nudge", pr_number);
        return;
    };

    // Check if this PR is linked to a task, and handle based on task status.
    if let Some((task_id, channel_lead_names)) = {
        let ps = state.persistent_state.lock().await;
        let pr_task_map = super::state::pr_to_task_map_from_sessions(&ps.sessions);
        pr_task_map
            .get(&pr_number)
            .cloned()
            .map(|tid| (tid, ps.channel_lead_names()))
    } && let Some(task) = state.task_store.load(&task_id).ok()
    {
        if task.status == crate::task_store::TaskStatus::InProgress {
            // Route the review feedback to the task owner instead of the PR owner.
            // This handles cases where a task was reassigned (e.g., via orphan recovery)
            // and the PR metadata still shows the original author.
            if !task.agent_name.is_empty() {
                let task_owner_active = state
                    .coworkers
                    .list()
                    .iter()
                    .any(|c| c.name.eq_ignore_ascii_case(&task.agent_name));

                if task_owner_active {
                    debug!(
                        "PR #{} linked to task !{} with active owner {} — routing review feedback to task owner instead of PR owner {}",
                        pr_number, task_id, task.agent_name, owner
                    );
                    owner = task.agent_name.clone();
                }
            }
        } else if crate::rules::review_comment_creates_followup(&task.status) {
            // Task is completed — the original coworker session is gone.
            // Only create a follow-up task if the PR was actually opened by a
            // coworker. For non-coworker PRs (lead, channel lead, external),
            // notify @user instead — auto-creating tasks for PRs the daemon
            // didn't author leads to spurious task churn.
            if !is_non_lead_coworker(&owner, &state.project_name, &channel_lead_names) {
                // Check cooldown before notifying — same pattern as all other nudge sites
                {
                    let tracker = state.pr_issue_tracker.lock().await;
                    if !tracker.should_nudge(pr_number, PrIssueType::ReviewComment) {
                        debug!(
                            "PR #{} non-coworker review comment nudge on cooldown, skipping",
                            pr_number
                        );
                        return;
                    }
                }
                debug!(
                    "PR #{} linked to completed task !{} but owner {} is not a coworker — notifying user instead of creating follow-up task",
                    pr_number, task_id, owner
                );
                let user_msg = format!(
                    "@user PR #{} has new review feedback from {} — please address it.",
                    pr_number, activity.actor
                );
                let effects = vec![
                    Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: user_msg,
                        channel: Some(state.channel_router.default_channel_name().to_string()),
                        auto_output: false,
                        message_type: None,
                        nudge_type: None,
                        tool_data: None,
                        provider: None,
                        tool_use_id: None,
                        parent_tool_use_id: None,
                    },
                    Effect::RecordPrNudge {
                        pr_number,
                        issue_type: PrIssueType::ReviewComment,
                    },
                ];
                super::effects::execute_effects(effects, state).await;
                return;
            }

            {
                let tracker = state.pr_issue_tracker.lock().await;
                if !tracker.should_nudge(pr_number, PrIssueType::ReviewComment) {
                    debug!(
                        "PR #{} review comment nudge on cooldown (completed task), skipping",
                        pr_number
                    );
                    return;
                }
            }
            let subject = format!("Address review feedback on PR #{}", pr_number);
            let description = format!(
                "PR #{} received review feedback from {} after task !{} was completed. Please check the PR and address the feedback.",
                pr_number, activity.actor, task_id
            );
            debug!(
                "PR #{} linked to completed task !{} — creating follow-up task for review feedback from {}",
                pr_number, task_id, activity.actor
            );
            let effects = vec![
                Effect::CreateTask {
                    dir_key: state.paths.dir_key().to_string(),
                    subject,
                    description,
                    pr: Some(pr_number),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type: PrIssueType::ReviewComment,
                },
            ];
            super::effects::execute_effects(effects, state).await;
            return;
        }
    }

    // Author posted a comment on their own PR — notify the reviewer
    // (e.g., author is asking a follow-up question about review feedback)
    if owner == activity.actor {
        debug!(
            "PR #{} comment is from author {} — checking for reviewer to notify",
            pr_number, activity.actor
        );

        // Look up the reviewer span and task association from persistent state
        let (reviewer_name, reviewer_session_id, task_id) = {
            let ps = state.persistent_state.lock().await;
            let span = ps.active_reviewer_for_pr(pr_number);
            match span {
                Some(s) => {
                    let name = s.name.clone();
                    let sid = if s.session_id.is_empty() {
                        None
                    } else {
                        Some(s.session_id.clone())
                    };
                    let tid = super::state::pr_to_task_map_from_sessions(&ps.sessions)
                        .get(&pr_number)
                        .cloned();
                    (name, sid, tid)
                }
                None => {
                    debug!("PR #{} has no active reviewer span, skipping", pr_number);
                    return;
                }
            }
        };
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
            vec![Effect::nudge_session(
                state.session_id_for_name(&reviewer_name).await,
                nudge_msg,
            )]
        } else if let Some(session_id) = reviewer_session_id {
            // Reviewer stopped — resume their session with the follow-up context.
            // Override role, provider, and model: coworker() defaults to
            // Coworker/sonnet, but this is a reviewer session that should
            // use Reviewer provider + model (matching startup.rs recovery).
            let mut config = crate::launch::LaunchConfig::resume_reviewer(
                reviewer_name.clone(),
                state.paths.dir_key().to_string(),
                session_id.clone(),
                Some(nudge_msg),
                task_id,
            );
            config.model = super::helpers::resolve_model_for_role(
                state.paths.dir_key(),
                config.auth_provider,
                &config.agent_type,
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

    // Fetch all review content to embed in the nudge so the coworker sees both
    // formal GitHub reviews (e.g., Codex) and Midtown coworker reviews (issue
    // comments). Without this, coworkers running `gh pr view --json reviews`
    // would only see formal reviews and miss coworker issue-comment reviews.
    let review_content = fetch_review_content(pr_number).await;
    let nudge_msg = format!(
        "Your PR #{} has review feedback from {}. Please address it and merge if appropriate.{}",
        pr_number,
        activity.actor,
        review_content.as_deref().unwrap_or("")
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_busy_session_names().await;
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(c.as_str()))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let pr_ctx = {
        let tc = task_channel_map_from_store(&state.task_store);
        let ps = state.persistent_state.lock().await;
        PrContext::from_persistent_state(&ps, pr_number, tc)
    };

    // Decide action using pure decision function with handoff support
    let at_task_limit = state.is_at_task_limit();
    let action = crate::rules::decide_pr_action(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        at_task_limit,
        &nudge_msg,
        crate::rules::PrActionContext::PrComment {
            actor: activity.actor.clone(),
        },
    );

    let action_name = pr_action_name(&action);

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let is_actionable = !matches!(action, crate::rules::PrAction::Skip { .. });
    let mut effects = action_to_effects(
        action,
        pr_number,
        "",
        PrIssueType::ReviewComment,
        state,
        &pr_ctx,
    );

    log_pr_decision(&PrDecisionEntry {
        repo_name: state.paths.dir_key(),
        pr_number,
        title: "",
        owner: &owner,
        issue_type: PrIssueType::ReviewComment,
        action_name,
        effects: &effects,
        ctx: &pr_ctx,
        owner_is_active: active_coworkers.contains(&owner),
        owner_is_idle: idle_coworkers.contains(&owner),
        at_task_limit,
        source: "webhook",
    });

    // If this is a lead/* branch, also nudge the lead so they see review feedback
    if let Some(branch) = get_pr_branch_async(pr_number).await
        && is_lead_branch(&branch)
    {
        let lead_nudge_msg = format!(
            "Your PR #{} has review feedback from {}. Please address it and merge if appropriate.",
            pr_number, activity.actor
        );
        effects.push(Effect::nudge_channel_lead(
            state.channel_router.default_channel_name(),
            lead_nudge_msg,
        ));
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

    // Resolve owner via session/task/branch data.
    let owner = resolve_pr_owner_from_state(state, pr_number).await;

    let Some(owner) = owner else {
        debug!(
            "PR #{} has no coworker owner, skipping webhook {} nudge",
            pr_number, issue_type
        );
        return;
    };

    // Embed review content so the coworker sees the full review body
    // (both formal reviews and Midtown coworker issue comments).
    let review_content = fetch_review_content(pr_number).await;
    let nudge_msg = format!(
        "PR #{} — {}: {}{}",
        pr_number,
        issue_type,
        get_issue_action(issue_type),
        review_content.as_deref().unwrap_or("")
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_busy_session_names().await;
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(c.as_str()))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let pr_ctx = {
        let tc = task_channel_map_from_store(&state.task_store);
        let ps = state.persistent_state.lock().await;
        let mut ctx = PrContext::from_persistent_state(&ps, pr_number, tc);

        // Defense-in-depth: check spans for an active reviewer on this PR.
        if !ctx.has_active_reviewer {
            ctx.has_active_reviewer = ps.active_reviewer_for_pr(pr_number).is_some();
        }

        ctx
    };

    let at_task_limit = state.is_at_task_limit();
    let action = crate::rules::decide_pr_action(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        at_task_limit,
        &nudge_msg,
        crate::rules::PrActionContext::PrIssue,
    );

    let action_name = pr_action_name(&action);

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects = action_to_effects(action, pr_number, "", issue_type, state, &pr_ctx);

    log_pr_decision(&PrDecisionEntry {
        repo_name: state.paths.dir_key(),
        pr_number,
        title: "",
        owner: &owner,
        issue_type,
        action_name,
        effects: &effects,
        ctx: &pr_ctx,
        owner_is_active: active_coworkers.contains(&owner),
        owner_is_idle: idle_coworkers.contains(&owner),
        at_task_limit,
        source: "webhook",
    });

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
    info!(
        "PR #{} CI check '{}' failed (webhook) — resolving owner to nudge",
        pr_number, failure.check_name
    );

    // Check cooldown
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, PrIssueType::CiFailed) {
            info!("PR #{} CI failure nudge on cooldown, skipping", pr_number);
            return;
        }
    }

    // Resolve owner via session/task/branch data.
    let owner = resolve_pr_owner_from_state(state, pr_number).await;

    let Some(owner) = owner else {
        warn!(
            "PR #{} has no coworker owner, cannot nudge about CI failure",
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
    let busy_coworkers = state.get_busy_session_names().await;
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(c.as_str()))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let pr_ctx = {
        let tc = task_channel_map_from_store(&state.task_store);
        let ps = state.persistent_state.lock().await;
        PrContext::from_persistent_state(&ps, pr_number, tc)
    };

    let at_task_limit = state.is_at_task_limit();
    let action = crate::rules::decide_pr_action(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        at_task_limit,
        &nudge_msg,
        crate::rules::PrActionContext::PrIssue,
    );

    let action_name = pr_action_name(&action);

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects = action_to_effects(action, pr_number, "", PrIssueType::CiFailed, state, &pr_ctx);

    log_pr_decision(&PrDecisionEntry {
        repo_name: state.paths.dir_key(),
        pr_number,
        title: "",
        owner: &owner,
        issue_type: PrIssueType::CiFailed,
        action_name,
        effects: &effects,
        ctx: &pr_ctx,
        owner_is_active: active_coworkers.contains(&owner),
        owner_is_idle: idle_coworkers.contains(&owner),
        at_task_limit,
        source: "webhook",
    });

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

/// Reconciles orphaned PRs by nudging the lead when a PR is reviewed + CI green
/// but has no associated in_progress task.
///
/// Uses the pre-computed `open_prs_data` from tick state to avoid I/O.
///
/// This handles the case where a PR was opened under the old lifecycle (task completed
/// on PR open), leaving the PR orphaned with no one to merge it even after review + CI green.
///
/// A PR is considered orphaned if:
/// 1. It has a known branch in the worktree registry or uses the task-* prefix
/// 2. It has a completed review (in `reviewed_prs`)
/// 3. All CI checks are passing (`all_ci_checks_passed`)
/// 4. There's no in_progress task linked to it (not in `pr_task_associations`)
/// 5. The lead has not already been nudged about this PR (`orphaned_pr_lead_nudges_sent`)
///
/// For each orphaned PR, nudges the lead to decide the next action (tell the author
/// to merge, or handle it manually). Does NOT create a task — the lead decides.
///
/// This is the PR equivalent of orphan task recovery. Pure decision function that
/// returns effects, following the same pattern as `reconcile_tasks_in_review()`.
pub fn reconcile_orphaned_prs(
    ps: &super::state::DaemonPersistentState,
    tasks: &[crate::task_store::Task],
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Iterate over open PRs from the tick state
    for pr in &ps.tick_open_prs {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) => n,
            None => continue,
        };

        // Only consider PRs with known branches or task-* prefixes
        let branch = match pr.get("headRefName").and_then(|r| r.as_str()) {
            Some(b) => b,
            None => continue,
        };

        // Check if it's a task branch or lead branch
        let has_valid_prefix = branch.starts_with("task-") || is_lead_branch(branch);

        if !has_valid_prefix {
            continue;
        }

        // Skip if there's a non-completed task linked to this PR via any of three sources:
        // 1. Session-derived associations (ephemeral — lost after session GC)
        // 2. PR title parsing (`github_open_pr_task_ids` — stable while PR is open)
        // 3. Task's own `pr` field on disk (persistent)
        //
        // Sources 2 and 3 exclude completed tasks: a completed task with an
        // unmerged PR is genuinely orphaned — the lead should be nudged to merge.
        //
        // If we previously nudged the lead about this PR, clear the record so
        // re-nudging is possible if the task later completes without merging.
        let has_index_link = ps.tick_pr_task_index.pr_has_task(&pr_number);
        let has_title_link = ps
            .tick_pr_task_index
            .github_task_pr_pairs()
            .any(|(tid, pr)| {
                pr == pr_number
                    && tasks.iter().any(|t| {
                        t.id == *tid && t.status != crate::task_store::TaskStatus::Completed
                    })
            });
        let has_task_pr_link = tasks.iter().any(|t| {
            t.pr == Some(pr_number) && t.status != crate::task_store::TaskStatus::Completed
        });

        if has_index_link || has_title_link || has_task_pr_link {
            if ps.tick_orphaned_pr_nudges_sent.contains(&pr_number) {
                effects.push(Effect::ClearOrphanedPrLeadNudge { pr_number });
            }
            continue;
        }

        // Skip if the lead has already been nudged about this PR (prevents repeated nudges)
        if ps.tick_orphaned_pr_nudges_sent.contains(&pr_number) {
            continue;
        }

        // Check if PR has been reviewed
        if !ps.github.reviewed_prs.contains(&pr_number) {
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
            "Found orphaned PR #{} ({}) - reviewed, CI green, no active task — nudging lead",
            pr_number, title
        );

        // Nudge the lead to decide what to do with this PR
        effects.push(Effect::nudge_channel_lead(
            &ps.tick_project_name,
            format!(
                "PR #{} ({}) is reviewed and CI is green, but has no active task. \
                 Please check the PR and either tell the author to merge it or handle it manually.",
                pr_number, title
            ),
        ));
        // Record that we've nudged the lead so we don't repeat on every tick
        effects.push(Effect::RecordOrphanedPrLeadNudge { pr_number });
    }

    effects
}

/// Polling fallback for PR→task auto-link.
///
/// The webhook path in `mod.rs` emits `SetTaskPr` when a PR is opened with
/// `[Midtown !XXX]` in the title. But if the webhook server was not running
/// or missed the event, the task's `pr` field stays `None` and auto-completion
/// never fires.
///
/// This pure function runs on every `PrPollTick` as a reconciliation pass:
/// for every (task_id, pr_number) pair from the PR task index
/// (derived from open PR titles), it checks whether the corresponding task
/// already has `task.pr` set correctly. If not, it emits `Effect::SetTaskPr`
/// to repair the missing link.
pub fn collect_pr_task_link_effects(
    ps: &super::state::DaemonPersistentState,
    tasks: &[crate::task_store::Task],
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for (task_id_str, pr_number) in ps.tick_pr_task_index.github_task_pr_pairs() {
        // Find the task by ID
        let task = tasks.iter().find(|t| t.id == task_id_str);

        // Only emit if the link is missing or points to the wrong PR.
        // Skip completed tasks — their PR may still be open (e.g., manual close),
        // but emitting SetTaskPr on every tick would cause unnecessary disk writes.
        let needs_link = match task {
            Some(t) if t.status == crate::task_store::TaskStatus::Completed => false,
            Some(t) => t.pr != Some(pr_number),
            None => false, // task not found — skip, nothing to link
        };

        if needs_link {
            effects.push(Effect::SetTaskPr {
                task_id: task_id_str.to_string(),
                pr_number,
                dir_key: ps.tick_dir_key.clone(),
            });
        }
    }

    effects
}

/// Generates CleanupMergedWorktree effects to remove the worktree directory and
/// registry entry after the PR is merged.
///
/// Called during polling ticks to clean up task-based worktrees after
/// their PRs are merged.
pub fn collect_merged_pr_cleanup_effects(ps: &super::state::DaemonPersistentState) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Use pre-computed PR → branch mapping from tick state
    for &pr_num in &ps.tick_merged_pr_numbers {
        if let Some(branch) = ps.tick_merged_pr_branches.get(&pr_num) {
            debug!(
                "PR #{} merged, scheduling worktree cleanup for branch {}",
                pr_num, branch
            );

            // Build a descriptive channel message with task ID when available
            let assignment = ps.worktree_registry.get_by_pr(pr_num);
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
            effects.push(Effect::PostSystemMessage {
                message,
                channel: Some(OPS_CHANNEL.to_string()),
            });
        }
    }

    effects
}

/// Nudge active coworkers with open PRs to rebase after a PR merges to main.
///
/// When a PR merges, other coworkers' branches may drift. This function emits
/// `NudgeCoworker` effects telling each eligible coworker to rebase,
/// along with guidance about re-reading files after the rebase completes.
///
/// Skips:
/// - The coworker whose PR just merged (they're being cleaned up)
/// - Coworkers on the merge-rebase nudge cooldown
/// - Coworkers without an active session (no `name_session_map` entry)
pub fn collect_merge_rebase_nudge_effects(ps: &super::state::DaemonPersistentState) -> Vec<Effect> {
    if ps.tick_merged_pr_numbers.is_empty() {
        return vec![];
    }

    // Only nudge for PRs that haven't already been processed. Without this filter,
    // `gh pr list --state merged --limit 10` returns the same PRs every fetch,
    // causing coworkers to be re-nudged every cooldown cycle for old merges.
    let new_merged_prs: Vec<u64> = {
        let mut prs: Vec<u64> = ps
            .tick_merged_pr_numbers
            .iter()
            .copied()
            .filter(|pr_num| !ps.tick_rebase_nudge_processed_prs.contains(pr_num))
            .collect();
        prs.sort_unstable();
        prs
    };

    if new_merged_prs.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();

    // Build a human-readable list of merged PR numbers for the nudge message
    let pr_list = new_merged_prs
        .iter()
        .map(|n| format!("#{}", n))
        .collect::<Vec<_>>()
        .join(", ");

    let open_pr_coworkers = sessions_with_open_prs(ps);
    let merged_pr_coworkers = sessions_with_merged_prs(ps);

    for coworker_name in &open_pr_coworkers {
        // Skip the coworker(s) whose PR just merged
        if merged_pr_coworkers.contains(coworker_name) {
            continue;
        }

        // Skip coworkers without an active session
        if !ps.tick_name_session_map.contains_key(coworker_name) {
            continue;
        }

        // Skip coworkers on cooldown
        if ps
            .tick_merge_rebase_nudge_cooldown_names
            .contains(coworker_name)
        {
            continue;
        }

        let message = format!(
            "A PR ({pr_list}) just merged to main. Please rebase your branch to pick up the changes:\n\
             1. Run: git fetch origin && git rebase origin/main\n\
             2. Resolve any conflicts if they arise\n\
             3. IMPORTANT: After rebasing, you MUST re-read any file before editing it. \
             The rebase may have changed file contents, and your context window has stale versions. \
             Using the Edit or Write tool without re-reading first could silently revert changes \
             from the merged PR."
        );

        info!(
            coworker = %coworker_name,
            merged_prs = %pr_list,
            "Nudging coworker to rebase after PR merge"
        );

        effects.push(Effect::NudgeCoworker {
            name: coworker_name.clone(),
            message,
            nudge_type: "merge_rebase".to_string(),
            on_success: vec![],
        });
        effects.push(Effect::RecordCooldown {
            category: "merge_rebase_nudge".to_string(),
            key: coworker_name.clone(),
        });
    }

    // Mark these merged PR numbers as processed so they won't trigger nudges
    // on subsequent ticks when `gh pr list --state merged` returns the same PRs.
    for pr_num in &new_merged_prs {
        effects.push(Effect::RecordCooldown {
            category: "merge_rebase_pr_processed".to_string(),
            key: pr_num.to_string(),
        });
    }

    effects
}

// ---------------------------------------------------------------------------
// PR decision logging
// ---------------------------------------------------------------------------

/// Extract the variant name of an Effect as a static string.
///
/// Used by `log_pr_decision` to build a human-readable summary of emitted
/// effects without requiring `Serialize` on the Effect enum.
fn effect_variant_name(e: &Effect) -> &'static str {
    match e {
        Effect::SpawnCoworker(_) => "SpawnCoworker",
        Effect::ShutdownCoworker { .. } => "ShutdownCoworker",
        Effect::ShutdownCoworkerWithCallbacks { .. } => "ShutdownCoworkerWithCallbacks",
        Effect::ResumeCoworker { .. } => "ResumeCoworker",
        Effect::NudgeCoworker { .. } => "NudgeCoworker",
        Effect::PostToChannel { .. } => "PostToChannel",
        Effect::PostSystemMessage { .. } => "PostSystemMessage",
        Effect::BroadcastCoworkerUpdate { .. } => "BroadcastCoworkerUpdate",
        Effect::RecordCooldown { .. } => "RecordCooldown",
        Effect::SetUsageLimitNudge { .. } => "SetUsageLimitNudge",
        Effect::ClearUsageLimitNudge => "ClearUsageLimitNudge",
        Effect::ResetTaskToPending { .. } => "ResetTaskToPending",
        Effect::ClearSessionForTask { .. } => "ClearSessionForTask",
        Effect::ClearSavedSessionId { .. } => "ClearSavedSessionId",
        Effect::ClearSessionWorkingDir { .. } => "ClearSessionWorkingDir",
        Effect::SpawnCoworkerWithCallbacks { .. } => "SpawnCoworkerWithCallbacks",
        Effect::MarkRemindersFired { .. } => "MarkRemindersFired",
        Effect::AdvanceCronEvalTimestamps { .. } => "AdvanceCronEvalTimestamps",
        Effect::RecordPrNudge { .. } => "RecordPrNudge",
        Effect::RecordPermanentPrNudge { .. } => "RecordPermanentPrNudge",
        Effect::RecordTaskAssignment { .. } => "RecordTaskAssignment",
        Effect::RecordReviewerEscalation { .. } => "RecordReviewerEscalation",
        Effect::RecordOrphanedPrLeadNudge { .. } => "RecordOrphanedPrLeadNudge",
        Effect::ClearOrphanedPrLeadNudge { .. } => "ClearOrphanedPrLeadNudge",
        Effect::RerunWorkflow { .. } => "RerunWorkflow",
        Effect::UpdatePrComment { .. } => "UpdatePrComment",
        Effect::LinkPrToSession { .. } => "LinkPrToSession",
        Effect::CompleteTask { .. } => "CompleteTask",
        Effect::ClearBlockedBy { .. } => "ClearBlockedBy",
        Effect::SetTaskPr { .. } => "SetTaskPr",
        Effect::SendPushNotification { .. } => "SendPushNotification",
        Effect::CleanStaleBranches => "CleanStaleBranches",
        Effect::CleanWorktreeTarget { .. } => "CleanWorktreeTarget",
        Effect::CleanupMergedWorktree { .. } => "CleanupMergedWorktree",
        Effect::CleanupStaleWorktree { .. } => "CleanupStaleWorktree",
        Effect::CleanupOrphanedWorktrees { .. } => "CleanupOrphanedWorktrees",
        Effect::GarbageCollectState { .. } => "GarbageCollectState",
        Effect::EnsureWorktree { .. } => "EnsureWorktree",
        Effect::BindCoworkerToWorktree { .. } => "BindCoworkerToWorktree",
        Effect::RegisterWorktreeAssignment { .. } => "RegisterWorktreeAssignment",
        Effect::UpdateRateLimit(_) => "UpdateRateLimit",
        Effect::CreateChannel { .. } => "CreateChannel",
        Effect::ArchiveChannel { .. } => "ArchiveChannel",
        Effect::MergeChannels { .. } => "MergeChannels",
        Effect::AssignTaskChannel { .. } => "AssignTaskChannel",
        Effect::UnassignTask { .. } => "UnassignTask",
        Effect::ResetAbandonedTask { .. } => "ResetAbandonedTask",
        Effect::CreateTask { .. } => "CreateTask",
        Effect::SaveChannelLeadSession { .. } => "SaveChannelLeadSession",
        Effect::MarkProfileLimited { .. } => "MarkProfileLimited",
        Effect::ClearProfileLimit { .. } => "ClearProfileLimit",
        Effect::AutoDetachCoworker { .. } => "AutoDetachCoworker",
        Effect::NudgeChannelLead { .. } => "NudgeChannelLead",
        Effect::NudgeSession { .. } => "NudgeSession",
        Effect::ShutdownSession { .. } => "ShutdownSession",
        Effect::RecordSession { .. } => "RecordSession",
        Effect::MergePr { .. } => "MergePr",
        Effect::AutoMergePr { .. } => "AutoMergePr",
        Effect::PostPrComment { .. } => "PostPrComment",
        Effect::EmitWorkflowEvent(_) => "EmitWorkflowEvent",
        Effect::PostInsight { .. } => "PostInsight",
        Effect::RespawnChannelLead { .. } => "RespawnChannelLead",
        Effect::TaskPrompt { .. } => "TaskPrompt",
        Effect::CreateReviewTask { .. } => "CreateReviewTask",
        Effect::CreateTaskSessionSpan { .. } => "CreateTaskSessionSpan",
        Effect::CloseTaskSessionSpan { .. } => "CloseTaskSessionSpan",
        Effect::SpawnForTask { .. } => "SpawnForTask",
    }
}

/// Captures the full context of a single PR decision for JSONL logging.
struct PrDecisionEntry<'a> {
    repo_name: &'a str,
    pr_number: u64,
    title: &'a str,
    owner: &'a str,
    issue_type: PrIssueType,
    action_name: &'a str,
    effects: &'a [Effect],
    ctx: &'a PrContext,
    owner_is_active: bool,
    owner_is_idle: bool,
    at_task_limit: bool,
    /// "polling" or "webhook" — distinguishes the trigger source.
    source: &'a str,
}

/// Log a single PR decision as a JSONL line to `~/.midtown/projects/<repo>/pr-decisions.jsonl`.
///
/// Each entry captures the full decision context: which PR, what issue was detected,
/// what action the rules engine chose, and which effects were emitted. This creates
/// a corpus for verifying functional equivalence when migrating to workflow scripts.
///
/// Logging failures are silently swallowed (debug-logged) — this must never crash the daemon.
fn log_pr_decision(entry: &PrDecisionEntry<'_>) {
    use std::fs::OpenOptions;
    use std::io::Write;

    let channel = entry.ctx.get_channel(entry.pr_number);
    let task_id = entry.ctx.pr_task_associations.get(&entry.pr_number);

    let json = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "pr": entry.pr_number,
        "title": entry.title,
        "owner": entry.owner,
        "issue": entry.issue_type.to_string(),
        "owner_active": entry.owner_is_active,
        "owner_idle": entry.owner_is_idle,
        "at_task_limit": entry.at_task_limit,
        "has_active_reviewer": entry.ctx.has_active_reviewer,
        "task_id": task_id,
        "channel": channel,
        "action": entry.action_name,
        "source": entry.source,
        "effect_count": entry.effects.len(),
        "effects": entry.effects.iter().map(effect_variant_name).collect::<Vec<_>>(),
    });

    let dir = crate::paths::projects_dir_for_repo(entry.repo_name);
    let path = dir.join("pr-decisions.jsonl");
    if let Err(e) = (|| -> std::io::Result<()> {
        std::fs::create_dir_all(&dir)?;
        let mut file = OpenOptions::new().append(true).create(true).open(&path)?;
        writeln!(file, "{}", json)?;
        Ok(())
    })() {
        debug!("Failed to write PR decision log: {}", e);
    }
}

/// Extract a display name from a `PrAction` variant for logging.
fn pr_action_name(action: &crate::rules::PrAction) -> &'static str {
    match action {
        crate::rules::PrAction::NudgeOwner { .. } => "NudgeOwner",
        crate::rules::PrAction::SpawnOwner { .. } => "SpawnOwner",
        crate::rules::PrAction::PostToChannel { .. } => "PostToChannel",
        crate::rules::PrAction::Skip { .. } => "Skip",
    }
}

// ---------------------------------------------------------------------------
// Post-rebase diff guard
// ---------------------------------------------------------------------------

/// Structured input for the pure rebase regression decision function.
///
/// Populated by `check_for_rebase_regressions()` from git commands, then fed
/// into `evaluate_rebase_regression()` which is a pure function returning effects.
#[derive(Debug, Clone)]
pub struct RebaseRegressionInput {
    /// Coworker name (lowercase).
    pub coworker_name: String,
    /// Files changed on main since the merge-base (what the rebase brought in).
    pub files_changed_on_main: HashSet<String>,
    /// Files modified by recent post-rebase commits.
    pub files_in_recent_commits: HashSet<String>,
    /// Whether a rebase was detected in the reflog within the lookback window.
    pub rebase_detected: bool,
}

/// Pure decision function: given structured rebase regression data, determine
/// whether to flag a regression and return the appropriate effects.
///
/// Returns effects (nudge + cooldown + ops message) if:
/// 1. A recent rebase was detected in the reflog
/// 2. Recent commits touch files that also changed on main during rebase
///
/// This function does NO I/O — it only examines the structured input.
pub fn evaluate_rebase_regression(input: &RebaseRegressionInput) -> Vec<Effect> {
    if !input.rebase_detected {
        return vec![];
    }

    let overlapping_files: Vec<&String> = input
        .files_in_recent_commits
        .iter()
        .filter(|f| input.files_changed_on_main.contains(*f))
        .collect();

    if overlapping_files.is_empty() {
        return vec![];
    }

    let mut sorted_files: Vec<&str> = overlapping_files.iter().map(|s| s.as_str()).collect();
    sorted_files.sort_unstable();
    let file_list = sorted_files.join(", ");

    let nudge_message = format!(
        "⚠️ Post-rebase regression detected: your recent commit(s) modified files that also \
         changed on main during your rebase. Please verify you haven't accidentally reverted \
         merged changes in these files: {file_list}\n\
         Re-read these files with the Read tool and check your changes against what's on main."
    );

    let ops_message = format!(
        "⚠️ Rebase regression warning for {}: post-rebase commits touch files that changed \
         on main ({file_list})",
        input.coworker_name
    );

    vec![
        Effect::NudgeCoworker {
            name: input.coworker_name.clone(),
            message: nudge_message,
            nudge_type: "rebase_regression".to_string(),
            on_success: vec![],
        },
        Effect::RecordCooldown {
            category: "rebase_regression".to_string(),
            key: input.coworker_name.clone(),
        },
        Effect::PostSystemMessage {
            message: ops_message,
            channel: Some(OPS_CHANNEL.to_string()),
        },
    ]
}

/// Run a git command in a worktree directory and return stdout lines.
///
/// Returns an empty vec on any error (worktree missing, git not found, etc.).
fn run_git_in_worktree(working_dir: &str, args: &[&str]) -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(working_dir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        }
        Ok(out) => {
            debug!(
                dir = %working_dir,
                args = ?args,
                stderr = %String::from_utf8_lossy(&out.stderr),
                "git command failed in worktree"
            );
            vec![]
        }
        Err(e) => {
            debug!(
                dir = %working_dir,
                args = ?args,
                error = %e,
                "git command error in worktree"
            );
            vec![]
        }
    }
}

/// Check for post-rebase regressions across all open PRs.
///
/// For each coworker with an open PR:
/// 1. Finds their worktree via session records
/// 2. Checks the git reflog for a recent `rebase (finish)` entry (last 30 min)
/// 3. If rebased, compares files changed on main vs files in recent commits
/// 4. If overlap detected, nudges the coworker and posts to ops
///
/// Called from `evaluate_tick(PrPollTick)`.
pub async fn check_for_rebase_regressions(ps: &super::state::DaemonPersistentState) -> Vec<Effect> {
    let mut effects = Vec::new();
    let open_pr_coworkers = sessions_with_open_prs(ps);

    for coworker_name in &open_pr_coworkers {
        // Skip coworkers on cooldown
        if ps
            .tick_rebase_regression_cooldown_names
            .contains(coworker_name)
        {
            continue;
        }

        // Skip coworkers without an active session
        if !ps.tick_name_session_map.contains_key(coworker_name) {
            continue;
        }

        // Find the working directory for this coworker's session
        let working_dir = ps
            .tick_name_session_map
            .get(coworker_name)
            .and_then(|sid| ps.sessions.get(sid))
            .map(|rec| rec.working_dir.as_str())
            .unwrap_or("");

        if working_dir.is_empty() || !std::path::Path::new(working_dir).exists() {
            continue;
        }

        // Spawn blocking git work on a thread pool to avoid blocking the tokio runtime
        let wd = working_dir.to_string();
        let cw_name = coworker_name.clone();
        let result =
            tokio::task::spawn_blocking(move || collect_rebase_regression_input(&wd, &cw_name))
                .await;

        match result {
            Ok(Some(input)) => {
                effects.extend(evaluate_rebase_regression(&input));
            }
            Ok(None) => {}
            Err(e) => {
                debug!(
                    coworker = %coworker_name,
                    error = %e,
                    "Failed to collect rebase regression input"
                );
            }
        }
    }

    effects
}

/// Maximum number of recent commits to check for post-rebase regressions.
const REBASE_REGRESSION_RECENT_COMMITS: usize = 3;

/// Collect git data for rebase regression analysis (runs on blocking thread).
///
/// Returns `None` if the worktree is not a valid git repo or if no rebase
/// was detected recently.
///
/// **Key insight on merge-base after rebase**: After `git rebase origin/main`,
/// `merge-base HEAD origin/main` returns `origin/main` itself (the branch now
/// has `origin/main` as an ancestor), so `merge_base..origin/main` would be
/// empty. Instead, we extract the pre-rebase HEAD from the reflog to find the
/// original fork point, then compute files that changed on main between the
/// pre-rebase fork point and `origin/main`.
fn collect_rebase_regression_input(
    working_dir: &str,
    coworker_name: &str,
) -> Option<RebaseRegressionInput> {
    // Get reflog with both subject (%gs) and commit hash (%H) plus timestamp (%ci)
    let reflog_lines =
        run_git_in_worktree(working_dir, &["reflog", "--format=%H %gs %ci", "-n", "50"]);

    // Find the pre-rebase HEAD: the reflog entry BEFORE the most recent
    // `rebase (start)` entry. This is the commit the branch pointed to before
    // the rebase began. We need this to compute the original merge-base with
    // origin/main (before the rebase moved the branch).
    let mut rebase_start_idx = None;
    let mut rebase_finish_recent = false;

    for (i, line) in reflog_lines.iter().enumerate() {
        if line.contains("rebase (finish)") && !rebase_finish_recent {
            rebase_finish_recent = parse_reflog_timestamp_is_recent(
                line,
                super::constants::REBASE_REGRESSION_WINDOW_SECS as i64,
            );
        }
        if line.contains("rebase (start)") && rebase_finish_recent {
            rebase_start_idx = Some(i);
            break;
        }
    }

    if !rebase_finish_recent {
        return None;
    }

    // The entry after rebase (start) in the reflog is the pre-rebase HEAD
    // (reflog is newest-first, so "after" means index + 1)
    let pre_rebase_sha = rebase_start_idx
        .and_then(|idx| reflog_lines.get(idx + 1))
        .and_then(|line| line.split_whitespace().next())
        .map(|s| s.to_string());

    // Compute files changed on main using the pre-rebase fork point
    let files_changed_on_main = if let Some(ref pre_rebase_head) = pre_rebase_sha {
        // Find the original merge-base (before rebase moved the branch)
        let original_merge_base_lines =
            run_git_in_worktree(working_dir, &["merge-base", pre_rebase_head, "origin/main"]);
        if let Some(original_base) = original_merge_base_lines.first() {
            let main_changed = run_git_in_worktree(
                working_dir,
                &[
                    "diff",
                    "--name-only",
                    &format!("{original_base}..origin/main"),
                ],
            );
            main_changed.into_iter().collect::<HashSet<String>>()
        } else {
            return None;
        }
    } else {
        // Fallback: if we can't find the pre-rebase HEAD (e.g., reflog was pruned),
        // use a simpler heuristic — check what's different between the branch and main.
        // This is less precise but better than skipping entirely.
        return None;
    };

    if files_changed_on_main.is_empty() {
        return None;
    }

    // Get commits on the PR branch after origin/main (the coworker's own commits)
    let pr_commits = run_git_in_worktree(working_dir, &["log", "--format=%H", "origin/main..HEAD"]);

    // Look at only the most recent commits (post-rebase work)
    let recent_commits: Vec<&String> = pr_commits
        .iter()
        .take(REBASE_REGRESSION_RECENT_COMMITS)
        .collect();
    if recent_commits.is_empty() {
        return None;
    }

    // Collect files touched by recent commits
    let mut files_in_recent_commits = HashSet::new();
    for commit in &recent_commits {
        let files = run_git_in_worktree(
            working_dir,
            &["diff", "--name-only", &format!("{commit}^..{commit}")],
        );
        files_in_recent_commits.extend(files);
    }

    Some(RebaseRegressionInput {
        coworker_name: coworker_name.to_string(),
        files_changed_on_main,
        files_in_recent_commits,
        rebase_detected: true,
    })
}

/// Parse a reflog line's timestamp and check if it's within `max_age_secs` of now.
///
/// Reflog lines from `--format=%gs %ci` look like:
///   "rebase (finish): returning to refs/heads/branch 2026-03-11 10:30:00 -0700"
///
/// Returns `true` if the timestamp is recent enough, or `true` if parsing fails
/// (fail-open: better to check than miss a regression).
fn parse_reflog_timestamp_is_recent(line: &str, max_age_secs: i64) -> bool {
    static TIMESTAMP_RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2} [+-]\d{4})").unwrap()
    });

    let Some(caps) = TIMESTAMP_RE.captures(line) else {
        return true; // fail-open
    };

    let timestamp_str = &caps[1];
    match chrono::DateTime::parse_from_str(timestamp_str, "%Y-%m-%d %H:%M:%S %z") {
        Ok(ts) => {
            let age = chrono::Utc::now().signed_duration_since(ts);
            age.num_seconds() < max_age_secs
        }
        Err(_) => true, // fail-open
    }
}

#[path = "pr_tests.rs"]
#[cfg(test)]
#[allow(
    unused_variables,
    unused_mut,
    clippy::vec_init_then_push,
    clippy::field_reassign_with_default
)]
mod tests;

#[path = "pr_review_feedback_tests.rs"]
#[cfg(test)]
mod review_feedback_tests;

#[path = "pr_ci_retry_tests.rs"]
#[cfg(test)]
mod ci_retry_tests;

#[path = "pr_rebase_nudge_tests.rs"]
#[cfg(test)]
mod rebase_nudge_tests;

#[path = "pr_diff_guard_tests.rs"]
#[cfg(test)]
mod diff_guard_tests;

#[path = "pr_name_collision_tests.rs"]
#[cfg(test)]
mod name_collision_tests;
