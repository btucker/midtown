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

/// Check if a specific PR is merged by querying GitHub directly.
///
/// This bypasses the cached merged PR list to avoid race conditions where:
/// 1. A PR merges
/// 2. Auto-completion fails (or hasn't run yet)
/// 3. Coworker shuts down
/// 4. Orphan recovery runs before the next merged PR cache refresh (5 min interval)
/// 5. Coworker gets recovered and creates duplicate PR
///
/// Returns `true` if the PR is merged, `false` if open/closed, `None` if the check fails.
/// Determine whether an orphaned task should be recovered.
///
/// Decision function: returns `true` if the task should be recovered,
/// `false` if it should be skipped. A task should NOT be recovered if:
/// - It is already completed (race condition: RPC marked it done after snapshot)
/// - It has a canonical PR (with [Midtown !XX] in PR title) that is merged
///
/// The function does NOT skip recovery for contextual PR mentions in task text
/// (e.g., "PR #940 fix insufficient"). Only the canonical task-to-PR link via
/// pr_task_associations matters.
fn should_recover_task(
    task: &crate::tasks::Task,
    merged_pr_numbers: &HashSet<u64>,
    tasks_with_open_prs: &HashSet<String>,
    pr_task_associations: &HashMap<u64, String>,
    _repo_path: &std::path::Path,
) -> bool {
    // Check if task is already completed
    // Race condition: coworker reports completion via RPC, task is marked completed,
    // but snapshot was collected before in_progress_tasks refreshed.
    if task.status == crate::tasks::TaskStatus::Completed {
        debug!(
            "Skipping orphan recovery for task !{}: already completed",
            task.id
        );
        return false;
    }

    // Check if this task has an associated MERGED PR (canonical link via [Midtown !XX] in PR title).
    // Do NOT skip recovery for contextual PR mentions in task text (e.g., "PR #940 fix insufficient").
    //
    // Bug fix: The old logic extracted ALL PR numbers from task text and skipped recovery if those
    // PRs were merged. This incorrectly treated contextual mentions as task completion.
    //
    // New logic: Only skip if a PR with [Midtown !{task_id}] in its title is merged.
    // This is the canonical task-to-PR link used by auto-completion.

    // Check pr_task_associations: merged PRs that have [Midtown !XX] in their title
    for (pr_number, task_id) in pr_task_associations {
        if task_id == &task.id && merged_pr_numbers.contains(pr_number) {
            debug!(
                "Skipping orphan recovery for task !{}: associated PR #{} is merged",
                task.id, pr_number
            );
            return false;
        }
    }

    // If the task has an OPEN PR (in tasks_with_open_prs), allow recovery.
    // The coworker may have crashed mid-work, so orphan recovery should respawn.
    if tasks_with_open_prs.contains(&task.id) {
        debug!(
            "Allowing orphan recovery for task !{}: has open PR but coworker is down",
            task.id
        );
        return true;
    }

    // No associated PR found — this is a non-PR task (investigation, review, etc.)
    // or a task that hasn't opened a PR yet. Allow recovery.
    true
}

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

    // Get primary repo path for GitHub API calls
    let repo_path = state
        .all_repo_paths
        .first()
        .expect("daemon state must have at least one repo path");

    // Filter out in_progress tasks whose PRs have already been merged or that
    // are already completed. These tasks are stale and will be auto-completed
    // by the PR merge cleanup path. Attempting orphan recovery on them creates
    // a loop: spawn → coworker sees task done → goes idle → grace period
    // expires → spawn again.

    // Convert tasks_with_open_prs (HashMap<String, u64>) to a HashSet of task IDs for efficient lookup
    let tasks_with_open_prs_set: HashSet<String> =
        snap.tasks_with_open_prs.keys().cloned().collect();

    let in_progress_tasks_active: Vec<(String, String, String)> = snap
        .in_progress_tasks
        .iter()
        .filter(|(task_id, _task_subject, _owner)| {
            // Read full task from disk to check both subject and description for PR number
            let task = match crate::tasks::read_task(task_id) {
                Some(t) => t,
                None => return true, // Task doesn't exist on disk? Keep it for recovery attempt
            };

            should_recover_task(
                &task,
                &snap.merged_pr_numbers,
                &tasks_with_open_prs_set,
                &snap.pr_task_associations,
                repo_path,
            )
        })
        .cloned()
        .collect();

    if in_progress_tasks_active.is_empty() {
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
        &in_progress_tasks_active,
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

    // Set channel from task if available
    let channel = snap
        .all_tasks
        .iter()
        .find(|t| t.id == recovery.task_id)
        .and_then(|t| t.channel.clone());
    config.channel = channel.clone();

    // Set model from task_model mapping if available.
    // task_model_map stores "provider/model" (e.g., "claude/opus") but
    // LaunchConfig.model expects just the model alias (e.g., "opus").
    if let Some(full_model) = snap.task_model_map.get(&recovery.task_id)
        && let Some(model_alias) = full_model.split('/').nth(1)
    {
        config.model = model_alias.to_string();
    }

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

    // Pre-spawn effects: create worktree and register assignment BEFORE spawning.
    // prepare_spawn() validates working_dir exists, so the worktree must exist first.
    let mut pre_spawn = vec![Effect::EnsureWorktree {
        worktree_id: worktree_id.clone(),
        path: wt_path.clone(),
    }];

    if needs_registration {
        pre_spawn.push(Effect::RegisterWorktreeAssignment {
            assignment: crate::worktree_registry::WorktreeAssignment {
                worktree_id: worktree_id.clone(),
                branch_name: worktree_id.clone(),
                task_id: Some(recovery.task_id.clone()),
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            },
        });
    }

    // Post-spawn success effects
    let on_success = vec![
        Effect::BindCoworkerToWorktree {
            worktree_id: worktree_id.clone(),
            coworker: recovery.owner.clone(),
        },
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
                "♻️ Recovered coworker {} for orphaned task !{}",
                recovery.owner, recovery.task_id
            ),
            channel: channel.clone(),
        },
    ];

    // EnsureWorktree + RegisterWorktreeAssignment run first, then spawn
    pre_spawn.push(Effect::SpawnCoworkerWithCallbacks {
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
                channel,
            },
        ],
    });
    pre_spawn
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

    // Get in_progress tasks with owners and channels
    let in_progress = crate::tasks::read_tasks()
        .into_iter()
        .filter(|t| t.status == crate::tasks::TaskStatus::InProgress)
        .collect::<Vec<_>>();

    // Build a map of owner -> (task_id, task_subject, channel)
    let mut owner_tasks: HashMap<String, (String, String, Option<String>)> = HashMap::new();
    for task in &in_progress {
        if let Some(ref owner) = task.owner {
            let owner_lower = owner.trim().trim_matches('"').to_lowercase();
            if !owner_lower.is_empty() {
                owner_tasks.insert(
                    owner_lower,
                    (task.id.clone(), task.subject.clone(), task.channel.clone()),
                );
            }
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
    owner_tasks: &HashMap<String, (String, String, Option<String>)>,
    reviewer_prs: &HashMap<String, u64>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for name in discovered {
        let name_lower = name.to_lowercase();

        // Check for an in_progress task owned by this coworker
        if let Some((task_id, task_subject, channel)) = owner_tasks.get(&name_lower) {
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
                session_id: None,
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "♻️ Nudged discovered coworker {} to resume task !{}",
                    name, task_id
                ),
                channel: channel.clone(),
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
                session_id: None,
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "♻️ Nudged discovered reviewer {} to resume PR #{} review",
                    name, pr_number
                ),
                channel: None,
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
                session_id: None,
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "🔪 Killed duplicate worker {} on task !{} ({}) - {} started earlier",
                    duplicate, task_id, task_subject, keeper
                ),
                channel: None,
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
///
/// `in_progress_task_owners`: Names of coworkers with assigned in_progress tasks.
/// Used to suppress warnings for worktrees with no corresponding active work.
pub(super) async fn gather_orphan_cleanup_data(
    state: &DaemonState,
    in_progress_task_owners: &[String],
) -> Option<OrphanCleanupData> {
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
                // Suppress warnings for worktrees with no corresponding in_progress task.
                // When a coworker is idle with no assigned work, their orphaned worktree
                // represents abandoned/completed work, not an interrupted task needing recovery.
                if !in_progress_task_owners.contains(name) {
                    debug!(
                        "Suppressing orphan warning for {} (no in_progress task)",
                        name
                    );
                    return false;
                }
                // Only track worktrees that pass the filter (have an in_progress task).
                // This ensures first_detected is set when the task is actually assigned,
                // preserving the 60s grace period.
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
                let should_cleanup = coworkers.is_branch_pr_merged(&name)
                    || coworkers.is_worktree_head_on_main(&name);

                if should_cleanup {
                    let reason = if coworkers.is_branch_pr_merged(&name) {
                        "PR merged"
                    } else {
                        "HEAD on main"
                    };
                    match coworkers.force_cleanup_worktree(&name) {
                        Ok(()) => {
                            info!("Auto-cleaned orphaned worktree for {} ({})", name, reason);
                            cleaned.push(name);
                        }
                        Err(e) => {
                            warn!(
                                "Failed to cleanup worktree for {} ({}): {}",
                                name, reason, e
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
        // orphans not in the `remaining` subset (same rationale as line 802).
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
            channel: None,
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

    // Track coworkers spawned/assigned across both Case 1 (owned tasks) and
    // Case 2 (unowned tasks) to prevent the same coworker from being targeted
    // by both cases in a single tick. Case 1 inserts on spawn, Case 2 checks
    // this set in addition to its own names_assigned_this_tick.
    let mut coworkers_dispatched_this_tick: HashSet<String> = HashSet::new();

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

        // Skip tasks that already have an in-flight spawn from a previous tick.
        // This prevents cross-tick duplicate spawns when the spawn takes longer
        // than one tick interval to complete (same mechanism as Case 2).
        if state.is_task_spawn_in_flight(task_id) {
            debug!(
                "Task !{} already has in-flight spawn, skipping duplicate",
                task_id
            );
            continue;
        }

        // Skip if this owner is already assigned to THIS SPECIFIC TASK.
        // Prevents nudge loops where the same pending-with-owner task gets
        // re-nudged every time the 300s cooldown expires. Once a task is assigned,
        // it stays assigned until the coworker completes it or shuts down.
        if snap
            .coworker_task_assignments
            .get(&owner.to_lowercase())
            .is_some_and(|assigned_task_id| assigned_task_id == task_id)
        {
            debug!(
                "Task !{}: skipping {} (already assigned to this task)",
                task_id, owner
            );
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
                    session_id: None,
                    on_success: vec![
                        Effect::RecordCooldown {
                            category: "task_nudge".to_string(),
                            key: task_key.clone(),
                        },
                        Effect::RecordTaskAssignment {
                            coworker: o.clone(),
                            task_id: tid.clone(),
                        },
                    ],
                });
            }
            crate::rules::PendingTaskAction::SpawnOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                // Skip if we already spawned this coworker in this tick.
                // Prevents duplicate spawns when multiple pending tasks have the same owner.
                if coworkers_dispatched_this_tick.contains(&o.to_lowercase()) {
                    debug!(
                        "Already spawned {} this tick — skipping duplicate spawn for task !{}",
                        o, tid
                    );
                    continue;
                }

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

                // Set model from task_model mapping if available.
                // Extract just the model alias from "provider/model" format.
                if let Some(full_model) = snap.task_model_map.get(tid)
                    && let Some(model_alias) = full_model.split('/').nth(1)
                {
                    config.model = model_alias.to_string();
                }

                // Pre-spawn: create worktree and register assignment BEFORE spawning.
                // prepare_spawn() validates working_dir exists, so the worktree must exist first.
                effects.push(Effect::EnsureWorktree {
                    worktree_id: worktree_id.clone(),
                    path: wt_path.clone(),
                });

                if needs_registration {
                    effects.push(Effect::RegisterWorktreeAssignment {
                        assignment: crate::worktree_registry::WorktreeAssignment {
                            worktree_id: worktree_id.clone(),
                            branch_name: worktree_id.clone(),
                            task_id: Some(tid.clone()),
                            current_coworker: None,
                            pr_number: None,
                            created_at: chrono::Utc::now(),
                            completed_at: None,
                        },
                    });
                }

                // Post-spawn success effects
                // Include RecordTaskAssignment so mark_in_flight_spawns_from_effects()
                // can track this spawn across ticks and prevent duplicate spawns if
                // the spawn takes longer than one tick interval to complete.
                let on_success = vec![
                    Effect::RecordTaskAssignment {
                        coworker: o.clone(),
                        task_id: tid.clone(),
                    },
                    Effect::BindCoworkerToWorktree {
                        worktree_id: worktree_id.clone(),
                        coworker: o.clone(),
                    },
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
                        channel: None,
                    },
                ];

                effects.push(Effect::SpawnCoworkerWithCallbacks {
                    config,
                    on_success,
                    on_failure: vec![],
                });

                // Mark this coworker as spawned to prevent duplicate spawns in this tick
                coworkers_dispatched_this_tick.insert(o.to_lowercase());
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
            "PR review state: {} unserved PR(s) need review ({} total, {} already have reviewers), {} active reviewers — task dispatch proceeds independently",
            unserved_prs, snap.prs_needing_review, prs_with_reviewers, active_review_count
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
        // Split into three sources: persistent busyness (from snapshot),
        // same-tick assignments (from this Case 2 loop), and cross-case
        // dispatches (from Case 1's pending-with-owners spawns).
        let is_busy_from_snapshot = snap.busy_coworkers.contains(&coworker_name.to_lowercase());
        let assigned_this_tick_case2 =
            names_assigned_this_tick.contains(&coworker_name.to_lowercase());
        let dispatched_by_case1 =
            coworkers_dispatched_this_tick.contains(&coworker_name.to_lowercase());

        // Always skip if Case 1 already dispatched this coworker — it will pick up
        // grouped tasks after spawning. This check applies regardless of grouping.
        if dispatched_by_case1 {
            debug!(
                "Task !{}: skipping {} (already dispatched by Case 1 pending-with-owners)",
                task.id, coworker_name
            );
            continue;
        }

        // Skip if this coworker is already assigned to THIS SPECIFIC TASK.
        // Prevents nudge/spawn loops where grouped tasks get re-assigned every tick
        // because the busy check is bypassed for grouped tasks. The coworker may be
        // busy with this exact task from a previous tick's assignment.
        if snap
            .coworker_task_assignments
            .get(&coworker_name.to_lowercase())
            .is_some_and(|assigned_task_id| assigned_task_id == &task.id)
        {
            debug!(
                "Task !{}: skipping {} (already assigned to this task)",
                task.id, coworker_name
            );
            continue;
        }

        // Skip running coworkers that are busy or reviewing.
        // Grouped tasks (same PR, blockedBy) are allowed to go to coworkers
        // that are busy from *previous ticks* (cross-tick grouping).
        // However, always skip if already assigned *this tick* — one nudge
        // per coworker per tick is sufficient, even for grouped tasks.
        if already_running
            && (is_coworker_reviewer
                || assigned_this_tick_case2
                || (is_busy_from_snapshot && !was_grouped))
        {
            debug!(
                "Task !{}: skipping coworker {} (busy_snapshot={}, assigned_tick={}, reviewer={}, grouped={})",
                task.id,
                coworker_name,
                is_busy_from_snapshot,
                assigned_this_tick_case2,
                is_coworker_reviewer,
                was_grouped
            );
            continue;
        }

        // For fresh-spawn names (not grouped), prevent assigning multiple tasks
        // to the same not-yet-spawned coworker within the same tick.
        if !already_running && (assigned_this_tick_case2 || is_busy_from_snapshot) && !was_grouped {
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
        coworkers_dispatched_this_tick.insert(coworker_name.to_lowercase());

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
                session_id: None,
                on_success: vec![
                    Effect::RecordTaskAssignment {
                        coworker: coworker_name.clone(),
                        task_id: task.id.clone(),
                    },
                    Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: channel_msg,
                        channel: task.channel.clone(),
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
            config.channel = task.channel.clone();

            // Set model from task_model mapping if available.
            // Extract just the model alias from "provider/model" format.
            if let Some(full_model) = snap.task_model_map.get(&task.id)
                && let Some(model_alias) = full_model.split('/').nth(1)
            {
                config.model = model_alias.to_string();
            }

            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
                config::get_personality(),
            );

            // Pre-spawn: create worktree and register assignment BEFORE spawning.
            // prepare_spawn() validates working_dir exists, so the worktree must exist first.
            effects.push(Effect::EnsureWorktree {
                worktree_id: worktree_id.clone(),
                path: wt_path.clone(),
            });

            if needs_registration {
                effects.push(Effect::RegisterWorktreeAssignment {
                    assignment: crate::worktree_registry::WorktreeAssignment {
                        worktree_id: worktree_id.clone(),
                        branch_name: worktree_id.clone(),
                        task_id: Some(task.id.clone()),
                        current_coworker: None,
                        pr_number: None,
                        created_at: chrono::Utc::now(),
                        completed_at: None,
                    },
                });
            }

            // Post-spawn success effects
            let on_success = vec![
                Effect::BindCoworkerToWorktree {
                    worktree_id: worktree_id.clone(),
                    coworker: coworker_name.clone(),
                },
                Effect::BroadcastCoworkerUpdate {
                    name: coworker_name.clone(),
                    status: "running".to_string(),
                    current_task: None,
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: channel_msg,
                    channel: task.channel.clone(),
                },
            ];

            effects.push(Effect::AssignAndSpawn {
                task_id: task.id.clone(),
                owner: coworker_name.clone(),
                repo_name: snap.repo_name.clone(),
                config,
                on_success,
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
            channel: None,
        },
    ]
}

/// Build effects to auto-complete tasks when all PRs referenced in their description are merged.
///
/// This handles cases where the task is NOT linked to a PR via `[Midtown #XX]` in the PR title:
/// - Meta-tasks: "Merge reviewed PRs: #901-#910"
/// - Sub-tasks: "Address PR #904 review feedback"
/// - Fix-PR tasks: "Fix PR #908"
///
/// Tasks linked via `[Midtown #XX]` are handled by `build_task_completion_effects` (webhook path).
/// This function skips those tasks to avoid double-completion.
///
/// Returns effects to complete tasks whose description references only merged PRs.
pub(super) fn build_description_based_completion_effects(
    snap: &snapshot::WorldSnapshot,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for task in &snap.all_tasks {
        // Only consider in_progress tasks (completed tasks are already filtered out by this check)
        if task.status != crate::tasks::TaskStatus::InProgress {
            continue;
        }

        // Extract PR numbers from both subject and description (matching orphan recovery logic)
        let mut all_text = task.subject.clone();
        if let Some(desc) = &task.description {
            all_text.push('\n');
            all_text.push_str(desc);
        }

        let pr_numbers = crate::tasks::extract_pr_numbers_from_text(&all_text);

        // Skip if no PR references found
        if pr_numbers.is_empty() {
            continue;
        }

        // Check if ALL referenced PRs are merged
        let all_merged = pr_numbers
            .iter()
            .all(|pr_num| snap.merged_pr_numbers.contains(pr_num));

        if all_merged {
            let task_id_str = task.id.clone();
            effects.push(Effect::CompleteTask {
                task_id: task_id_str.clone(),
                repo_name: snap.repo_name.clone(),
            });
            effects.push(Effect::ClearBlockedBy {
                completed_task_id: task_id_str.clone(),
                repo_name: snap.repo_name.clone(),
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "✅ Auto-completed task !{} (all referenced PRs merged: {})",
                    task.id,
                    pr_numbers
                        .iter()
                        .map(|n| format!("#{}", n))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                channel: None,
            });
        }
    }

    effects
}

// ============================================================================
// Task unassignment for PRs in review
// ============================================================================

/// Find tasks that should be unassigned because their PR is in review.
///
/// A task is "in review" when:
/// 1. It's in_progress with an owner
/// 2. Its task_id appears in `tasks_with_open_prs` (has a PrAuthorSession)
/// 3. The owner is NOT active (not in active_names)
///
/// Returns `UnassignTask` effects for each such task. This is a pure decision
/// function — reads snapshot data and returns effects without performing I/O.
///
/// Runs every tick to handle timing races between PR detection and idle shutdown:
/// - PR detected before idle → unassigned on next tick after shutdown
/// - PR detected after idle → unassigned when PrAuthorSession is stored
pub(super) fn reconcile_tasks_in_review(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let mut effects = vec![];

    for (task_id, _subject, owner) in &snap.in_progress_tasks {
        let owner_clean = owner.trim().trim_matches('"').to_lowercase();
        if owner_clean.is_empty() {
            continue;
        }

        // Only consider tasks with an associated open PR
        if !snap.tasks_with_open_prs.contains_key(task_id) {
            continue;
        }

        // Only unassign if the owner is NOT active (already shut down / on break)
        if snap.active_names.contains(&owner_clean) {
            continue;
        }

        debug!(
            "Task !{} has open PR and owner {} is inactive — unassigning",
            task_id, owner_clean
        );

        effects.push(Effect::UnassignTask {
            task_id: task_id.clone(),
            repo_name: snap.repo_name.clone(),
        });
    }

    effects
}

// Test helper function exposed for integration tests
#[doc(hidden)]
pub fn should_recover_task_test_helper(
    task: &crate::tasks::Task,
    merged_pr_numbers: &HashSet<u64>,
    tasks_with_open_prs: &HashSet<String>,
    pr_task_associations: &HashMap<u64, String>,
    repo_path: &std::path::Path,
) -> bool {
    should_recover_task(
        task,
        merged_pr_numbers,
        tasks_with_open_prs,
        pr_task_associations,
        repo_path,
    )
}

// ============================================================================
// Task reset for orphaned tasks (owner on break, no PR)
// ============================================================================

/// Reset tasks that are orphaned because their owner went on break.
///
/// A task is orphaned when:
/// 1. It's in_progress with an owner
/// 2. It does NOT have an open PR (no entry in `tasks_with_open_prs`)
/// 3. The owner is NOT active (not in active_names)
/// 4. Grace period has expired since owner stopped (respects `coworker_stop_times`)
///
/// This handles the case where a coworker goes on break before opening a PR.
/// Tasks with open PRs are handled by `reconcile_tasks_in_review`.
///
/// Grace period check prevents conflict with `check_and_recover_orphans()`:
/// - Within grace period → orphan recovery can attempt respawn (e.g., with existing worktree)
/// - After grace period → reset to pending (orphan recovery already had a chance)
///
/// Returns `ResetTaskToPending` effects for each orphaned task. This is a pure
/// decision function — reads snapshot data and returns effects without performing I/O.
pub(super) fn reset_orphaned_tasks(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let mut effects = vec![];

    // Compute recently-stopped coworkers (within grace period).
    // This matches the logic in check_and_recover_orphans() to prevent
    // conflicting effects for the same task in the same tick.
    let grace_period = chrono::Duration::seconds(ORPHAN_RECOVERY_GRACE_PERIOD.as_secs() as i64);
    let recently_stopped: HashSet<String> = snap
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect();

    for (task_id, _subject, owner) in &snap.in_progress_tasks {
        let owner_clean = owner.trim().trim_matches('"').to_lowercase();
        if owner_clean.is_empty() {
            continue;
        }

        // Only consider tasks WITHOUT an associated open PR
        // (tasks with PRs are handled by reconcile_tasks_in_review)
        if snap.tasks_with_open_prs.contains_key(task_id) {
            continue;
        }

        // Only reset if the owner is NOT active (already shut down / on break)
        if snap.active_names.contains(&owner_clean) {
            continue;
        }

        // Skip if owner stopped recently (within grace period).
        // Orphan recovery should have priority during grace period.
        if recently_stopped.contains(&owner_clean) {
            debug!(
                "Task !{} owner {} stopped recently (grace period) — deferring to orphan recovery",
                task_id, owner_clean
            );
            continue;
        }

        debug!(
            "Task !{} has no PR and owner {} is inactive (past grace period) — resetting to pending",
            task_id, owner_clean
        );

        effects.push(Effect::ResetTaskToPending {
            task_id: task_id.clone(),
            repo_name: snap.repo_name.clone(),
        });
    }

    effects
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
            Effect::PostToChannel {
                sender, message, ..
            } => {
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
            Effect::PostToChannel {
                sender, message, ..
            } => {
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

    #[test]
    fn test_description_based_completion_all_prs_merged() {
        use crate::tasks::{Task, TaskStatus};
        use std::collections::HashSet;

        // Task with description referencing multiple PRs
        let task = Task {
            id: "1100".to_string(),
            subject: "Meta task".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: Some("Merge reviewed PRs: #901, #902, #903".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // All referenced PRs are merged
        let mut merged_pr_numbers = HashSet::new();
        merged_pr_numbers.insert(901);
        merged_pr_numbers.insert(902);
        merged_pr_numbers.insert(903);

        let snap = snapshot::WorldSnapshot {
            all_tasks: vec![task],
            merged_pr_numbers,
            repo_name: "test-repo".to_string(),
            ..snapshot::minimal_snapshot_for_test()
        };

        let effects = build_description_based_completion_effects(&snap);

        assert_eq!(effects.len(), 3, "Should return 3 effects");

        // Verify CompleteTask effect
        match &effects[0] {
            Effect::CompleteTask { task_id, repo_name } => {
                assert_eq!(task_id, "1100");
                assert_eq!(repo_name, "test-repo");
            }
            _ => panic!("First effect should be CompleteTask"),
        }

        // Verify channel message mentions all PRs
        match &effects[2] {
            Effect::PostToChannel { message, .. } => {
                assert!(message.contains("#901"));
                assert!(message.contains("#902"));
                assert!(message.contains("#903"));
            }
            _ => panic!("Third effect should be PostToChannel"),
        }
    }

    #[test]
    fn test_description_based_completion_some_prs_not_merged() {
        use crate::tasks::{Task, TaskStatus};
        use std::collections::HashSet;

        let task = Task {
            id: "1101".to_string(),
            subject: "Meta task".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: Some("Merge PRs: #901, #902, #903".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // Only some PRs are merged
        let mut merged_pr_numbers = HashSet::new();
        merged_pr_numbers.insert(901);
        merged_pr_numbers.insert(902);
        // PR #903 is NOT merged

        let snap = snapshot::WorldSnapshot {
            all_tasks: vec![task],
            merged_pr_numbers,
            repo_name: "test-repo".to_string(),
            ..snapshot::minimal_snapshot_for_test()
        };

        let effects = build_description_based_completion_effects(&snap);

        assert!(
            effects.is_empty(),
            "Should not complete task when not all PRs are merged"
        );
    }

    #[test]
    fn test_description_based_completion_no_pr_references() {
        use crate::tasks::{Task, TaskStatus};

        let task = Task {
            id: "1102".to_string(),
            subject: "Some task".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: Some("No PR references in this description".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let snap = snapshot::WorldSnapshot {
            all_tasks: vec![task],
            repo_name: "test-repo".to_string(),
            ..snapshot::minimal_snapshot_for_test()
        };

        let effects = build_description_based_completion_effects(&snap);

        assert!(
            effects.is_empty(),
            "Should not complete task with no PR references"
        );
    }

    #[test]
    fn test_description_based_completion_skips_pending_tasks() {
        use crate::tasks::{Task, TaskStatus};
        use std::collections::HashSet;

        let task = Task {
            id: "1103".to_string(),
            subject: "Pending task".to_string(),
            status: TaskStatus::Pending, // Not InProgress
            owner: None,
            description: Some("Fix PR #904".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let mut merged_pr_numbers = HashSet::new();
        merged_pr_numbers.insert(904);

        let snap = snapshot::WorldSnapshot {
            all_tasks: vec![task],
            merged_pr_numbers,
            repo_name: "test-repo".to_string(),
            ..snapshot::minimal_snapshot_for_test()
        };

        let effects = build_description_based_completion_effects(&snap);

        assert!(
            effects.is_empty(),
            "Should not complete non-InProgress tasks"
        );
    }

    #[test]
    fn test_description_based_completion_no_description() {
        use crate::tasks::{Task, TaskStatus};

        let task = Task {
            id: "1104".to_string(),
            subject: "Task without description".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: None, // No description
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let snap = snapshot::WorldSnapshot {
            all_tasks: vec![task],
            repo_name: "test-repo".to_string(),
            ..snapshot::minimal_snapshot_for_test()
        };

        let effects = build_description_based_completion_effects(&snap);

        assert!(
            effects.is_empty(),
            "Should not complete task with no description"
        );
    }

    #[test]
    fn test_description_based_completion_skips_already_completed_tasks() {
        use crate::tasks::{Task, TaskStatus};
        use std::collections::HashSet;

        // Simulate a task that was already completed by the webhook/title-based path.
        // The description-based path should skip it to avoid double-completion.
        let completed_task = Task {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            status: TaskStatus::Completed, // Already completed by title-based path
            owner: Some("york".to_string()),
            description: Some("Fix PR #904 review feedback".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // Also add an in_progress task with PR references
        let in_progress_task = Task {
            id: "43".to_string(),
            subject: "Meta task".to_string(),
            status: TaskStatus::InProgress,
            owner: Some("york".to_string()),
            description: Some("Merge PRs: #904, #905".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let mut merged_pr_numbers = HashSet::new();
        merged_pr_numbers.insert(904);
        merged_pr_numbers.insert(905);

        let snap = snapshot::WorldSnapshot {
            all_tasks: vec![completed_task, in_progress_task],
            merged_pr_numbers,
            repo_name: "test-repo".to_string(),
            ..snapshot::minimal_snapshot_for_test()
        };

        let effects = build_description_based_completion_effects(&snap);

        // Should only produce effects for task 43, not task 42
        let complete_task_ids: Vec<&String> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::CompleteTask { task_id, .. } => Some(task_id),
                _ => None,
            })
            .collect();

        assert_eq!(complete_task_ids, vec!["43"]);
        assert!(
            !complete_task_ids.contains(&&"42".to_string()),
            "Should not double-complete already-completed task 42"
        );
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
            ("42".to_string(), "Fix auth bug".to_string(), None),
        );
        let reviewer_prs = HashMap::new();

        let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
        // NudgeCoworker + PostToChannel
        assert_eq!(effects.len(), 2);
        match &effects[0] {
            Effect::NudgeCoworker { name, message, .. } => {
                assert_eq!(name, "lexington");
                assert!(message.contains("Resume task !42"));
            }
            _ => panic!("Expected NudgeCoworker"),
        }
        match &effects[1] {
            Effect::PostToChannel {
                sender, message, ..
            } => {
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
            ("42".to_string(), "Fix auth bug".to_string(), None),
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
            ("42".to_string(), "Fix auth bug".to_string(), None),
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

    #[test]
    fn test_discovered_nudges_routes_to_task_channel() {
        let discovered = vec!["lexington".to_string()];
        let mut owner_tasks = HashMap::new();
        owner_tasks.insert(
            "lexington".to_string(),
            (
                "42".to_string(),
                "Fix auth bug".to_string(),
                Some("feature-auth".to_string()),
            ),
        );
        let reviewer_prs = HashMap::new();

        let effects = decide_discovered_coworker_nudges(&discovered, &owner_tasks, &reviewer_prs);
        assert_eq!(effects.len(), 2);
        // Check that PostToChannel uses the task's channel
        match &effects[1] {
            Effect::PostToChannel { channel, .. } => {
                assert_eq!(channel, &Some("feature-auth".to_string()));
            }
            _ => panic!("Expected PostToChannel"),
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
                channel: None,
                created_at: Some(SystemTime::now()),
            }],
            tasks_with_worktrees: HashSet::new(), // Task not in registry yet
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            is_at_dev_limit: false,
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
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
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();

        let effects = spawn_for_pending_tasks(&snap, &state);

        // Pre-spawn effects (EnsureWorktree, RegisterWorktreeAssignment) are top-level,
        // followed by AssignAndSpawn with post-spawn effects in on_success.
        assert!(
            effects.len() >= 2,
            "Should have pre-spawn effects + AssignAndSpawn"
        );

        // EnsureWorktree should be a top-level effect (before spawn)
        let ensure_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
            .count();
        assert_eq!(ensure_count, 1, "Should have top-level EnsureWorktree");

        // RegisterWorktreeAssignment should be a top-level effect (before spawn)
        let register_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        assert_eq!(
            register_count, 1,
            "Should have top-level RegisterWorktreeAssignment"
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

        // BindCoworkerToWorktree stays in on_success (runs after spawn)
        let bind_count = assign_and_spawn
            .2
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
            .count();

        assert_eq!(
            bind_count, 1,
            "Should have BindCoworkerToWorktree in on_success"
        );
        assert_eq!(
            bind_count, 1,
            "Should have BindCoworkerToWorktree in on_success"
        );

        // Verify the top-level RegisterWorktreeAssignment has correct fields
        let register_effect = effects
            .iter()
            .find_map(|e| {
                if let Effect::RegisterWorktreeAssignment { assignment } = e {
                    Some(assignment)
                } else {
                    None
                }
            })
            .expect("Should have top-level RegisterWorktreeAssignment");

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
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            is_at_dev_limit: false,
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            active_reviewers: HashSet::new(),
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
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
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
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

    #[test]
    fn test_spawn_for_pending_tasks_skips_when_owner_has_pending_task() {
        // Scenario: Task !1063 is pending with owner=broadway, but broadway ALSO
        // owns task !1062 which is ALSO still pending (not yet in_progress).
        // This happens when:
        // 1. Broadway is spawned for task !1062 (pending → assigned to broadway)
        // 2. Before broadway claims !1062 (sets it to in_progress), task !1063
        //    is assigned to broadway via grouping logic
        // 3. Now both tasks are pending, owner=broadway, but broadway doesn't
        //    exist yet (spawn may have failed or is still starting)
        // 4. The daemon should NOT try to spawn broadway again for !1063
        //
        // This reproduces the bug where the daemon repeatedly tried to spawn
        // broadway for !1063 every 5 seconds.
        let snap = snapshot::WorldSnapshot {
            pending_tasks_with_owners: vec![
                (
                    "1062".to_string(),
                    "Some other task".to_string(),
                    "broadway".to_string(),
                ),
                (
                    "1063".to_string(),
                    "Address review feedback on PR #869 and merge".to_string(),
                    "broadway".to_string(),
                ),
            ],
            // broadway is NOT active (spawn failed or hasn't completed yet)
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            // broadway has NO in_progress tasks (both are still pending)
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            in_progress_tasks: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            merged_pr_numbers: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            attached_coworkers: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = spawn_for_pending_tasks(&snap, &state);

        // Count how many SpawnCoworkerWithCallbacks effects are generated for broadway
        let spawn_count = effects
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::SpawnCoworkerWithCallbacks { config, .. }
                        if config.name.to_lowercase() == "broadway"
                )
            })
            .count();

        // Without the fix, this would generate TWO spawns for broadway (one per pending task).
        // The coworkers_dispatched_this_tick set prevents this by tracking spawned coworkers.
        assert!(
            spawn_count <= 1,
            "Should generate at most ONE spawn for broadway, got {}. Multiple pending tasks \
             with the same owner should not cause duplicate spawns in the same tick.",
            spawn_count
        );
    }

    #[test]
    fn test_spawn_owner_includes_record_task_assignment_for_cross_tick_dedup() {
        // Verify that SpawnCoworkerWithCallbacks from the SpawnOwner branch
        // includes RecordTaskAssignment in on_success, enabling
        // mark_in_flight_spawns_from_effects() to track it across ticks.
        let snap = snapshot::WorldSnapshot {
            pending_tasks_with_owners: vec![(
                "42".to_string(),
                "Add auth endpoint".to_string(),
                "broadway".to_string(),
            )],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            in_progress_tasks: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            merged_pr_numbers: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            attached_coworkers: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = spawn_for_pending_tasks(&snap, &state);

        // Find the SpawnCoworkerWithCallbacks effect
        let spawn_effect = effects
            .iter()
            .find_map(|e| {
                if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                    Some(on_success)
                } else {
                    None
                }
            })
            .expect("Should have SpawnCoworkerWithCallbacks for broadway");

        // Verify RecordTaskAssignment is in on_success
        let has_record = spawn_effect.iter().any(|e| {
            matches!(
                e,
                Effect::RecordTaskAssignment { coworker, task_id }
                    if coworker == "broadway" && task_id == "42"
            )
        });
        assert!(
            has_record,
            "SpawnCoworkerWithCallbacks on_success must include RecordTaskAssignment \
             for cross-tick spawn deduplication"
        );
    }

    #[test]
    fn test_cross_tick_dedup_skips_in_flight_owned_task() {
        // Simulate two consecutive ticks: the first tick spawned broadway for
        // task !42 (marking it in-flight), the second tick should skip it.
        let snap = snapshot::WorldSnapshot {
            pending_tasks_with_owners: vec![(
                "42".to_string(),
                "Add auth endpoint".to_string(),
                "broadway".to_string(),
            )],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            in_progress_tasks: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            merged_pr_numbers: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            attached_coworkers: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();

        // Simulate tick 1: generates spawn effects
        let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
        let spawn_count_tick1 = effects_tick1
            .iter()
            .filter(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }))
            .count();
        assert_eq!(spawn_count_tick1, 1, "Tick 1 should spawn broadway");

        // Mark in-flight (normally done by the daemon between ticks)
        state.mark_in_flight_spawns_from_effects(&effects_tick1);

        // Simulate tick 2: should skip because task !42 is already in-flight
        let effects_tick2 = spawn_for_pending_tasks(&snap, &state);
        let spawn_count_tick2 = effects_tick2
            .iter()
            .filter(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }))
            .count();
        assert_eq!(
            spawn_count_tick2, 0,
            "Tick 2 should NOT re-spawn broadway — task !42 is already in-flight"
        );
    }

    #[test]
    fn test_cross_case_dedup_prevents_same_coworker_from_case1_and_case2() {
        // Scenario: Task !42 is pending with owner=broadway (Case 1),
        // and task !43 is pending WITHOUT owner but references PR #100
        // which broadway is working on (Case 2 would group it to broadway).
        // Case 2 should skip broadway because Case 1 already dispatched it.
        use crate::tasks::Task;

        let snap = snapshot::WorldSnapshot {
            // Case 1: broadway has a pending owned task
            pending_tasks_with_owners: vec![(
                "42".to_string(),
                "Add auth endpoint".to_string(),
                "broadway".to_string(),
            )],
            // Case 2: unowned task referencing PR #100
            pending_tasks_without_owners: vec![Task {
                id: "43".to_string(),
                subject: "Review feedback on PR #100 [Midtown !43]".to_string(),
                status: crate::tasks::TaskStatus::Pending,
                owner: None,
                description: None,
                blocked_by: vec![],
                channel: None,
                created_at: None,
            }],
            // broadway is NOT running (will be spawned by Case 1)
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            in_progress_tasks: vec![
                // Existing in-progress task for broadway on PR #100 (so Case 2 groups to broadway)
                (
                    "40".to_string(),
                    "Implement feature [Midtown !40] PR #100".to_string(),
                    "broadway".to_string(),
                ),
            ],
            all_tasks: vec![Task {
                id: "40".to_string(),
                subject: "Implement feature [Midtown !40] PR #100".to_string(),
                status: crate::tasks::TaskStatus::InProgress,
                owner: Some("broadway".to_string()),
                description: None,
                blocked_by: vec![],
                channel: None,
                created_at: None,
            }],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            merged_pr_numbers: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            attached_coworkers: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = spawn_for_pending_tasks(&snap, &state);

        // Count total effects targeting broadway
        let broadway_spawns = effects
            .iter()
            .filter(|e| match e {
                Effect::SpawnCoworkerWithCallbacks { config, .. } => {
                    config.name.to_lowercase() == "broadway"
                }
                Effect::AssignAndSpawn { owner, .. } => owner.to_lowercase() == "broadway",
                Effect::NudgeCoworkerWithCallbacks { name, .. } => {
                    name.to_lowercase() == "broadway"
                }
                _ => false,
            })
            .count();

        assert!(
            broadway_spawns <= 1,
            "Should generate at most ONE spawn/nudge for broadway across both Case 1 and Case 2, \
             got {}. Cross-case deduplication should prevent Case 2 from targeting a coworker \
             already dispatched by Case 1.",
            broadway_spawns
        );
    }

    #[test]
    fn test_spawn_for_pending_tasks_skips_via_snapshot_assignment_check() {
        // Test the pure decision pattern: verify that spawn_for_pending_tasks
        // correctly skips a task when coworker_task_assignments (in WorldSnapshot)
        // shows the owner is already assigned to that specific task.
        // This test verifies the refactored code uses the snapshot data
        // (pure decision) rather than calling state.is_coworker_assigned_to_task()
        // (impure decision with .lock()).

        let mut assignments = HashMap::new();
        assignments.insert("broadway".to_string(), "42".to_string());

        let snap = snapshot::WorldSnapshot {
            pending_tasks_with_owners: vec![(
                "42".to_string(),
                "Add auth endpoint".to_string(),
                "broadway".to_string(),
            )],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            busy_coworkers: HashSet::new(),
            // KEY: broadway is already assigned to task !42 in the snapshot
            coworker_task_assignments: assignments,
            in_progress_tasks: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            merged_pr_numbers: HashSet::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            attached_coworkers: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = spawn_for_pending_tasks(&snap, &state);

        // Should generate NO effects because broadway is already assigned to task !42
        assert_eq!(
            effects.len(),
            0,
            "Should generate no effects when owner is already assigned to the task \
             (verified via coworker_task_assignments in snapshot)"
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
            active_session_ids: HashSet::new(),
            coworkers_with_open_prs: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashSet::new(),
            tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
            task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
                .into_iter()
                .collect(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = check_and_recover_orphans(&snap, &state);

        // Pre-spawn effects (EnsureWorktree) are top-level, then SpawnCoworkerWithCallbacks
        assert!(
            effects.len() >= 2,
            "Should have pre-spawn EnsureWorktree + SpawnCoworkerWithCallbacks"
        );

        // EnsureWorktree should be a top-level effect (before spawn)
        let ensure_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::EnsureWorktree { worktree_id, .. } if worktree_id == "task-42-add-auth-endpoint"))
            .count();
        assert_eq!(
            ensure_count, 1,
            "Should have top-level EnsureWorktree for existing worktree"
        );

        // Should NOT have RegisterWorktreeAssignment (worktree already registered)
        let register_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        assert_eq!(
            register_count, 0,
            "Should NOT register worktree again (already exists)"
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

        let expected_path =
            crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
        assert_eq!(
            config.working_dir,
            Some(expected_path),
            "Should set working_dir to the existing task worktree"
        );

        // BindCoworkerToWorktree stays in on_success (runs after spawn)
        let bind_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { worktree_id, coworker } if worktree_id == "task-42-add-auth-endpoint" && coworker == "lexington"))
            .count();
        assert_eq!(
            bind_count, 1,
            "Should have BindCoworkerToWorktree to rebind"
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
            active_session_ids: HashSet::new(),
            coworkers_with_open_prs: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            coworker_stop_times: HashMap::new(),
            attached_coworkers: HashSet::new(),
            tasks_with_worktrees: HashSet::new(), // No worktree registered
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            running_coworkers: vec![],
            active_coworkers: vec![],
            coworker_snapshots: vec![],
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = check_and_recover_orphans(&snap, &state);

        // Pre-spawn effects (EnsureWorktree, RegisterWorktreeAssignment) are top-level,
        // followed by SpawnCoworkerWithCallbacks with post-spawn effects in on_success.
        assert!(
            effects.len() >= 3,
            "Should have EnsureWorktree + RegisterWorktreeAssignment + SpawnCoworkerWithCallbacks"
        );

        // EnsureWorktree should be a top-level effect (before spawn)
        let ensure_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
            .count();
        assert_eq!(ensure_count, 1, "Should have top-level EnsureWorktree");

        // RegisterWorktreeAssignment should be a top-level effect (before spawn)
        let register_effect = effects
            .iter()
            .find_map(|e| {
                if let Effect::RegisterWorktreeAssignment { assignment } = e {
                    Some(assignment)
                } else {
                    None
                }
            })
            .expect("Should have top-level RegisterWorktreeAssignment");

        assert_eq!(register_effect.task_id, Some("42".to_string()));
        assert!(
            register_effect
                .worktree_id
                .contains("task-42-add-auth-endpoint")
        );
        assert_eq!(register_effect.current_coworker, None);

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

        // BindCoworkerToWorktree stays in on_success (runs after spawn)
        let bind_count = on_success
            .iter()
            .filter(|e| matches!(e, Effect::BindCoworkerToWorktree { .. }))
            .count();
        assert_eq!(
            bind_count, 1,
            "Should have BindCoworkerToWorktree in on_success"
        );
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
                channel: None,
                created_at: Some(SystemTime::now()),
            }],
            tasks_with_worktrees: ["42".to_string()].into_iter().collect(),
            task_worktree_map: [("42".to_string(), "task-42-add-auth-endpoint".to_string())]
                .into_iter()
                .collect(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            worktree_branch_owners: HashMap::new(),
            merged_pr_branches: HashMap::new(),
            is_at_dev_limit: false,
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
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
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = spawn_for_pending_tasks(&snap, &state);

        // Pre-spawn EnsureWorktree is top-level, then AssignAndSpawn
        assert!(
            effects.len() >= 2,
            "Should have pre-spawn EnsureWorktree + AssignAndSpawn"
        );

        // EnsureWorktree should be a top-level effect (before spawn)
        let ensure_effects: Vec<_> = effects
            .iter()
            .filter(|e| matches!(e, Effect::EnsureWorktree { .. }))
            .collect();
        assert_eq!(
            ensure_effects.len(),
            1,
            "Should have top-level EnsureWorktree"
        );
        if let Effect::EnsureWorktree { worktree_id, .. } = ensure_effects[0] {
            assert_eq!(
                worktree_id, "task-42-add-auth-endpoint",
                "Should ensure the existing worktree, not a new one"
            );
        }

        // Should NOT have RegisterWorktreeAssignment (worktree already exists)
        let register_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::RegisterWorktreeAssignment { .. }))
            .count();
        assert_eq!(
            register_count, 0,
            "Should NOT re-register existing worktree"
        );

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

        // Working dir should point to the EXISTING worktree
        let expected_path =
            crate::paths::worktrees_dir_for_repo("test-repo").join("task-42-add-auth-endpoint");
        assert_eq!(
            config.working_dir,
            Some(expected_path),
            "Should reuse existing worktree path"
        );

        // BindCoworkerToWorktree stays in on_success (runs after spawn)
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
    }

    // ======================================================================
    // reconcile_tasks_in_review tests
    // ======================================================================

    /// Helper to create a minimal WorldSnapshot for reconciliation tests.
    fn make_reconcile_snapshot(
        in_progress_tasks: Vec<(String, String, String)>,
        tasks_with_open_prs: HashMap<String, u64>,
        active_names: HashSet<String>,
    ) -> snapshot::WorldSnapshot {
        snapshot::WorldSnapshot {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names,
            active_session_ids: HashSet::new(),
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            in_progress_tasks,
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs,
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        }
    }

    #[test]
    fn test_reconcile_tasks_in_review_inactive_owner_emits_unassign() {
        // Task !42 is in_progress, owned by york, has open PR, york is NOT active
        let in_progress = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "york".to_string(),
        )];
        let mut tasks_with_open_prs = HashMap::new();
        tasks_with_open_prs.insert("42".to_string(), 100u64);
        let active_names = HashSet::new(); // york is NOT active

        let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
        let effects = reconcile_tasks_in_review(&snap);

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::UnassignTask { task_id, repo_name } => {
                assert_eq!(task_id, "42");
                assert_eq!(repo_name, "test-repo");
            }
            other => panic!("Expected UnassignTask, got {:?}", other),
        }
    }

    #[test]
    fn test_reconcile_tasks_in_review_active_owner_no_effect() {
        // Task !42 is in_progress, owned by york, has open PR, york IS active
        let in_progress = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "york".to_string(),
        )];
        let mut tasks_with_open_prs = HashMap::new();
        tasks_with_open_prs.insert("42".to_string(), 100u64);
        let mut active_names = HashSet::new();
        active_names.insert("york".to_string());

        let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
        let effects = reconcile_tasks_in_review(&snap);

        assert!(effects.is_empty(), "Should not unassign active coworker");
    }

    #[test]
    fn test_reconcile_tasks_in_review_no_pr_no_effect() {
        // Task !42 is in_progress, owned by york, NO open PR, york is NOT active
        let in_progress = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "york".to_string(),
        )];
        let tasks_with_open_prs = HashMap::new(); // No PR
        let active_names = HashSet::new();

        let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
        let effects = reconcile_tasks_in_review(&snap);

        assert!(
            effects.is_empty(),
            "Should not unassign task without open PR"
        );
    }

    #[test]
    fn test_reconcile_tasks_in_review_empty_owner_skipped() {
        // Task !42 is in_progress with empty owner (already unassigned)
        let in_progress = vec![("42".to_string(), "Fix auth bug".to_string(), "".to_string())];
        let mut tasks_with_open_prs = HashMap::new();
        tasks_with_open_prs.insert("42".to_string(), 100u64);
        let active_names = HashSet::new();

        let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
        let effects = reconcile_tasks_in_review(&snap);

        assert!(
            effects.is_empty(),
            "Should not unassign already-unowned task"
        );
    }

    // ======================================================================
    // reset_orphaned_tasks tests
    // ======================================================================

    #[test]
    fn test_reset_orphaned_tasks_inactive_owner_no_pr() {
        // Bug !1157: Task !1146 is in_progress, owned by columbus, NO open PR,
        // columbus is NOT active (went on break) → should reset to pending
        let in_progress = vec![(
            "1146".to_string(),
            "Address review feedback and merge PR #912".to_string(),
            "columbus".to_string(),
        )];
        let tasks_with_open_prs = HashMap::new(); // No PR yet
        let active_names = HashSet::new(); // columbus is NOT active

        let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
        let effects = reset_orphaned_tasks(&snap);

        assert_eq!(effects.len(), 1, "Should reset orphaned task");
        match &effects[0] {
            Effect::ResetTaskToPending { task_id, repo_name } => {
                assert_eq!(task_id, "1146");
                assert_eq!(repo_name, "test-repo");
            }
            other => panic!("Expected ResetTaskToPending, got {:?}", other),
        }
    }

    #[test]
    fn test_reset_orphaned_tasks_active_owner_no_effect() {
        // Task !42 is in_progress, owned by york, NO open PR, york IS active
        // Should NOT reset (coworker is still working on it)
        let in_progress = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "york".to_string(),
        )];
        let tasks_with_open_prs = HashMap::new();
        let mut active_names = HashSet::new();
        active_names.insert("york".to_string());

        let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
        let effects = reset_orphaned_tasks(&snap);

        assert!(effects.is_empty(), "Should not reset task for active owner");
    }

    #[test]
    fn test_reset_orphaned_tasks_with_pr_no_effect() {
        // Task !42 is in_progress, owned by york, HAS open PR, york is NOT active
        // Should NOT reset (reconcile_tasks_in_review handles PR cases)
        let in_progress = vec![(
            "42".to_string(),
            "Fix auth bug".to_string(),
            "york".to_string(),
        )];
        let mut tasks_with_open_prs = HashMap::new();
        tasks_with_open_prs.insert("42".to_string(), 100u64);
        let active_names = HashSet::new();

        let snap = make_reconcile_snapshot(in_progress, tasks_with_open_prs, active_names);
        let effects = reset_orphaned_tasks(&snap);

        assert!(
            effects.is_empty(),
            "Should not reset task with open PR (handled by reconcile_tasks_in_review)"
        );
    }

    #[test]
    fn test_grouped_task_skips_if_already_assigned() {
        // Regression test for nudge/spawn loop bug, using captured production snapshot.
        // Scenario: Task !1107 (pending, no owner) references PR #912 in its subject.
        // Task !1106 (in_progress, owned by york) mentions "PR #912" in its description.
        // The grouping logic finds york as the PR owner → groups !1107 to york.
        // York is already running and busy, but grouped tasks bypass the busy check.
        // Without the is_coworker_assigned_to_task guard, this nudge fires every tick.
        let fixture = include_str!(
            "../../tests/fixtures/snapshot/snapshot-spawn-loop-york-1107-20260210-205810.json"
        );
        let snap: snapshot::WorldSnapshot =
            serde_json::from_str(fixture).expect("deserialize captured snapshot");

        // Verify fixture prerequisites: york is active and busy, task !1107 is pending
        assert!(snap.active_names.contains("york"), "york should be active");
        assert!(snap.busy_coworkers.contains("york"), "york should be busy");
        assert!(
            snap.pending_tasks_without_owners
                .iter()
                .any(|t| t.id == "1107"),
            "task !1107 should be pending without owner"
        );

        let state = make_test_state();

        // Tick 1: Task !1107 groups to york (PR #912), generates nudge
        let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
        let nudge_count_tick1 = effects_tick1
            .iter()
            .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
            .count();
        assert_eq!(
            nudge_count_tick1, 1,
            "Tick 1 should nudge york with task !1107"
        );

        // Simulate the nudge executing and recording the assignment
        state.record_task_assignment("york", "1107");

        // Tick 2: Task !1107 is still pending, york is busy with !1107 now.
        // Create a new snapshot that includes the assignment.
        let snap_tick2 = snapshot::WorldSnapshot {
            coworker_task_assignments: {
                let mut assignments = HashMap::new();
                assignments.insert("york".to_string(), "1107".to_string());
                assignments
            },
            ..snap
        };
        let effects_tick2 = spawn_for_pending_tasks(&snap_tick2, &state);
        let nudge_count_tick2 = effects_tick2
            .iter()
            .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
            .count();
        assert_eq!(
            nudge_count_tick2, 0,
            "Tick 2 should NOT re-nudge york — task !1107 is already assigned to york"
        );
    }

    #[test]
    fn test_spawn_coworker_with_callbacks_records_task_assignment() {
        // Regression test for spawn loop bug (Case 1: pending task with owner).
        // When a coworker isn't running but has a pending task, SpawnCoworkerWithCallbacks
        // must include RecordTaskAssignment in on_success to prevent re-spawning every tick.
        //
        // Note: The captured fixture snapshot-spawn-loop-york-1110 doesn't contain
        // pending-with-owner tasks (tasks were already in_progress when captured), so
        // this test uses a minimal constructed snapshot to isolate Case 1 behavior.
        let fixture = include_str!(
            "../../tests/fixtures/snapshot/snapshot-spawn-loop-york-1107-20260210-205810.json"
        );
        let mut snap: snapshot::WorldSnapshot =
            serde_json::from_str(fixture).expect("deserialize captured snapshot");

        // Override to test Case 1: pending task WITH owner, coworker NOT running.
        // Clear Case 2 tasks and set up a Case 1 scenario.
        snap.pending_tasks_without_owners.clear();
        snap.pending_tasks_with_owners = vec![(
            "1107".to_string(),
            "Investigate PR #912 — no CI checks running".to_string(),
            "york".to_string(),
        )];
        snap.active_names.clear(); // york is NOT running
        snap.busy_coworkers.clear();
        snap.in_progress_tasks.clear();

        let state = make_test_state();

        // Tick 1: generates SpawnCoworkerWithCallbacks with RecordTaskAssignment
        let effects = spawn_for_pending_tasks(&snap, &state);
        let spawn_count = effects
            .iter()
            .filter(|e| matches!(e, Effect::SpawnCoworkerWithCallbacks { .. }))
            .count();
        assert_eq!(spawn_count, 1, "Tick 1 should spawn york");

        // Verify the effect has RecordTaskAssignment in on_success
        let has_record_assignment = effects.iter().any(|e| {
            if let Effect::SpawnCoworkerWithCallbacks { on_success, .. } = e {
                on_success
                    .iter()
                    .any(|e| matches!(e, Effect::RecordTaskAssignment { .. }))
            } else {
                false
            }
        });
        assert!(
            has_record_assignment,
            "SpawnCoworkerWithCallbacks should have RecordTaskAssignment in on_success"
        );

        // Mark in-flight (daemon does this between evaluate_tick and execute_effects)
        state.mark_in_flight_spawns_from_effects(&effects);
        assert!(
            state.is_task_spawn_in_flight("1107"),
            "Task !1107 should be marked in-flight before execution"
        );
    }

    #[test]
    fn test_case1_nudge_records_assignment_and_prevents_loop() {
        // Regression test: Case 1 (pending task with owner) NudgeOwner must include
        // RecordTaskAssignment in on_success, so that after the nudge cooldown
        // expires, the task isn't re-nudged indefinitely.
        let fixture = include_str!(
            "../../tests/fixtures/snapshot/snapshot-spawn-loop-york-1107-20260210-205810.json"
        );
        let mut snap: snapshot::WorldSnapshot =
            serde_json::from_str(fixture).expect("deserialize captured snapshot");

        // Set up Case 1 scenario: task with owner, coworker IS running but NOT busy
        // (triggers NudgeOwner rather than Skip due to has_in_progress_task)
        snap.pending_tasks_without_owners.clear();
        snap.pending_tasks_with_owners = vec![(
            "1107".to_string(),
            "Investigate PR #912 — no CI checks running".to_string(),
            "york".to_string(),
        )];
        // york is active (already in fixture), but clear busy state so NudgeOwner fires
        snap.busy_coworkers.clear();
        snap.in_progress_tasks.clear();

        let state = make_test_state();

        // Tick 1: NudgeOwner fires with RecordTaskAssignment in on_success
        let effects_tick1 = spawn_for_pending_tasks(&snap, &state);
        let nudge_effects: Vec<_> = effects_tick1
            .iter()
            .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
            .collect();
        assert_eq!(nudge_effects.len(), 1, "Tick 1 should nudge york");

        // Verify RecordTaskAssignment is in on_success
        let has_assignment = nudge_effects.iter().any(|e| {
            if let Effect::NudgeCoworkerWithCallbacks { on_success, .. } = e {
                on_success
                    .iter()
                    .any(|e| matches!(e, Effect::RecordTaskAssignment { .. }))
            } else {
                false
            }
        });
        assert!(
            has_assignment,
            "NudgeOwner on_success should include RecordTaskAssignment"
        );

        // Simulate the nudge executing and recording the assignment
        state.record_task_assignment("york", "1107");

        // Tick 2: Create a new snapshot that includes the assignment in coworker_task_assignments.
        // The guard should use snap.coworker_task_assignments to prevent re-nudge (pure decision pattern).
        let snap_tick2 = snapshot::WorldSnapshot {
            coworker_task_assignments: {
                let mut assignments = HashMap::new();
                assignments.insert("york".to_string(), "1107".to_string());
                assignments
            },
            ..snap
        };
        let effects_tick2 = spawn_for_pending_tasks(&snap_tick2, &state);
        let nudge_count_tick2 = effects_tick2
            .iter()
            .filter(|e| matches!(e, Effect::NudgeCoworkerWithCallbacks { .. }))
            .count();
        assert_eq!(
            nudge_count_tick2, 0,
            "Tick 2 should NOT re-nudge york — already assigned to task !1107"
        );
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

        // Leak temp_dir so it survives the test
        let base_dir = temp_dir.path().to_path_buf();
        std::mem::forget(temp_dir);

        let channel_router = crate::ChannelRouter::new(&base_dir, "midtown");

        DaemonState::new(
            "/tmp/test.sock".into(),
            cm,
            "test-repo".to_string(),
            vec![base_dir.clone()],
            channel_router,
            None,
            10,
            None,
            "main".to_string(),
        )
        .expect("daemon state")
    }

    // ======================================================================
    // should_recover_task (pure decision function) tests
    // ======================================================================

    #[test]
    fn test_should_recover_task_skips_completed_tasks() {
        use crate::tasks::{Task, TaskStatus};

        let completed_task = Task {
            id: "1120".to_string(),
            subject: "Fix orphan recovery loop".to_string(),
            description: None,
            status: TaskStatus::Completed,
            owner: Some("vernon".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let merged_prs = HashSet::new();
        assert!(
            !should_recover_task(
                &completed_task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                std::path::Path::new(".")
            ),
            "Should NOT recover a completed task"
        );
    }

    #[test]
    fn test_should_recover_task_with_contextual_pr_mention_in_subject() {
        use crate::tasks::{Task, TaskStatus};

        // Task !1120 mentions PR #923 in subject, but PR #923 is NOT the task's PR.
        // This is a contextual mention (e.g., "Merge PR #923 [Midtown !1120]" means
        // the task is ABOUT merging #923, not that #923 IS the task's PR).
        let task = Task {
            id: "1120".to_string(),
            subject: "Merge PR #923 [Midtown !1120]".to_string(),
            description: None,
            status: TaskStatus::InProgress,
            owner: Some("vernon".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // PR #923 is merged, but it's not associated with task !1120
        let merged_prs: HashSet<u64> = [923].into_iter().collect();

        // New behavior: SHOULD recover because PR #923 doesn't have [Midtown !1120] in its title
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                std::path::Path::new(".")
            ),
            "Should recover task with contextual PR mention (PR is not the task's canonical PR)"
        );
    }

    #[test]
    fn test_should_recover_task_with_contextual_pr_mention_in_description() {
        use crate::tasks::{Task, TaskStatus};

        // Task mentions PR #925 in description as context
        let task = Task {
            id: "1121".to_string(),
            subject: "Address review feedback".to_string(),
            description: Some("Fixes from PR #925 review".to_string()),
            status: TaskStatus::InProgress,
            owner: Some("park".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // PR #925 is merged, but it's not associated with task !1121
        let merged_prs: HashSet<u64> = [925].into_iter().collect();

        // New behavior: SHOULD recover because PR #925 is just contextual mention
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                std::path::Path::new(".")
            ),
            "Should recover task with contextual PR mention in description"
        );
    }

    #[test]
    fn test_should_recover_task_allows_active_in_progress_task() {
        use crate::tasks::{Task, TaskStatus};

        let task = Task {
            id: "42".to_string(),
            subject: "Add auth endpoint".to_string(),
            description: None,
            status: TaskStatus::InProgress,
            owner: Some("lexington".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let merged_prs = HashSet::new();
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                std::path::Path::new(".")
            ),
            "Should recover an active in-progress task with no merged PR"
        );
    }

    #[test]
    fn test_should_recover_task_allows_task_with_unmerged_pr() {
        use crate::tasks::{Task, TaskStatus};

        let task = Task {
            id: "1120".to_string(),
            subject: "Merge PR #999999 [Midtown !1120]".to_string(), // Use non-existent PR number
            description: None,
            status: TaskStatus::InProgress,
            owner: Some("vernon".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // PR #999999 is NOT in the merged set (and doesn't exist in repo)
        // The GitHub API check will fail (PR not found) but the function
        // should be conservative and allow recovery.
        let merged_prs: HashSet<u64> = [900, 910].into_iter().collect();
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                std::path::Path::new(".")
            ),
            "Should recover a task whose PR is NOT yet merged (cache miss, API fails)"
        );
    }

    #[test]
    #[ignore] // Obsolete test - no longer does GitHub API checks for contextual PR mentions
    fn test_should_recover_task_checks_github_when_cache_stale() {
        use crate::tasks::{Task, TaskStatus};

        // This test is obsolete after the fix for issue #1147.
        // The new behavior no longer checks GitHub API for contextual PR mentions.
        // It only skips recovery when pr_task_associations contains the canonical link.
        let task = Task {
            id: "1129".to_string(),
            subject: "Fix task !1129 [Midtown !1129]".to_string(),
            description: Some("PR #935".to_string()),
            status: TaskStatus::InProgress,
            owner: Some("riverside".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // PR #935 is NOT in the cache
        let merged_prs: HashSet<u64> = HashSet::new();

        // New behavior: SHOULD recover because PR #935 is just a contextual mention
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                std::path::Path::new(".")
            ),
            "Should recover task with contextual PR mention (no longer checks GitHub API)"
        );
    }

    #[test]
    fn test_should_recover_task_with_bare_hash_pr_reference() {
        use crate::tasks::{Task, TaskStatus};

        // Task with bare "#904" format - this is a contextual reference, not a canonical link
        let task = Task {
            id: "1122".to_string(),
            subject: "Fix #904 review feedback".to_string(),
            description: None,
            status: TaskStatus::InProgress,
            owner: Some("columbus".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let merged_prs: HashSet<u64> = [904].into_iter().collect();
        let repo_path = std::path::Path::new("/tmp/test-repo");

        // New behavior: SHOULD recover because #904 is contextual mention
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                repo_path
            ),
            "Should recover task with contextual PR reference (even bare # format)"
        );
    }

    #[test]
    fn test_should_recover_task_recovers_multi_pr_with_only_some_merged() {
        use crate::tasks::{Task, TaskStatus};

        // Task referencing PRs #901, #902, #903, but only #901 is merged
        // should_recover_task() should return true (task needs recovery)
        // because auto-completion won't fire until ALL PRs are merged
        let task = Task {
            id: "1123".to_string(),
            subject: "Merge PRs #901, #902, #903".to_string(),
            description: Some("Consolidate multiple related PRs".to_string()),
            status: TaskStatus::InProgress,
            owner: Some("madison".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // Only #901 is merged; #902 and #903 are still open
        let merged_prs: HashSet<u64> = [901].into_iter().collect();
        let repo_path = std::path::Path::new("/tmp/test-repo");
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                repo_path
            ),
            "Should recover task with multi-PR reference where only SOME PRs are merged"
        );
    }

    #[test]
    fn test_should_recover_task_with_multi_pr_when_all_merged() {
        use crate::tasks::{Task, TaskStatus};

        // Task referencing PRs #901, #902, #903 - these are contextual mentions
        let task = Task {
            id: "1124".to_string(),
            subject: "Merge PRs #901, #902, #903".to_string(),
            description: Some("Consolidate multiple related PRs".to_string()),
            status: TaskStatus::InProgress,
            owner: Some("madison".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        // All PRs are merged, but they're not the task's canonical PR
        let merged_prs: HashSet<u64> = [901, 902, 903].into_iter().collect();
        let repo_path = std::path::Path::new("/tmp/test-repo");

        // New behavior: SHOULD recover because these are contextual mentions
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                repo_path
            ),
            "Should recover task with contextual multi-PR references"
        );
    }

    #[test]
    fn test_should_recover_task_with_pr_in_subject_only() {
        use crate::tasks::{Task, TaskStatus};

        // Task with PR reference only in subject - this is a contextual reference
        let task = Task {
            id: "1125".to_string(),
            subject: "Close PR #905".to_string(),
            description: Some("Final cleanup tasks".to_string()),
            status: TaskStatus::InProgress,
            owner: Some("broadway".to_string()),
            blocked_by: vec![],
            channel: None,
            created_at: None,
        };

        let merged_prs: HashSet<u64> = [905].into_iter().collect();
        let repo_path = std::path::Path::new("/tmp/test-repo");

        // New behavior: SHOULD recover because PR #905 is just a contextual mention
        assert!(
            should_recover_task(
                &task,
                &merged_prs,
                &HashSet::new(),
                &HashMap::new(),
                repo_path
            ),
            "Should recover task with contextual PR mention in subject"
        );
    }

    #[test]
    fn test_spawn_extracts_model_alias_from_provider_model_format() {
        use crate::tasks::{Task, TaskStatus};
        use std::time::SystemTime;

        // Setup: task with model "claude/opus" in task_model_map
        let mut task_model_map = HashMap::new();
        task_model_map.insert("42".to_string(), "claude/opus".to_string());

        let snap = snapshot::WorldSnapshot {
            pending_tasks_without_owners: vec![Task {
                id: "42".to_string(),
                subject: "Complex algorithm task".to_string(),
                status: TaskStatus::Pending,
                owner: None,
                blocked_by: vec![],
                description: None,
                channel: None,
                created_at: Some(SystemTime::now()),
            }],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            is_at_dev_limit: false,
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
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
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map,
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let state = make_test_state();
        let effects = spawn_for_pending_tasks(&snap, &state);

        // Find the AssignAndSpawn effect and check its LaunchConfig
        let spawn_config = effects
            .iter()
            .find_map(|e| {
                if let Effect::AssignAndSpawn { config, .. } = e {
                    Some(config)
                } else {
                    None
                }
            })
            .expect("Should have AssignAndSpawn effect");

        // LaunchConfig.model should be just "opus" (not "claude/opus")
        assert_eq!(
            spawn_config.model, "opus",
            "LaunchConfig.model should be just the model alias 'opus', not the full 'claude/opus'"
        );
    }
}
