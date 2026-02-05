//! Task dispatch — orphan recovery, duplicate detection, pending task spawning.
//!
//! These functions run on the `TaskDispatchTick` event and coordinate coworker
//! lifecycle around the shared task list. They read from `WorldSnapshot` and
//! return `Vec<Effect>` for execution by the effect runner.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::message::Message;
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

    // Decide which orphan (if any) to recover using pure decision function
    let recovery = crate::rules::decide_orphan_recovery(
        &snap.in_progress_tasks,
        &snap.active_names,
        snap.is_at_dev_limit,
        &snap.coworkers_with_open_prs,
        &snap.ci_passed_pr_coworkers,
        &snap.review_feedback_pr_coworkers,
    );

    let Some(recovery) = recovery else {
        return vec![];
    };

    // Check per-coworker spawn failure cooldown to prevent infinite retry loops
    {
        let cooldowns = state.cooldowns.lock().unwrap();
        if !cooldowns.check("spawn_failure", &recovery.owner, SPAWN_FAILURE_COOLDOWN) {
            debug!(
                "Spawn failure cooldown active for {} — skipping orphan recovery for task #{}",
                recovery.owner, recovery.task_id
            );
            return vec![];
        }
    }

    info!(
        "Detected orphaned task #{} owned by {} - attempting recovery",
        recovery.task_id, recovery.owner
    );

    let prompt = format_task_prompt(
        &recovery.task_id,
        &format!(
            "You've been assigned task #{}: {}. Your previous session was interrupted but your worktree and branch are still intact. Check your git status and get started!",
            recovery.task_id, recovery.task_subject
        ),
    );

    // Spawn fresh (no --continue) — the coworker keeps the same name so they
    // retain their worktree and branch. This is the same path as normal task
    // assignment, just reusing the previous coworker name.
    let config = crate::tmux::ClaudeLaunchConfig::coworker(
        recovery.owner.clone(),
        state.repo_name.clone(),
        crate::tmux::SessionMode::Fresh,
        Some(prompt),
    );

    // Return spawn effect with success/failure callbacks
    vec![Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success: vec![
            Effect::BroadcastCoworkerUpdate {
                name: recovery.owner.clone(),
                status: "running".to_string(),
                current_task: None,
            },
            Effect::RecordCooldown {
                category: "orphan_spawn".to_string(),
                key: "global".to_string(),
            },
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "♻️ Recovered coworker {} for orphaned task #{}",
                    recovery.owner, recovery.task_id
                ),
            },
        ],
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
                    "🔄 Task #{} reset to pending - {} could not be respawned (backing off for {}s)",
                    recovery.task_id,
                    recovery.owner,
                    SPAWN_FAILURE_COOLDOWN.as_secs()
                ),
            },
        ],
    }]
}

/// Nudge coworkers that were discovered from tmux on daemon startup.
///
/// After a daemon restart, existing coworkers are found in tmux but they may
/// be stuck waiting for input or idle. This function checks if each discovered
/// coworker has an assigned task (in_progress with them as owner) or a reviewer
/// assignment (in github-state.json), and nudges them to continue.
///
/// This runs once at startup, with a short delay to let coworkers settle.
pub(super) async fn nudge_discovered_coworkers(state: &DaemonState) {
    let discovered = state.coworkers.take_discovered_on_startup();
    if discovered.is_empty() {
        return;
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
    let mut owner_tasks: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for (task_id, task_subject, owner) in &in_progress {
        let owner_lower = owner.trim().trim_matches('"').to_lowercase();
        if !owner_lower.is_empty() {
            owner_tasks.insert(owner_lower, (task_id.clone(), task_subject.clone()));
        }
    }

    // Check reviewer assignments from daemon-state.json
    let reviewer_prs: std::collections::HashMap<String, u64> = {
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

    for name in &discovered {
        let name_lower = name.to_lowercase();

        // Check for an in_progress task owned by this coworker
        if let Some((task_id, task_subject)) = owner_tasks.get(&name_lower) {
            let prompt = format_task_prompt(
                task_id,
                &format!(
                    "Resume task #{}: {}. The daemon was restarted and discovered you still running. Check your git status and continue where you left off.",
                    task_id, task_subject
                ),
            );

            info!(
                "Nudging discovered coworker {} to resume task #{}",
                name, task_id
            );

            if let Err(e) = state.coworkers.nudge(name, &prompt) {
                warn!("Failed to nudge discovered coworker {}: {}", name, e);
            }

            // Post recovery message to channel
            let msg = Message::text(
                "midtown",
                format!(
                    "♻️ Nudged discovered coworker {} to resume task #{}",
                    name, task_id
                ),
            );
            if let Err(e) = state.send_and_broadcast(&msg) {
                warn!("Failed to post discovery nudge message: {}", e);
            }
        } else if let Some(pr_number) = reviewer_prs.get(&name_lower) {
            // Coworker was assigned to review a PR
            let prompt = crate::agents::reviewer_resume_prompt(*pr_number);

            info!(
                "Nudging discovered coworker {} to resume review of PR #{}",
                name, pr_number
            );

            if let Err(e) = state.coworkers.nudge(name, &prompt) {
                warn!("Failed to nudge discovered reviewer {}: {}", name, e);
            }

            let msg = Message::text(
                "midtown",
                format!(
                    "♻️ Nudged discovered reviewer {} to resume PR #{} review",
                    name, pr_number
                ),
            );
            if let Err(e) = state.send_and_broadcast(&msg) {
                warn!("Failed to post discovery nudge message: {}", e);
            }
        } else {
            debug!(
                "Discovered coworker {} has no assigned task or review - skipping nudge",
                name
            );
        }

        // Small delay between nudges to avoid overwhelming tmux
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
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
            "Detected {} duplicate workers on task #{} ({}): {:?}",
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
            "Keeping {} (started {:?}) for task #{}",
            keeper, keeper_time, task_id
        );

        for (duplicate, dup_time) in workers_with_times.into_iter().skip(1) {
            warn!(
                "Killing duplicate worker {} (started {:?}) for task #{} - {} is already working on it",
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
                    "🔪 Killed duplicate worker {} on task #{} ({}) - {} started earlier",
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

/// Clean up orphaned worktrees that have no active coworker.
///
/// Worktrees with no commits beyond the base branch are deleted.
/// Worktrees whose PRs were merged (squash-merge) are also cleaned up.
/// Worktrees with genuinely unmerged commits are flagged to the Lead via channel.
///
/// The worktree operations run in a blocking task to avoid blocking the async
/// runtime. We process a limited number per tick to avoid saturating the
/// blocking thread pool.
///
/// Returns effects for clearing reviewer assignments of orphaned coworkers,
/// but only after the first PR poll completes (when we have accurate PR data).
/// Coworkers with open PRs are excluded from reviewer assignment clearing
/// since they may be "on break" waiting for reviews.
pub(super) async fn cleanup_orphaned_worktrees(state: &DaemonState) -> Vec<Effect> {
    let mut effects = Vec::new();

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
    let (pr_poll_initialized, open_pr_owners) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        (cache.pr_poll_initialized, cache.open_pr_owners.clone())
    };
    if should_skip_orphan_flagging(pr_poll_initialized) {
        debug!("Skipping orphan flagging - PR poll not yet initialized");
        return effects;
    }

    // Queue effect to clear reviewer assignments for orphaned coworkers.
    // Uses pure helper function for testability.
    if let Some(orphans) =
        compute_orphans_for_reviewer_clearing(pr_poll_initialized, all_orphaned, &open_pr_owners)
    {
        effects.push(Effect::ClearOrphanedReviewerAssignments {
            orphaned_coworkers: orphans,
        });
    }

    // Filter out worktrees whose branches have open PRs (by coworker name).
    let flagged = filter_orphans_with_open_prs(flagged, &open_pr_owners);

    // Partition worktrees by whether their PR was merged.
    // Merged PRs can be safely cleaned up; unmerged need investigation.
    let merged_pr_branches = {
        let cache = state.pr_coworker_cache.read().unwrap();
        cache.merged_pr_branches.clone()
    };
    let (merged_pr_worktrees, unmerged) =
        partition_orphans_by_merged_status(flagged, &merged_pr_branches, |name| {
            branch_map.get(name).cloned().flatten()
        });

    // Clean up worktrees whose PRs were merged (squash-merge case).
    // This runs in a blocking task since it involves git commands.
    if !merged_pr_worktrees.is_empty() {
        let coworkers = state.coworkers.clone();
        let to_cleanup = merged_pr_worktrees.clone();
        tokio::task::spawn_blocking(move || {
            for name in to_cleanup {
                match coworkers.force_cleanup_worktree(&name) {
                    Ok(()) => {
                        info!("Cleaned up orphaned worktree for {} (PR was merged)", name);
                    }
                    Err(e) => {
                        warn!("Failed to cleanup merged-PR worktree for {}: {}", name, e);
                    }
                }
            }
        })
        .await
        .ok();
    }

    for name in &unmerged {
        debug!("Orphan worktree flagged (no open or merged PR): {}", name);
    }

    let mut tracker = state.orphan_tracker.write().unwrap();

    // Prune entries for worktrees that are no longer flagged
    tracker.prune(&unmerged);

    // Track newly flagged worktrees and collect those due for a warning
    let due_for_warning: Vec<_> = unmerged
        .into_iter()
        .filter(|name| {
            tracker.track(name.clone());
            tracker.should_warn(name)
        })
        .collect();

    if due_for_warning.is_empty() {
        return effects;
    }

    // Record warnings and log (rate-limited by OrphanTracker)
    for name in &due_for_warning {
        warn!(
            "Orphaned worktree for {} has unmerged commits - flagging to lead",
            name
        );
        tracker.record_warn(name);
    }
    drop(tracker);

    // Notify @lead about orphaned worktrees with unmerged commits
    let names_list = due_for_warning.join(", ");
    let nudge_text = format!(
        "⚠️ @lead Orphaned worktrees with unmerged commits: {}. \
         Please investigate and decide whether to merge or delete these branches.",
        names_list
    );
    let msg = Message::system(nudge_text.clone());
    if let Err(e) = state.send_and_broadcast(&msg) {
        warn!("Failed to send orphan flag message: {}", e);
    }

    // Directly nudge the lead (don't rely solely on chat monitor).
    // This matches the pattern used for CI failures on the default branch.
    if let Err(e) = state.coworkers.nudge_lead(&nudge_text) {
        warn!("Failed to nudge lead for orphaned worktrees: {}", e);
    } else {
        info!("Nudged lead about orphaned worktrees with unmerged commits");
    }

    // Send push notification for mobile alerts
    state.send_push_notification(
        "Orphaned worktrees need attention",
        &nudge_text,
        "orphan_warning",
    );

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

    // Case 1: Pending tasks with owners assigned but coworker not running
    let pending_with_owners = &snap.pending_tasks_with_owners;
    for (task_id, task_subject, owner) in pending_with_owners.iter() {
        // Check nudge cooldown for this task
        let task_key = format!("pending-{}", task_id);
        let on_nudge_cooldown = {
            let cooldowns = state.cooldowns.lock().unwrap();
            !cooldowns.check("task_nudge", &task_key, Duration::from_secs(300))
        };

        // Check if the owner is an active reviewer (reviewers should not be nudged
        // about main task list updates — they have their own review assignments)
        let is_owner_reviewer = snap.active_reviewers.contains(&owner.to_lowercase());

        // Decide action using pure decision function
        let action = crate::rules::decide_pending_task_action(
            task_id,
            task_subject,
            owner,
            &snap.active_names,
            snap.is_at_dev_limit,
            on_nudge_cooldown,
            is_owner_reviewer,
        );

        match action {
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                let nudge_msg = format_task_prompt(
                    tid,
                    &format!("You have pending task #{}: {}. Get started!", tid, subj),
                );
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
                    "Pending task #{} is assigned to {} but coworker not running - spawning",
                    tid, o
                );
                let prompt = format_task_prompt(
                    tid,
                    &format!("You've been assigned task #{}: {}. Get started!", tid, subj),
                );
                let config = crate::tmux::ClaudeLaunchConfig::coworker(
                    o.clone(),
                    state.repo_name.clone(),
                    crate::tmux::SessionMode::Resume,
                    Some(prompt),
                );
                effects.push(Effect::SpawnCoworkerWithCallbacks {
                    config,
                    on_success: vec![
                        Effect::BroadcastCoworkerUpdate {
                            name: o.clone(),
                            status: "running".to_string(),
                            current_task: None,
                        },
                        Effect::PostToChannel {
                            sender: "midtown".to_string(),
                            message: daemon_messages::called_in_pending_task(
                                o,
                                &tid.to_string(),
                                config::get_personality(),
                            ),
                        },
                    ],
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

    // Prioritize PR reviews over new task pickup: if there are PRs waiting for review
    // that don't already have an assigned reviewer, and we have capacity to spawn
    // reviewers, defer new task assignment. This ensures PRs don't wait while coworkers
    // pick up new work — but doesn't block task dispatch when all PRs are already covered.
    let active_review_count = snap.active_reviewers.len();
    let prs_with_reviewers = snap
        .reviewer_pr_assignments
        .values()
        .collect::<HashSet<_>>()
        .len();
    let unserved_prs = snap.prs_needing_review.saturating_sub(prs_with_reviewers);
    if unserved_prs > 0
        && active_review_count < MAX_CONCURRENT_REVIEWS
        && !snap.is_at_coworker_limit
    {
        debug!(
            "Deferring unowned task pickup: {} unserved PR(s) need review ({} total, {} already have reviewers), {}/{} active reviewers",
            unserved_prs,
            snap.prs_needing_review,
            prs_with_reviewers,
            active_review_count,
            MAX_CONCURRENT_REVIEWS
        );
        return effects;
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
                "Dev coworkers limit reached, deferring unowned task #{}",
                task.id
            );
            break;
        }

        // Skip tasks that already have an in-flight AssignAndSpawn effect.
        // This prevents the race condition where a new tick sees a task as pending
        // before the previous tick's AssignAndSpawn effect has completed its disk write.
        if state.is_task_spawn_in_flight(&task.id) {
            debug!(
                "Task #{} already has in-flight spawn, skipping duplicate",
                task.id
            );
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
                        "Task #{} references PR #{} - assigning to in-memory owner {}",
                        task.id, pr_num, name
                    );
                    break 'resolve Some(name.clone());
                }
                // Check disk for previously assigned PR tasks
                if let Some(existing_owner) =
                    crate::tasks::find_pr_owner_in_tasks(&pr_num, all_tasks)
                {
                    info!(
                        "Task #{} references PR #{} - assigning to existing owner {}",
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
                        "Task #{} blocked by #{} - assigning to same owner {}",
                        task.id, blocked_by_id, name
                    );
                    break 'resolve Some(name.clone());
                }
            }
            // Check disk for blockedBy owners
            if let Some(owner) = crate::tasks::find_owner_via_blocked_by(task, all_tasks) {
                info!(
                    "Task #{} blocked by owned task - assigning to {}",
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
                debug!("No available coworker slots for unowned task #{}", task.id);
                break;
            };
            debug!("Task #{}: allocated fresh coworker name {}", task.id, name,);
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
        // With isolated task lists, the daemon tracks assignments internally.
        // Also check within-tick assignments to prevent duplicate fresh-spawns.
        let is_busy = snap.busy_coworkers.contains(&coworker_name.to_lowercase())
            || names_assigned_this_tick.contains(&coworker_name.to_lowercase());

        // Skip running coworkers that are busy or reviewing.
        // Grouped tasks (same PR, blockedBy) are allowed to go to busy coworkers
        // since they represent intentionally related work.
        if already_running && (is_coworker_reviewer || (is_busy && !was_grouped)) {
            debug!(
                "Task #{}: skipping coworker {} (busy={}, reviewer={}, grouped={})",
                task.id, coworker_name, is_busy, is_coworker_reviewer, was_grouped
            );
            continue;
        }

        // For fresh-spawn names (not grouped), prevent assigning multiple tasks
        // to the same not-yet-spawned coworker within the same tick.
        if !already_running && is_busy && !was_grouped {
            debug!(
                "Task #{}: skipping {} (already assigned this tick)",
                task.id, coworker_name
            );
            continue;
        }

        info!(
            "Proposing task #{} for {} (already_running={})",
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

        // Build the prompt message
        let prompt = format_task_prompt(
            &task.id,
            &format!(
                "You've been assigned task #{}: {}. Get started!",
                task.id, task.subject
            ),
        );

        if already_running {
            // Step 2a: Coworker is already running (grouped task) — assign ownership, then nudge
            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
                config::get_personality(),
            );
            effects.push(Effect::AssignTaskOwner {
                task_id: task.id.clone(),
                owner: coworker_name.clone(),
            });
            effects.push(Effect::NudgeCoworkerWithCallbacks {
                name: coworker_name.clone(),
                message: prompt,
                on_success: vec![Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: channel_msg,
                }],
            });
        } else {
            // Step 2b: Spawn a new coworker — assign ownership atomically with spawn
            let config = crate::tmux::ClaudeLaunchConfig::coworker(
                coworker_name.clone(),
                state.repo_name.clone(),
                crate::tmux::SessionMode::Fresh,
                Some(prompt.clone()),
            );
            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
                config::get_personality(),
            );
            effects.push(Effect::AssignAndSpawn {
                task_id: task.id.clone(),
                owner: coworker_name.clone(),
                repo_name: snap.repo_name.clone(),
                config,
                on_success: vec![
                    Effect::BroadcastCoworkerUpdate {
                        name: coworker_name.clone(),
                        status: "running".to_string(),
                        current_task: None,
                    },
                    Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: channel_msg,
                    },
                ],
                on_failure: vec![],
            });
        }
    }

    effects
}

// ============================================================================
// Task completion for PR opened
// ============================================================================

/// Build effects to auto-complete a task when its PR is opened.
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
                "✅ Auto-completed task #{} (PR #{} opened)",
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
}
