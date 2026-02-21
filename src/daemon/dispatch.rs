//! Task dispatch — session-aware in_progress recovery, duplicate detection, pending task spawning.
//!
//! These functions run on the `TaskDispatchTick` event and coordinate coworker
//! lifecycle around the shared task list. They read from `WorldSnapshot` and
//! return `Vec<Effect>` for execution by the effect runner.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::daemon_messages;

use super::constants::*;
use super::effects::{self, Effect};
use super::{DaemonState, snapshot};

// ============================================================================
// Worktree setup helpers
// ============================================================================

/// Pre-spawn effects for setting up a worktree for a task.
struct WorktreeSetup {
    worktree_id: String,
    path: std::path::PathBuf,
    /// Effects to run before spawning (EnsureWorktree, optional RegisterWorktreeAssignment).
    pre_spawn_effects: Vec<Effect>,
}

/// Resolve and prepare a worktree for a task, reusing an existing one if registered.
///
/// Returns pre-spawn effects (EnsureWorktree + optional RegisterWorktreeAssignment)
/// and the resolved worktree path for use in LaunchConfig.working_dir.
fn prepare_task_worktree(
    task_id: &str,
    task_subject: &str,
    repo_name: &str,
    snap: &snapshot::WorldSnapshot,
) -> WorktreeSetup {
    let (worktree_id, needs_registration) =
        if let Some(existing_wt_id) = snap.task_worktree_map.get(task_id) {
            (existing_wt_id.clone(), false)
        } else {
            (
                crate::worktree_registry::branch_slug_for_task(task_id, task_subject),
                true,
            )
        };

    let path = crate::paths::worktrees_dir_for_repo(repo_name).join(&worktree_id);

    let mut pre_spawn_effects = vec![Effect::EnsureWorktree {
        worktree_id: worktree_id.clone(),
        path: path.clone(),
    }];

    if needs_registration {
        pre_spawn_effects.push(Effect::RegisterWorktreeAssignment {
            assignment: crate::worktree_registry::WorktreeAssignment {
                worktree_id: worktree_id.clone(),
                branch_name: worktree_id.clone(),
                task_id: Some(task_id.to_string()),
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now(),
                completed_at: None,
            },
        });
    }

    WorktreeSetup {
        worktree_id,
        path,
        pre_spawn_effects,
    }
}

// ============================================================================
// Plan content helpers
// ============================================================================

/// Build plan content to append to a coworker's initial prompt.
///
/// Build plan and execution skill prompt sections for a task.
///
/// Checks `task_plan_map` for plan content and `task_execution_skill_map` for an
/// explicit skill instruction. Returns empty string if neither is associated.
fn build_plan_prompt_section(task_id: &str, snap: &snapshot::WorldSnapshot) -> String {
    let plan_path = snap.task_plan_map.get(task_id);
    let execution_skill = snap.task_execution_skill_map.get(task_id);

    if plan_path.is_none() && execution_skill.is_none() {
        return String::new();
    }

    let mut section = String::new();

    // Add execution skill instruction if specified
    if let Some(skill) = execution_skill {
        section.push_str(&format!(
            "\n\n## Execution Skill\n\n\
             **Use the `superpowers:{}` skill to execute your assigned task.** \
             Invoke it before starting implementation.",
            skill
        ));
    }

    // Add plan content if available
    if let Some(plan_path) = plan_path {
        let plan_content = match std::fs::read_to_string(plan_path) {
            Ok(content) => content,
            Err(e) => {
                warn!(
                    "Failed to read plan file for task !{}: {} (path: {})",
                    task_id, e, plan_path
                );
                return section;
            }
        };

        section.push_str(&format!(
            "\n\n## Plan Context\n\n\
             Your task is part of a larger implementation plan. The full plan is included below \
             for context — it will help you understand the architecture, how your piece fits in, \
             and what decisions have already been made. **You are only responsible for your assigned \
             task above, not the entire plan.**\n\n\
             <plan>\n{}\n</plan>",
            plan_content
        ));
    }

    section
}

// ============================================================================
// Task completion helpers
// ============================================================================

/// Build the standard triple of effects for completing a task: CompleteTask + ClearBlockedBy + PostToChannel.
fn task_completed_effects(task_id: &str, repo_name: &str, channel_message: String) -> Vec<Effect> {
    vec![
        Effect::CompleteTask {
            task_id: task_id.to_string(),
            repo_name: repo_name.to_string(),
        },
        Effect::ClearBlockedBy {
            completed_task_id: task_id.to_string(),
            repo_name: repo_name.to_string(),
        },
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: channel_message,
            channel: None,
        },
    ]
}

// ============================================================================
// Session-centric helpers
// ============================================================================

/// Look up the session record for a task, if one exists.
/// Returns None if no session is associated with this task.
#[cfg(test)]
fn find_session_for_task<'a>(
    task_id: &str,
    snap: &'a snapshot::WorldSnapshot,
) -> Option<&'a crate::daemon::state::SessionRecord> {
    let session_id = snap.session_task_map.get(task_id)?;
    snap.sessions.get(session_id)
}

// ============================================================================
// Orphan task recovery
// ============================================================================

/// Parse PR state from `gh pr view --jq '.state'` output.
///
/// Returns `true` if the state is "MERGED", `false` otherwise.
fn parse_pr_merged_state(stdout: &str) -> bool {
    stdout.trim() == "MERGED"
}

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
fn is_pr_merged(pr_number: u64, repo_path: &std::path::Path) -> Option<bool> {
    let output = std::process::Command::new("gh")
        .current_dir(repo_path)
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "state",
            "--jq",
            ".state",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let state = String::from_utf8_lossy(&output.stdout);
            Some(parse_pr_merged_state(&state))
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            debug!(
                "Failed to check PR #{} state via gh CLI: {}",
                pr_number,
                stderr.trim()
            );
            None
        }
        Err(e) => {
            warn!("Failed to execute gh pr view for PR #{}: {}", pr_number, e);
            None
        }
    }
}

/// Determine whether an orphaned task should be recovered.
///
/// Production dispatch uses `should_recover_task_optional_repo` (which handles
/// optional repo_path). This version is kept for the integration test helper
/// `should_recover_task_test_helper` and the legacy `check_and_recover_orphans`
/// test path.
fn should_recover_task(
    task: &crate::tasks::Task,
    merged_pr_numbers: &HashSet<u64>,
    repo_path: &std::path::Path,
    tasks_with_open_prs: &HashMap<String, u64>,
    github_open_pr_task_ids: &HashMap<String, u64>,
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

    // Check if this task has an open PR tracked via pr_task_associations.
    // This prevents duplicate coworkers from being spawned when a task already
    // has an open PR but the task.pr field isn't set yet.
    // IMPORTANT: Only skip recovery if the PR is NOT merged. pr_author_sessions
    // cleanup is async, so stale entries for merged PRs can linger. We must
    // cross-reference merged_pr_numbers to avoid incorrectly skipping tasks.
    if let Some(&pr_number) = tasks_with_open_prs.get(&task.id) {
        if !merged_pr_numbers.contains(&pr_number) {
            debug!(
                "Skipping orphan recovery for task !{}: has open PR via pr_task_associations (PR #{})",
                task.id, pr_number
            );
            return false;
        } else {
            debug!(
                "Task !{} is in pr_task_associations but PR #{} is merged, allowing recovery for auto-completion",
                task.id, pr_number
            );
        }
    }

    // Check if this task has an explicit PR association that's already merged.
    // ONLY check the explicit pr field - never fall back to text extraction.
    // This prevents false positives like task 1142 which mentioned "PR #940 fix insufficient"
    // as context but was actually creating a different PR.
    if let Some(pr_number) = task.pr {
        // Check cache first (fast path)
        if merged_pr_numbers.contains(&pr_number) {
            debug!(
                "Skipping orphan recovery for task !{}: explicit PR #{} is in merged cache",
                task.id, pr_number
            );
            return false;
        }

        // Cache miss — check GitHub directly (safety net against stale cache)
        // The merged PR cache only includes last 10 PRs and refreshes every 5 minutes.
        // This direct check prevents duplicate PRs when:
        // 1. A PR merges but auto-completion fails
        // 2. Coworker shuts down before next cache refresh
        // 3. Orphan recovery would otherwise spawn duplicate work
        match is_pr_merged(pr_number, repo_path) {
            Some(true) => {
                info!(
                    "Skipping orphan recovery for task !{}: explicit PR #{} is merged (direct check)",
                    task.id, pr_number
                );
                return false;
            }
            Some(false) => {
                debug!(
                    "PR #{} is open/closed (not merged), allowing orphan recovery for task !{}",
                    pr_number, task.id
                );
            }
            None => {
                // GitHub API check failed — be conservative and allow recovery.
                // If the PR was actually merged, auto-completion will clean it up.
                warn!(
                    "Failed to check PR #{} merge status for task !{}, allowing recovery",
                    pr_number, task.id
                );
            }
        }
    }

    // Defense-in-depth: Check if GitHub has an open PR with [Midtown !{task_id}] in the title.
    // This data is pre-collected during snapshot from open_prs_data (no I/O here).
    // Catches cases where:
    // 1. A PR was created but pr_author_sessions wasn't updated yet
    // 2. Daemon restarted before the PR association was persisted
    // 3. The task.pr field hasn't been set yet
    if let Some(&open_pr) = github_open_pr_task_ids.get(&task.id) {
        info!(
            "Skipping orphan recovery for task !{}: found open PR #{} via GitHub PR title pattern",
            task.id, open_pr
        );
        return false;
    }

    // No associated PR found — this is a non-PR task (investigation, review, etc.)
    // or a task that hasn't opened a PR yet. Allow recovery.
    true
}

/// Check for orphaned tasks and auto-recover coworkers.
///
/// An orphaned task is one that is `in_progress` but the owning coworker
/// is no longer active (no running session). If the coworker's worktree still
/// exists, we respawn them and nudge them to resume work.
///
/// Rate limiting: Only spawns ONE coworker per tick with a cooldown between
/// spawns to prevent window flashing from spawn storms.
// Backward-compat test infrastructure: exercises the original orphan recovery
// path (case 3 only) independently of the full session-aware dispatch.
#[cfg(test)]
#[allow(dead_code)]
pub fn check_and_recover_orphans(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    check_and_recover_orphans_with_task_lookup(snap, state, crate::tasks::read_task)
}

// Backward-compat test infrastructure: testable version with injectable task lookup.
#[cfg(test)]
fn check_and_recover_orphans_with_task_lookup<F>(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
    task_lookup: F,
) -> Vec<effects::Effect>
where
    F: Fn(&str) -> Option<crate::tasks::Task>,
{
    // Check cooldown - skip if we spawned too recently (pre-evaluated in snapshot)
    if snap.orphan_spawn_cooldown_active {
        debug!("Orphan recovery cooldown active");
        return vec![];
    }

    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

    // Get primary repo path for GitHub API calls
    // NOTE: This is a pre-existing impurity (I/O for PR merge status checks).
    // The cooldown checks have been moved to the snapshot; the repo_path usage
    // remains here until should_recover_task() is fully migrated to snapshot data.
    let repo_path = state
        .all_repo_paths
        .first()
        .expect("daemon state must have at least one repo path");

    // Filter out in_progress tasks whose PRs have already been merged, that
    // have open PRs (via pr_task_associations), or that are already completed.
    // Also filter out tasks with session records — those are handled by
    // dispatch_via_sessions which has full session context for recovery.
    // These tasks are stale and will be auto-completed by the PR merge cleanup
    // path (merged/completed) or already have active work (open PR). Attempting
    // orphan recovery creates a loop: spawn → coworker sees task done → goes
    // idle → grace period expires → spawn again.
    let in_progress_tasks_active: Vec<(String, String, String)> = snap
        .in_progress_tasks
        .iter()
        .filter(|(task_id, _task_subject, _owner)| {
            // Skip tasks that have a session record — dispatch_via_sessions handles them.
            if snap.session_task_map.contains_key(task_id.as_str()) {
                debug!(
                    "Orphan recovery skipping task !{} — has session record, handled by dispatch_via_sessions",
                    task_id
                );
                return false;
            }
            // Read full task from disk to check both subject and description for PR number
            let task = match task_lookup(task_id) {
                Some(t) => t,
                None => return true, // Task doesn't exist on disk? Keep it for recovery attempt
            };

            should_recover_task(
                &task,
                &snap.merged_pr_numbers,
                repo_path,
                &snap.tasks_with_open_prs,
                &snap.github_open_pr_task_ids,
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
    let channel_lead_names: std::collections::HashSet<String> =
        snap.channel_lead_sessions.keys().cloned().collect();
    let orphan_ctx = crate::rules::OrphanRecoveryContext {
        in_progress: &in_progress_tasks_active,
        active_names: &snap.active_names,
        at_dev_limit: snap.is_at_dev_limit,
        coworkers_with_open_prs: &snap.coworkers_with_open_prs,
        review_feedback_pr_coworkers: &snap.review_feedback_pr_coworkers,
        recently_stopped: &recently_stopped,
        attached_coworkers: &snap.attached_coworkers,
        channel_lead_names: &channel_lead_names,
    };
    let recovery = crate::rules::decide_orphan_recovery(&orphan_ctx);

    let Some(recovery) = recovery else {
        return vec![];
    };

    // Check per-coworker spawn failure cooldown to prevent infinite retry loops
    // (pre-evaluated in snapshot)
    if snap
        .spawn_failure_cooldown_names
        .contains(&recovery.owner.to_lowercase())
    {
        debug!(
            "Spawn failure cooldown active for {} — skipping orphan recovery for task !{}",
            recovery.owner, recovery.task_id
        );
        return vec![];
    }

    info!(
        "Detected orphaned task !{} owned by {} - attempting recovery",
        recovery.task_id, recovery.owner
    );

    let plan_section = build_plan_prompt_section(&recovery.task_id, snap);
    let prompt = crate::agents::coworker_recovery_prompt(
        &recovery.task_id,
        &recovery.task_subject,
        &plan_section,
    );

    // Set channel from task if available
    let channel = snap
        .all_tasks
        .iter()
        .find(|t| t.id == recovery.task_id)
        .and_then(|t| t.channel.clone());

    // ── Session-aware resume path ──────────────────────────────────────
    // Check if there's a dead session record for this task that we can resume
    // instead of spawning fresh. Reviewer tasks always get fresh sessions.
    let session_record = find_session_for_task(&recovery.task_id, snap);
    if let Some(record) = session_record
        && !record.is_running
        && !record.is_reviewer
    {
        info!(
            "Resuming session {} for orphaned task !{} (owner: {})",
            record.session_id, recovery.task_id, recovery.owner
        );

        // Prepare worktree (reuse existing or create new)
        let wt = prepare_task_worktree(
            &recovery.task_id,
            &recovery.task_subject,
            &state.repo_name,
            snap,
        );

        let mut config = crate::launch::LaunchConfig::coworker(
            recovery.owner.clone(),
            state.repo_name.clone(),
            crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
            Some(prompt),
        );
        config.working_dir = Some(wt.path);
        config.channel = channel.clone();
        config.apply_task_model(&snap.task_model_map, &recovery.task_id);

        let on_success = vec![
            Effect::RecordTaskAssignment {
                coworker: recovery.owner.clone(),
                task_id: recovery.task_id.clone(),
            },
            Effect::BindCoworkerToWorktree {
                worktree_id: wt.worktree_id,
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
                    "♻️ Resumed session {} for orphaned task !{} (coworker {})",
                    record.session_id, recovery.task_id, recovery.owner
                ),
                channel: Some(OPS_CHANNEL.to_string()),
            },
        ];

        let mut effects = wt.pre_spawn_effects;
        effects.push(Effect::SpawnCoworkerWithCallbacks {
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
                        "🔄 Task !{} reset to pending - session resume for {} failed (backing off for {}s)",
                        recovery.task_id,
                        recovery.owner,
                        SPAWN_FAILURE_COOLDOWN.as_secs()
                    ),
                    channel: Some(OPS_CHANNEL.to_string()),
                },
            ],
        });

        return effects;
    }

    // ── Fresh spawn path (legacy / no session record) ──────────────────
    // Prepare worktree (reuse existing or create new)
    let wt = prepare_task_worktree(
        &recovery.task_id,
        &recovery.task_subject,
        &state.repo_name,
        snap,
    );

    let mut config = crate::launch::LaunchConfig::coworker(
        recovery.owner.clone(),
        state.repo_name.clone(),
        crate::launch::SessionMode::Fresh,
        Some(prompt),
    );
    config.working_dir = Some(wt.path);
    config.channel = channel.clone();

    // Apply task model if available (sets both provider and model)
    config.apply_task_model(&snap.task_model_map, &recovery.task_id);

    // Pre-spawn effects: create worktree and register assignment BEFORE spawning.
    let mut pre_spawn = wt.pre_spawn_effects;

    // Post-spawn success effects
    let on_success = vec![
        Effect::RecordTaskAssignment {
            coworker: recovery.owner.clone(),
            task_id: recovery.task_id.clone(),
        },
        Effect::BindCoworkerToWorktree {
            worktree_id: wt.worktree_id,
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
            channel: Some(OPS_CHANNEL.to_string()),
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
                channel: Some(OPS_CHANNEL.to_string()),
            },
        ],
    });
    pre_spawn
}

/// Extract task IDs claimed by dispatch_via_sessions effects.
///
/// Scans effects for `RecordTaskAssignment` — both as top-level effects
/// and nested inside `SpawnCoworkerWithCallbacks` on_success callbacks.
/// Used by `events.rs` to build an exclusion set for
/// `spawn_for_pending_tasks_excluding`, preventing dual-spawn when
/// in_progress recovery and pending dispatch both target the same task in one tick.
pub(super) fn extract_claimed_task_ids_from_effects(effects: &[Effect]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for effect in effects {
        match effect {
            // Legacy fresh spawn path: RecordTaskAssignment is nested in on_success
            Effect::SpawnCoworkerWithCallbacks { on_success, .. } => {
                for sub in on_success {
                    if let Effect::RecordTaskAssignment { task_id, .. } = sub {
                        ids.insert(task_id.clone());
                    }
                }
            }
            // Session-aware resume path: RecordTaskAssignment is top-level
            Effect::RecordTaskAssignment { task_id, .. } => {
                ids.insert(task_id.clone());
            }
            _ => {}
        }
    }
    ids
}

/// Session-aware dispatch for all in_progress tasks.
///
/// Pre-filter: skips tasks owned by empty owners, the Lead, or channel leads
/// (looked up via `channel_lead_sessions`). These are not managed by the
/// coworker dispatch loop and must not be recovered as regular coworkers.
///
/// For remaining tasks, handles three cases:
/// 1. Task has running session -> skip (being worked on)
/// 2. Task has stopped session -> resume via SpawnCoworkerWithCallbacks,
///    unless the coworker is an active reviewer (skip to avoid interrupting
///    their review work)
/// 3. Task has no session record -> apply recovery filtering (PR merge checks,
///    dev limit, grace period) and fresh spawn if eligible
///
/// Replaces the former `check_and_recover_orphans` which handled case 3 separately.
/// Rate-limited to one spawn per tick across all paths.
///
/// Note: not fully pure — `build_plan_prompt_section` reads plan files from disk.
/// Cooldown state is pre-evaluated into the snapshot by `collect_world_snapshot()`.
pub(super) fn dispatch_via_sessions(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    let repo_path = state.all_repo_paths.first().map(|p| p.as_path());
    dispatch_via_sessions_with_task_lookup(snap, repo_path, crate::tasks::read_task)
}

/// Internal implementation of dispatch_via_sessions, testable without DaemonState.
#[cfg(test)]
pub(super) fn dispatch_via_sessions_for_test<F>(
    snap: &snapshot::WorldSnapshot,
    repo_path: Option<&std::path::Path>,
    task_lookup: F,
) -> Vec<effects::Effect>
where
    F: Fn(&str) -> Option<crate::tasks::Task>,
{
    dispatch_via_sessions_with_task_lookup(snap, repo_path, task_lookup)
}

/// Snapshot-only dispatch_via_sessions for integration tests (no DaemonState needed).
///
/// Uses `crate::tasks::read_task` for task lookup and no repo_path (skips
/// direct GitHub PR merge checks).
#[doc(hidden)]
pub fn dispatch_via_sessions_snapshot_only(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
    dispatch_via_sessions_with_task_lookup(snap, None, crate::tasks::read_task)
}

fn dispatch_via_sessions_with_task_lookup<F>(
    snap: &snapshot::WorldSnapshot,
    repo_path: Option<&std::path::Path>,
    task_lookup: F,
) -> Vec<effects::Effect>
where
    F: Fn(&str) -> Option<crate::tasks::Task>,
{
    // Check cooldown - skip if we dispatched too recently
    if snap.session_dispatch_cooldown_active {
        debug!("Session dispatch cooldown active");
        return vec![];
    }

    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();
    let mut tasks_without_sessions: Vec<(String, String, String)> = Vec::new();

    for (task_id, task_subject, owner) in &snap.in_progress_tasks {
        // Skip empty owners, Lead, or channel leads — these are not managed by
        // the coworker dispatch loop and must not be recovered as regular coworkers.
        if owner.is_empty()
            || owner.eq_ignore_ascii_case(&snap.repo_name)
            || snap
                .channel_lead_sessions
                .contains_key(&owner.to_lowercase())
        {
            continue;
        }

        // Check if this task has a session record.
        let session_id = match snap.session_task_map.get(task_id) {
            Some(id) => id,
            None => {
                // No session record — collect for legacy fallback path below.
                tasks_without_sessions.push((task_id.clone(), task_subject.clone(), owner.clone()));
                continue;
            }
        };

        let record = match snap.sessions.get(session_id) {
            Some(r) => r,
            None => {
                warn!(
                    "Session {} referenced by task !{} not found in sessions map",
                    session_id, task_id
                );
                continue;
            }
        };

        // If the session is running (either by persisted flag or live in active_session_ids),
        // the task is handled — skip.
        //
        // active_session_ids is checked in addition to is_running because spawn_coworker
        // uses or_insert_with for existing session records, leaving is_running=false even
        // after a successful resume. The live process check ensures we don't loop on the
        // same stopped-session record every tick while the coworker is actually running.
        if record.is_running || snap.active_session_ids.contains(&record.session_id) {
            debug!(
                "Task !{} has running session {} -- no recovery needed",
                task_id, record.session_id
            );
            continue;
        }

        // Session is stopped -- attempt recovery using session data.
        // Use preferred_name for name continuity.
        let coworker_name = record
            .preferred_name
            .as_deref()
            .or(record.current_name.as_deref())
            .unwrap_or(owner);

        // Skip if this coworker is currently serving as a reviewer.
        // A coworker can have a stopped task session AND a running reviewer session.
        // Resuming the task session would interrupt their review work.
        if snap
            .active_reviewers
            .contains(&coworker_name.to_lowercase())
        {
            debug!(
                "Session dispatch: skipping task !{} — coworker {} is an active reviewer",
                task_id, coworker_name
            );
            continue;
        }

        // Check per-coworker spawn failure cooldown (pre-evaluated in snapshot)
        if snap
            .spawn_failure_cooldown_names
            .contains(&coworker_name.to_lowercase())
        {
            debug!(
                "Spawn failure cooldown active for {} -- skipping session dispatch for task !{}",
                coworker_name, task_id
            );
            continue;
        }

        info!(
            "Session dispatch: recovering task !{} via stopped session {} (preferred_name: {})",
            task_id, record.session_id, coworker_name
        );

        let plan_section = build_plan_prompt_section(task_id, snap);
        let prompt = crate::agents::coworker_recovery_prompt(task_id, task_subject, &plan_section);

        // Prepare worktree (reuse existing or create new) and build config.
        // Uses prepare_task_worktree to keep the worktree registry current and
        // emit EnsureWorktree / BindCoworkerToWorktree effects.
        let wt = prepare_task_worktree(task_id, task_subject, &snap.repo_name, snap);

        let mut config = crate::launch::LaunchConfig::coworker(
            coworker_name.to_string(),
            snap.repo_name.clone(),
            crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
            Some(prompt),
        );
        // Prefer the session's recorded working_dir (actual location on disk).
        // Fall back to the computed worktree path from the registry.
        let working_dir = if !record.working_dir.is_empty() {
            std::path::PathBuf::from(&record.working_dir)
        } else {
            wt.path.clone()
        };
        config.working_dir = Some(working_dir);

        let channel = snap
            .all_tasks
            .iter()
            .find(|t| t.id == *task_id)
            .and_then(|t| t.channel.clone());
        config.channel = channel.clone();

        config.apply_task_model(&snap.task_model_map, task_id);

        let on_success = vec![
            Effect::RecordTaskAssignment {
                coworker: coworker_name.to_string(),
                task_id: task_id.clone(),
            },
            Effect::BindCoworkerToWorktree {
                worktree_id: wt.worktree_id,
                coworker: coworker_name.to_string(),
            },
            Effect::BroadcastCoworkerUpdate {
                name: coworker_name.to_string(),
                status: "running".to_string(),
                current_task: None,
            },
            Effect::RecordCooldown {
                category: "session_dispatch".to_string(),
                key: "global".to_string(),
            },
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "Session dispatch: recovered task !{} via session {} (coworker {})",
                    task_id, record.session_id, coworker_name
                ),
                channel: Some(OPS_CHANNEL.to_string()),
            },
        ];

        // Prepend worktree setup effects (EnsureWorktree + optional registration)
        let mut pre_spawn = wt.pre_spawn_effects;
        pre_spawn.push(Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure: vec![
                Effect::RecordCooldown {
                    category: "spawn_failure".to_string(),
                    key: coworker_name.to_string(),
                },
                Effect::ResetTaskToPending {
                    task_id: task_id.clone(),
                    repo_name: snap.repo_name.clone(),
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "Task !{} reset to pending - session dispatch for {} failed (backing off for {}s)",
                        task_id,
                        coworker_name,
                        SPAWN_FAILURE_COOLDOWN.as_secs()
                    ),
                    channel: Some(OPS_CHANNEL.to_string()),
                },
            ],
        });
        effects.extend(pre_spawn);

        // Only spawn one coworker per tick (same rate limiting as orphan recovery)
        break;
    }

    // If the session-aware loop already spawned a coworker, return immediately.
    // Rate-limited to one spawn per tick.
    if !effects.is_empty() {
        return effects;
    }

    // ── Fallback: handle tasks WITHOUT session records ──────────────────
    // This replaces the former check_and_recover_orphans path. Tasks here
    // are in_progress but have no session data — either legacy tasks or
    // tasks whose session was lost.
    if tasks_without_sessions.is_empty() {
        return effects;
    }

    // At dev limit — cannot spawn any more coworkers.
    if snap.is_at_dev_limit {
        debug!("At dev limit — skipping no-session fallback dispatch");
        return effects;
    }

    // Compute recently-stopped coworkers (within grace period).
    // When a coworker completes work and goes idle -> shutdown, the task may
    // not yet be marked done. This grace period prevents false orphan recovery
    // by giving the system time to process the task completion.
    let grace_period = chrono::Duration::seconds(ORPHAN_RECOVERY_GRACE_PERIOD.as_secs() as i64);
    let recently_stopped: HashSet<String> = snap
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect();

    // Use the same pure decision function from rules.rs that orphan recovery used.
    // This ensures identical filtering behavior (active check, attached check,
    // recently-stopped grace period, open PR without feedback check).
    let channel_lead_names: std::collections::HashSet<String> =
        snap.channel_lead_sessions.keys().cloned().collect();
    let orphan_ctx = crate::rules::OrphanRecoveryContext {
        in_progress: &tasks_without_sessions,
        active_names: &snap.active_names,
        at_dev_limit: snap.is_at_dev_limit,
        coworkers_with_open_prs: &snap.coworkers_with_open_prs,
        review_feedback_pr_coworkers: &snap.review_feedback_pr_coworkers,
        recently_stopped: &recently_stopped,
        attached_coworkers: &snap.attached_coworkers,
        channel_lead_names: &channel_lead_names,
    };
    let recovery = match crate::rules::decide_orphan_recovery(&orphan_ctx) {
        Some(r) => r,
        None => return effects,
    };

    // Read full task from disk to apply should_recover_task filtering
    // (PR merge checks, completed check, open PR checks).
    if let Some(task) = task_lookup(&recovery.task_id)
        && !should_recover_task_optional_repo(
            &task,
            &snap.merged_pr_numbers,
            repo_path,
            &snap.tasks_with_open_prs,
            &snap.github_open_pr_task_ids,
        )
    {
        debug!(
            "Task !{} failed should_recover_task filtering — skipping fresh spawn",
            recovery.task_id
        );
        return effects;
    }

    // Check per-coworker spawn failure cooldown (pre-evaluated in snapshot)
    if snap
        .spawn_failure_cooldown_names
        .contains(&recovery.owner.to_lowercase())
    {
        debug!(
            "Spawn failure cooldown active for {} — skipping fresh spawn for task !{}",
            recovery.owner, recovery.task_id
        );
        return effects;
    }

    info!(
        "Session dispatch (no-session fallback): fresh spawn for task !{} (owner: {})",
        recovery.task_id, recovery.owner
    );

    let plan_section = build_plan_prompt_section(&recovery.task_id, snap);
    let prompt = crate::agents::coworker_recovery_prompt(
        &recovery.task_id,
        &recovery.task_subject,
        &plan_section,
    );

    // Set channel from task if available
    let channel = snap
        .all_tasks
        .iter()
        .find(|t| t.id == recovery.task_id)
        .and_then(|t| t.channel.clone());

    // Prepare worktree (reuse existing or create new)
    let wt = prepare_task_worktree(
        &recovery.task_id,
        &recovery.task_subject,
        &snap.repo_name,
        snap,
    );

    let mut config = crate::launch::LaunchConfig::coworker(
        recovery.owner.clone(),
        snap.repo_name.clone(),
        crate::launch::SessionMode::Fresh,
        Some(prompt),
    );
    config.working_dir = Some(wt.path);
    config.channel = channel;

    // Apply task model if available (sets both provider and model)
    config.apply_task_model(&snap.task_model_map, &recovery.task_id);

    // Pre-spawn effects: create worktree and register assignment BEFORE spawning.
    let mut pre_spawn = wt.pre_spawn_effects;

    // Post-spawn success effects
    let on_success = vec![
        Effect::RecordTaskAssignment {
            coworker: recovery.owner.clone(),
            task_id: recovery.task_id.clone(),
        },
        Effect::BindCoworkerToWorktree {
            worktree_id: wt.worktree_id,
            coworker: recovery.owner.clone(),
        },
        Effect::BroadcastCoworkerUpdate {
            name: recovery.owner.clone(),
            status: "running".to_string(),
            current_task: None,
        },
        Effect::RecordCooldown {
            category: "session_dispatch".to_string(),
            key: "global".to_string(),
        },
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "Session dispatch: fresh spawn for orphaned task !{} (coworker {})",
                recovery.task_id, recovery.owner
            ),
            channel: Some(OPS_CHANNEL.to_string()),
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
                    "Task !{} reset to pending - {} could not be spawned (backing off for {}s)",
                    recovery.task_id,
                    recovery.owner,
                    SPAWN_FAILURE_COOLDOWN.as_secs()
                ),
                channel: Some(OPS_CHANNEL.to_string()),
            },
        ],
    });
    effects.extend(pre_spawn);

    effects
}

/// Variant of `should_recover_task` that accepts an optional repo_path.
/// When repo_path is None, skips the `is_pr_merged` direct GitHub check
/// (used in tests where GitHub API is unavailable).
fn should_recover_task_optional_repo(
    task: &crate::tasks::Task,
    merged_pr_numbers: &HashSet<u64>,
    repo_path: Option<&std::path::Path>,
    tasks_with_open_prs: &HashMap<String, u64>,
    github_open_pr_task_ids: &HashMap<String, u64>,
) -> bool {
    // Check if task is already completed
    if task.status == crate::tasks::TaskStatus::Completed {
        debug!("Skipping recovery for task !{}: already completed", task.id);
        return false;
    }

    // Check if this task has an open PR tracked via pr_task_associations.
    if let Some(&pr_number) = tasks_with_open_prs.get(&task.id) {
        if !merged_pr_numbers.contains(&pr_number) {
            debug!(
                "Skipping recovery for task !{}: has open PR via pr_task_associations (PR #{})",
                task.id, pr_number
            );
            return false;
        } else {
            debug!(
                "Task !{} is in pr_task_associations but PR #{} is merged, allowing recovery for auto-completion",
                task.id, pr_number
            );
        }
    }

    // Check if this task has an explicit PR association that's already merged.
    if let Some(pr_number) = task.pr {
        // Check cache first (fast path)
        if merged_pr_numbers.contains(&pr_number) {
            debug!(
                "Skipping recovery for task !{}: explicit PR #{} is in merged cache",
                task.id, pr_number
            );
            return false;
        }

        // Cache miss — check GitHub directly (safety net against stale cache).
        // Only attempt when repo_path is available (skipped in tests).
        if let Some(path) = repo_path {
            match is_pr_merged(pr_number, path) {
                Some(true) => {
                    info!(
                        "Skipping recovery for task !{}: explicit PR #{} is merged (direct check)",
                        task.id, pr_number
                    );
                    return false;
                }
                Some(false) => {
                    debug!(
                        "PR #{} is open/closed (not merged), allowing recovery for task !{}",
                        pr_number, task.id
                    );
                }
                None => {
                    warn!(
                        "Failed to check PR #{} merge status for task !{}, allowing recovery",
                        pr_number, task.id
                    );
                }
            }
        }
    }

    // Defense-in-depth: Check if GitHub has an open PR with [Midtown !{task_id}] in the title.
    if let Some(&open_pr) = github_open_pr_task_ids.get(&task.id) {
        info!(
            "Skipping recovery for task !{}: found open PR #{} via GitHub PR title pattern",
            task.id, open_pr
        );
        return false;
    }

    true
}

/// Gather data and build effects for nudging coworkers discovered on daemon startup.
///
/// Gather data and build effects for nudging coworkers discovered on daemon startup.
///
/// After a daemon restart, session recovery is handled by the startup module
/// using persistent state. This function is a no-op kept for API compatibility.
///
/// Historical note: This used to scan for running coworkers and nudge them
/// to resume work. That logic now lives in the startup module.
pub(super) async fn gather_discovered_coworker_nudges(_state: &DaemonState) -> Vec<Effect> {
    // Session recovery is handled by the startup module using persistent state.
    vec![]
}

/// Build effects for nudging discovered coworkers based on their task/review assignments.
///
/// Pure function: takes immutable data, returns effects. All I/O (nudging,
/// channel posting) flows through Effect variants.
///
/// NOTE: This function is now only used by tests. It was part of the old session
/// recovery system that has been replaced by the startup module.
#[cfg(test)]
fn decide_discovered_coworker_nudges(
    discovered: &[String],
    owner_tasks: &HashMap<String, (String, String, Option<String>)>,
    reviewer_prs: &HashMap<String, u64>,
    name_session_map: &HashMap<String, String>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for name in discovered {
        let name_lower = name.to_lowercase();

        // Check for an in_progress task owned by this coworker
        if let Some((task_id, task_subject, _channel)) = owner_tasks.get(&name_lower) {
            let prompt = crate::agents::coworker_recovery_prompt(task_id, task_subject, "");

            info!(
                "Nudging discovered coworker {} to resume task !{}",
                name, task_id
            );

            let session_id = name_session_map
                .get(&name_lower)
                .cloned()
                .unwrap_or_default();
            effects.push(Effect::NudgeSession {
                session_id,
                reason: super::wake_reason::WakeReason::Nudge { message: prompt },
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "♻️ Nudged discovered coworker {} to resume task !{}",
                    name, task_id
                ),
                channel: Some(OPS_CHANNEL.to_string()),
            });
        } else if let Some(pr_number) = reviewer_prs.get(&name_lower) {
            let prompt = crate::agents::reviewer_resume_prompt(*pr_number);

            info!(
                "Nudging discovered coworker {} to resume review of PR #{}",
                name, pr_number
            );

            let session_id = name_session_map
                .get(&name_lower)
                .cloned()
                .unwrap_or_default();
            effects.push(Effect::NudgeSession {
                session_id,
                reason: super::wake_reason::WakeReason::Nudge { message: prompt },
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "♻️ Nudged discovered reviewer {} to resume PR #{} review",
                    name, pr_number
                ),
                channel: Some(OPS_CHANNEL.to_string()),
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
pub fn check_for_duplicate_task_workers(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

    // Build a map of task_id -> list of owners
    let mut task_workers: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, _subject, owner) in &snap.in_progress_tasks {
        // Skip empty owners, Lead, or channel leads — these are not managed by
        // the coworker dispatch loop and should not trigger duplicate detection.
        if owner.is_empty()
            || owner.eq_ignore_ascii_case(&snap.repo_name)
            || snap
                .channel_lead_sessions
                .contains_key(&owner.to_lowercase())
        {
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
                channel: Some(OPS_CHANNEL.to_string()),
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
                // Guard against race: coworker may be actively running but
                // not yet reflected in the orphan detection path.
                if coworkers.get(&name).is_some() {
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
pub fn decide_orphan_cleanup(data: &OrphanCleanupData) -> Vec<Effect> {
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
            channel: Some(OPS_CHANNEL.to_string()),
        });
    }

    // Warn about orphaned worktrees with genuinely unmerged commits.
    if !data.due_for_warning.is_empty() {
        let names_list = data.due_for_warning.join(", ");
        let nudge_text = format!(
            "⚠️ @ops Orphaned worktrees with unmerged commits (not on any PR): {}. \
             Please investigate and decide whether to merge or delete these branches.",
            names_list
        );

        effects.push(Effect::PostSystemMessage {
            message: nudge_text.clone(),
            channel: Some(OPS_CHANNEL.to_string()),
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

/// Convenience wrapper that calls `spawn_for_pending_tasks_excluding` with no exclusions.
/// Use this when orphan recovery runs in a separate tick and there are no same-tick
/// task IDs to exclude.
pub(super) fn spawn_for_pending_tasks(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<effects::Effect> {
    spawn_for_pending_tasks_excluding(snap, state, &std::collections::HashSet::new())
}

/// Handles two cases:
/// 1. Pending tasks with owners - spawn/nudge the assigned coworker if not running
/// 2. Pending tasks without owners - spawn a new coworker, assign the task, and nudge
///
/// `excluded_task_ids`: Task IDs already claimed by orphan recovery in this tick.
/// Pending dispatch skips these to avoid dual-spawn when a task appears in both
/// `in_progress_tasks` (orphaned) and `pending_tasks_without_owners` simultaneously.
pub(super) fn spawn_for_pending_tasks_excluding(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
    excluded_task_ids: &std::collections::HashSet<String>,
) -> Vec<effects::Effect> {
    // Skip task assignment if daemon is draining (graceful shutdown in progress)
    if state.draining.load(std::sync::atomic::Ordering::SeqCst) {
        debug!("Daemon is draining, skipping task assignment");
        return Vec::new();
    }

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
        // Skip tasks whose explicit PR field references a merged PR.
        // This indicates the task's work is IN that PR (not just about it).
        // Pattern matching task descriptions would cause false positives when
        // tasks reference merged PRs for context (e.g., "Fix bug from PR #123").
        let task_pr_merged = snap
            .all_tasks
            .iter()
            .find(|t| &t.id == task_id)
            .and_then(|t| t.pr)
            .map(|pr_num| snap.merged_pr_numbers.contains(&pr_num))
            .unwrap_or(false);

        if task_pr_merged {
            let pr_num = snap
                .all_tasks
                .iter()
                .find(|t| &t.id == task_id)
                .and_then(|t| t.pr)
                .unwrap(); // Safe: we just checked it exists
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

        // Channel leads are not managed by the coworker dispatch loop — skip their tasks.
        let is_channel_lead = snap
            .channel_lead_sessions
            .contains_key(&owner.to_lowercase());

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
            is_channel_lead,
        );

        match action {
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                let nudge_msg = crate::agents::coworker_nudge_prompt(tid, subj);
                // Deliver via mailbox (non-urgent task assignment to idle coworker).
                // Deliver via mailbox for non-urgent task assignment.
                effects.push(Effect::DeliverMailboxMessage {
                    name: o.clone(),
                    message: nudge_msg.clone(),
                    summary: Some(format!("Task !{} assignment", tid)),
                });
                let session_id = snap
                    .name_session_map
                    .get(&o.to_lowercase())
                    .cloned()
                    .unwrap_or_default();
                effects.push(Effect::NudgeSessionWithCallbacks {
                    session_id,
                    reason: super::wake_reason::WakeReason::Nudge { message: nudge_msg },
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
                let plan_section = build_plan_prompt_section(tid, snap);
                let prompt = crate::agents::coworker_task_prompt(tid, subj, &plan_section);

                let wt = prepare_task_worktree(tid, subj, &state.repo_name, snap);

                let mut config = crate::launch::LaunchConfig::coworker(
                    o.clone(),
                    state.repo_name.clone(),
                    crate::launch::SessionMode::Resume,
                    Some(prompt),
                );
                config.working_dir = Some(wt.path);

                // Apply task model if available (sets both provider and model)
                config.apply_task_model(&snap.task_model_map, tid);

                // Pre-spawn effects: create worktree and register assignment BEFORE spawning.
                effects.extend(wt.pre_spawn_effects);

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
                        worktree_id: wt.worktree_id,
                        coworker: o.clone(),
                    },
                    Effect::BroadcastCoworkerUpdate {
                        name: o.clone(),
                        status: "running".to_string(),
                        current_task: None,
                    },
                    Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: daemon_messages::called_in_pending_task(o, &tid.to_string()),
                        channel: Some(OPS_CHANNEL.to_string()),
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
    // Track the number of NEW spawns queued in this tick (for dev limit enforcement).
    // Spawns to already-running coworkers (grouped tasks) don't count — only fresh spawns
    // that will create new coworker processes.
    let mut spawns_queued_this_tick: usize = 0;
    // Dev cap = max_coworkers (REVIEW_HEADROOM does NOT reduce dev slots).
    // Reviewers may exceed max_coworkers by up to REVIEW_HEADROOM via is_at_coworker_limit().
    // With max=8 and REVIEW_HEADROOM=2: dev_cap=8, reviewer_cap=10.
    let dev_cap = state.max_coworkers;
    // Use running coworkers from snapshot, not all coworkers from internal map.
    // The internal map includes stopped coworkers until they're cleaned up, which
    // incorrectly blocks task dispatch when all coworkers are stopped.
    // Exclude the lead and channel leads: headless lead and channel lead sessions
    // register in CoworkerManager but are not dev/reviewer slots. Including them
    // would incorrectly consume dev slots and cause under-spawning.
    let channel_lead_names: std::collections::HashSet<&str> = snap
        .channel_lead_sessions
        .keys()
        .map(|s| s.as_str())
        .collect();
    let current_coworker_count = snap
        .running_coworkers
        .iter()
        .filter(|cw| {
            !cw.name.eq_ignore_ascii_case(&snap.repo_name)
                && !channel_lead_names.contains(cw.name.as_str())
        })
        .count();

    for task in pending_unowned.iter() {
        // Re-check dev limit after each spawn decision, accounting for spawns queued this tick.
        // This prevents spawning beyond the dev cap when multiple tasks are processed in one tick.
        let effective_count = current_coworker_count + spawns_queued_this_tick;
        if effective_count >= dev_cap {
            debug!(
                "Dev coworkers limit reached ({}+{} >= {}), deferring unowned task !{}",
                current_coworker_count, spawns_queued_this_tick, dev_cap, task.id
            );
            break;
        }

        // Skip tasks already claimed by orphan recovery in this tick.
        // Orphan recovery and pending dispatch both run on the same snapshot, so a task
        // can appear as both in_progress (orphaned) and pending simultaneously. Skipping
        // here prevents dual spawns where two different coworkers target the same task.
        if excluded_task_ids.contains(&task.id) {
            debug!(
                "Task !{} already claimed by orphan recovery this tick, skipping pending dispatch",
                task.id
            );
            continue;
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

        // Skip tasks whose explicit PR field references a merged PR.
        // This indicates the task's work is IN that PR (not just about it).
        // Pattern matching task descriptions would cause false positives when
        // tasks reference merged PRs for context (e.g., "Fix bug from PR #123").
        if let Some(pr_num) = task.pr
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

        // Session-aware dispatch: if this pending task has a stopped session
        // from a previous attempt, resume it instead of spawning fresh.
        // This preserves context and worktree state from the previous run.
        if let Some(session_id) = snap.session_task_map.get(&task.id)
            && let Some(record) = snap.sessions.get(session_id)
        {
            if !record.is_running {
                info!(
                    "Pending task !{} has stopped session {} — resuming instead of spawning fresh",
                    task.id, record.session_id
                );
                let plan_section = build_plan_prompt_section(&task.id, snap);
                let prompt =
                    crate::agents::coworker_recovery_prompt(&task.id, &task.subject, &plan_section);
                let wt = prepare_task_worktree(&task.id, &task.subject, &snap.repo_name, snap);
                let working_dir = if !record.working_dir.is_empty() {
                    std::path::PathBuf::from(&record.working_dir)
                } else {
                    wt.path.clone()
                };
                let mut config = crate::launch::LaunchConfig::coworker(
                    record.preferred_name.clone().unwrap_or_default(),
                    snap.repo_name.clone(),
                    crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
                    Some(prompt),
                );
                config.working_dir = Some(working_dir.clone());
                config.channel = task.channel.clone();
                config.apply_task_model(&snap.task_model_map, &task.id);

                effects.extend(wt.pre_spawn_effects);
                effects.push(effects::Effect::SpawnSession {
                    session_id: record.session_id.clone(),
                    task_id: task.id.clone(),
                    working_dir,
                    initial_prompt: config.initial_prompt.clone().unwrap_or_default(),
                    preferred_name: record.preferred_name.clone(),
                    is_reviewer: false,
                    resume: true,
                    config: Box::new(config),
                });
                spawns_queued_this_tick += 1;
                continue;
            }
            // Session is running — task is already being worked on. Skip.
            if record.is_running {
                debug!(
                    "Pending task !{} has running session {} — skipping dispatch",
                    task.id, record.session_id
                );
                continue;
            }
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
            let channel_lead_names: std::collections::HashSet<String> =
                snap.channel_lead_sessions.keys().cloned().collect();
            let Some(name) = state
                .coworkers
                .next_available_name_excluding(&channel_lead_names)
            else {
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
        let plan_section = build_plan_prompt_section(&task.id, snap);
        let prompt = if already_running {
            crate::agents::coworker_claim_prompt(&task.id, &task.subject, &plan_section)
        } else {
            crate::agents::coworker_task_prompt(&task.id, &task.subject, &plan_section)
        };

        if already_running {
            // Step 2a: Coworker is already running (grouped task) — nudge to claim the task.
            // The coworker runs `midtown task claim`, which writes ownership directly
            // via the daemon's RPC handler.
            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
            );
            let session_id = snap
                .name_session_map
                .get(&coworker_name.to_lowercase())
                .cloned()
                .unwrap_or_default();
            effects.push(Effect::NudgeSessionWithCallbacks {
                session_id,
                reason: super::wake_reason::WakeReason::Nudge { message: prompt },
                on_success: vec![
                    Effect::RecordTaskAssignment {
                        coworker: coworker_name.clone(),
                        task_id: task.id.clone(),
                    },
                    Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: channel_msg,
                        channel: Some(OPS_CHANNEL.to_string()),
                    },
                ],
            });
        } else {
            // Step 2b: Spawn a new coworker — assign ownership atomically with spawn
            let wt = prepare_task_worktree(&task.id, &task.subject, &state.repo_name, snap);

            let mut config = crate::launch::LaunchConfig::coworker(
                coworker_name.clone(),
                state.repo_name.clone(),
                crate::launch::SessionMode::Fresh,
                Some(prompt.clone()),
            );
            config.working_dir = Some(wt.path);
            config.channel = task.channel.clone();

            // Apply task model if available (sets both provider and model)
            config.apply_task_model(&snap.task_model_map, &task.id);

            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
            );

            // Pre-spawn effects: create worktree and register assignment BEFORE spawning.
            effects.extend(wt.pre_spawn_effects);

            // Post-spawn success effects
            let on_success = vec![
                Effect::BindCoworkerToWorktree {
                    worktree_id: wt.worktree_id,
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
                    channel: Some(OPS_CHANNEL.to_string()),
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
            // Increment spawn counter to enforce dev limit within this tick
            spawns_queued_this_tick += 1;
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

    task_completed_effects(
        &task_id.to_string(),
        repo_name,
        format!(
            "✅ Auto-completed task !{} (PR #{} merged)",
            task_id, pr_number
        ),
    )
}

/// Build effects to auto-complete tasks when all PRs referenced in their subject are merged.
///
/// This handles cases where the task is NOT linked to a PR via `[Midtown #XX]` in the PR title:
/// - Meta-tasks: "Merge reviewed PRs: #901-#910"
/// - Sub-tasks: "Address PR #904 review feedback"
/// - Fix-PR tasks: "Fix PR #908"
///
/// Tasks linked via `[Midtown #XX]` are handled by `build_task_completion_effects` (webhook path).
/// This function skips those tasks to avoid double-completion.
///
/// Returns effects to complete tasks whose subject references only merged PRs.
pub fn build_subject_based_completion_effects(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    for task in &snap.all_tasks {
        // Only consider in_progress tasks (completed tasks are already filtered out by this check)
        if task.status != crate::tasks::TaskStatus::InProgress {
            continue;
        }

        // Two paths for auto-completion:
        // 1. Explicit PR field (preferred) - set via --pr flag or auto-detected from PR title
        // 2. Text extraction (fallback) - for meta-tasks like "Merge PRs: #901, #902, #903"

        if let Some(pr_number) = task.pr {
            // Path 1: Task has explicit PR association
            // This prevents false positives (e.g., task mentions "PR #940 fix insufficient" as context)
            if snap.merged_pr_numbers.contains(&pr_number) {
                effects.extend(task_completed_effects(
                    &task.id,
                    &snap.repo_name,
                    format!(
                        "✅ Auto-completed task !{} (PR #{} merged)",
                        task.id, pr_number
                    ),
                ));
            }
        } else {
            // Path 2: No explicit PR field - extract PR numbers from subject only.
            //
            // This supports meta-tasks like "Merge reviewed PRs: #901, #902, #903"
            // or sub-tasks like "Address PR #904 review feedback" where the PR
            // numbers appear in the task title.
            //
            // Deliberately exclude task.description: descriptions often contain PR
            // numbers as contextual background (e.g., "the bug first appeared in
            // PR #1273..."), and scanning them causes false positives where a
            // task is auto-completed because PRs it merely mentions have merged.
            let pr_numbers = crate::tasks::extract_pr_numbers_from_text(&task.subject);

            // Skip if no PR references found
            if pr_numbers.is_empty() {
                continue;
            }

            // Check if ALL referenced PRs are merged
            let all_merged = pr_numbers
                .iter()
                .all(|pr_num| snap.merged_pr_numbers.contains(pr_num));

            if all_merged {
                let pr_list = pr_numbers
                    .iter()
                    .map(|n| format!("#{}", n))
                    .collect::<Vec<_>>()
                    .join(", ");
                effects.extend(task_completed_effects(
                    &task.id,
                    &snap.repo_name,
                    format!(
                        "✅ Auto-completed task !{} (all referenced PRs merged: {})",
                        task.id, pr_list
                    ),
                ));
            }
        }
    }

    effects
}

// ============================================================================
// Task unassignment for PRs in review
// ============================================================================

// Test helper function exposed for integration tests
#[doc(hidden)]
pub fn should_recover_task_test_helper(
    task: &crate::tasks::Task,
    merged_pr_numbers: &HashSet<u64>,
    repo_path: &std::path::Path,
    tasks_with_open_prs: &HashMap<String, u64>,
    github_open_pr_task_ids: &HashMap<String, u64>,
) -> bool {
    should_recover_task(
        task,
        merged_pr_numbers,
        repo_path,
        tasks_with_open_prs,
        github_open_pr_task_ids,
    )
}

// ============================================================================
// Task reset for orphaned tasks (owner on break, no PR)
// ============================================================================

/// Reset tasks that are orphaned — either ownerless or their owner went on break.
///
/// A task is reset to pending when:
///
/// **Ownerless tasks** (no owner field):
/// 1. It's in_progress with no owner
/// 2. It does NOT have an open PR (no entry in `tasks_with_open_prs` or `github_open_pr_task_ids`)
///
/// These are reset immediately — no grace period since there's no owner to recover.
///
/// **Owned tasks** (owner went on break):
/// 1. It's in_progress with an owner
/// 2. It does NOT have an open PR
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
pub fn reset_orphaned_tasks(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
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

    for (task_id, subject, owner) in &snap.in_progress_tasks {
        let owner_clean = owner.trim().trim_matches('"').to_lowercase();

        // Only consider tasks WITHOUT an associated open PR
        // (tasks with PRs are handled by reconcile_tasks_in_review)
        // Check both sources: pr_author_sessions (tasks_with_open_prs) and GitHub API
        // (github_open_pr_task_ids). After a daemon restart, pr_author_sessions is empty
        // but github_open_pr_task_ids is repopulated from the GitHub API — tasks must be
        // protected from reset even when only the GitHub source has them.
        // NOTE: This guard must fire before the ownerless check so that ownerless tasks
        // with open PRs are also protected.
        if snap.tasks_with_open_prs.contains_key(task_id)
            || snap.github_open_pr_task_ids.contains_key(task_id)
        {
            continue;
        }

        // Protect tasks that REFERENCE an open PR in their subject
        // (e.g., "Address review feedback on PR #1032") — these don't own
        // the PR but shouldn't be reset while the PR is still open.
        // This check runs before the ownerless check so that ownerless review
        // tasks (e.g., owner cleared on break) are also protected.
        if let Some(pr_num_str) = crate::tasks::extract_pr_number(subject)
            && let Ok(pr_num) = pr_num_str.parse::<u64>()
        {
            let pr_is_open = snap
                .open_prs_data
                .iter()
                .any(|pr| pr.get("number").and_then(|n| n.as_u64()) == Some(pr_num));
            if pr_is_open {
                debug!(
                    "Task !{} references open PR #{} — skipping orphan reset",
                    task_id, pr_num
                );
                continue;
            }
        }

        // Ownerless in_progress tasks have no active worker — reset to pending
        // so they can be re-dispatched. No grace period needed since there is
        // no owner to recover.
        if owner_clean.is_empty() {
            debug!(
                "Task !{} is in_progress with no owner — resetting to pending",
                task_id
            );
            effects.push(Effect::ResetTaskToPending {
                task_id: task_id.clone(),
                repo_name: snap.repo_name.clone(),
            });
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

#[path = "dispatch_dev_limit_tests.rs"]
#[cfg(test)]
mod dispatch_dev_limit_tests;

#[path = "dispatch_session_tests.rs"]
#[cfg(test)]
mod dispatch_session_tests;

#[path = "dispatch_tests.rs"]
#[cfg(test)]
mod tests;
