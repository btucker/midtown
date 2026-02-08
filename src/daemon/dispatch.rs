//! Task dispatch — orphan recovery, duplicate detection, pending task spawning.
//!
//! These functions run on the `TaskDispatchTick` event and coordinate coworker
//! lifecycle around the shared task list. They read from `WorldSnapshot` and
//! return `Vec<Effect>` for execution by the effect runner.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::{config, daemon_messages};

use super::constants::*;
use super::effects::{self, Effect};
use super::helpers::format_task_prompt;
use super::{DaemonState, snapshot};

// ============================================================================
// Orphan task recovery
// ============================================================================

/// Check for orphaned tasks and auto-recover coworkers.
///
/// An orphaned task is one that is `in_progress` but the owning coworker
/// is no longer active (no tmux window). If the coworker's worktree still
/// exists, we respawn them and nudge them to resume work.
///
/// Rate limiting: Only spawns ONE coworker per tick with a cooldown between
/// spawns to prevent window flashing from spawn storms.
pub(super) fn check_and_recover_orphans(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    // Check cooldown - skip if we spawned too recently
    {
        let cooldowns = state.cooldowns.lock().unwrap();
        if !cooldowns.check("orphan_spawn", "global", ORPHAN_SPAWN_COOLDOWN) {
            debug!("Orphan recovery cooldown active");
            return vec![];
        }
    }

    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

    // Compute recently-stopped coworkers (within grace period).
    // When a coworker completes work and goes idle → shutdown, the task may
    // not yet be marked done. This grace period prevents false orphan recovery
    // by giving the system time to process the task completion.
    let grace_period = chrono::Duration::seconds(ORPHAN_RECOVERY_GRACE_PERIOD.as_secs() as i64);
    let recently_stopped: HashSet<String> = snap
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect();

    // Decide which orphan (if any) to recover using pure decision function
    let recovery = crate::rules::decide_orphan_recovery(
        &snap.in_progress_tasks,
        &snap.active_names,
        snap.is_at_dev_limit,
        &snap.coworkers_with_open_prs,
        &snap.review_feedback_pr_coworkers,
        &recently_stopped,
        &snap.attached_coworkers,
    );

    let Some(recovery) = recovery else {
        return vec![];
    };

    // Check per-coworker spawn failure cooldown to prevent infinite retry loops
    {
        let cooldowns = state.cooldowns.lock().unwrap();
        if !cooldowns.check("spawn_failure", &recovery.owner, SPAWN_FAILURE_COOLDOWN) {
            debug!(
                "Spawn failure cooldown active for {} — skipping orphan recovery for task !{}",
                recovery.owner, recovery.task_id
            );
            return vec![];
        }
    }

    info!(
        "Detected orphaned task !{} owned by {} - attempting recovery",
        recovery.task_id, recovery.owner
    );

    let prompt = format_task_prompt(
        &recovery.task_id,
        &format!(
            "You've been assigned task !{}: {}. Your previous session was interrupted but your worktree and branch are still intact. Check your git status and get started!",
            recovery.task_id, recovery.task_subject
        ),
    );

    // Look up existing task worktree from the registry (via snapshot).
    // If the task already has a worktree, reuse it — this preserves build cache
    // and partial work even when a different coworker is assigned.
    let mut config = crate::launch::LaunchConfig::coworker(
        recovery.owner.clone(),
        state.repo_name.clone(),
        crate::launch::SessionMode::Fresh,
        Some(prompt),
    );

    // Reuse existing worktree if one is registered for this task (reassignment case).
    // Otherwise, compute a new worktree_id from the task subject.
    let (worktree_id, needs_registration) =
        if let Some(existing_wt_id) = snap.task_worktree_map.get(&recovery.task_id) {
            (existing_wt_id.clone(), false)
        } else {
            (
                crate::worktree_registry::branch_slug_for_task(
                    &recovery.task_id,
                    &recovery.task_subject,
                ),
                true,
            )
        };
    let wt_path = crate::paths::worktrees_dir_for_repo(&state.repo_name).join(&worktree_id);
    config.working_dir = Some(wt_path.clone());

    // Build success effects — worktree setup prepended, then standard spawn effects
    let mut on_success = vec![];

    // Ensure the worktree exists BEFORE spawning
    on_success.push(Effect::EnsureWorktree {
        worktree_id: worktree_id.clone(),
        path: wt_path.clone(),
    });

    // Register the task → worktree mapping if this is the first time
    if needs_registration {
        on_success.push(Effect::RegisterWorktreeAssignment {
            assignment: crate::worktree_registry::WorktreeAssignment {
                worktree_id: worktree_id.clone(),
                branch_name: worktree_id.clone(), // Branch name matches worktree_id for task worktrees
                task_id: Some(recovery.task_id.clone()),
                current_coworker: None, // Will be set by BindCoworkerToWorktree
                pr_number: None,
                created_at: chrono::Utc::now(),
            },
        });
    }

    // Always bind the coworker to the worktree when spawning
    on_success.push(Effect::BindCoworkerToWorktree {
        worktree_id: worktree_id.clone(),
        coworker: recovery.owner.clone(),
    });

    on_success.push(Effect::BroadcastCoworkerUpdate {
        name: recovery.owner.clone(),
        status: "running".to_string(),
        current_task: None,
    });
    on_success.push(Effect::RecordCooldown {
        category: "orphan_spawn".to_string(),
        key: "global".to_string(),
    });
    on_success.push(Effect::PostToChannel {
        sender: "midtown".to_string(),
        message: format!(
            "♻️ Recovered coworker {} for orphaned task !{}",
            recovery.owner, recovery.task_id
        ),
    });

    // Return spawn effect with success/failure callbacks
    vec![Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success,
        on_failure: vec![
            Effect::RecordCooldown {
                category: "spawn_failure".to_string(),
                key: recovery.owner.clone(),
            },
            Effect::ResetTaskToPending {
                task_id: recovery.task_id.clone(),
                repo_name: snap.repo_name.clone(),
            },
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "🔄 Task !{} reset to pending - {} could not be respawned (backing off for {}s)",
                    recovery.task_id,
                    recovery.owner,
                    SPAWN_FAILURE_COOLDOWN.as_secs()
                ),
            },
        ],
    }]
}

/// Gather data and build effects for nudging coworkers discovered on daemon startup.
///
/// After a daemon restart, existing coworkers are found in tmux but they may
/// be stuck waiting for input or idle. This function checks if each discovered
/// coworker has an assigned task (in_progress with them as owner) or a reviewer
/// assignment (in github-state.json), and returns nudge effects.
///
/// The caller is responsible for the initial startup delay and executing effects.
pub(super) async fn gather_discovered_coworker_nudges(state: &DaemonState) -> Vec<Effect> {
    let discovered = state.coworkers.take_discovered_on_startup();
    if discovered.is_empty() {
        return vec![];
    }

    info!(
        "Checking {} discovered coworker(s) for tasks to resume",
        discovered.len()
    );

    // Small delay to let things settle after daemon startup
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    // Get in_progress tasks with owners
    let in_progress = crate::tasks::get_in_progress_tasks_with_subjects();

    // Build a map of owner -> (task_id, task_subject)
    let mut owner_tasks: HashMap<String, (String, String)> = HashMap::new();
    for (task_id, task_subject, owner) in &in_progress {
        let owner_lower = owner.trim().trim_matches('"').to_lowercase();
        if !owner_lower.is_empty() {
            owner_tasks.insert(owner_lower, (task_id.clone(), task_subject.clone()));
        }
    }

    // Check reviewer assignments from daemon-state.json
    let reviewer_prs: HashMap<String, u64> = {
        let ps = state.persistent_state.lock().await;
        discovered
            .iter()
            .filter_map(|name| {
                ps.github
                    .pr_for_reviewer(name)
                    .map(|pr| (name.to_lowercase(), pr))
            })
            .collect()
    };

    // Build effects using pure decision function
    decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs)
}

/// Build effects for nudging discovered coworkers based on their task/review assignments.
///
/// Pure function: takes immutable data, returns effects. All I/O (nudging,
/// channel posting) flows through Effect variants.
fn decide_discovered_coworker_nudges(
    discovered: &[String],
    owner_tasks: &HashMap<String, (String, String)>,
    reviewer_prs: &HashMap<String, u64>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for name in discovered {
        let name_lower = name.to_lowercase();

        // Check for an in_progress task owned by this coworker
        if let Some((task_id, task_subject)) = owner_tasks.get(&name_lower) {
            let prompt = format_task_prompt(
                task_id,
                &format!(
                    "Resume task !{}: {}. The daemon was restarted and discovered you still running. Check your git status and continue where you left off.",
                    task_id, task_subject
                ),
            );

            info!(
                "Nudging discovered coworker {} to resume task !{}",
                name, task_id
            );

            effects.push(Effect::NudgeCoworker {
                name: name.clone(),
                message: prompt,
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "♻️ Nudged discovered coworker {} to resume task !{}",
                    name, task_id
                ),
            });
        } else if let Some(pr_number) = reviewer_prs.get(&name_lower) {
            let prompt = crate::agents::reviewer_resume_prompt(*pr_number);

            info!(
                "Nudging discovered coworker {} to resume review of PR #{}",
                name, pr_number
            );

            effects.push(Effect::NudgeCoworker {
                name: name.clone(),
                message: prompt,
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "♻️ Nudged discovered reviewer {} to resume PR #{} review",
                    name, pr_number
                ),
            });
        } else {
            debug!(
                "Discovered coworker {} has no assigned task or review - skipping nudge",
                name
            );
        }
    }

    effects
}

/// Detect and kill duplicate task workers.
///
/// When multiple coworkers end up working on the same task (e.g., due to race
/// conditions in task claiming), this function detects the duplicates and kills
/// all but the earliest-started worker. This prevents wasted effort and duplicate PRs.
///
/// The function:
/// 1. Gets all in_progress tasks with their owners
/// 2. Groups tasks by task ID to find duplicates
/// 3. For tasks with multiple workers, keeps the one that started earliest
/// 4. Shuts down the duplicate workers with an explanatory message
pub(super) fn check_for_duplicate_task_workers(
    snap: &snapshot::WorldSnapshot,
) -> Vec<effects::Effect> {
    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

    // Build a map of task_id -> list of owners
    let mut task_workers: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, _subject, owner) in &snap.in_progress_tasks {
        // Skip empty owners or Lead
        if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
            continue;
        }
        task_workers
            .entry(task_id.clone())
            .or_default()
            .push(owner.clone());
    }

    let mut effects = Vec::new();

    // Find tasks with multiple workers and determine who to kill
    for (task_id, workers) in task_workers {
        if workers.len() <= 1 {
            continue;
        }

        // Get the task subject for logging
        let task_subject = snap
            .in_progress_tasks
            .iter()
            .find(|(id, _, _)| id == &task_id)
            .map(|(_, s, _)| s.as_str())
            .unwrap_or("unknown");

        info!(
            "Detected {} duplicate workers on task !{} ({}): {:?}",
            workers.len(),
            task_id,
            task_subject,
            workers
        );

        // Sort workers by start time (earliest first)
        // Workers not found in active list go to the end (will be killed)
        let mut workers_with_times: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = workers
            .into_iter()
            .map(|name| {
                let start_time = snap.coworker_start_times.get(&name.to_lowercase()).copied();
                (name, start_time)
            })
            .collect();

        workers_with_times.sort_by(|a, b| {
            match (&a.1, &b.1) {
                (Some(t1), Some(t2)) => t1.cmp(t2),          // Earlier time first
                (Some(_), None) => std::cmp::Ordering::Less, // Known time beats unknown
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });

        // Keep the first (earliest) worker, kill the rest
        let (keeper, keeper_time) = workers_with_times[0].clone();
        info!(
            "Keeping {} (started {:?}) for task !{}",
            keeper, keeper_time, task_id
        );

        for (duplicate, dup_time) in workers_with_times.into_iter().skip(1) {
            warn!(
                "Killing duplicate worker {} (started {:?}) for task !{} - {} is already working on it",
                duplicate, dup_time, task_id, keeper
            );

            effects.push(Effect::BroadcastCoworkerUpdate {
                name: duplicate.clone(),
                status: "stopped".to_string(),
                current_task: None,
            });
            effects.push(Effect::ShutdownCoworker {
                name: duplicate.clone(),
                message: String::new(),
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "🔪 Killed duplicate worker {} on task !{} ({}) - {} started earlier",
                    duplicate, task_id, task_subject, keeper
                ),
            });
        }
    }

    effects
}

// ============================================================================
// Pending task auto-spawn
// ============================================================================

/// Decide whether to skip orphan flagging based on PR poll initialization state.
///
/// During startup, orphan checks run every 10s but PR poll runs every 30s.
/// If we flag orphans before we have PR data, we'd incorrectly warn about
/// worktrees that have open PRs (because open_pr_owners is still empty).
///
/// Pure function for testability.
fn should_skip_orphan_flagging(pr_poll_initialized: bool) -> bool {
    !pr_poll_initialized
}

/// Compute which orphaned coworkers should have their reviewer assignments cleared.
///
/// Returns `None` if we should skip clearing (PR poll not yet initialized).
/// Returns `Some(vec)` with the filtered list of orphans (excluding those with open PRs).
///
/// During startup, we don't have accurate PR data, so we can't safely clear
/// reviewer assignments without risking clearing assignments for coworkers who
/// legitimately have open PRs and are just "on break".
///
/// Pure function for testability.
fn compute_orphans_for_reviewer_clearing(
    pr_poll_initialized: bool,
    all_orphaned: Vec<String>,
    open_pr_owners: &HashSet<String>,
) -> Option<Vec<String>> {
    if !pr_poll_initialized {
        return None;
    }
    let filtered = filter_orphans_with_open_prs(all_orphaned, open_pr_owners);
    if filtered.is_empty() {
        None
    } else {
        Some(filtered)
    }
}

/// Filter out worktrees that have open PRs.
///
/// A worktree with an open PR is not orphaned — it's just waiting for review/merge.
/// Pure function for testability.
fn filter_orphans_with_open_prs(
    flagged: Vec<String>,
    open_pr_owners: &HashSet<String>,
) -> Vec<String> {
    flagged
        .into_iter()
        .filter(|name| !open_pr_owners.contains(name))
        .collect()
}

/// Filter out worktrees whose exact branch matches a recently merged PR.
///
/// Unlike open PR filtering (by coworker name), merged PR filtering must be
/// done by exact branch name to avoid hiding genuinely orphaned worktrees.
/// If a coworker has branch A merged and branch B orphaned, only A should
/// be filtered out, not B.
///
/// Returns (coworker_name, should_filter) pairs.
/// Partition orphaned worktrees by whether their PR was merged.
///
/// Returns (merged_prs, unmerged) where:
/// - merged_prs: worktrees whose exact branch was merged (safe to clean up)
/// - unmerged: worktrees with no matching merged PR (need investigation)
fn partition_orphans_by_merged_status(
    flagged: Vec<String>,
    merged_pr_branches: &HashSet<String>,
    get_branch_for_coworker: impl Fn(&str) -> Option<String>,
) -> (Vec<String>, Vec<String>) {
    let mut merged = Vec::new();
    let mut unmerged = Vec::new();

    for name in flagged {
        if let Some(branch) = get_branch_for_coworker(&name) {
            if merged_pr_branches.contains(&branch) {
                merged.push(name);
            } else {
                unmerged.push(name);
            }
        } else {
            // Detached HEAD - no branch name to check against merged PRs.
            // Worktrees only reach this function if safe_cleanup() returned false,
            // which for detached HEAD means has_uncommitted_changes() was true.
            // Force-deleting would lose that uncommitted work.
            // Treat as unmerged so the Lead gets a warning and can investigate.
            unmerged.push(name);
        }
    }

    (merged, unmerged)
}

/// Data gathered from blocking worktree operations and PR cache for orphan cleanup.
///
/// Collected once in the async wrapper, then passed to the pure decision function.
pub(super) struct OrphanCleanupData {
    /// All orphaned worktree names (before any filtering).
    pub all_orphaned: Vec<String>,
    /// Worktrees whose PRs were merged (safe to force-delete).
    pub merged_worktrees_to_cleanup: Vec<String>,
    /// Whether the first PR poll has completed.
    pub pr_poll_initialized: bool,
    /// Coworkers who have open PRs (excluded from cleanup/clearing).
    pub open_pr_owners: HashSet<String>,
    /// Worktrees auto-cleaned via gh CLI fallback (squash-merged PRs not in cache).
    pub gh_cleaned: Vec<String>,
    /// Worktrees due for a warning (orphan tracker determined they need alerting).
    pub due_for_warning: Vec<String>,
    /// Whether the stale branch cleanup cooldown has expired.
    pub stale_branch_cleanup_due: bool,
}

/// Gather data needed for orphan worktree cleanup decisions.
///
/// Runs blocking git operations in a separate thread pool and reads PR cache
/// state. Also consults the orphan tracker to determine which worktrees need
/// warnings (the tracker is in-memory state management, not I/O).
///
/// Returns `None` if the PR poll hasn't initialized yet (too early to decide).
pub(super) async fn gather_orphan_cleanup_data(state: &DaemonState) -> Option<OrphanCleanupData> {
    // Clone the coworker manager for use in the blocking task.
    // CoworkerManager is Clone and contains Arc<> internally.
    let coworkers = state.coworkers.clone();

    // Run the blocking worktree operations (git commands only - no gh CLI) in a
    // separate thread pool. Process at most 2 worktrees per tick to avoid
    // saturating the blocking thread pool and causing RPC timeouts.
    // Also get the full list of orphaned worktrees for state cleanup.
    let (all_orphaned, flagged, branch_map) = tokio::task::spawn_blocking(move || {
        // First get all orphaned worktrees (before cleanup modifies the list)
        let all_orphaned = coworkers.find_orphaned_worktree_names();
        let flagged = coworkers.cleanup_orphaned_worktrees(Some(2));
        // Pre-fetch branch names for all flagged worktrees (avoids blocking git
        // calls later in the async context)
        let branch_map: HashMap<String, Option<String>> = flagged
            .iter()
            .map(|name| (name.clone(), coworkers.get_worktree_branch(name)))
            .collect();
        (all_orphaned, flagged, branch_map)
    })
    .await
    .unwrap_or_else(|e| {
        warn!("Worktree cleanup task panicked: {}", e);
        (vec![], vec![], HashMap::new())
    });

    // Skip orphan flagging and reviewer assignment clearing until the first PR poll completes.
    let (pr_poll_initialized, open_pr_owners, merged_pr_branches) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        (
            cache.pr_poll_initialized,
            cache.open_pr_owners.clone(),
            cache.merged_pr_branches.clone(),
        )
    };
    if should_skip_orphan_flagging(pr_poll_initialized) {
        debug!("Skipping orphan flagging - PR poll not yet initialized");
        return None;
    }

    // Filter and partition using pure helper functions.
    let filtered = filter_orphans_with_open_prs(flagged, &open_pr_owners);
    let (merged_pr_worktrees, unmerged) =
        partition_orphans_by_merged_status(filtered, &merged_pr_branches, |name| {
            branch_map.get(name).cloned().flatten()
        });

    for name in &unmerged {
        debug!("Orphan worktree flagged (no open or merged PR): {}", name);
    }

    // Collect worktrees due for warning using the tracker (scoped to drop before awaits)
    let due_for_warning = {
        let mut tracker = state.orphan_tracker.write().unwrap();
        // Prune using the FULL orphan list, not the filtered `unmerged` subset.
        // Using `unmerged` (which is capped at 2 per tick, then filtered by open PRs
        // and merged status) would drop tracker entries for orphans not in the current
        // batch, losing their warned_at timestamps and causing repeat warnings.
        tracker.prune(&all_orphaned);
        unmerged
            .into_iter()
            .filter(|name| {
                tracker.track(name.clone());
                tracker.should_warn(name)
            })
            .collect::<Vec<_>>()
    };

    // Before warning the Lead, do a final gh CLI check for worktrees that might
    // have squash-merged PRs not in the cache. This only runs when we're about
    // to warn (after the 60s grace period), not every tick.
    let (gh_cleaned, due_for_warning) = if due_for_warning.is_empty() {
        (vec![], due_for_warning)
    } else {
        let coworkers = state.coworkers.clone();
        let to_check = due_for_warning.clone();
        let (cleaned, remaining) = tokio::task::spawn_blocking(move || {
            let mut cleaned = Vec::new();
            let mut remaining = Vec::new();
            for name in to_check {
                // Guard against tmux race: coworker may exist in tmux but
                // not yet be in the daemon's internal map.
                if coworkers.has_tmux_window(&name) {
                    remaining.push(name);
                    continue;
                }
                if coworkers.is_branch_pr_merged(&name) {
                    match coworkers.force_cleanup_worktree(&name) {
                        Ok(()) => {
                            info!(
                                "Auto-cleaned orphaned worktree for {} (gh confirmed PR merged)",
                                name
                            );
                            cleaned.push(name);
                        }
                        Err(e) => {
                            warn!(
                                "Failed to cleanup gh-confirmed merged worktree for {}: {}",
                                name, e
                            );
                            remaining.push(name);
                        }
                    }
                } else {
                    remaining.push(name);
                }
            }
            (cleaned, remaining)
        })
        .await
        .unwrap_or_else(|e| {
            warn!("gh PR merged check panicked: {}", e);
            (vec![], due_for_warning.clone())
        });

        // Prune using the FULL orphan list to preserve warned_at timestamps for
        // orphans not in the `remaining` subset (same rationale as line 611).
        if !cleaned.is_empty() {
            let mut tracker = state.orphan_tracker.write().unwrap();
            tracker.prune(&all_orphaned);
        }

        (cleaned, remaining)
    };

    // Record warnings for worktrees that are genuinely due
    if !due_for_warning.is_empty() {
        let mut tracker = state.orphan_tracker.write().unwrap();
        for name in &due_for_warning {
            warn!(
                "Orphaned worktree for {} has unmerged commits not on any PR - flagging to lead",
                name
            );
            tracker.record_warn(name);
        }
    }

    // Check stale branch cleanup cooldown (in-memory state, not I/O)
    let stale_branch_cleanup_due = {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        if cooldowns.check("stale_branch_cleanup", "global", Duration::from_secs(300)) {
            // Record immediately to prevent TOCTTOU races where concurrent ticks
            // pass the check before any records the cooldown.
            cooldowns.record("stale_branch_cleanup", "global");
            true
        } else {
            false
        }
    };

    Some(OrphanCleanupData {
        all_orphaned,
        merged_worktrees_to_cleanup: merged_pr_worktrees,
        pr_poll_initialized,
        open_pr_owners,
        gh_cleaned,
        due_for_warning,
        stale_branch_cleanup_due,
    })
}

/// Build effects for orphan worktree cleanup based on gathered data.
///
/// Pure function: takes immutable data, returns effects. All I/O flows through
/// Effect variants executed by `effects::execute_effects`.
pub(super) fn decide_orphan_cleanup(data: &OrphanCleanupData) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Clear reviewer assignments for orphaned coworkers.
    if let Some(orphans) = compute_orphans_for_reviewer_clearing(
        data.pr_poll_initialized,
        data.all_orphaned.clone(),
        &data.open_pr_owners,
    ) {
        effects.push(Effect::ClearOrphanedReviewerAssignments {
            orphaned_coworkers: orphans,
        });
    }

    // Force-delete worktrees whose PRs were merged (squash-merge case).
    if !data.merged_worktrees_to_cleanup.is_empty() {
        effects.push(Effect::ForceCleanupWorktrees {
            names: data.merged_worktrees_to_cleanup.clone(),
        });
    }

    // Post channel messages for worktrees auto-cleaned via gh CLI fallback.
    for name in &data.gh_cleaned {
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🧹 Auto-cleaned orphaned worktree for {} (PR was merged)",
                name
            ),
        });
    }

    // Warn about orphaned worktrees with genuinely unmerged commits.
    if !data.due_for_warning.is_empty() {
        let names_list = data.due_for_warning.join(", ");
        let nudge_text = format!(
            "⚠️ @lead Orphaned worktrees with unmerged commits (not on any PR): {}. \
             Please investigate and decide whether to merge or delete these branches.",
            names_list
        );

        effects.push(Effect::PostSystemMessage {
            message: nudge_text.clone(),
        });
        effects.push(Effect::NudgeLead {
            message: nudge_text.clone(),
        });
        effects.push(Effect::SendPushNotification {
            title: "Orphaned worktrees need attention".to_string(),
            body: nudge_text,
            tag: "orphan_warning".to_string(),
        });
    }

    // Clean stale branches if cooldown expired.
    if data.stale_branch_cleanup_due {
        effects.push(Effect::CleanStaleBranches);
    }

    effects
}

/// Handles two cases:
/// 1. Pending tasks with owners - spawn/nudge the assigned coworker if not running
/// 2. Pending tasks without owners - spawn a new coworker, assign the task, and nudge
pub(super) fn spawn_for_pending_tasks(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    debug!(
        "Task assignment state: active={}",
        snap.running_coworkers.len()
    );

    let mut effects = Vec::new();

    // Case 1: Pending tasks with owners assigned but coworker not running.
    // With the daemon-managed task.claim flow, this case is rare (claims set
    // both owner and in_progress directly). It mainly handles backward compatibility
    // with pre-existing tasks or tasks where the Lead manually set an owner.
    let pending_with_owners = &snap.pending_tasks_with_owners;
    for (task_id, task_subject, owner) in pending_with_owners.iter() {
        // Skip tasks whose referenced PR is already merged. These are stale —
        // the work is done (PR merged) but the task wasn't auto-completed.
        if let Some(pr_num_str) = crate::tasks::extract_pr_number(task_subject)
            && let Ok(pr_num) = pr_num_str.parse::<u64>()
            && snap.merged_pr_numbers.contains(&pr_num)
        {
            info!(
                "Auto-completing stale task !{}: PR #{} has been merged",
                task_id, pr_num
            );
            effects.push(Effect::CompleteTask {
                task_id: task_id.clone(),
                repo_name: snap.repo_name.clone(),
            });
            effects.push(Effect::ClearBlockedBy {
                completed_task_id: task_id.clone(),
                repo_name: snap.repo_name.clone(),
            });
            continue;
        }

        // Check nudge cooldown for this task
        let task_key = format!("pending-{}", task_id);
        let on_nudge_cooldown = {
            let cooldowns = state.cooldowns.lock().unwrap();
            !cooldowns.check("task_nudge", &task_key, Duration::from_secs(300))
        };

        // Check if the owner is an active reviewer (reviewers should not be nudged
        // about main task list updates — they have their own review assignments)
        let is_owner_reviewer = snap.active_reviewers.contains(&owner.to_lowercase());

        // Check if the owner already has an in_progress task (one-task-per-coworker invariant).
        // Uses the pre-computed busy_coworkers HashSet (O(1)) rather than scanning
        // in_progress_tasks (O(n)), following the snapshot pre-computation pattern.
        let has_in_progress_task = snap.busy_coworkers.contains(&owner.to_lowercase());

        // Decide action using pure decision function
        let action = crate::rules::decide_pending_task_action(
            task_id,
            task_subject,
            owner,
            &snap.active_names,
            snap.is_at_dev_limit,
            on_nudge_cooldown,
            is_owner_reviewer,
            has_in_progress_task,
        );

        match action {
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                let nudge_msg = format_task_prompt(
                    tid,
                    &format!("You have pending task !{}: {}. Get started!", tid, subj),
                );
                // Deliver via mailbox (non-urgent task assignment to idle coworker).
                // Also send via tmux as fallback in case mailbox isn't polled.
                effects.push(Effect::DeliverMailboxMessage {
                    name: o.clone(),
                    message: nudge_msg.clone(),
                    summary: Some(format!("Task !{} assignment", tid)),
                });
                effects.push(Effect::NudgeCoworkerWithCallbacks {
                    name: o.clone(),
                    message: nudge_msg,
                    on_success: vec![Effect::RecordCooldown {
                        category: "task_nudge".to_string(),
                        key: task_key.clone(),
                    }],
                });
            }
            crate::rules::PendingTaskAction::SpawnOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                info!(
                    "Pending task !{} is assigned to {} but coworker not running - spawning",
                    tid, o
                );
                let prompt = format_task_prompt(
                    tid,
                    &format!("You've been assigned task !{}: {}. Get started!", tid, subj),
                );

                // Reuse existing worktree if one is registered for this task (reassignment case).
                // Otherwise, compute a new worktree_id from the task subject.
                let (worktree_id, needs_registration) =
                    if let Some(existing_wt_id) = snap.task_worktree_map.get(tid.as_str()) {
                        (existing_wt_id.clone(), false)
                    } else {
                        (
                            crate::worktree_registry::branch_slug_for_task(tid, subj),
                            true,
                        )
                    };
                let wt_path =
                    crate::paths::worktrees_dir_for_repo(&state.repo_name).join(&worktree_id);

                let mut config = crate::launch::LaunchConfig::coworker(
                    o.clone(),
                    state.repo_name.clone(),
                    crate::launch::SessionMode::Resume,
                    Some(prompt),
                );
                config.working_dir = Some(wt_path.clone());

                let mut spawn_effects = Vec::new();

                // Ensure the worktree exists BEFORE spawning (fixes effect pattern violation)
                spawn_effects.push(Effect::EnsureWorktree {
                    worktree_id: worktree_id.clone(),
                    path: wt_path.clone(),
                });

                // Register the task → worktree mapping if this is the first time
                if needs_registration {
                    spawn_effects.push(Effect::RegisterWorktreeAssignment {
                        assignment: crate::worktree_registry::WorktreeAssignment {
                            worktree_id: worktree_id.clone(),
                            branch_name: worktree_id.clone(), // Branch name matches worktree_id for task worktrees
                            task_id: Some(tid.clone()),
                            current_coworker: None, // Will be set by BindCoworkerToWorktree
                            pr_number: None,
                            created_at: chrono::Utc::now(),
                        },
                    });
                }

                // Always bind the coworker to the worktree when spawning
                spawn_effects.push(Effect::BindCoworkerToWorktree {
                    worktree_id: worktree_id.clone(),
                    coworker: o.clone(),
                });

                spawn_effects.push(Effect::BroadcastCoworkerUpdate {
                    name: o.clone(),
                    status: "running".to_string(),
                    current_task: None,
                });
                spawn_effects.push(Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: daemon_messages::called_in_pending_task(
                        o,
                        &tid.to_string(),
                        config::get_personality(),
                    ),
                });

                effects.push(Effect::SpawnCoworkerWithCallbacks {
                    config,
                    on_success: spawn_effects,
                    on_failure: vec![],
                });
            }
            crate::rules::PendingTaskAction::Skip { ref reason } => {
                debug!("{}", reason);
            }
        }
    }

    // Case 2: Pending tasks without owners - assign ownership atomically, then spawn
    let pending_unowned = &snap.pending_tasks_without_owners;

    // Log PR review priority state for diagnostics, but never block task dispatch.
    // Previously this did `return effects` which created a deadlock: idle coworkers
    // sat with no work while the daemon waited for a reviewer to be spawned.
    // Reviewer spawning is handled independently in pr.rs — it doesn't need task
    // dispatch to be deferred.
    let active_review_count = snap.active_reviewers.len();
    let prs_with_reviewers = snap
        .reviewer_pr_assignments
        .values()
        .collect::<HashSet<_>>()
        .len();
    let unserved_prs = snap.prs_needing_review.saturating_sub(prs_with_reviewers);
    if unserved_prs > 0 {
        debug!(
            "PR review state: {} unserved PR(s) need review ({} total, {} already have reviewers), {}/{} active reviewers — task dispatch proceeds independently",
            unserved_prs,
            snap.prs_needing_review,
            prs_with_reviewers,
            active_review_count,
            MAX_CONCURRENT_REVIEWS
        );
    }

    // All tasks from snapshot for relationship lookups (blockedBy, PR owner search)
    let all_tasks = &snap.all_tasks;
    // Track PR# → coworker and task_id → coworker assignments made during this loop iteration.
    // This prevents assigning different coworkers to sub-tasks of the same PR review
    // when multiple sub-tasks are processed in the same tick.
    let mut pr_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut task_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    // Track coworker names assigned within this tick to prevent duplicate assignments.
    // This handles the case where next_available_name() returns the same name for
    // two unrelated tasks because the first spawn hasn't executed yet.
    let mut names_assigned_this_tick: HashSet<String> = HashSet::new();
    for task in pending_unowned.iter() {
        // Check dev coworkers limit before spawning (reserve slots for reviewers)
        if snap.is_at_dev_limit {
            debug!(
                "Dev coworkers limit reached, deferring unowned task !{}",
                task.id
            );
            break;
        }

        // Skip tasks that already have an in-flight AssignAndSpawn effect.
        // This prevents the race condition where a new tick sees a task as pending
        // before the previous tick's AssignAndSpawn effect has completed its disk write.
        if state.is_task_spawn_in_flight(&task.id) {
            debug!(
                "Task !{} already has in-flight spawn, skipping duplicate",
                task.id
            );
            continue;
        }

        // Skip tasks whose referenced PR is already merged.
        if let Some(pr_num_str) = crate::tasks::extract_pr_number_from_task(task)
            && let Ok(pr_num) = pr_num_str.parse::<u64>()
            && snap.merged_pr_numbers.contains(&pr_num)
        {
            info!(
                "Auto-completing stale task !{}: PR #{} has been merged",
                task.id, pr_num
            );
            effects.push(Effect::CompleteTask {
                task_id: task.id.clone(),
                repo_name: snap.repo_name.clone(),
            });
            effects.push(Effect::ClearBlockedBy {
                completed_task_id: task.id.clone(),
                repo_name: snap.repo_name.clone(),
            });
            continue;
        }

        // Step 1: Determine the coworker name by checking multiple grouping strategies.
        // Priority: in-memory PR map → in-memory blockedBy map → disk PR owner →
        //           blockedBy relationship → new coworker name
        let grouped_name: Option<String> = 'resolve: {
            // Strategy A: Extract PR number from subject or description
            if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(task) {
                // Check in-memory map first (handles same-tick assignments)
                if let Some(name) = pr_coworker_map.get(&pr_num) {
                    info!(
                        "Task !{} references PR #{} - assigning to in-memory owner {}",
                        task.id, pr_num, name
                    );
                    break 'resolve Some(name.clone());
                }
                // Check disk for previously assigned PR tasks
                if let Some(existing_owner) =
                    crate::tasks::find_pr_owner_in_tasks(&pr_num, all_tasks)
                {
                    info!(
                        "Task !{} references PR #{} - assigning to existing owner {}",
                        task.id, pr_num, existing_owner
                    );
                    break 'resolve Some(existing_owner);
                }
            }

            // Strategy B: Check blockedBy relationships
            // If this task is blocked by a task that was assigned in this loop, use that owner
            for blocked_by_id in &task.blocked_by {
                if let Some(name) = task_coworker_map.get(blocked_by_id) {
                    info!(
                        "Task !{} blocked by #{} - assigning to same owner {}",
                        task.id, blocked_by_id, name
                    );
                    break 'resolve Some(name.clone());
                }
            }
            // Check disk for blockedBy owners
            if let Some(owner) = crate::tasks::find_owner_via_blocked_by(task, all_tasks) {
                info!(
                    "Task !{} blocked by owned task - assigning to {}",
                    task.id, owner
                );
                break 'resolve Some(owner);
            }

            None
        };

        // Step 1b: Use grouped name if found, otherwise allocate a fresh coworker.
        // We always spawn fresh rather than reusing idle coworkers — idle coworkers
        // get shut down by the idle check loop, keeping the lifecycle simple:
        // spawn → work → PR → idle → shutdown.
        let was_grouped = grouped_name.is_some();
        let coworker_name = if let Some(name) = grouped_name {
            name
        } else {
            let Some(name) = state.coworkers.next_available_name() else {
                debug!("No available coworker slots for unowned task !{}", task.id);
                break;
            };
            debug!("Task !{}: allocated fresh coworker name {}", task.id, name,);
            name
        };

        // Check if this coworker is already running (grouped to an active coworker)
        let already_running = snap.active_names.contains(&coworker_name.to_lowercase());

        // Check if this coworker is an active reviewer (reviewers should not
        // receive dev task assignments — they have their own review work)
        let is_coworker_reviewer = snap
            .active_reviewers
            .contains(&coworker_name.to_lowercase());

        // Check if this coworker is already busy with an assigned task.
        // Split into two sources: persistent busyness (from snapshot) and
        // same-tick assignments (from this loop iteration).
        let is_busy_from_snapshot = snap.busy_coworkers.contains(&coworker_name.to_lowercase());
        let assigned_this_tick = names_assigned_this_tick.contains(&coworker_name.to_lowercase());

        // Skip running coworkers that are busy or reviewing.
        // Grouped tasks (same PR, blockedBy) are allowed to go to coworkers
        // that are busy from *previous ticks* (cross-tick grouping).
        // However, always skip if already assigned *this tick* — one nudge
        // per coworker per tick is sufficient, even for grouped tasks.
        if already_running
            && (is_coworker_reviewer
                || assigned_this_tick
                || (is_busy_from_snapshot && !was_grouped))
        {
            debug!(
                "Task !{}: skipping coworker {} (busy_snapshot={}, assigned_tick={}, reviewer={}, grouped={})",
                task.id,
                coworker_name,
                is_busy_from_snapshot,
                assigned_this_tick,
                is_coworker_reviewer,
                was_grouped
            );
            continue;
        }

        // For fresh-spawn names (not grouped), prevent assigning multiple tasks
        // to the same not-yet-spawned coworker within the same tick.
        if !already_running && (assigned_this_tick || is_busy_from_snapshot) && !was_grouped {
            debug!(
                "Task !{}: skipping {} (already assigned this tick)",
                task.id, coworker_name
            );
            continue;
        }

        info!(
            "Proposing task !{} for {} (already_running={})",
            task.id, coworker_name, already_running
        );

        // Record this assignment in in-memory maps for same-tick grouping.
        // These are ephemeral — they only coordinate decisions within this tick.
        // The actual disk write happens in the effect executor.
        task_coworker_map.insert(task.id.clone(), coworker_name.clone());
        if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(task) {
            pr_coworker_map.insert(pr_num, coworker_name.clone());
        }
        names_assigned_this_tick.insert(coworker_name.to_lowercase());

        // Build the prompt message — already-running coworkers need explicit claim instruction
        let prompt = if already_running {
            format_task_prompt(
                &task.id,
                &format!(
                    "You've been assigned task !{}: {}. Run `midtown task claim {}` to claim it, then get started!",
                    task.id, task.subject, task.id
                ),
            )
        } else {
            format_task_prompt(
                &task.id,
                &format!(
                    "You've been assigned task !{}: {}. Get started!",
                    task.id, task.subject
                ),
            )
        };

        if already_running {
            // Step 2a: Coworker is already running (grouped task) — nudge to claim the task.
            // The coworker runs `midtown task claim`, which writes ownership directly
            // via the daemon's RPC handler.
            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
                config::get_personality(),
            );
            effects.push(Effect::NudgeCoworkerWithCallbacks {
                name: coworker_name.clone(),
                message: prompt,
                on_success: vec![
                    Effect::RecordTaskAssignment {
                        coworker: coworker_name.clone(),
                        task_id: task.id.clone(),
                    },
                    Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: channel_msg,
                    },
                ],
            });
        } else {
            // Step 2b: Spawn a new coworker — assign ownership atomically with spawn
            // Reuse existing worktree if one is registered for this task (reassignment case).
            // Otherwise, compute a new worktree_id from the task subject.
            let (worktree_id, needs_registration) =
                if let Some(existing_wt_id) = snap.task_worktree_map.get(&task.id) {
                    (existing_wt_id.clone(), false)
                } else {
                    (
                        crate::worktree_registry::branch_slug_for_task(&task.id, &task.subject),
                        true,
                    )
                };
            let wt_path = crate::paths::worktrees_dir_for_repo(&state.repo_name).join(&worktree_id);

            let mut config = crate::launch::LaunchConfig::coworker(
                coworker_name.clone(),
                state.repo_name.clone(),
                crate::launch::SessionMode::Fresh,
                Some(prompt.clone()),
            );
            config.working_dir = Some(wt_path.clone());

            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
                config::get_personality(),
            );

            let mut spawn_effects = Vec::new();

            // Ensure the worktree exists BEFORE spawning (fixes effect pattern violation)
            spawn_effects.push(Effect::EnsureWorktree {
                worktree_id: worktree_id.clone(),
                path: wt_path.clone(),
            });

            // Register the task → worktree mapping if this is the first time
            if needs_registration {
                spawn_effects.push(Effect::RegisterWorktreeAssignment {
                    assignment: crate::worktree_registry::WorktreeAssignment {
                        worktree_id: worktree_id.clone(),
                        branch_name: worktree_id.clone(), // Branch name matches worktree_id for task worktrees
                        task_id: Some(task.id.clone()),
                        current_coworker: None, // Will be set by BindCoworkerToWorktree
                        pr_number: None,
                        created_at: chrono::Utc::now(),
                    },
                });
            }

            // Always bind the coworker to the worktree when spawning
            spawn_effects.push(Effect::BindCoworkerToWorktree {
                worktree_id: worktree_id.clone(),
                coworker: coworker_name.clone(),
            });

            spawn_effects.push(Effect::BroadcastCoworkerUpdate {
                name: coworker_name.clone(),
                status: "running".to_string(),
                current_task: None,
            });
            spawn_effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: channel_msg,
            });

            effects.push(Effect::AssignAndSpawn {
                task_id: task.id.clone(),
                owner: coworker_name.clone(),
                repo_name: snap.repo_name.clone(),
                config,
                on_success: spawn_effects,
                on_failure: vec![],
            });
        }
    }

    effects
}

// ============================================================================
// Task completion for PR merged
// ============================================================================

/// Build effects to auto-complete a task when its PR is merged.
///
/// This is a pure function that extracts the task ID from a PR title
/// (looking for `[Midtown #XX]`) and returns the effects needed to:
/// 1. Mark the task as completed
/// 2. Clear the task from other tasks' `blockedBy` arrays
/// 3. Post a notification to the channel
///
/// Returns an empty vector if no task ID is found in the title.
pub(super) fn build_task_completion_effects(
    pr_title: &str,
    pr_number: u64,
    repo_name: &str,
) -> Vec<Effect> {
    let Some(task_id) = crate::tasks::extract_task_id_from_pr_title(pr_title) else {
        return vec![];
    };

    let task_id_str = task_id.to_string();
    vec![
        Effect::CompleteTask {
            task_id: task_id_str.clone(),
            repo_name: repo_name.to_string(),
        },
        Effect::ClearBlockedBy {
            completed_task_id: task_id_str.clone(),
            repo_name: repo_name.to_string(),
        },
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "✅ Auto-completed task !{} (PR #{} merged)",
                task_id, pr_number
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duplicate_worker_sorting_by_start_time() {
        use chrono::{Duration, Utc};

        let now = Utc::now();
        let earlier = now - Duration::minutes(5);
        let later = now + Duration::minutes(5);

        let mut workers: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = vec![
            ("later_worker".to_string(), Some(later)),
            ("earlier_worker".to_string(), Some(earlier)),
            ("now_worker".to_string(), Some(now)),
        ];

        workers.sort_by(|a, b| match (&a.1, &b.1) {
            (Some(t1), Some(t2)) => t1.cmp(t2),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        assert_eq!(workers[0].0, "earlier_worker");
        assert_eq!(workers[1].0, "now_worker");
        assert_eq!(workers[2].0, "later_worker");
    }

    #[test]
    fn test_duplicate_worker_sorting_with_unknown_times() {
        use chrono::Utc;

        let now = Utc::now();

        let mut workers: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = vec![
            ("unknown_worker".to_string(), None),
            ("known_worker".to_string(), Some(now)),
            ("another_unknown".to_string(), None),
        ];

        workers.sort_by(|a, b| match (&a.1, &b.1) {
            (Some(t1), Some(t2)) => t1.cmp(t2),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        assert_eq!(workers[0].0, "known_worker");
        assert!(workers[1].1.is_none());
        assert!(workers[2].1.is_none());
    }

    #[test]
    fn test_filter_orphans_with_open_prs_filters_by_owner() {
        let flagged = vec![
            "amsterdam".to_string(),
            "riverside".to_string(),
            "park".to_string(),
        ];
        let open_pr_owners: HashSet<String> = ["riverside".to_string()].into_iter().collect();

        let result = filter_orphans_with_open_prs(flagged, &open_pr_owners);
        assert_eq!(result, vec!["amsterdam", "park"]);
    }

    #[test]
    fn test_filter_orphans_with_open_prs_all_have_open_prs() {
        let flagged = vec!["amsterdam".to_string(), "riverside".to_string()];
        let open_pr_owners: HashSet<String> = ["amsterdam".to_string(), "riverside".to_string()]
            .into_iter()
            .collect();

        let result = filter_orphans_with_open_prs(flagged, &open_pr_owners);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_orphans_with_open_prs_none_have_open_prs() {
        let flagged = vec!["amsterdam".to_string(), "park".to_string()];
        let open_pr_owners: HashSet<String> = HashSet::new();

        let result = filter_orphans_with_open_prs(flagged, &open_pr_owners);
        assert_eq!(result, vec!["amsterdam", "park"]);
    }

    #[test]
    fn test_partition_orphans_by_merged_status_exact_match() {
        // Scenario: york has a squash-merged PR on branch "york/feature-a".
        // The worktree shows "unmerged commits" because commit SHAs differ,
        // but the PR was actually merged.
        // York should be in the "merged" partition, amsterdam in "unmerged".
        let flagged = vec![
            "amsterdam".to_string(), // genuinely orphaned, branch: amsterdam/abandoned
            "york".to_string(),      // has merged PR, branch: york/feature-a
        ];
        let merged_pr_branches: HashSet<String> =
            ["york/feature-a".to_string()].into_iter().collect();

        // Mock function that returns branch names for each coworker
        let get_branch = |name: &str| -> Option<String> {
            match name {
                "york" => Some("york/feature-a".to_string()),
                "amsterdam" => Some("amsterdam/abandoned".to_string()),
                _ => None,
            }
        };

        let (merged, unmerged) =
            partition_orphans_by_merged_status(flagged, &merged_pr_branches, get_branch);

        // york's exact branch was merged - should be in merged partition
        assert_eq!(merged, vec!["york"]);
        // amsterdam is genuinely orphaned - should be in unmerged partition
        assert_eq!(unmerged, vec!["amsterdam"]);
    }

    #[test]
    fn test_partition_orphans_by_merged_status_different_branch() {
        // Scenario: york has a merged PR on branch "york/old-feature" but is now
        // working on "york/new-feature" which is orphaned.
        // The new branch should be in the "unmerged" partition.
        let flagged = vec!["york".to_string()];
        let merged_pr_branches: HashSet<String> =
            ["york/old-feature".to_string()].into_iter().collect();

        // York's current branch is different from the merged one
        let get_branch = |name: &str| -> Option<String> {
            match name {
                "york" => Some("york/new-feature".to_string()),
                _ => None,
            }
        };

        let (merged, unmerged) =
            partition_orphans_by_merged_status(flagged, &merged_pr_branches, get_branch);

        // york has a different branch - should be in unmerged partition
        assert!(merged.is_empty());
        assert_eq!(unmerged, vec!["york"]);
    }

    #[test]
    fn test_partition_orphans_by_merged_status_detached_head() {
        // Scenario: worktree is in detached HEAD state.
        // Worktrees only reach partition if safe_cleanup() returned false.
        // For detached HEAD, has_commits_beyond_base() returns false, so the only
        // reason it's flagged is has_uncommitted_changes() returned true.
        // We must warn (unmerged) rather than force-delete (merged) to prevent data loss.
        let flagged = vec![
            "columbus".to_string(), // detached HEAD, get_branch returns None
            "york".to_string(),     // has branch with unmerged commits
        ];
        let merged_pr_branches: HashSet<String> = HashSet::new();

        // columbus is in detached HEAD (None), york has a branch
        let get_branch = |name: &str| -> Option<String> {
            match name {
                "columbus" => None, // Detached HEAD
                "york" => Some("york/feature-a".to_string()),
                _ => None,
            }
        };

        let (merged, unmerged) =
            partition_orphans_by_merged_status(flagged, &merged_pr_branches, get_branch);

        // columbus is detached HEAD - goes to unmerged to warn Lead about uncommitted changes
        // york has a branch not in merged list - also goes to unmerged
        assert!(merged.is_empty());
        assert_eq!(unmerged, vec!["columbus", "york"]);
    }

    #[test]
    fn test_should_skip_orphan_flagging_before_pr_poll() {
        // During startup, PR poll hasn't run yet - should skip flagging
        // to avoid false positives (worktrees with open PRs incorrectly
        // flagged as orphaned because we don't have PR data yet)
        assert!(should_skip_orphan_flagging(false));
    }

    #[test]
    fn test_should_not_skip_orphan_flagging_after_pr_poll() {
        // After first PR poll completes, we have open_pr_owners data
        // and can safely flag orphans
        assert!(!should_skip_orphan_flagging(true));
    }

    #[test]
    fn test_compute_orphans_for_reviewer_clearing_skips_before_pr_poll() {
        // Bug scenario: During startup, PR poll hasn't run yet. If we clear
        // reviewer assignments, we'd incorrectly clear them for coworkers who
        // have open PRs (because open_pr_owners is empty until PR poll runs).
        let all_orphaned = vec!["amsterdam".to_string(), "york".to_string()];
        let open_pr_owners: HashSet<String> = HashSet::new(); // Empty during startup

        // Before PR poll initialized, should return None (skip clearing)
        let result = compute_orphans_for_reviewer_clearing(false, all_orphaned, &open_pr_owners);
        assert!(
            result.is_none(),
            "Should skip reviewer clearing before PR poll initializes"
        );
    }

    #[test]
    fn test_compute_orphans_for_reviewer_clearing_filters_open_pr_owners() {
        // After PR poll: amsterdam has an open PR, york doesn't.
        // Only york should have their reviewer assignment cleared.
        let all_orphaned = vec!["amsterdam".to_string(), "york".to_string()];
        let open_pr_owners: HashSet<String> = ["amsterdam".to_string()].into_iter().collect();

        let result = compute_orphans_for_reviewer_clearing(true, all_orphaned, &open_pr_owners);
        assert_eq!(
            result,
            Some(vec!["york".to_string()]),
            "Should only clear reviewer assignments for orphans without open PRs"
        );
    }

    #[test]
    fn test_compute_orphans_for_reviewer_clearing_all_have_open_prs() {
        // All orphaned coworkers have open PRs - should return None
        let all_orphaned = vec!["amsterdam".to_string(), "york".to_string()];
        let open_pr_owners: HashSet<String> = ["amsterdam".to_string(), "york".to_string()]
            .into_iter()
            .collect();

        let result = compute_orphans_for_reviewer_clearing(true, all_orphaned, &open_pr_owners);
        assert!(
            result.is_none(),
            "Should return None when all orphans have open PRs"
        );
    }

    #[test]
    fn test_compute_orphans_for_reviewer_clearing_none_orphaned() {
        // No orphaned worktrees - should return None
        let all_orphaned: Vec<String> = vec![];
        let open_pr_owners: HashSet<String> = HashSet::new();

        let result = compute_orphans_for_reviewer_clearing(true, all_orphaned, &open_pr_owners);
        assert!(result.is_none(), "Should return None when no orphans");
    }

    #[test]
    fn test_build_task_completion_effects_with_task_id() {
        let effects =
            build_task_completion_effects("feat: Add auth endpoint [Midtown #42]", 123, "myrepo");

        assert_eq!(effects.len(), 3, "Should return 3 effects");

        // Verify CompleteTask effect
        match &effects[0] {
            Effect::CompleteTask { task_id, repo_name } => {
                assert_eq!(task_id, "42");
                assert_eq!(repo_name, "myrepo");
            }
            _ => panic!("First effect should be CompleteTask"),
        }

        // Verify ClearBlockedBy effect
        match &effects[1] {
            Effect::ClearBlockedBy {
                completed_task_id,
                repo_name,
            } => {
                assert_eq!(completed_task_id, "42");
                assert_eq!(repo_name, "myrepo");
            }
            _ => panic!("Second effect should be ClearBlockedBy"),
        }

        // Verify PostToChannel effect
        match &effects[2] {
            Effect::PostToChannel { sender, message } => {
                assert_eq!(sender, "midtown");
                assert!(message.contains("42"));
                assert!(message.contains("123"));
            }
            _ => panic!("Third effect should be PostToChannel"),
        }
    }

    #[test]
    fn test_build_task_completion_effects_without_task_id() {
        let effects = build_task_completion_effects("feat: Add auth endpoint", 123, "myrepo");

        assert!(
            effects.is_empty(),
            "Should return empty vec when no task ID in title"
        );
    }

    #[test]
    fn test_build_task_completion_effects_message_says_merged() {
        let effects =
            build_task_completion_effects("feat: Add auth endpoint [Midtown #42]", 123, "myrepo");

        // Verify the channel message says "merged" not "opened"
        match &effects[2] {
            Effect::PostToChannel { sender, message } => {
                assert_eq!(sender, "midtown");
                assert!(
                    message.contains("merged"),
                    "Message should say 'merged', got: {}",
                    message
                );
                assert!(
                    !message.contains("opened"),
                    "Message should not say 'opened', got: {}",
                    message
                );
            }
            _ => panic!("Third effect should be PostToChannel"),
        }
    }

    // ======================================================================
    // decide_orphan_cleanup tests
    // ======================================================================

    #[test]
    fn test_decide_orphan_cleanup_empty_data() {
        let data = OrphanCleanupData {
            all_orphaned: vec![],
            merged_worktrees_to_cleanup: vec![],
            pr_poll_initialized: true,
            open_pr_owners: HashSet::new(),
            gh_cleaned: vec![],
            due_for_warning: vec![],
            stale_branch_cleanup_due: false,
        };
        let effects = decide_orphan_cleanup(&data);
        assert!(effects.is_empty());
    }

    #[test]
    fn test_decide_orphan_cleanup_clears_reviewer_assignments() {
        let data = OrphanCleanupData {
            all_orphaned: vec!["amsterdam".to_string(), "york".to_string()],
            merged_worktrees_to_cleanup: vec![],
            pr_poll_initialized: true,
            open_pr_owners: ["amsterdam".to_string()].into_iter().collect(),
            gh_cleaned: vec![],
            due_for_warning: vec![],
            stale_branch_cleanup_due: false,
        };
        let effects = decide_orphan_cleanup(&data);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::ClearOrphanedReviewerAssignments { orphaned_coworkers } => {
                assert_eq!(orphaned_coworkers, &vec!["york".to_string()]);
            }
            _ => panic!("Expected ClearOrphanedReviewerAssignments"),
        }
    }

    #[test]
    fn test_decide_orphan_cleanup_force_deletes_merged_worktrees() {
        let data = OrphanCleanupData {
            all_orphaned: vec![],
            merged_worktrees_to_cleanup: vec!["york".to_string(), "park".to_string()],
            pr_poll_initialized: true,
            open_pr_owners: HashSet::new(),
            gh_cleaned: vec![],
            due_for_warning: vec![],
            stale_branch_cleanup_due: false,
        };
        let effects = decide_orphan_cleanup(&data);
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::ForceCleanupWorktrees { names } => {
                assert_eq!(names, &vec!["york".to_string(), "park".to_string()]);
            }
            _ => panic!("Expected ForceCleanupWorktrees"),
        }
    }

    #[test]
    fn test_decide_orphan_cleanup_warns_about_unmerged() {
        let data = OrphanCleanupData {
            all_orphaned: vec![],
            merged_worktrees_to_cleanup: vec![],
            pr_poll_initialized: true,
            open_pr_owners: HashSet::new(),
            gh_cleaned: vec![],
            due_for_warning: vec!["amsterdam".to_string()],
            stale_branch_cleanup_due: false,
        };
        let effects = decide_orphan_cleanup(&data);
        // Should produce: PostSystemMessage, NudgeLead, SendPushNotification
        assert_eq!(effects.len(), 3);
        assert!(matches!(&effects[0], Effect::PostSystemMessage { .. }));
        assert!(matches!(&effects[1], Effect::NudgeLead { .. }));
        assert!(matches!(
            &effects[2],
            Effect::SendPushNotification { tag, .. } if tag == "orphan_warning"
        ));
    }

    #[test]
    fn test_decide_orphan_cleanup_full_scenario() {
        // All three kinds of effects: reviewer clearing, force cleanup, warnings
        let data = OrphanCleanupData {
            all_orphaned: vec![
                "amsterdam".to_string(),
                "york".to_string(),
                "park".to_string(),
            ],
            merged_worktrees_to_cleanup: vec!["york".to_string()],
            pr_poll_initialized: true,
            open_pr_owners: ["amsterdam".to_string()].into_iter().collect(),
            gh_cleaned: vec![],
            due_for_warning: vec!["park".to_string()],
            stale_branch_cleanup_due: false,
        };
        let effects = decide_orphan_cleanup(&data);
        // ClearOrphanedReviewerAssignments(york, park) + ForceCleanupWorktrees(york) +
        // PostSystemMessage + NudgeLead + SendPushNotification
        assert_eq!(effects.len(), 5);
        assert!(matches!(
            &effects[0],
            Effect::ClearOrphanedReviewerAssignments { .. }
        ));
        assert!(matches!(&effects[1], Effect::ForceCleanupWorktrees { .. }));
        assert!(matches!(&effects[2], Effect::PostSystemMessage { .. }));
        assert!(matches!(&effects[3], Effect::NudgeLead { .. }));
        assert!(matches!(&effects[4], Effect::SendPushNotification { .. }));
    }

    #[test]
    fn test_decide_orphan_cleanup_gh_cleaned_posts_to_channel() {
        let data = OrphanCleanupData {
            all_orphaned: vec![],
            merged_worktrees_to_cleanup: vec![],
            pr_poll_initialized: true,
            open_pr_owners: HashSet::new(),
            gh_cleaned: vec!["york".to_string(), "park".to_string()],
            due_for_warning: vec![],
            stale_branch_cleanup_due: false,
        };
        let effects = decide_orphan_cleanup(&data);
        assert_eq!(effects.len(), 2);
        for effect in &effects {
            assert!(matches!(effect, Effect::PostToChannel { sender, .. } if sender == "midtown"));
        }
    }

    #[test]
    fn test_decide_orphan_cleanup_stale_branch_cleanup() {
        let data = OrphanCleanupData {
            all_orphaned: vec![],
            merged_worktrees_to_cleanup: vec![],
            pr_poll_initialized: true,
            open_pr_owners: HashSet::new(),
            gh_cleaned: vec![],
            due_for_warning: vec![],
            stale_branch_cleanup_due: true,
        };
        let effects = decide_orphan_cleanup(&data);
        assert_eq!(effects.len(), 1);
        assert!(matches!(&effects[0], Effect::CleanStaleBranches));
    }

    // ======================================================================
    // decide_discovered_coworker_nudges tests
    // ======================================================================

    #[test]
    fn test_discovered_nudges_empty() {
        let effects = decide_discovered_coworker_nudges(&[], &HashMap::new(), &HashMap::new());
        assert!(effects.is_empty());
    }

    #[test]
    fn test_discovered_nudges_task_owner() {
        let discovered = vec!["lexington".to_string()];
        let mut owner_tasks = HashMap::new();
        owner_tasks.insert(
            "lexington".to_string(),
            ("42".to_string(), "Fix auth bug".to_string()),
        );
        let reviewer_prs = HashMap::new();

        let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
        // NudgeCoworker + PostToChannel
        assert_eq!(effects.len(), 2);
        match &effects[0] {
            Effect::NudgeCoworker { name, message } => {
                assert_eq!(name, "lexington");
                assert!(message.contains("Resume task !42"));
            }
            _ => panic!("Expected NudgeCoworker"),
        }
        match &effects[1] {
            Effect::PostToChannel { sender, message } => {
                assert_eq!(sender, "midtown");
                assert!(message.contains("lexington"));
                assert!(message.contains("task !42"));
            }
            _ => panic!("Expected PostToChannel"),
        }
    }

    #[test]
    fn test_discovered_nudges_reviewer() {
        let discovered = vec!["park".to_string()];
        let owner_tasks = HashMap::new();
        let mut reviewer_prs = HashMap::new();
        reviewer_prs.insert("park".to_string(), 99);

        let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
        // NudgeCoworker + PostToChannel
        assert_eq!(effects.len(), 2);
        match &effects[0] {
            Effect::NudgeCoworker { name, .. } => {
                assert_eq!(name, "park");
            }
            _ => panic!("Expected NudgeCoworker"),
        }
        match &effects[1] {
            Effect::PostToChannel { message, .. } => {
                assert!(message.contains("PR #99"));
            }
            _ => panic!("Expected PostToChannel"),
        }
    }

    #[test]
    fn test_discovered_nudges_no_assignment() {
        let discovered = vec!["broadway".to_string()];
        let owner_tasks = HashMap::new();
        let reviewer_prs = HashMap::new();

        let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
        assert!(
            effects.is_empty(),
            "Coworker with no task or review should produce no effects"
        );
    }

    #[test]
    fn test_discovered_nudges_mixed() {
        let discovered = vec![
            "lexington".to_string(), // has task
            "park".to_string(),      // has review
            "broadway".to_string(),  // no assignment
        ];
        let mut owner_tasks = HashMap::new();
        owner_tasks.insert(
            "lexington".to_string(),
            ("42".to_string(), "Fix auth bug".to_string()),
        );
        let mut reviewer_prs = HashMap::new();
        reviewer_prs.insert("park".to_string(), 99);

        let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
        // lexington: NudgeCoworker + PostToChannel = 2
        // park: NudgeCoworker + PostToChannel = 2
        // broadway: nothing = 0
        assert_eq!(effects.len(), 4);
    }

    #[test]
    fn test_discovered_nudges_task_takes_priority_over_review() {
        // If a coworker has both a task and a review assignment,
        // the task takes priority (task check comes first in code)
        let discovered = vec!["lexington".to_string()];
        let mut owner_tasks = HashMap::new();
        owner_tasks.insert(
            "lexington".to_string(),
            ("42".to_string(), "Fix auth bug".to_string()),
        );
        let mut reviewer_prs = HashMap::new();
        reviewer_prs.insert("lexington".to_string(), 99);

        let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
        assert_eq!(effects.len(), 2);
        // Should nudge about the task, not the review
        match &effects[0] {
            Effect::NudgeCoworker { message, .. } => {
                assert!(message.contains("Resume task !42"));
            }
            _ => panic!("Expected NudgeCoworker"),
        }
    }

    // ======================================================================
    // WorktreeRegistry integration tests
    // ======================================================================

    #[test]
    fn test_spawn_for_pending_tasks_generates_registry_effects_new_task() {
        use crate::tasks::{Task, TaskStatus};
        use std::time::SystemTime;

        // Setup: create a snapshot with a pending task (no owner, not in registry)
        let snap = snapshot::WorldSnapshot {
            pending_tasks_without_owners: vec![Task {
                id: "42".to_string(),
                subject: "Add auth endpoint".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                blocked_by: vec![],
                description: None,
                created_at: Some(SystemTime::now()),
            }],
            tasks_with_worktrees: HashSet::new(), // Task not in registry yet
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            is_at_dev_limit: false,
            active_names: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
        };

        let state = make_test_state();

        let effects = spawn_for_pending_tasks(&snap, &state);

        // The registry effects are inside the on_success of AssignAndSpawn
        assert_eq!(
            effects.len(),
            1,
            "Should generate exactly one top-level effect"
        );

        let assign_and_spawn = effects
            .iter()
            .find_map(|e| {
                if let Effect::AssignAndSpawn {
                    task_id,
                    owner,
                    on_success,
                    ..
                } = e
                {
                    Some((task_id, owner, on_success))
                } else {
                    None
                }
            })
            .expect("Should have AssignAndSpawn effect");

        assert_eq!(assign_and_spawn.0, "42");

        // Verify RegisterWorktreeAssignment is in on_success
        let register_count = assign_and_spawn
            .2
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        let bind_count = assign_and_spawn
            .2
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
            .count();

        assert_eq!(
            register_count, 1,
            "Should have RegisterWorktreeAssignment in on_success"
        );
        assert_eq!(
            bind_count, 1,
            "Should have BindCoworkerToWorktree in on_success"
        );

        // Verify RegisterWorktreeAssignment has correct fields
        let register_effect = assign_and_spawn
            .2
            .iter()
            .find_map(|e| {
                if let Effect::RegisterWorktreeAssignment { assignment } = e {
                    Some(assignment)
                } else {
                    None
                }
            })
            .expect("Should have RegisterWorktreeAssignment");

        assert_eq!(register_effect.task_id, Some("42".to_string()));
        assert!(register_effect.worktree_id.starts_with("task-42-"));
        assert_eq!(register_effect.branch_name, register_effect.worktree_id);
    }

    #[test]
    fn test_spawn_for_pending_tasks_reuses_worktree_for_owned_task() {
        // Setup: pending task with owner, task already in registry
        let snap = snapshot::WorldSnapshot {
            pending_tasks_with_owners: vec![(
                "42".to_string(),
                "Add auth endpoint".to_string(),
                "lexington".to_string(),
            )],
            tasks_with_worktrees: ["42".to_string()].into_iter().collect(), // Task already has worktree
            task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
                .into_iter()
                .collect(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            is_at_dev_limit: false,
            active_names: HashSet::new(),
            active_reviewers: HashSet::new(),
            busy_coworkers: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            all_tasks: vec![],
            pending_tasks_without_owners: vec![],
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
        };

        let state = make_test_state();

        let effects = spawn_for_pending_tasks(&snap, &state);

        // Find the SpawnCoworkerWithCallbacks effect (for owned pending tasks, uses this variant)
        let spawn_effect = effects
            .iter()
            .find_map(|e| {
                if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                    Some(on_success)
                } else {
                    None
                }
            })
            .expect("Should have SpawnCoworkerWithCallbacks effect for owned pending task");

        // Should NOT generate RegisterWorktreeAssignment (worktree already exists)
        // SHOULD generate BindCoworkerToWorktree (rebind to new owner)
        let register_count = spawn_effect
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        let bind_count = spawn_effect
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
            .count();

        assert_eq!(
            register_count, 0,
            "Should NOT generate RegisterWorktreeAssignment for existing worktree"
        );
        assert_eq!(
            bind_count, 1,
            "Should generate BindCoworkerToWorktree to rebind"
        );
    }

    // ======================================================================
    // Worktree reuse on reassignment tests
    // ======================================================================

    #[test]
    fn test_orphan_recovery_reuses_existing_task_worktree() {
        // Scenario: Task !42 was owned by "lexington" who died. The task has
        // an existing worktree "task-42-add-auth-endpoint" registered. When
        // recovering, the spawn should reuse that worktree and bind the coworker.
        let snap = snapshot::WorldSnapshot {
            in_progress_tasks: vec![(
                "42".to_string(),
                "Add auth endpoint".to_string(),
                "lexington".to_string(),
            )],
            active_names: HashSet::new(), // lexington is NOT active (orphaned)
            coworkers_with_open_prs: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashSet::new(),
            tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
            task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
                .into_iter()
                .collect(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            busy_coworkers: HashSet::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
        };

        let state = make_test_state();
        let effects = check_and_recover_orphans(&snap, &state);

        assert_eq!(
            effects.len(),
            1,
            "Should generate exactly one top-level effect"
        );

        // Verify SpawnCoworkerWithCallbacks has working_dir set to the existing worktree
        let spawn = effects
            .iter()
            .find_map(|e| {
                if let Effect::SpawnCoworkerWithCallbacks {
                    config, on_success, ..
                } = e
                {
                    Some((config, on_success))
                } else {
                    None
                }
            })
            .expect("Should have SpawnCoworkerWithCallbacks");

        let (config, on_success) = spawn;

        // Working dir should point to the existing worktree
        let expected_path =
            crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
        assert_eq!(
            config.working_dir,
            Some(expected_path),
            "Should set working_dir to the existing task worktree"
        );

        // on_success should include EnsureWorktree and BindCoworkerToWorktree
        let ensure_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::EnsureWorktree { worktree_id, .. } if worktree_id == "task-42-add-auth-endpoint"))
            .count();
        let bind_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { worktree_id, coworker } if worktree_id == "task-42-add-auth-endpoint" && coworker == "lexington"))
            .count();

        assert_eq!(
            ensure_count, 1,
            "Should have EnsureWorktree for existing worktree"
        );
        assert_eq!(
            bind_count, 1,
            "Should have BindCoworkerToWorktree to rebind"
        );

        // Should NOT have RegisterWorktreeAssignment (worktree already registered)
        let register_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        assert_eq!(
            register_count, 0,
            "Should NOT register worktree again (already exists)"
        );
    }

    #[test]
    fn test_orphan_recovery_creates_new_worktree_when_none_exists() {
        // Scenario: Task !42 was owned by "lexington" who died, but the task
        // has NO worktree registered (legacy/pre-registry task). The spawn
        // should compute a new worktree_id, set working_dir, and emit
        // EnsureWorktree + RegisterWorktreeAssignment + BindCoworkerToWorktree.
        let snap = snapshot::WorldSnapshot {
            in_progress_tasks: vec![(
                "42".to_string(),
                "Add auth endpoint".to_string(),
                "lexington".to_string(),
            )],
            active_names: HashSet::new(),
            coworkers_with_open_prs: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashSet::new(),
            tasks_with_worktrees: HashSet::new(), // No worktree registered
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            busy_coworkers: HashSet::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
        };

        let state = make_test_state();
        let effects = check_and_recover_orphans(&snap, &state);

        assert_eq!(effects.len(), 1);

        let spawn = effects
            .iter()
            .find_map(|e| {
                if let Effect::SpawnCoworkerWithCallbacks {
                    config, on_success, ..
                } = e
                {
                    Some((config, on_success))
                } else {
                    None
                }
            })
            .expect("Should have SpawnCoworkerWithCallbacks");

        let (config, on_success) = spawn;

        // Working dir SHOULD be set to computed worktree path
        assert!(
            config.working_dir.is_some(),
            "Should set working_dir to computed worktree path"
        );
        let working_dir = config.working_dir.as_ref().unwrap();
        assert!(
            working_dir
                .to_string_lossy()
                .contains("task-42-add-auth-endpoint"),
            "Working dir should be computed from task subject: {:?}",
            working_dir
        );

        // on_success should include EnsureWorktree, RegisterWorktreeAssignment, and BindCoworkerToWorktree
        let ensure_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
            .count();
        let register_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        let bind_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
            .count();

        assert_eq!(ensure_count, 1, "Should have EnsureWorktree");
        assert_eq!(
            register_count, 1,
            "Should have RegisterWorktreeAssignment for new worktree"
        );
        assert_eq!(bind_count, 1, "Should have BindCoworkerToWorktree");

        // Verify the RegisterWorktreeAssignment effect has correct data
        let register_effect = on_success
            .iter()
            .find_map(|e| {
                if let Effect::RegisterWorktreeAssignment { assignment } = e {
                    Some(assignment)
                } else {
                    None
                }
            })
            .expect("Should have RegisterWorktreeAssignment");

        assert_eq!(register_effect.task_id, Some("42".to_string()));
        assert!(
            register_effect
                .worktree_id
                .contains("task-42-add-auth-endpoint")
        );
        assert_eq!(register_effect.current_coworker, None); // Set by BindCoworkerToWorktree
    }

    #[test]
    fn test_spawn_for_pending_unowned_reuses_existing_worktree() {
        // Scenario: Task !42 was previously owned by another coworker who died.
        // The task was reset to pending (no owner). It already has a worktree
        // "task-42-add-auth-endpoint" registered. A new coworker should reuse it.
        use crate::tasks::{Task, TaskStatus};
        use std::time::SystemTime;

        let snap = snapshot::WorldSnapshot {
            pending_tasks_without_owners: vec![Task {
                id: "42".to_string(),
                subject: "Add auth endpoint".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                blocked_by: vec![],
                description: None,
                created_at: Some(SystemTime::now()),
            }],
            tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
            task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
                .into_iter()
                .collect(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            is_at_dev_limit: false,
            active_names: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
        };

        let state = make_test_state();
        let effects = spawn_for_pending_tasks(&snap, &state);

        assert_eq!(effects.len(), 1);

        let assign_and_spawn = effects
            .iter()
            .find_map(|e| {
                if let Effect::AssignAndSpawn {
                    config, on_success, ..
                } = e
                {
                    Some((config, on_success))
                } else {
                    None
                }
            })
            .expect("Should have AssignAndSpawn");

        let (config, on_success) = assign_and_spawn;

        // Working dir should point to the EXISTING worktree (not a freshly computed one)
        let expected_path =
            crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
        assert_eq!(
            config.working_dir,
            Some(expected_path),
            "Should reuse existing worktree path"
        );

        // Should NOT generate RegisterWorktreeAssignment (worktree already exists)
        let register_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        assert_eq!(
            register_count, 0,
            "Should NOT re-register existing worktree"
        );

        // SHOULD generate BindCoworkerToWorktree with the existing worktree_id
        let bind_effects: Vec<_> = on_success
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
            .collect();
        assert_eq!(
            bind_effects.len(),
            1,
            "Should bind coworker to existing worktree"
        );

        if let Effect::BindCoworkerToWorktree { worktree_id, .. } = bind_effects[0] {
            assert_eq!(
                worktree_id, "task-42-add-auth-endpoint",
                "Should bind to the existing worktree, not a new one"
            );
        }

        // SHOULD generate EnsureWorktree with the existing worktree_id
        let ensure_effects: Vec<_> = on_success
            .iter()
            .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
            .collect();
        assert_eq!(
            ensure_effects.len(),
            1,
            "Should ensure existing worktree exists"
        );

        if let Effect::EnsureWorktree { worktree_id, .. } = ensure_effects[0] {
            assert_eq!(
                worktree_id, "task-42-add-auth-endpoint",
                "Should ensure the existing worktree, not a new one"
            );
        }
    }

    /// Helper to create minimal DaemonState for testing
    fn make_test_state() -> DaemonState {
        use std::process::Command;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("temp dir");
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git commit");

        let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
            .expect("worktree manager");
        let cm = crate::coworker::CoworkerManager::new("test-session", wm);
        let channel_dir = temp_dir.path().join("channel");
        std::fs::create_dir_all(&channel_dir).expect("channel dir");
        let channel = crate::channel::Channel::new(&channel_dir, "midtown").expect("channel");

        // Leak temp_dir so it survives the test
        std::mem::forget(temp_dir);

        DaemonState::new(
            "/tmp/test.sock".into(),
            cm,
            "test-repo".to_string(),
            vec![],
            channel,
            None,
            10,
            None,
            "main".to_string(),
        )
        .expect("daemon state")
    }
}
