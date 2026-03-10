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

use crate::daemon_messages;

use super::DaemonState;
use super::constants::*;
use super::effects::Effect;
use super::helpers::is_lead_branch;
use super::helpers::*;
use super::snapshot::WorldSnapshot;
use super::trackers::{PrIssueType, StuckConditionType};

/// Get list of coworker names who have open PRs.
///
/// Coworkers with open PRs should NEVER be sent on a break.
///
/// Uses cached data from the latest `poll_prs_for_issues` call.
/// Returns empty on the first tick before the poll populates the cache.
pub(super) fn get_coworkers_with_open_prs(state: &DaemonState) -> Vec<String> {
    let cache = state.pr_coworker_cache.read().unwrap();
    cache.open_pr_owners.iter().cloned().collect()
}

/// Resolve a PR's owner via the session-centric path:
/// PR number → task_id → session_id → session.current_name (or preferred_name).
///
/// Returns `Some(name)` if a session record exists with a name allocation,
/// or `None` if any link in the chain is missing (no task association, no session,
/// or session has neither current_name nor preferred_name).
///
/// This gives session-based routing priority over branch-based lookup. When a
/// coworker is reassigned to a different name on restart, the session record
/// tracks the current name, so PRs route to the correct coworker.
///
/// Falls back to `preferred_name` when the session is suspended and has released
/// `current_name`. This handles the case where a coworker finishes and releases its
/// name but `preferred_name` still identifies who authored the PR.
fn resolve_pr_owner_from_session(
    pr_number: u64,
    pr_task_associations: &HashMap<u64, String>,
    session_task_map: &HashMap<String, String>,
    sessions: &HashMap<String, super::state::SessionRecord>,
) -> Option<String> {
    let task_id = pr_task_associations.get(&pr_number)?;
    let session_id = session_task_map.get(task_id)?;
    let session = sessions.get(session_id)?;
    session
        .current_name
        .clone()
        .or_else(|| session.preferred_name.clone())
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
    /// Session-centric resume info: if the PR's task has a stopped session,
    /// this holds the session_id for resume.
    task_session_id: Option<String>,
    /// Whether this PR has an active reviewer (assigned or in reviewing phase).
    /// Used to suppress both `PrApproved` workflow events AND inline nudge effects
    /// while a reviewer is still working, so the contract remains:
    /// "pr.approved = safe to merge".
    has_active_reviewer: bool,
    /// Channel→workflow assignments for checking if a channel has a workflow.
    channel_workflows: HashMap<String, String>,
}

impl PrContext {
    /// Extract all PR decision context from persistent state for a given PR.
    ///
    /// Caller must hold `persistent_state.lock().await`. This method reads
    /// channel routing data (shared across all PRs) and session context
    /// (specific to `pr_number`) in a single pass.
    fn from_persistent_state(ps: &super::state::DaemonPersistentState, pr_number: u64) -> Self {
        let pr_task_associations = ps.github.pr_to_task_map();

        let session_context =
            ps.github
                .get_pr_author_session(pr_number)
                .map(|s| crate::rules::PrSessionContext {
                    session_id: s.session_id.clone(),
                    branch: s.branch.clone(),
                    original_author: s.original_author.clone(),
                    pr_number,
                });

        // Session-centric resume: PR → task → session
        let task_session_id = pr_task_associations.get(&pr_number).and_then(|task_id| {
            ps.sessions
                .values()
                .find(|s| s.task_id.as_deref() == Some(task_id))
                .map(|s| s.session_id.clone())
        });

        // Gate check: reviewer assigned in github-state (raw presence, no timeout).
        // Uses get_reviewer() like the RPC merge gate (!1896) so the workflow event
        // layer stays consistent with the merge enforcement layer.
        //
        // Bypass: if the review is already cached (complete), don't suppress
        // PrApproved even if the assignment hasn't been cleared yet. This handles
        // the race between webhook review completion and poll-tick assignment removal.
        let has_active_reviewer =
            ps.github.get_reviewer(pr_number).is_some() && !ps.github.has_cached_review(pr_number);

        Self {
            pr_task_associations,
            task_channel: ps.task_channel.clone(),
            session_context,
            task_session_id,
            has_active_reviewer,
            channel_workflows: ps.channel_workflows.clone(),
        }
    }

    /// Extract only channel routing data (when session context isn't needed).
    ///
    /// Note: `has_active_reviewer` defaults to `false` because this constructor
    /// is only used for `ReviewComplete` contexts where the reviewer has already
    /// finished. Do NOT use this for `PrIssueType::Approved` code paths.
    fn routing_only(ps: &super::state::DaemonPersistentState) -> Self {
        Self {
            pr_task_associations: ps.github.pr_to_task_map(),
            task_channel: ps.task_channel.clone(),
            session_context: None,
            task_session_id: None,
            has_active_reviewer: false,
            channel_workflows: ps.channel_workflows.clone(),
        }
    }

    /// Look up the topic channel for a PR based on its associated task.
    fn get_channel(&self, pr_number: u64) -> Option<String> {
        let task_id = self.pr_task_associations.get(&pr_number)?;
        self.task_channel.get(task_id).cloned()
    }

    /// Defense-in-depth: check snapshot signals for an active reviewer.
    ///
    /// Uses OR logic to catch two independent edge cases:
    /// 1. Coworker in Reviewing phase with a matching assignment for this PR
    /// 2. Assignment exists (in snapshot) but coworker hasn't entered Reviewing phase yet
    ///
    /// Either signal independently indicates the reviewer is still working.
    /// A coworker in Reviewing phase with no assignment (cleared) or an
    /// assignment to a *different* PR does not count — without PR-specific
    /// evidence we cannot suppress PrApproved for an unrelated PR.
    fn augment_reviewer_from_snapshot(
        &mut self,
        pr_number: u64,
        snap: &super::snapshot::WorldSnapshot,
    ) {
        if self.has_active_reviewer {
            return; // Already flagged via get_reviewer()
        }

        // Signal A: any coworker assigned to this PR in the snapshot
        let has_snapshot_assignment = snap
            .reviewer
            .reviewer_pr_assignments
            .iter()
            .any(|(_, &assigned_pr)| assigned_pr == pr_number);

        // Signal B: any coworker in Reviewing phase assigned to this PR
        let has_reviewing_phase = snap.reviewer.reviewing_phase_coworkers.iter().any(|name| {
            snap.reviewer.reviewer_pr_assignments.get(name).copied() == Some(pr_number)
        });

        self.has_active_reviewer = has_snapshot_assignment || has_reviewing_phase;
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

/// Get coworker names that have recently merged PRs.
///
/// Uses a time-based cache to reduce API calls. Merged PR status is only refreshed
/// every 5 minutes since merge events aren't time-critical.
///
/// The `branch_owners` map (from the worktree registry) is needed to resolve
/// task-based branch names (e.g., "task-42-fix-auth") to coworker names.
pub(super) fn get_coworkers_with_merged_prs(
    state: &DaemonState,
    branch_owners: &HashMap<String, String>,
) -> HashSet<String> {
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
                    .filter_map(|b| coworker_from_branch(b, branch_owners))
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
pub(super) fn detect_abandoned_pr_tasks(
    snap: &WorldSnapshot,
    open_pr_numbers: &[u64],
    dir_key: &str,
) -> Vec<Effect> {
    let open_set: HashSet<u64> = open_pr_numbers.iter().copied().collect();
    let mut effects = Vec::new();

    // Check each PR with an associated task ID
    for (pr_number, task_id) in &snap.pr.pr_task_associations {
        // PR is closed if it's not in the open set and wasn't merged
        let is_closed = !open_set.contains(pr_number);
        let is_merged = snap.pr.merged_pr_numbers.contains(pr_number);

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
                        snap.pr
                            .pr_task_associations
                            .iter()
                            .any(|(other_pr, other_task_id)| {
                                other_task_id == task_id
                                    && other_pr != pr_number
                                    && snap.pr.merged_pr_numbers.contains(other_pr)
                            });

                    // Check if task.pr field points to a merged PR
                    let task_pr_merged = task
                        .and_then(|t| t.pr)
                        .map(|pr| snap.pr.merged_pr_numbers.contains(&pr))
                        .unwrap_or(false);

                    task_completed || has_merged_sibling || task_pr_merged
                };

                if !work_already_landed {
                    effects.push(Effect::ResetAbandonedTask {
                        task_id: task_id.clone(),
                        pr_number: *pr_number,
                        dir_key: dir_key.to_string(),
                    });
                }
            }
        }
    }

    effects
}

/// Resolve the owner of a PR from snapshot data.
///
/// Tries session-based resolution first (PR# → task → session → current_name),
/// then task-based metadata from snapshot tasks (PR title/task ID, branch
/// task ID pattern, and task.pr field), then branch-based lookup via the
/// worktree registry's branch_owners map, and finally PR body frontmatter
/// (`<!-- midtown: name -->`) as a crash-resilient fallback.
/// Returns `None` if no path yields an owner.
fn resolve_pr_owner(pf: &PrFields<'_>, snap: &WorldSnapshot) -> Option<String> {
    resolve_pr_owner_from_session(
        pf.number,
        &snap.pr.pr_task_associations,
        &snap.session_task_map,
        &snap.sessions,
    )
    .or_else(|| {
        resolve_pr_owner_from_task_metadata(pf.number, pf.title, pf.head_ref, &snap.all_tasks)
    })
    .or_else(|| coworker_from_branch(pf.head_ref, &snap.worktree_branch_owners))
    .or_else(|| resolve_pr_owner_from_body(pf.body()))
}

fn extract_task_id_from_head_ref(head_ref: &str) -> Option<u64> {
    head_ref
        .rsplit('/')
        .next()
        .and_then(|branch| branch.strip_prefix("task-"))
        .and_then(|rest| rest.split('-').next())
        .and_then(|task_id| task_id.parse().ok())
}

fn resolve_pr_owner_from_task_metadata(
    pr_number: u64,
    title: &str,
    head_ref: &str,
    all_tasks: &[crate::tasks::Task],
) -> Option<String> {
    let owner_for_task_id = |task_id: u64, all_tasks: &[crate::tasks::Task]| {
        let task_id_str = task_id.to_string();
        all_tasks
            .iter()
            .find(|task| task.id == task_id_str && task.owner.is_some())
            .and_then(|task| task.owner.clone())
    };

    if let Some(task_id) = crate::tasks::extract_task_id_from_pr_title(title)
        && let Some(owner) = owner_for_task_id(task_id, all_tasks)
    {
        return Some(owner);
    }

    if let Some(task_id) = extract_task_id_from_head_ref(head_ref)
        && let Some(owner) = owner_for_task_id(task_id, all_tasks)
    {
        return Some(owner);
    }

    all_tasks
        .iter()
        .find(|task| task.pr == Some(pr_number) && task.owner.is_some())
        .and_then(|task| task.owner.clone())
}

/// Extract coworker name from PR body `<!-- midtown: name -->` frontmatter.
///
/// This is a crash-resilient fallback: the frontmatter lives on GitHub and
/// survives daemon restarts, auth storms, and session record loss. All
/// coworker PRs include this frontmatter per the system prompt convention.
fn resolve_pr_owner_from_body(body: &str) -> Option<String> {
    let marker = "midtown:";
    let marker_pos = body.find(marker)?;
    let before = &body[..marker_pos];
    if !before.contains("<!--") {
        return None;
    }
    let after_marker = &body[marker_pos + marker.len()..];
    let end = after_marker.find("-->")?;
    let name = after_marker[..end].trim();
    // Ignore "midtown" — that's the lead, not a coworker
    if name.is_empty() || name.eq_ignore_ascii_case("midtown") {
        None
    } else {
        Some(name.to_string())
    }
}

/// Resolve PR owner from persistent state (used by webhook handlers).
///
/// Locks persistent_state once, tries session → task metadata → branch →
/// PR worktree registry → fallback (caller-provided, typically from body
/// frontmatter via `determine_pr_coworker()`).
async fn resolve_pr_owner_from_state(
    state: &DaemonState,
    pr_number: u64,
    title: &str,
    head_ref: Option<&str>,
    owner_fallback: Option<&str>,
) -> Option<String> {
    let all_tasks = crate::tasks::read_tasks_for_repo(Some(state.paths.dir_key()));
    let head = head_ref.unwrap_or("");

    let owner = {
        let ps = state.persistent_state.lock().await;
        let pr_task_associations = ps.github.pr_to_task_map();

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

        let branch_owners: HashMap<String, String> = ps
            .worktree_registry
            .all_assignments()
            .values()
            .filter_map(|assignment| {
                assignment
                    .current_coworker
                    .as_ref()
                    .map(|owner| (assignment.branch_name.clone(), owner.clone()))
            })
            .collect();

        resolve_pr_owner_from_session(
            pr_number,
            &pr_task_associations,
            &session_task_map,
            &ps.sessions,
        )
        .or_else(|| resolve_pr_owner_from_task_metadata(pr_number, title, head, &all_tasks))
        .or_else(|| coworker_from_branch(head, &branch_owners))
        .or_else(|| {
            ps.worktree_registry
                .get_by_pr(pr_number)
                .and_then(|assignment| assignment.current_coworker.clone())
        })
    };

    owner.or_else(|| owner_fallback.map(|o| o.to_string()))
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
/// Cleans up: PR issue tracker, persistent state (expired reviewer assignments,
/// session ID backfill, stale webhook events), cooldowns, and RPC response cache.
/// Preserves assignments for running reviewer coworkers so active reviews aren't
/// interrupted.
async fn cleanup_pr_tracking_state(
    state: &DaemonState,
    snap: &WorldSnapshot,
    running_coworker_names: &HashSet<String>,
    running_reviewer_session_ids: &HashSet<String>,
    review_branch_owners: &HashSet<String>,
) {
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.cleanup();
    }
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .cleanup_expired_preserving(running_coworker_names, Some(running_reviewer_session_ids));
        // Backfill reviewer_session_id for assignments created before the session
        // started (optimistic assignment pattern: assign before spawn completes).
        let reviewer_session_map: HashMap<String, String> = snap
            .coworkers
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
}

/// Update PR-related caches and detect abandoned PRs.
///
/// Updates: open PR owner cache, formatted PR data for RPC, CI-passed owner cache,
/// PR break sessions. Also detects abandoned PRs (closed without merge) and cleans
/// up persistent reviewer assignments for closed PRs.
async fn update_pr_caches(
    state: &DaemonState,
    snap: &WorldSnapshot,
    prs: &[serde_json::Value],
    running_coworker_names: &HashSet<String>,
    running_reviewer_session_ids: &HashSet<String>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Cache open PR owners for reuse by get_coworkers_with_open_prs
    {
        let owners: HashSet<String> = prs
            .iter()
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(|branch| coworker_from_branch(branch, &snap.worktree_branch_owners))
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.open_pr_owners = owners;
    }

    // Cache full open PR data for RPC responses (avoids gh CLI calls in handle_status).
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
                let pf = PrFields::from_json(pr);
                let status = format_pr_status_for_rpc(pr);
                let task_id = crate::tasks::extract_task_id_from_pr_title(pf.title);
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
                    .and_then(|branch| coworker_from_branch(branch, &snap.worktree_branch_owners))
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.ci_passed_pr_owners = ci_passed;
        cache.pr_poll_initialized = true;
    }

    // Cleanup saved PR break sessions for coworkers whose PRs are no longer open
    {
        let active_pr_coworkers: HashSet<String> = prs
            .iter()
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(|branch| coworker_from_branch(branch, &snap.worktree_branch_owners))
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
    effects.extend(detect_abandoned_pr_tasks(
        snap,
        &open_pr_numbers,
        state.paths.dir_key(),
    ));

    // Clean up persistent reviewer assignments for PRs that are no longer open.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_closed_prs(&open_pr_numbers);
        ps.github
            .cleanup_expired_preserving(running_coworker_names, Some(running_reviewer_session_ids));
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!("Failed to save daemon-state.json after cleanup: {}", e);
        }
    }

    effects
}

/// Process per-PR issue detection and generate nudge effects.
///
/// For each non-draft PR: resolves the owner, detects actionable issues (merge
/// conflicts, CI failures, review status), handles orphaned PRs, and generates
/// nudge effects using the author-driven merge decision model.
async fn process_pr_issue_nudges(
    snap: &WorldSnapshot,
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

        // Session-first, task metadata fallback, then branch prefix fallback.
        // PR# → task → session → name → branch prefix → None.
        let owner_opt = resolve_pr_owner(&pf, snap);
        let issues = detect_pr_issues(pr);

        // Handle PRs whose owner is not currently active (on break, never spawned, etc.)
        if let Some(ref owner) = owner_opt {
            let has_active_worktree = snap.worktree_branch_owners.values().any(|o| o == owner)
                || snap.worktree_branch_owners.contains_key(pf.head_ref);

            if !has_active_worktree && !issues.is_empty() {
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
            let should_nudge = {
                let tracker = state.pr_issue_tracker.lock().await;
                tracker.should_nudge(pf.number, issue_type)
            };

            if !should_nudge {
                continue;
            }

            use crate::rules::decide_pr_issue_action_with_handoff;

            let review_content = match issue_type {
                PrIssueType::ChangesRequested | PrIssueType::Approved => {
                    fetch_review_content(pf.number).await
                }
                _ => None,
            };

            let message = format!(
                "PR #{} ({}) - {}: {}{}",
                pf.number,
                truncate_str(pf.title, 40),
                issue_type,
                get_issue_action(issue_type),
                review_content.as_deref().unwrap_or("")
            );

            let (mut pr_ctx, channel_lead_names) = {
                let ps = state.persistent_state.lock().await;
                (
                    PrContext::from_persistent_state(&ps, pf.number),
                    ps.channel_lead_names(),
                )
            };

            pr_ctx.augment_reviewer_from_snapshot(pf.number, snap);

            let at_dev_limit = state.is_at_dev_limit(&channel_lead_names);
            let action = decide_pr_issue_action_with_handoff(
                &owner,
                active_coworkers,
                idle_coworkers,
                at_dev_limit,
                pr_ctx.session_context.as_ref(),
                &message,
            );

            let action_name = pr_action_name(&action);

            let new_effects =
                pr_action_to_effects(action, pf.number, pf.title, issue_type, state, &pr_ctx);

            log_pr_decision(&PrDecisionEntry {
                repo_name: state.paths.dir_key(),
                pr_number: pf.number,
                title: pf.title,
                owner: &owner,
                issue_type,
                action_name,
                effects: &new_effects,
                ctx: &pr_ctx,
                owner_is_active: active_coworkers.contains(&owner),
                owner_is_idle: idle_coworkers.contains(&owner),
                at_dev_limit,
                source: "polling",
            });

            effects.extend(new_effects);
        }
    }

    effects
}

/// Update review status caches after processing PR issues.
///
/// Pre-collects review status, computes prs_needing_review count, and caches
/// coworker names whose PRs have CI passed + review feedback.
fn update_review_status_cache(
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
    branch_owners: &HashMap<String, String>,
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

    let review_feedback: HashSet<String> = prs
        .iter()
        .filter(|pr| {
            let pf = PrFields::from_json(pr);
            all_ci_checks_passed(pr)
                && reviewed_prs.contains(&pf.number)
                && pf.review_decision() != "APPROVED"
        })
        .filter_map(|pr| {
            pr.get("headRefName")
                .and_then(|r| r.as_str())
                .and_then(|branch| coworker_from_branch(branch, branch_owners))
        })
        .collect();

    let mut cache = state.pr_coworker_cache.write().unwrap();
    cache.prs_needing_review = prs_needing_review;
    cache.review_feedback_pr_owners = review_feedback;
}

/// Detect external/fork PRs from polling data and record them in persistent state.
///
/// Compares each PR's `headRepositoryOwner` against the base repo owner.
/// For newly detected external PRs, generates a channel notification effect
/// directed at the user (not agents).
async fn detect_and_block_external_prs(
    state: &DaemonState,
    snap: &WorldSnapshot,
    prs: &[serde_json::Value],
) -> Vec<Effect> {
    let mut effects = Vec::new();
    let repo_owner = match snap.repo_owner.as_deref() {
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
                channel: if snap.default_channel.is_empty() {
                    None
                } else {
                    Some(snap.default_channel.clone())
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
pub(super) async fn poll_prs_for_issues(
    snap: &WorldSnapshot,
    state: &DaemonState,
) -> Result<Vec<Effect>, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Polling PRs for actionable issues...");

    let mut effects: Vec<Effect> = Vec::new();

    // Get list of active coworkers from snapshot (consistent with other tick handlers)
    let active_coworkers: Vec<String> = snap
        .coworkers
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
        .coworkers
        .running_coworkers
        .iter()
        .map(|c| c.name.clone())
        .filter(|name| {
            review_branch_owners.contains(&name.to_lowercase())
                && !snap
                    .health
                    .usage_limited_coworkers
                    .contains(&name.to_lowercase())
        })
        .collect();
    // Build session ID set for same reviewer-subset — enables session-based matching
    // in cleanup_expired_preserving when assignments carry a reviewer_session_id.
    let running_reviewer_session_ids: HashSet<String> = snap
        .coworkers
        .running_coworkers
        .iter()
        .filter(|c| {
            review_branch_owners.contains(&c.name.to_lowercase())
                && !snap
                    .health
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

    cleanup_pr_tracking_state(
        state,
        snap,
        &running_coworker_names,
        &running_reviewer_session_ids,
        &review_branch_owners,
    )
    .await;

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
    effects.extend(detect_and_block_external_prs(state, snap, &prs).await);

    // Collect ALL open PR numbers before filtering, so cleanup_closed_external_prs
    // sees the full set and doesn't purge still-open blocked external PRs.
    let all_open_pr_numbers: Vec<u64> = prs
        .iter()
        .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
        .collect();

    let prs: Vec<serde_json::Value> = {
        let ps = state.persistent_state.lock().await;
        prs.into_iter()
            .filter(|pr| {
                let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                !ps.github.is_blocked_external_pr(pr_number)
            })
            .collect()
    };

    effects.extend(
        update_pr_caches(
            state,
            snap,
            &prs,
            &running_coworker_names,
            &running_reviewer_session_ids,
        )
        .await,
    );

    // Clean up external PR tracking for truly closed PRs, using the unfiltered
    // open PR list so blocked-but-still-open external PRs are preserved.
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_closed_external_prs(&all_open_pr_numbers);
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!(
                "Failed to save daemon-state.json after external PR cleanup: {}",
                e
            );
        }
    }

    effects.extend(
        process_pr_issue_nudges(snap, state, &prs, &active_coworkers, &idle_coworkers).await,
    );

    // Polling fallback for review comment notifications (when webhooks are degraded)
    effects.extend(
        collect_comment_notification_effects(snap, state, &prs, &active_coworkers, &idle_coworkers)
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
    effects.extend(collect_reviewer_effects(snap, state, &prs, &pre_fetched_review_content).await);

    update_review_status_cache(state, &prs, &reviewed_prs, &snap.worktree_branch_owners);

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
            &snap.worktree_branch_owners,
            review_mode,
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
    snap: &WorldSnapshot,
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
        let owner = match resolve_pr_owner(&pf, snap) {
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

        // Use pre-fetched review content (fetched at the top of poll_prs_for_issues
        // to keep this function free of I/O — CLAUDE.md: "Decision functions are pure").
        let review_suffix = pre_fetched_review_content
            .get(&pr_number)
            .map(|s| s.as_str())
            .unwrap_or("");
        let message = format!(
            "PR #{} ({}) - {}: {}{}",
            pr_number,
            truncate_str(pf.title, 40),
            PrIssueType::GreenWithFeedback,
            get_issue_action(PrIssueType::GreenWithFeedback),
            review_suffix
        );

        // Extract all decision context from persistent state in one lock
        let (mut pr_ctx, channel_lead_names) = {
            let ps = state.persistent_state.lock().await;
            (
                PrContext::from_persistent_state(&ps, pr_number),
                ps.channel_lead_names(),
            )
        };

        // Defense-in-depth: also check reviewing_phase_coworkers from snapshot.
        pr_ctx.augment_reviewer_from_snapshot(pr_number, snap);

        // Decide action using handoff-aware decision function (matches webhook path)
        let at_dev_limit = state.is_at_dev_limit(&channel_lead_names);
        let action = crate::rules::decide_pr_issue_action_with_handoff(
            &owner,
            active_coworkers,
            idle_coworkers,
            at_dev_limit,
            pr_ctx.session_context.as_ref(),
            &message,
        );

        let action_name = pr_action_name(&action);

        let new_effects = pr_action_to_effects(
            action,
            pr_number,
            pf.title,
            PrIssueType::GreenWithFeedback,
            state,
            &pr_ctx,
        );

        log_pr_decision(&PrDecisionEntry {
            repo_name: state.paths.dir_key(),
            pr_number,
            title: pf.title,
            owner: &owner,
            issue_type: PrIssueType::GreenWithFeedback,
            action_name,
            effects: &new_effects,
            ctx: &pr_ctx,
            owner_is_active: active_coworkers.contains(&owner),
            owner_is_idle: idle_coworkers.contains(&owner),
            at_dev_limit,
            source: "polling",
        });

        effects.extend(new_effects);
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
/// Gates `PrApproved` events: when `ctx.has_active_reviewer` is true, both the
/// workflow event and inline effects are suppressed. The Approved cooldown is
/// cleared when the reviewer finishes (see `collect_reviewer_effects`),
/// allowing re-evaluation on the next tick. See !1902.
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

    // Build the workflow event for this issue type (if task-linked with a channel).
    // This is computed upfront so it can be emitted in both the script-authoritative
    // path and the fallback inline-effects path.
    let workflow_event = if let (Some(channel_name), Some(task_id)) =
        (&channel, ctx.pr_task_associations.get(&pr_number))
    {
        match issue_type {
            PrIssueType::Approved if ctx.has_active_reviewer => {
                // Suppress while a reviewer is still working. Neither inline effects
                // nor the workflow event fire — no cooldown is recorded either, so
                // should_nudge() will pass on the next tick after the reviewer
                // finishes and the Approved cooldown is cleared.
                debug!(
                    "PR #{}: suppressing PrApproved — reviewer still active",
                    pr_number
                );
                return vec![];
            }
            PrIssueType::Approved => Some(crate::workflow::WorkflowEvent::PrApproved {
                channel: channel_name.clone(),
                task_id: task_id.clone(),
                pr_number,
            }),
            PrIssueType::ChangesRequested => {
                Some(crate::workflow::WorkflowEvent::PrChangesRequested {
                    channel: channel_name.clone(),
                    task_id: task_id.clone(),
                    pr_number,
                })
            }
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
            // These issue types don't have workflow event counterparts.
            PrIssueType::ReviewComment | PrIssueType::ReviewComplete | PrIssueType::NeedsReview => {
                None
            }
        }
    } else {
        None
    };

    // When a workflow script exists AND we have a workflow event, the script is
    // authoritative for simple nudge actions (NudgeOwner, SpawnOwner, PostToChannel):
    // emit only cooldown tracking + the event. The script handles nudging via
    // rpc.nudge_coworker().
    //
    // HandoffToCoworker is excluded: it involves spawning a different coworker with
    // session context and task reassignment, which rpc.nudge_coworker() cannot
    // replicate. Those effects fire alongside the workflow event instead.
    if let Some(ref event) = workflow_event {
        let is_handoff = matches!(action, PrAction::HandoffToCoworker { .. });

        if !is_handoff {
            let has_workflow = channel
                .as_ref()
                .is_some_and(|ch| ctx.channel_workflows.contains_key(ch));

            if has_workflow {
                // Workflow is authoritative — emit cooldown tracking + event only.
                // This fires even for Skip actions so the workflow's state machine
                // stays in sync.
                return vec![
                    Effect::RecordPrNudge {
                        pr_number,
                        issue_type,
                    },
                    Effect::EmitWorkflowEvent(event.clone()),
                ];
            }
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

    // Fallback: inline effects for issue types without workflow events
    // (ReviewComment, NeedsReview), PRs without channel/task
    // associations, or channels without a configured workflow script.
    // When a workflow event exists, it's appended alongside inline effects.
    let mut effects = match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::nudge_session_with_callbacks(
                state.session_id_for_name(&owner),
                message,
                vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            )]
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
                    // Try session-centric path: PR → task → session
                    match &ctx.task_session_id {
                        Some(sid) => crate::launch::SessionMode::ResumeSession(sid.clone()),
                        None => crate::launch::SessionMode::Resume,
                    }
                }
            };
            let config = crate::launch::LaunchConfig::coworker(
                owner.clone(),
                state.paths.dir_key().to_string(),
                session_mode,
                Some(message),
                ctx.pr_task_associations.get(&pr_number).cloned(),
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
                    channel: Some(OPS_CHANNEL.to_string()),
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
    branch_owners: &'a HashMap<String, String>,
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
    branch_owners: &HashMap<String, String>,
    review_mode: crate::config::ReviewMode,
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
            .filter(|&n| ps.github.is_assigned(n))
            .collect();
        let active_reviewers: HashSet<u64> = prs
            .iter()
            .filter_map(|pr| {
                let n = pr.get("number").and_then(|n| n.as_u64())?;
                if ps.github.get_reviewer(n).is_some() && !ps.github.has_cached_review(n) {
                    Some(n)
                } else {
                    None
                }
            })
            .collect();
        let channel_lead_names = ps.channel_lead_names();
        let has_available_slots = state.has_available_coworker_slot(&channel_lead_names);
        let pr_task_associations = ps.github.pr_to_task_map();
        let task_channel = ps.task_channel.clone();
        let channel_workflows = ps.channel_workflows.clone();
        StuckEvalContext {
            review_mode,
            branch_owners,
            channel_lead_names,
            has_available_slots,
            running_coworkers: state.coworkers.list_running(),
            project_name: &state.project_name,
            assigned_prs: assigned,
            active_reviewer_prs: active_reviewers,
            pr_task_associations,
            task_channel,
            channel_workflows,
        }
    };

    for pr in prs {
        let pf = PrFields::from_json(pr);
        if pf.number == 0 || pf.is_draft {
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

    // When a workflow is assigned, replace AutoMergePr with EmitWorkflowEvent(PrAutoMerge)
    // so the workflow controls whether to proceed with auto-merge.
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
                    if has_workflow {
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
        let pr_author = coworker_from_branch(pf.head_ref, ctx.branch_owners);
        let mut busy: Vec<String> = ctx
            .running_coworkers
            .iter()
            .filter(|cw| is_non_lead_coworker(&cw.name, ctx.project_name, &ctx.channel_lead_names))
            .map(|cw| cw.name.clone())
            .collect();
        busy.sort();
        format_no_reviewer_reason(&busy, pr_author.as_deref())
    };

    if should_escalate(prior_nudges) {
        let context = if is_assigned && ctx.has_available_slots {
            "A reviewer was assigned but hasn't posted a review, and coworker slots are available. This looks like a daemon bug.".to_string()
        } else if !is_assigned && ctx.has_available_slots {
            "Coworker slots are available but no reviewer was assigned. This looks like a daemon bug.".to_string()
        } else if is_assigned {
            "A reviewer was assigned but hasn't posted a review.".to_string()
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
    let busy_coworkers = state.get_all_busy_coworkers();
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

        let task_info = crate::tasks::get_in_progress_tasks_with_subjects()
            .into_iter()
            .find(|(_, _, owner)| owner.eq_ignore_ascii_case(name))
            .map(|(id, subject, _)| format!("task !{} ({})", id, truncate_str(&subject, 30)))
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
                state.session_id_for_name(name),
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
                        &snap.default_channel,
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
        let owner = match coworker_from_branch(head_ref, &snap.worktree_branch_owners) {
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
        let (pr_ctx, channel_lead_names) = {
            let ps = state.persistent_state.lock().await;
            (
                PrContext::from_persistent_state(&ps, pr_number),
                ps.channel_lead_names(),
            )
        };

        // If the linked task is completed, create a follow-up task rather than
        // trying to spawn/resume the original coworker with stale session context.
        if let Some(task_id) = pr_ctx.pr_task_associations.get(&pr_number)
            && let Some(task) = crate::tasks::read_task(task_id)
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

        // Decide action using handoff-aware decision function (preserves session
        // resume and idle-coworker handoff capabilities)
        let action = crate::rules::decide_pr_comment_action_with_handoff(
            &owner,
            "reviewer", // Generic actor since we don't know the specific commenter from polling
            active_coworkers,
            idle_coworkers,
            state.is_at_dev_limit(&channel_lead_names),
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
            effects.push(Effect::nudge_channel_lead(
                &snap.default_channel,
                lead_nudge_msg,
            ));
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
            vec![Effect::nudge_session_with_callbacks(
                state.session_id_for_name(&owner),
                message,
                vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            )]
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
                state.paths.dir_key().to_string(),
                session_mode,
                Some(message),
                ctx.pr_task_associations.get(&pr_number).cloned(),
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
                    message: crate::daemon_messages::called_in_review_feedback(&owner, pr_number),
                    channel: Some(OPS_CHANNEL.to_string()),
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
                    channel: Some(OPS_CHANNEL.to_string()),
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
    let mut config = crate::launch::LaunchConfig::pr_handoff(
        assignee.to_string(),
        state.paths.dir_key().to_string(),
        session_id,
        pr_number,
        branch,
        original_author,
    );
    // Pass the PR's linked task ID so the handoff coworker knows its task
    config.task_id = ctx.pr_task_associations.get(&pr_number).cloned();

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
            channel: Some(OPS_CHANNEL.to_string()),
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
            channel: Some(OPS_CHANNEL.to_string()),
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
    ];

    vec![Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success,
        on_failure,
    }]
}

/// Collect effects for spawning reviewers for PRs that need code review.
///
/// Identifies PRs that need review (not drafts, old enough, no completed review,
/// not already assigned) and returns effects to spawn reviewer coworkers.
/// Uses `SpawnCoworkerWithCallbacks` so that reviewer assignment and channel
/// messages only happen on successful spawn.
async fn collect_reviewer_effects(
    snap: &WorldSnapshot,
    state: &DaemonState,
    prs: &[serde_json::Value],
    pre_fetched_review_content: &HashMap<u64, String>,
) -> Vec<Effect> {
    collect_reviewer_effects_with_source(
        &snap.worktree_branch_owners,
        &snap.worktree_registry,
        &snap.coworkers.active_names,
        state,
        prs,
        crate::github_state::AssignmentSource::PollingFallback,
        pre_fetched_review_content,
    )
    .await
}

pub(crate) async fn collect_reviewer_effects_with_source(
    branch_owners: &std::collections::HashMap<String, String>,
    worktree_registry: &crate::worktree_registry::WorktreeRegistry,
    active_names: &std::collections::HashSet<String>,
    state: &DaemonState,
    prs: &[serde_json::Value],
    source: crate::github_state::AssignmentSource,
    pre_fetched_review_content: &HashMap<u64, String>,
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
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    let (
        pr_ctx,
        all_tasks,
        pr_task_associations,
        session_task_map,
        sessions,
        is_at_dev_limit,
        pr_author_names,
    ) = {
        let ps = state.persistent_state.lock().await;
        let all_tasks = crate::tasks::read_tasks_for_repo(Some(state.paths.dir_key()));
        let pr_task_associations = ps.github.pr_to_task_map();
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
        let pr_ctx = PrContext::routing_only(&ps);
        let channel_lead_names = ps.channel_lead_names();
        let is_at_dev_limit = state.is_at_dev_limit(&channel_lead_names);
        // PR author fallback: maps PR# → original author name from stored author sessions.
        let pr_author_names: HashMap<u64, String> = ps
            .github
            .pr_author_sessions
            .iter()
            .map(|(pr, s)| (*pr, s.original_author.clone()))
            .collect();

        (
            pr_ctx,
            all_tasks,
            pr_task_associations,
            session_task_map,
            sessions,
            is_at_dev_limit,
            pr_author_names,
        )
    };

    for pr in prs {
        let pf = PrFields::from_json(pr);
        let pr_number = pf.number;
        if pr_number == 0 {
            continue;
        }

        let title = pf.title;

        // Skip draft PRs
        let is_draft = pf.is_draft;
        if is_draft {
            debug!("PR #{} is a draft, skipping auto-review", pr_number);
            continue;
        }

        // Check if PR is old enough (enforce review delay).
        //
        // When the polling fallback encounters a PR whose channel has a workflow
        // workflow, use the much longer PR_REVIEW_DELAY_SCRIPT_SECS. The workflow
        // spawns reviewers in real-time via rpc.spawn_reviewer() on pr.opened,
        // so polling should only act as a safety net for missed webhooks — not
        // race with the workflow.
        let review_delay = if source == crate::github_state::AssignmentSource::PollingFallback {
            let has_workflow = {
                let ps = state.persistent_state.lock().await;
                pr_task_associations
                    .get(&pr_number)
                    .and_then(|task_id| ps.task_channel.get(task_id).cloned())
                    .is_some_and(|channel| ps.channel_workflows.contains_key(&channel))
            };

            if has_workflow {
                PR_REVIEW_DELAY_SCRIPT_SECS
            } else {
                PR_REVIEW_DELAY_SECS
            }
        } else {
            PR_REVIEW_DELAY_SECS
        };

        if let Some(age_secs) = get_pr_age_secs(pr)
            && age_secs < review_delay
        {
            debug!(
                "PR #{} is too new ({}s < {}s), skipping auto-review",
                pr_number, age_secs, review_delay
            );
            continue;
        }

        // When polling, defer to webhooks if one recently handled this PR.
        // This prevents polling from spawning a duplicate reviewer when the
        // webhook already triggered reviewer spawning via the workflow script.
        if source == crate::github_state::AssignmentSource::PollingFallback {
            let ps = state.persistent_state.lock().await;
            if ps
                .github
                .webhook_recently_handled(pr_number, review_delay as i64 * 2)
            {
                debug!(
                    "PR #{} was recently handled by webhook, polling defers",
                    pr_number
                );
                continue;
            }
        }

        // Check if PR already has a completed review.
        if state.is_pr_reviewed(pr_number).await {
            debug!("PR #{} already has a completed review", pr_number);

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
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
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

            // Bug !2124: For user-authored PRs (lead/* branches), skip coworker
            // owner resolution entirely. The owner resolution chain can resolve
            // to a coworker via task metadata, causing the daemon to spawn a
            // coworker who sees a clean review, goes idle, and loops every
            // cooldown period. Only the user can act on their own PRs.
            //
            // Bug !2137: For lead branches, use has_nudge() (one-shot) instead
            // of should_nudge() (cooldown-based). The user can't act on the PR
            // from within a Claude session, so re-nudging every 10 minutes is
            // spam. Notify exactly once.
            if is_lead_branch(pf.head_ref) {
                let already_nudged = {
                    let tracker = state.pr_issue_tracker.lock().await;
                    tracker.has_nudge(pr_number, PrIssueType::ReviewComplete)
                };
                if already_nudged {
                    continue;
                }

                let review_suffix = pre_fetched_review_content
                    .get(&pr_number)
                    .map(|s| s.as_str())
                    .unwrap_or("");
                let nudge_msg = format!(
                    "Your PR #{} ({}) has a completed review — please address any feedback and merge if appropriate.{}",
                    pr_number,
                    truncate_str(title, 40),
                    review_suffix
                );
                let channel = pr_ctx.get_channel(pr_number);
                let user_msg = format!("@user {}", nudge_msg);
                effects.push(Effect::PostToChannel {
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
                });
                effects.push(Effect::RecordPermanentPrNudge {
                    pr_number,
                    issue_type: PrIssueType::ReviewComplete,
                });
                continue;
            }

            // Coworker PRs: one-shot nudging (same as lead-branch PRs)
            let already_nudged = {
                let tracker = state.pr_issue_tracker.lock().await;
                tracker.has_nudge(pr_number, PrIssueType::ReviewComplete)
            };
            if already_nudged {
                continue;
            }
            let review_suffix = pre_fetched_review_content
                .get(&pr_number)
                .map(|s| s.as_str())
                .unwrap_or("");
            let nudge_msg = format!(
                "Your PR #{} ({}) has a completed review — please address any feedback and merge if appropriate.{}",
                pr_number,
                truncate_str(title, 40),
                review_suffix
            );

            let owner = resolve_pr_owner_from_session(
                pr_number,
                &pr_task_associations,
                &session_task_map,
                &sessions,
            )
            .or_else(|| {
                resolve_pr_owner_from_task_metadata(pr_number, pf.title, pf.head_ref, &all_tasks)
            })
            .or_else(|| coworker_from_branch(pf.head_ref, branch_owners))
            .or_else(|| {
                // Fallback: check pr_author_sessions for the original PR creator.
                // This covers cases where the session record is gone but we stored
                // who created the PR.
                pr_author_names.get(&pr_number).cloned()
            })
            .or_else(|| {
                // Crash-resilient fallback: parse <!-- midtown: name --> from the PR
                // body. This survives daemon restarts and auth storms since the
                // frontmatter lives on GitHub, not in daemon memory.
                resolve_pr_owner_from_body(pf.body())
            })
            .or_else(|| {
                // Last resort: use daemon's pr_task_associations mapping (survives
                // cases where the task owner was unassigned but the task still exists).
                pr_task_associations.get(&pr_number).and_then(|task_id| {
                    all_tasks
                        .iter()
                        .find(|t| t.id == *task_id)
                        .and_then(|t| t.owner.clone())
                })
            });

            if let Some(owner) = owner {
                let action = crate::rules::decide_review_complete_action(
                    &owner,
                    &active_coworkers,
                    &idle_coworkers,
                    is_at_dev_limit,
                    &nudge_msg,
                );

                effects.extend(review_complete_action_to_effects(
                    action, pr_number, title, state, &pr_ctx,
                ));
                effects.push(Effect::RecordPermanentPrNudge {
                    pr_number,
                    issue_type: PrIssueType::ReviewComplete,
                });
                continue;
            }

            let channel = pr_ctx.get_channel(pr_number);
            // No coworker owns this PR — @mention the user so they see it
            let user_msg = format!("@user {}", nudge_msg);
            effects.push(Effect::PostToChannel {
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
            });
            effects.push(Effect::RecordPermanentPrNudge {
                pr_number,
                issue_type: PrIssueType::ReviewComplete,
            });

            continue;
        }

        if !spawn_local_reviewers {
            debug!(
                "PR #{} review pending but local reviewer spawn disabled (execution.review_mode={:?})",
                pr_number, review_mode
            );
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

        // Skip orphaned PRs (PRs whose author has no active worktree, no running coworker,
        // or can't be determined). These should not get auto-review spawned since the author
        // can't address feedback. The main PR loop already posts warnings for orphaned PRs
        // with critical issues.
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
        // 4. If no worktree found and the branch identifies a coworker owner, check whether
        //    that coworker is currently running (active coworkers can always address feedback)
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
                } else if let Some(owner) = coworker_from_branch(head_ref, branch_owners) {
                    // The branch identifies a coworker owner. Only treat as orphaned if
                    // the coworker is NOT currently active — an active coworker can always
                    // address review feedback regardless of whether a worktree is registered.
                    // Uses the caller-provided active_names (from WorldSnapshot) which includes
                    // both pane-based and headless sessions, unlike list_running() which only
                    // tracks pane-based coworkers.
                    let is_active = active_names.contains(&owner.to_lowercase());
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

        // Extract channel lead names for coworker limit check
        let channel_lead_names = {
            let ps = state.persistent_state.lock().await;
            ps.channel_lead_names()
        };

        // Check max coworkers limit before spawning
        if state.is_at_coworker_limit(&channel_lead_names) {
            debug!(
                "Max coworkers limit ({}) reached, cannot spawn reviewer for PR #{}",
                state.max_coworkers, pr_number
            );
            continue;
        }

        // Also exclude the PR author from reviewer selection to prevent self-review.
        // The author is identified via the branch_owners map.
        let mut excluded_names = channel_lead_names.clone();
        let pr_author = coworker_from_branch(head_ref, branch_owners);
        if let Some(ref author) = pr_author {
            excluded_names.insert(author.clone());
        }

        let reviewer_name = match state
            .coworkers
            .next_available_name_excluding(&excluded_names)
        {
            Some(name) => name,
            None => {
                warn!("No available coworker slots for reviewer");
                continue;
            }
        };

        // Compute worktree details for reviewer worktree
        let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);
        let wt_path = state.paths.worktrees_dir().join(&worktree_id);

        // Collision guard: abort spawn if the worktree is already bound to an active coworker.
        // The BindCoworkerToWorktree effect has its own collision guard, but by then the
        // session is already spawned. We must detect this earlier to avoid spawning a
        // reviewer that will run without a valid worktree binding.
        if let Some(existing) = worktree_registry.get(&worktree_id)
            && let Some(ref bound_to) = existing.current_coworker
            && active_names.contains(bound_to.to_lowercase().as_str())
        {
            warn!(
                "WORKTREE COLLISION: Aborting reviewer spawn for PR #{} — worktree {} already bound to ACTIVE coworker {}",
                pr_number, worktree_id, bound_to
            );
            continue;
        }

        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        debug!(
            "Spawning isolated coworker to review PR #{}: {}",
            pr_number,
            truncate_str(title, 40)
        );

        // reviewer() now takes the PR number and provider; both are used to generate
        // a provider-aware initial prompt in addition to spawn arguments.
        // restart_count=0 for new assignments (not a respawn).
        let auth_provider = crate::config::get_execution_provider_for_role(
            state.paths.dir_key(),
            crate::config::ExecutionRole::Reviewer,
        );
        let mut config = crate::launch::LaunchConfig::reviewer(
            reviewer_name.clone(),
            state.paths.dir_key(),
            pr_number,
            0,
            auth_provider,
        );
        config.model = super::helpers::normalize_model_for_provider_role(
            &config.model,
            config.auth_provider,
            &config.role,
        );
        config.working_dir = Some(wt_path.clone());

        // Route reviewer to the task's topic channel so `midtown channel post`
        // defaults to the right channel (via MIDTOWN_CHANNEL env var).
        config.channel = pr_ctx.get_channel(pr_number);

        // Pass the PR's linked task ID so the reviewer knows its task
        // without having to parse it from the PR title.
        config.task_id = pr_ctx.pr_task_associations.get(&pr_number).cloned();

        // Route escalation mentions (@{escalation_target}) to the channel lead
        // for this task's channel, falling back to the project lead if no channel
        // or no channel lead exists.
        if let Some(ref channel_name) = config.channel
            && channel_lead_names.contains(channel_name)
        {
            config.escalation_target = Some(channel_name.clone());
        }

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

        let mut on_success = vec![
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
                message: daemon_messages::called_in_reviewer(&reviewer_name, pr_number),
                channel: Some(OPS_CHANNEL.to_string()),
                auto_output: false,
                message_type: None,
                nudge_type: None,
                tool_data: None,
                provider: None,
                tool_use_id: None,
                parent_tool_use_id: None,
            },
            // DM separator so the reviewer's output streams to dm-<name>
            Effect::PostSystemMessage {
                message: format!("─── Reviewing PR #{} ───", pr_number),
                channel: Some(format!("dm-{}", reviewer_name)),
            },
            // Post the "Review in progress" placeholder comment on the PR.
            // The daemon handles this instead of the reviewer agent to avoid
            // prompt-compliance issues (e.g., escaped `!` in `<!-- midtown-placeholder -->`).
            // The comment ID is stored on the PrReviewerAssignment for later update.
            Effect::PostPrComment {
                pr_number,
                reviewer_name: reviewer_name.clone(),
                body: format!(
                    "<!-- midtown-placeholder -->\n## Review Status\n\n\
                     🔍 Review in progress by {}...\n\n---\n\
                     > [!NOTE]\n> This comment will be updated with the review results when complete.\n\n\
                     🌃 Co-built with [Midtown](https://github.com/btucker/midtown)",
                    reviewer_name
                ),
            },
        ];

        // Warn the PR author not to enable auto-merge while the review is in progress.
        // Without this warning, the author may run `gh pr merge --auto --squash` before
        // the reviewer finishes, causing the PR to merge as soon as CI passes — bypassing
        // the review entirely (as happened with PR #1523).
        if let Some(ref author) = pr_author {
            on_success.push(Effect::NudgeCoworkerByName {
                name: author.clone(),
                message: daemon_messages::reviewer_spawned_author_warning(
                    &reviewer_name,
                    pr_number,
                ),
            });
        }

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
                channel: Some(OPS_CHANNEL.to_string()),
                auto_output: false,
                message_type: None,
                nudge_type: None,
                tool_data: None,
                provider: None,
                tool_use_id: None,
                parent_tool_use_id: None,
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

    // Build the workflow event (if task-linked with a channel).
    let workflow_event = if let (Some(channel_name), Some(task_id)) =
        (&channel, ctx.pr_task_associations.get(&pr_number))
    {
        Some(crate::workflow::WorkflowEvent::ReviewerComplete {
            channel: channel_name.clone(),
            task_id: task_id.clone(),
            pr_number,
        })
    } else {
        None
    };

    // When a workflow script exists AND we have a workflow event, the script is
    // authoritative for simple nudge actions (NudgeOwner, SpawnOwner, PostToChannel):
    // emit only cooldown tracking + the event. The script handles nudging via
    // rpc.nudge_coworker().
    //
    // HandoffToCoworker is excluded: it involves spawning a different coworker with
    // session context and task reassignment, which rpc.nudge_coworker() cannot
    // replicate. Those effects fire alongside the workflow event instead.
    if let Some(ref event) = workflow_event {
        let is_handoff = matches!(action, PrAction::HandoffToCoworker { .. });

        if !is_handoff {
            let has_workflow = channel
                .as_ref()
                .is_some_and(|ch| ctx.channel_workflows.contains_key(ch));

            if has_workflow {
                return vec![
                    Effect::RecordPrNudge {
                        pr_number,
                        issue_type,
                    },
                    Effect::EmitWorkflowEvent(event.clone()),
                ];
            }
        }
    }

    // Skip actions: no inline effects. Still emit the workflow event if one was
    // built so the workflow's state machine stays in sync.
    if let PrAction::Skip { reason } = &action {
        debug!("{}", reason);
        let mut effects = Vec::new();
        if let Some(event) = workflow_event {
            effects.push(Effect::EmitWorkflowEvent(event));
        }
        return effects;
    }

    // Fallback: inline effects for PRs without channel/task associations or
    // channels without an assigned workflow. When a workflow event
    // exists, it's appended alongside inline effects.
    let mut effects = match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::nudge_session_with_callbacks(
                state.session_id_for_name(&owner),
                message,
                vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            )]
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
                state.paths.dir_key().to_string(),
                session_mode,
                Some(message),
                ctx.pr_task_associations.get(&pr_number).cloned(),
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
                    message: daemon_messages::called_in_review_feedback(&owner, pr_number),
                    channel: Some(OPS_CHANNEL.to_string()),
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
                    channel: Some(OPS_CHANNEL.to_string()),
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
        PrAction::Skip { reason } => {
            debug!("{}", reason);
            vec![]
        }
    };

    // Append workflow event alongside inline effects (no-op if no script exists,
    // but keeps the event available for future script adoption).
    if let Some(event) = workflow_event {
        effects.push(Effect::EmitWorkflowEvent(event));
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

            json_has_completed_review(&json, assigned_reviewer)
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
) -> bool {
    // Check formal reviews first (Codex / GitHub-native review flow).
    if let Some(reviews) = json.get("reviews").and_then(|v| v.as_array()) {
        for review in reviews {
            let state_upper = review
                .get("state")
                .and_then(|s| s.as_str())
                .map(|s| s.to_ascii_uppercase());

            let has_review_state = state_upper.as_deref().is_some_and(|s| {
                matches!(
                    s,
                    "APPROVED" | "CHANGES_REQUESTED" | "COMMENTED" | "DISMISSED"
                )
            });

            let has_review_body = review
                .get("body")
                .and_then(|b| b.as_str())
                .is_some_and(text_contains_review_signature);

            if has_review_state || has_review_body {
                let body = review.get("body").and_then(|b| b.as_str()).unwrap_or("");

                // For formal reviews with strong states (APPROVED / CHANGES_REQUESTED),
                // accept even without body attribution. These are intentional review
                // actions unlikely from bots, and the assigned reviewer may submit
                // them with an empty body. Weak states (COMMENTED / DISMISSED) still
                // require author verification to avoid bot false positives.
                let is_strong_state = state_upper
                    .as_deref()
                    .is_some_and(|s| matches!(s, "APPROVED" | "CHANGES_REQUESTED"));

                if review_author_matches(body, assigned_reviewer)
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
                && review_author_matches(body, assigned_reviewer)
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
/// The placeholder is identified by:
/// - Contains "Review in progress by" (from the reviewer template)
/// - Does NOT contain "<!-- midtown:" (not yet updated with final review)
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

    // Find the last placeholder comment: contains "Review in progress by"
    // but NOT the midtown frontmatter (which marks the review as completed)
    for comment in comments.iter().rev() {
        let body = comment.get("body").and_then(|b| b.as_str()).unwrap_or("");
        if body.contains("Review in progress by") && !body.contains("<!-- midtown:") {
            // Extract numeric ID from URL like:
            // https://github.com/owner/repo/pull/123#issuecomment-456789
            let url = comment.get("url").and_then(|u| u.as_str())?;
            let id = url.split("issuecomment-").nth(1)?.parse::<u64>().ok()?;
            return Some(id);
        }
    }
    None
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

    // Resolve owner via session/task/worktree data, with webhook owner as fallback.
    let owner = resolve_pr_owner_from_state(
        state,
        pr_number,
        "",
        branch.as_deref(),
        activity.owner_coworker.as_deref(),
    )
    .await;

    let Some(mut owner) = owner else {
        debug!("PR #{} has no coworker owner, skipping nudge", pr_number);
        return;
    };

    // Check if this PR is linked to a task, and handle based on task status.
    if let Some(task_id) = {
        let ps = state.persistent_state.lock().await;
        ps.github
            .pr_author_sessions
            .get(&pr_number)
            .and_then(|session| session.task_id.as_ref())
            .cloned()
    } && let Some(task) = crate::tasks::read_task(&task_id)
    {
        if task.status == crate::tasks::TaskStatus::InProgress {
            // Route the review feedback to the task owner instead of the PR owner.
            // This handles cases where a task was reassigned (e.g., via orphan recovery)
            // and the PR metadata still shows the original author.
            if let Some(task_owner) = task.owner {
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
        } else if crate::rules::review_comment_creates_followup(&task.status) {
            // Task is completed — the original coworker session is gone.
            // Create a follow-up task so normal dispatch handles it cleanly
            // instead of trying to resume a stale session.
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
    if activity
        .owner_coworker
        .as_ref()
        .is_some_and(|o| o == &activity.actor)
    {
        debug!(
            "PR #{} comment is from author {} — checking for reviewer to notify",
            pr_number, activity.actor
        );

        // Look up the reviewer assignment and task association from persistent state
        let (reviewer_info, task_id) = {
            let ps = state.persistent_state.lock().await;
            let reviewer = ps.github.pr_reviewers.get(&pr_number).cloned();
            let tid = ps.github.pr_to_task_map().get(&pr_number).cloned();
            (reviewer, tid)
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
            vec![Effect::nudge_session(
                state.session_id_for_name(&reviewer_name),
                nudge_msg,
            )]
        } else if let Some(session_id) = reviewer_session_id {
            // Reviewer stopped — resume their session with the follow-up context
            let config = crate::launch::LaunchConfig::coworker(
                reviewer_name.clone(),
                state.paths.dir_key().to_string(),
                crate::launch::SessionMode::ResumeSession(session_id.clone()),
                Some(nudge_msg),
                task_id,
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
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let (pr_ctx, channel_lead_names) = {
        let ps = state.persistent_state.lock().await;
        (
            PrContext::from_persistent_state(&ps, pr_number),
            ps.channel_lead_names(),
        )
    };

    // Decide action using pure decision function with handoff support
    let at_dev_limit = state.is_at_dev_limit(&channel_lead_names);
    let action = crate::rules::decide_pr_comment_action_with_handoff(
        &owner,
        &activity.actor,
        &active_coworkers,
        &idle_coworkers,
        at_dev_limit,
        pr_ctx.session_context.as_ref(),
        &nudge_msg,
    );

    let action_name = pr_action_name(&action);

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let is_actionable = !matches!(action, crate::rules::PrAction::Skip { .. });
    let mut effects = comment_action_to_effects(action, pr_number, "", state, &pr_ctx);

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
        at_dev_limit,
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

    // Resolve owner via session/task/worktree data, with webhook owner as fallback.
    let owner =
        resolve_pr_owner_from_state(state, pr_number, "", None, change.owner_coworker.as_deref())
            .await;

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
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Pre-collect reviewing-phase coworker names (read lock, then release)
    // so we can augment the reviewer check without holding two locks.
    let reviewing_names: std::collections::HashSet<String> = {
        let records = state.coworker_records.read().await;
        records
            .iter()
            .filter(|(_, rec)| {
                matches!(
                    rec.workflow_phase,
                    Some(crate::coworker_state::WorkflowPhase::Reviewing)
                )
            })
            .map(|(name, _)| name.to_lowercase())
            .collect()
    };

    // Extract all decision context from persistent state in one lock
    let (pr_ctx, channel_lead_names) = {
        let ps = state.persistent_state.lock().await;
        let mut ctx = PrContext::from_persistent_state(&ps, pr_number);

        // Defense-in-depth: OR logic matching augment_reviewer_from_snapshot.
        // Either signal independently indicates the reviewer is still working:
        //   A) assignment exists for this PR (coworker hasn't entered Reviewing phase yet)
        //   B) coworker in Reviewing phase with a matching assignment for this PR
        if !ctx.has_active_reviewer {
            let has_assignment = ps
                .github
                .assigned_reviewers()
                .any(|name| ps.github.pr_for_reviewer(name) == Some(pr_number));

            let has_reviewing_phase = reviewing_names
                .iter()
                .any(|name| ps.github.pr_for_reviewer(name) == Some(pr_number));

            ctx.has_active_reviewer = has_assignment || has_reviewing_phase;
        }

        (ctx, ps.channel_lead_names())
    };

    let at_dev_limit = state.is_at_dev_limit(&channel_lead_names);
    let action = crate::rules::decide_pr_issue_action_with_handoff(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        at_dev_limit,
        pr_ctx.session_context.as_ref(),
        &nudge_msg,
    );

    let action_name = pr_action_name(&action);

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects = pr_action_to_effects(action, pr_number, "", issue_type, state, &pr_ctx);

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
        at_dev_limit,
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

    // Resolve owner via session/task/worktree data, with webhook owner as fallback.
    let owner = resolve_pr_owner_from_state(
        state,
        pr_number,
        "",
        failure.head_ref.as_deref(),
        failure.owner_coworker.as_deref(),
    )
    .await;

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
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Extract all decision context from persistent state in one lock
    let (pr_ctx, channel_lead_names) = {
        let ps = state.persistent_state.lock().await;
        (
            PrContext::from_persistent_state(&ps, pr_number),
            ps.channel_lead_names(),
        )
    };

    let at_dev_limit = state.is_at_dev_limit(&channel_lead_names);
    let action = crate::rules::decide_pr_issue_action_with_handoff(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        at_dev_limit,
        pr_ctx.session_context.as_ref(),
        &nudge_msg,
    );

    let action_name = pr_action_name(&action);

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects =
        pr_action_to_effects(action, pr_number, "", PrIssueType::CiFailed, state, &pr_ctx);

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
        at_dev_limit,
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
/// Uses the pre-computed `open_prs_data` from WorldSnapshot to avoid I/O.
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
pub fn reconcile_orphaned_prs(snap: &WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Iterate over open PRs from the snapshot (pre-collected during collect_world_snapshot)
    for pr in &snap.pr.open_prs_data {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) => n,
            None => continue,
        };

        // Only consider PRs with known branches or task-* prefixes
        let branch = match pr.get("headRefName").and_then(|r| r.as_str()) {
            Some(b) => b,
            None => continue,
        };

        // Check if it's a coworker branch, task branch, or lead branch
        let has_valid_prefix = coworker_from_branch(branch, &snap.worktree_branch_owners).is_some()
            || branch.starts_with("task-")
            || is_lead_branch(branch);

        if !has_valid_prefix {
            continue;
        }

        // Skip if there's already an in_progress task linked to this PR.
        // If we previously nudged the lead about this PR, clear the record so
        // re-nudging is possible if the task later completes without merging.
        if snap.pr.pr_task_associations.contains_key(&pr_number) {
            if snap.pr.orphaned_pr_lead_nudges_sent.contains(&pr_number) {
                effects.push(Effect::ClearOrphanedPrLeadNudge { pr_number });
            }
            continue;
        }

        // Skip if the lead has already been nudged about this PR (prevents repeated nudges)
        if snap.pr.orphaned_pr_lead_nudges_sent.contains(&pr_number) {
            continue;
        }

        // Check if PR has been reviewed
        if !snap.reviewer.reviewed_prs.contains(&pr_number) {
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
            &snap.project_name,
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
/// for every (task_id, pr_number) pair in `snap.github_open_pr_task_ids`
/// (derived from open PR titles), it checks whether the corresponding task
/// already has `task.pr` set correctly. If not, it emits `Effect::SetTaskPr`
/// to repair the missing link.
pub fn collect_pr_task_link_effects(snap: &WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    for (task_id_str, &pr_number) in &snap.pr.github_open_pr_task_ids {
        // Find the task by ID
        let task = snap.all_tasks.iter().find(|t| &t.id == task_id_str);

        // Only emit if the link is missing or points to the wrong PR.
        // Skip completed tasks — their PR may still be open (e.g., manual close),
        // but emitting SetTaskPr on every tick would cause unnecessary disk writes.
        let needs_link = match task {
            Some(t) if t.status == crate::tasks::TaskStatus::Completed => false,
            Some(t) => t.pr != Some(pr_number),
            None => false, // task not found — skip, nothing to link
        };

        if needs_link {
            effects.push(Effect::SetTaskPr {
                task_id: task_id_str.clone(),
                pr_number,
                dir_key: snap.dir_key.clone(),
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
pub fn collect_merged_pr_cleanup_effects(snap: &WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Use pre-computed PR → branch mapping from snapshot
    for &pr_num in &snap.pr.merged_pr_numbers {
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
            effects.push(Effect::PostSystemMessage {
                message,
                channel: Some(OPS_CHANNEL.to_string()),
            });
        }
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
        Effect::NudgeCoworkerByName { .. } => "NudgeCoworkerByName",
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
        Effect::AssignAndSpawn { .. } => "AssignAndSpawn",
        Effect::MarkRemindersFired { .. } => "MarkRemindersFired",
        Effect::RecordPrNudge { .. } => "RecordPrNudge",
        Effect::RecordPermanentPrNudge { .. } => "RecordPermanentPrNudge",
        Effect::RecordTaskAssignment { .. } => "RecordTaskAssignment",
        Effect::ClearPrBreakSession { .. } => "ClearPrBreakSession",
        Effect::AssignReviewer { .. } => "AssignReviewer",
        Effect::RemoveReviewerAssignment { .. } => "RemoveReviewerAssignment",
        Effect::RecordReviewerEscalation { .. } => "RecordReviewerEscalation",
        Effect::RecordOrphanedPrLeadNudge { .. } => "RecordOrphanedPrLeadNudge",
        Effect::ClearOrphanedPrLeadNudge { .. } => "ClearOrphanedPrLeadNudge",
        Effect::ClearOrphanedReviewerAssignments { .. } => "ClearOrphanedReviewerAssignments",
        Effect::RerunWorkflow { .. } => "RerunWorkflow",
        Effect::UpdatePrComment { .. } => "UpdatePrComment",
        Effect::StorePrAuthorSession { .. } => "StorePrAuthorSession",
        Effect::CompleteTask { .. } => "CompleteTask",
        Effect::ClearBlockedBy { .. } => "ClearBlockedBy",
        Effect::SetTaskPr { .. } => "SetTaskPr",
        Effect::SendPushNotification { .. } => "SendPushNotification",
        Effect::CleanStaleBranches => "CleanStaleBranches",
        Effect::CleanWorktreeTarget { .. } => "CleanWorktreeTarget",
        Effect::CleanupMergedWorktree { .. } => "CleanupMergedWorktree",
        Effect::CleanupStaleWorktree { .. } => "CleanupStaleWorktree",
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
        Effect::NudgeSessionWithCallbacks { .. } => "NudgeSessionWithCallbacks",
        Effect::SpawnSession { .. } => "SpawnSession",
        Effect::ShutdownSession { .. } => "ShutdownSession",
        Effect::RecordSession { .. } => "RecordSession",
        Effect::ReleaseName { .. } => "ReleaseName",
        Effect::MergePr { .. } => "MergePr",
        Effect::AutoMergePr { .. } => "AutoMergePr",
        Effect::PostPrComment { .. } => "PostPrComment",
        Effect::EmitWorkflowEvent(_) => "EmitWorkflowEvent",
        Effect::RespawnFork { .. } => "RespawnFork",
        Effect::PostInsight { .. } => "PostInsight",
        Effect::RespawnChannelLead { .. } => "RespawnChannelLead",
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
    at_dev_limit: bool,
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
        "at_dev_limit": entry.at_dev_limit,
        "has_active_reviewer": entry.ctx.has_active_reviewer,
        "has_session_context": entry.ctx.session_context.is_some(),
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
        crate::rules::PrAction::HandoffToCoworker { .. } => "HandoffToCoworker",
        crate::rules::PrAction::PostToChannel { .. } => "PostToChannel",
        crate::rules::PrAction::Skip { .. } => "Skip",
    }
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
