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

    let prompt = format!(
        "You've been assigned task #{}: {}. Your previous session was interrupted but your worktree and branch are still intact. Check your git status and get started!",
        recovery.task_id, recovery.task_subject
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
            let prompt = format!(
                "Resume task #{}: {}. The daemon was restarted and discovered you still running. Check your git status and continue where you left off.",
                task_id, task_subject
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

/// Filter out worktrees that have open or recently merged PRs.
///
/// A worktree with an open PR is waiting for review/merge.
/// A worktree with a recently merged PR may show "unmerged commits" due to
/// squash-merge creating different commit SHAs, but the work was actually merged.
///
/// Pure function for testability.
fn filter_orphans_with_pr_work(
    flagged: Vec<String>,
    open_pr_owners: &HashSet<String>,
    merged_pr_owners: &HashSet<String>,
) -> Vec<String> {
    flagged
        .into_iter()
        .filter(|name| !open_pr_owners.contains(name) && !merged_pr_owners.contains(name))
        .collect()
}

/// Clean up orphaned worktrees that have no active coworker.
///
/// Worktrees with no commits beyond the base branch are deleted.
/// Worktrees with unmerged commits are flagged to the Lead via channel.
/// Worktrees whose branches have open or recently merged PRs are skipped — they're tracked work.
pub(super) fn cleanup_orphaned_worktrees(state: &DaemonState) {
    let flagged = state.coworkers.cleanup_orphaned_worktrees();

    // Filter out worktrees whose branches have open or recently merged PRs.
    // Merged PRs are checked because squash-merge creates different commit SHAs,
    // making git think there are "unmerged commits" when the work was actually merged.
    let (open_pr_owners, merged_pr_owners) = {
        let cache = state.pr_coworker_cache.read().unwrap();
        (cache.open_pr_owners.clone(), cache.merged_pr_owners.clone())
    };
    let flagged = filter_orphans_with_pr_work(flagged, &open_pr_owners, &merged_pr_owners);
    for name in &flagged {
        debug!("Orphan worktree flagged (no open or merged PR): {}", name);
    }

    let mut tracker = state.orphan_tracker.write().unwrap();

    // Prune entries for worktrees that are no longer flagged
    tracker.prune(&flagged);

    // Track newly flagged worktrees and collect those due for a warning
    let due_for_warning: Vec<_> = flagged
        .into_iter()
        .filter(|name| {
            tracker.track(name.clone());
            tracker.should_warn(name)
        })
        .collect();

    if due_for_warning.is_empty() {
        return;
    }

    // Record warnings
    for name in &due_for_warning {
        tracker.record_warn(name);
    }
    drop(tracker);

    // Notify @lead about orphaned worktrees with unmerged commits
    let names_list = due_for_warning.join(", ");
    let msg = Message::system(format!(
        "⚠️ @lead Orphaned worktrees with unmerged commits: {}. \
         Please investigate and decide whether to merge or delete these branches.",
        names_list
    ));
    if let Err(e) = state.send_and_broadcast(&msg) {
        warn!("Failed to send orphan flag message: {}", e);
    }
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

        // Decide action using pure decision function
        let action = crate::rules::decide_pending_task_action(
            task_id,
            task_subject,
            owner,
            &snap.active_names,
            snap.is_at_dev_limit,
            on_nudge_cooldown,
        );

        match action {
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                let nudge_msg = format!("You have pending task #{}: {}. Get started!", tid, subj);
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
                let prompt = format!("You've been assigned task #{}: {}. Get started!", tid, subj);
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
    // All tasks from snapshot for relationship lookups (blockedBy, PR owner search)
    let all_tasks = &snap.all_tasks;
    // Track PR# → coworker and task_id → coworker assignments made during this loop iteration.
    // This prevents assigning different coworkers to sub-tasks of the same PR review
    // when multiple sub-tasks are processed in the same tick.
    let mut pr_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut task_coworker_map: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
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

        // Build the prompt message
        let prompt = format!(
            "You've been assigned task #{}: {}. Get started!",
            task.id, task.subject
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
    fn test_filter_orphans_with_open_prs() {
        let flagged = vec![
            "amsterdam".to_string(),
            "riverside".to_string(),
            "park".to_string(),
        ];
        let open_pr_owners: HashSet<String> = ["riverside".to_string()].into_iter().collect();
        let merged_pr_owners: HashSet<String> = HashSet::new();

        let result = filter_orphans_with_pr_work(flagged, &open_pr_owners, &merged_pr_owners);
        assert_eq!(result, vec!["amsterdam", "park"]);
    }

    #[test]
    fn test_filter_orphans_all_have_open_prs() {
        let flagged = vec!["amsterdam".to_string(), "riverside".to_string()];
        let open_pr_owners: HashSet<String> = ["amsterdam".to_string(), "riverside".to_string()]
            .into_iter()
            .collect();
        let merged_pr_owners: HashSet<String> = HashSet::new();

        let result = filter_orphans_with_pr_work(flagged, &open_pr_owners, &merged_pr_owners);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_orphans_none_have_open_prs() {
        let flagged = vec!["amsterdam".to_string(), "park".to_string()];
        let open_pr_owners: HashSet<String> = HashSet::new();
        let merged_pr_owners: HashSet<String> = HashSet::new();

        let result = filter_orphans_with_pr_work(flagged, &open_pr_owners, &merged_pr_owners);
        assert_eq!(result, vec!["amsterdam", "park"]);
    }

    #[test]
    fn test_filter_orphans_with_merged_prs() {
        // Scenario: york has a squash-merged PR. The worktree shows "unmerged commits"
        // because commit SHAs differ, but the PR was actually merged.
        // This should NOT be flagged as an orphan.
        let flagged = vec![
            "amsterdam".to_string(), // genuinely orphaned
            "york".to_string(),      // has merged PR (squash-merge)
            "park".to_string(),      // has open PR
        ];
        let open_pr_owners: HashSet<String> = ["park".to_string()].into_iter().collect();
        let merged_pr_owners: HashSet<String> = ["york".to_string()].into_iter().collect();

        let result = filter_orphans_with_pr_work(flagged, &open_pr_owners, &merged_pr_owners);

        // Only amsterdam should be flagged - york's PR was merged, park has open PR
        assert_eq!(result, vec!["amsterdam"]);
    }
}
