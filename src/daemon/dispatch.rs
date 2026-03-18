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

/// Generate a session name for a task.
///
/// Creates a slug from the task ID and subject (e.g., "task-42-fix-login-bug").
/// If the generated name collides with an excluded name, appends a short random suffix.
fn generate_task_session_name(task_id: &str, subject: &str, excluded: &HashSet<String>) -> String {
    let slug = crate::worktree_registry::branch_slug_for_task(task_id, subject);
    let name = slug.to_lowercase();
    if !excluded.contains(&name) {
        return name;
    }
    // Append random suffix for uniqueness
    let suffix = fastrand::u32(1000..9999);
    format!("{}-{}", name, suffix)
}
use super::helpers::is_project_lead;
use super::{DaemonState, snapshot};

// ============================================================================
// Push notification deep-link URL helpers
// ============================================================================

/// Build a deep-link URL for push notifications.
///
/// Format: `/{project}?channel={channel}[&msg={msg_id}][&thread={thread_id}]`
pub fn build_push_deep_link(
    project_name: &str,
    channel: &str,
    msg_id: Option<&str>,
    thread_id: Option<&str>,
) -> String {
    let mut url = format!("/{}?channel={}", project_name, channel);
    if let Some(msg) = msg_id {
        url.push_str(&format!("&msg={}", msg));
    }
    if let Some(thread) = thread_id {
        url.push_str(&format!("&thread={}", thread));
    }
    url
}

// ============================================================================
// Lead-driven channel helpers
// ============================================================================

// ============================================================================
// Recently-stopped coworker helper
// ============================================================================

/// Compute the set of coworker names that stopped within the orphan recovery grace period.
fn compute_recently_stopped(snap: &snapshot::WorldSnapshot) -> HashSet<String> {
    let grace_period = chrono::Duration::seconds(ORPHAN_RECOVERY_GRACE_PERIOD.as_secs() as i64);
    snap.coworkers
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect()
}

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
    dir_key: &str,
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

    let path = crate::paths::worktrees_dir_for_repo(dir_key).join(&worktree_id);

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
/// Checks `task_plan_map` for plan content and `task_execution_skill_map` for an
/// explicit skill instruction. Returns empty string if neither is associated.
fn build_plan_prompt_section(task_id: &str, snap: &snapshot::WorldSnapshot) -> String {
    build_plan_prompt_section_from_parts(
        task_id,
        snap.task_plan_map.get(task_id).map(|s| s.as_str()),
        snap.task_execution_skill_map
            .get(task_id)
            .map(|s| s.as_str()),
    )
}

/// Build plan and execution skill prompt sections from raw values.
///
/// Standalone version of `build_plan_prompt_section` that doesn't require a
/// `WorldSnapshot`. Used by the `coworker.spawn` RPC handler (which reads
/// plan/skill data directly from persistent state) and by the snapshot-based
/// dispatch path (which delegates here via `build_plan_prompt_section`).
pub(super) fn build_plan_prompt_section_from_parts(
    task_id: &str,
    plan_path: Option<&str>,
    execution_skill: Option<&str>,
) -> String {
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
// Spawn decision helpers
// ============================================================================

/// A normalized spawn decision. All dispatch paths produce this struct;
/// `build_spawn_effects` converts it to effects.
pub(super) struct SpawnDecision {
    pub task_id: String,
    pub session_mode: crate::launch::SessionMode,
    pub preferred_name: Option<String>,
    pub cooldown_category: String,
}

/// Convert a SpawnDecision into spawn effects by looking up task
/// metadata from the snapshot.
fn build_spawn_effects(
    decision: &SpawnDecision,
    snap: &snapshot::WorldSnapshot,
) -> Vec<effects::Effect> {
    // Look up task metadata from snapshot (check all_tasks and pending lists)
    let task = snap
        .all_tasks
        .iter()
        .chain(snap.pending_tasks_without_owners.iter())
        .find(|t| t.id == decision.task_id);
    let task_subject = task.map(|t| t.subject.as_str()).unwrap_or("(unknown)");

    let channel = snap
        .task_channel
        .get(&decision.task_id)
        .cloned()
        .or_else(|| task.and_then(|t| t.channel.clone()));

    // Build prompt — includes resume context when session is being resumed
    let plan_section = build_plan_prompt_section(&decision.task_id, snap);
    let is_resume = matches!(
        decision.session_mode,
        crate::launch::SessionMode::ResumeSession(_)
    );
    let prompt = crate::agents::coworker_task_prompt(
        &decision.task_id,
        task_subject,
        &plan_section,
        is_resume,
    );

    // Prepare worktree
    let wt = prepare_task_worktree(&decision.task_id, task_subject, &snap.dir_key, snap);

    // Check for worktree collision — skip if bound to a different active coworker
    let preferred = decision.preferred_name.as_deref().unwrap_or("");
    if let Some(bound_coworker) = snap.worktree_collision(&wt.worktree_id, preferred) {
        debug!(
            "SpawnDecision for task !{}: skipping because worktree {} is bound to active coworker {}",
            decision.task_id, wt.worktree_id, bound_coworker
        );
        return vec![];
    }

    // Build launch config
    let mut config = crate::launch::LaunchConfig::coworker(
        String::new(), // name allocated at execution time by SpawnForTask
        snap.dir_key.clone(),
        decision.session_mode.clone(),
        Some(prompt),
        Some(decision.task_id.clone()),
    );
    config.working_dir = Some(wt.path);
    config.channel = channel;
    config.apply_task_model(&snap.task_model_map, &decision.task_id);

    // For session resume, clear stale working_dir if needed
    let mut all_effects = Vec::new();
    if let crate::launch::SessionMode::ResumeSession(ref session_id) = decision.session_mode
        && snap.stale_working_dir_sessions.contains(session_id)
    {
        warn!(
            "Session {}: stale working_dir detected; clearing for task !{}",
            session_id, decision.task_id
        );
        all_effects.push(effects::Effect::ClearSessionWorkingDir {
            session_id: session_id.clone(),
        });
    }

    // Combine worktree pre-spawn effects + spawn effect
    all_effects.extend(wt.pre_spawn_effects);
    all_effects.push(effects::Effect::SpawnForTask {
        task_id: decision.task_id.clone(),
        dir_key: snap.dir_key.clone(),
        preferred_name: decision.preferred_name.clone(),
        config: Box::new(config),
        worktree_id: wt.worktree_id,
        success_message: format!(
            "Spawned coworker for task !{} ({})",
            decision.task_id, task_subject
        ),
        failure_message: format!(
            "Task !{} reset to pending — could not spawn (backing off for {}s)",
            decision.task_id,
            SPAWN_FAILURE_COOLDOWN.as_secs()
        ),
        cooldown_category: decision.cooldown_category.clone(),
        extra_success_cooldowns: vec![],
        reviewer: None,
    });
    all_effects
}

// ============================================================================
// Task completion helpers
// ============================================================================

/// Build the standard effects for completing a task: CompleteTask + ClearBlockedBy + PostToChannel + SendPushNotification.
#[allow(clippy::too_many_arguments)]
fn task_completed_effects(
    task_id: &str,
    dir_key: &str,
    task_subject: &str,
    channel_message: String,
    channel: Option<String>,
    coworker: Option<String>,
    ctx: TaskEventContext,
    push_url: Option<String>,
) -> Vec<Effect> {
    let mut effects = vec![
        Effect::CompleteTask {
            task_id: task_id.to_string(),
            dir_key: dir_key.to_string(),
        },
        Effect::ClearBlockedBy {
            completed_task_id: task_id.to_string(),
            dir_key: dir_key.to_string(),
        },
        Effect::post_to_channel("midtown", channel_message, channel.clone()),
        Effect::SendPushNotification {
            title: format!("Task !{} completed", task_id),
            body: if task_subject.is_empty() {
                format!("Task !{} has been completed", task_id)
            } else {
                format!("Task !{}: {}", task_id, task_subject)
            },
            tag: format!("task_completed_{}", task_id),
            url: push_url,
        },
    ];
    // Emit workflow event when the task's channel is known.
    if let Some(ch) = channel {
        effects.push(Effect::EmitWorkflowEvent(
            crate::workflow::WorkflowEvent::TaskCompleted {
                channel: ch,
                task_id: task_id.to_string(),
                coworker,
                subject: task_subject.to_string(),
                description: ctx.description,
                thread_id: ctx.thread_id,
                message_id: ctx.message_id,
            },
        ));
    }
    effects
}

// ============================================================================
// Orphan task recovery
// ============================================================================

/// Determine if a task should be skipped for recovery/dispatch due to PR status.
///
/// Returns `true` if the task is "protected" — it should NOT be recovered/spawned.
/// Used by `collect_world_snapshot()` to pre-compute `pr_protected_tasks` and by
/// the integration test helper `should_recover_task_test_helper`.
///
/// Checks (in order):
/// 1. Task is already completed → always protected
/// 2. Task has a merged PR (via `tasks_with_open_prs` or `task.pr`) → always protected
///    (prevents recovery-loops regardless of session state)
/// 3. Task owner has no active session → not protected by open PRs (allows dispatch
///    of pending tasks or tasks whose owner went away)
/// 4. Task has an open PR via `tasks_with_open_prs` → protected
/// 5. Task has an open PR detected from GitHub PR titles (`github_open_pr_task_ids`) → protected
pub(crate) fn is_task_pr_protected(
    task: &crate::tasks::Task,
    merged_pr_numbers: &HashSet<u64>,
    pr_task_index: &snapshot::PrTaskIndex,
    active_names: &HashSet<String>,
) -> bool {
    // Completed tasks are always protected
    if task.status == crate::tasks::TaskStatus::Completed {
        debug!("Skipping recovery for task !{}: already completed", task.id);
        return true;
    }

    // Merged-PR guards fire BEFORE the active-owner check — a merged PR always
    // protects the task (preventing recovery-loops) regardless of session state.

    // Check session-derived PR mapping for a merged PR
    if let Some(pr_number) = pr_task_index.session_pr_for_task(&task.id)
        && merged_pr_numbers.contains(&pr_number)
    {
        debug!(
            "Task !{} is in pr_task_index (session) and PR #{} is merged — protected for auto-completion",
            task.id, pr_number
        );
        return true;
    }

    // Explicit task.pr pointing to a merged PR
    if let Some(pr_number) = task.pr
        && merged_pr_numbers.contains(&pr_number)
    {
        debug!(
            "Skipping recovery for task !{}: explicit PR #{} is in merged cache",
            task.id, pr_number
        );
        return true;
    }

    // If the task's owner has no active session, open-PR protection doesn't apply.
    // This handles the catch-22 where a pending task is created for an existing PR
    // (e.g., "rebase and land PR #X") — without this, nobody could pick it up.
    let owner_is_active = task
        .owner
        .as_ref()
        .is_some_and(|owner| active_names.contains(&owner.to_lowercase()));
    if !owner_is_active {
        debug!(
            "Task !{} has no active owner session — open-PR protection does not apply",
            task.id
        );
        return false;
    }

    // Open PR via session-derived mapping (not merged — merged case handled above)
    if let Some(pr_number) = pr_task_index.session_pr_for_task(&task.id) {
        debug!(
            "Skipping recovery for task !{}: has open PR via session data (PR #{})",
            task.id, pr_number
        );
        return true;
    }

    // Defense-in-depth: GitHub title pattern match
    if let Some(open_pr) = pr_task_index.github_pr_for_task(&task.id) {
        info!(
            "Skipping recovery for task !{}: found open PR #{} via GitHub PR title pattern",
            task.id, open_pr
        );
        return true;
    }

    false
}

/// Check for orphaned tasks and auto-recover coworkers.
///
/// An orphaned task is one that is `in_progress` but the owning coworker
/// is no longer active (no running session). If the coworker's worktree still
/// exists, we respawn them and nudge them to resume work.
///
/// Rate limiting: Only spawns ONE coworker per tick with a cooldown between
/// spawns to prevent window flashing from spawn storms.
/// Check for orphaned tasks and recover coworkers.
///
/// Handles ALL orphaned in-progress tasks regardless of whether they have
/// session records. Tasks with dead sessions get resumed; tasks without
/// sessions get fresh spawns.
pub(super) fn check_and_recover_orphans(
    snap: &snapshot::WorldSnapshot,
    _state: &DaemonState,
) -> Vec<effects::Effect> {
    check_and_recover_orphans_impl(snap)
}

// Test wrapper with injectable task lookup (unused parameter kept for test compat).
#[cfg(test)]
fn check_and_recover_orphans_with_task_lookup<F>(
    snap: &snapshot::WorldSnapshot,
    _task_lookup: F,
) -> Vec<effects::Effect>
where
    F: Fn(&str) -> Option<crate::tasks::Task>,
{
    check_and_recover_orphans_impl(snap)
}

fn check_and_recover_orphans_impl(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
    // Check cooldown - skip if we spawned too recently (pre-evaluated in snapshot)
    if snap.orphan_spawn_cooldown_active {
        debug!("Orphan recovery cooldown active");
        return vec![];
    }

    if snap.in_progress_tasks.is_empty() {
        return vec![];
    }

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
            // Skip tasks that are PR-protected (pre-computed in snapshot)
            if snap.pr_protected_tasks.contains(task_id) {
                debug!("Orphan recovery skipping task !{} — PR-protected", task_id);
                return false;
            }
            true
        })
        .cloned()
        .collect();

    if in_progress_tasks_active.is_empty() {
        return vec![];
    }

    let recently_stopped = compute_recently_stopped(snap);

    // Decide which orphan (if any) to recover using pure decision function
    let channel_lead_names = snap.channel_lead_names();
    let orphan_ctx = crate::rules::OrphanRecoveryContext {
        in_progress: &in_progress_tasks_active,
        active_names: &snap.coworkers.active_names,
        recently_stopped: &recently_stopped,
        attached_coworkers: &snap.coworkers.attached_coworkers,
        channel_lead_names,
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

    let (session_mode, preferred_name) = match snap.find_session_for_task(&recovery.task_id) {
        Some(record) if !record.is_running && record.agent_type != "midtown-code-reviewer" => (
            crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
            if record.name.is_empty() {
                None
            } else {
                Some(record.name.clone())
            },
        ),
        _ => (
            crate::launch::SessionMode::Fresh,
            Some(recovery.owner.clone()),
        ),
    };

    let decision = SpawnDecision {
        task_id: recovery.task_id.clone(),
        session_mode,
        preferred_name,
        cooldown_category: "orphan_spawn".to_string(),
    };
    build_spawn_effects(&decision, snap)
}

/// Session-aware dispatch for in_progress tasks that have session records.
///
/// Pre-filter: skips tasks owned by empty owners, the Lead, channel leads
/// (looked up via `channel_lead_sessions`), or tasks in lead-driven channels.
/// These are not managed by the coworker dispatch loop and must not be
/// recovered as regular coworkers.
///
/// For remaining tasks with session records, handles two cases:
/// 1. Task has running session -> skip (being worked on)
/// 2. Task has stopped session -> resume via SpawnForTask,
///    unless the coworker is an active reviewer (skip to avoid interrupting
///    their review work) or the session was recently recovered (per-session
///    cooldown prevents re-recovery spam when sessions die quickly)
///
/// Tasks without session records are handled by `check_and_recover_orphans`.
/// Rate-limited to one spawn per tick across all paths.
///
/// Note: not fully pure — `build_plan_prompt_section` reads plan files from disk.
/// Stale working-dir checks and cooldown state are pre-evaluated into the snapshot
/// by `collect_world_snapshot()`.
pub(super) fn dispatch_via_sessions(
    snap: &snapshot::WorldSnapshot,
    _state: &DaemonState,
) -> Vec<effects::Effect> {
    dispatch_via_sessions_inner(snap)
}

/// Internal implementation of dispatch_via_sessions, testable without DaemonState.
#[cfg(test)]
pub(super) fn dispatch_via_sessions_for_test(
    snap: &snapshot::WorldSnapshot,
) -> Vec<effects::Effect> {
    dispatch_via_sessions_inner(snap)
}

/// Snapshot-only dispatch_via_sessions for integration tests (no DaemonState needed).
#[doc(hidden)]
pub fn dispatch_via_sessions_snapshot_only(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
    dispatch_via_sessions_inner(snap)
}

fn dispatch_via_sessions_inner(snap: &snapshot::WorldSnapshot) -> Vec<effects::Effect> {
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
        let action = crate::rules::decide_session_recovery(task_id, task_subject, owner, snap);

        match action {
            crate::rules::SessionRecoveryAction::Skip(ref reason) => {
                if reason.is_stale_session_ref() {
                    warn!("task !{}: skipping session recovery — {}", task_id, reason);
                } else {
                    debug!("task !{}: skipping session recovery — {}", task_id, reason);
                }
                continue;
            }
            crate::rules::SessionRecoveryAction::FallbackToOrphan {
                task_id: ref tid,
                task_subject: ref subj,
                owner: ref o,
            } => {
                tasks_without_sessions.push((tid.clone(), subj.clone(), o.clone()));
                continue;
            }
            crate::rules::SessionRecoveryAction::Recover {
                ref task_id,
                ref task_subject,
                ref coworker_name,
                ref session_id,
            } => {
                // Look up the session record (guaranteed to exist since decide_session_recovery
                // returned Recover, but use guard for safety).
                let record = match snap.find_session_for_task(task_id) {
                    Some(r) => r,
                    None => continue,
                };

                info!(
                    "Session dispatch: recovering task !{} via stopped session {} (preferred_name: {})",
                    task_id, session_id, coworker_name
                );
                let _ = task_subject; // used by build_spawn_effects via snapshot lookup

                let decision = SpawnDecision {
                    task_id: task_id.clone(),
                    session_mode: crate::launch::SessionMode::ResumeSession(
                        record.session_id.clone(),
                    ),
                    preferred_name: Some(coworker_name.clone()),
                    cooldown_category: "session_dispatch".to_string(),
                };
                let mut spawn_effects = build_spawn_effects(&decision, snap);
                // Add per-session-id cooldown to prevent re-recovery on the next tick
                // even if the session dies quickly (see !1709 fix). The
                // recently_recovered_session_ids snapshot field checks this cooldown.
                for effect in &mut spawn_effects {
                    if let Effect::SpawnForTask {
                        extra_success_cooldowns,
                        ..
                    } = effect
                    {
                        extra_success_cooldowns
                            .push(("session_recovered".to_string(), record.session_id.clone()));
                    }
                }
                effects.extend(spawn_effects);

                // Only spawn one coworker per tick (same rate limiting as orphan recovery)
                break;
            }
        }
    }

    // No-session tasks are now handled by check_and_recover_orphans, which
    // covers all orphans (with or without session records) in a single path.
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
        // Skip empty owners, lead (repo-named or legacy "lead"), or channel leads —
        // these are not managed by the coworker dispatch loop.
        if owner.is_empty()
            || is_project_lead(owner, &snap.project_name)
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
                let start_time = snap
                    .coworkers
                    .coworker_start_times
                    .get(&name.to_lowercase())
                    .copied();
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
            effects.push(Effect::post_to_ops(format!(
                "🔪 Killed duplicate worker {} on task !{} ({}) - {} started earlier",
                duplicate, task_id, task_subject, keeper
            )));
        }
    }

    effects
}

// ============================================================================
// Pending task auto-spawn
// ============================================================================

/// Data gathered for periodic cleanup decisions (stale branches).
///
/// Collected once in the async wrapper, then passed to the pure decision function.
pub(super) struct StaleBranchCleanupData {
    /// Whether the stale branch cleanup cooldown has expired.
    pub stale_branch_cleanup_due: bool,
}

/// Gather data needed for periodic cleanup decisions.
///
/// Legacy coworker-named worktree cleanup has been removed. Task-based worktrees
/// are cleaned up via CleanupMergedWorktree / CleanupStaleWorktree effects.
///
/// Returns `None` if the PR poll hasn't initialized yet (too early to decide).
pub(super) async fn gather_stale_branch_cleanup_data(
    state: &DaemonState,
    _in_progress_task_owners: &[String],
) -> Option<StaleBranchCleanupData> {
    let pr_poll_initialized = {
        let cache = state.pr_poll_data.read().unwrap();
        cache.pr_poll_initialized
    };

    if !pr_poll_initialized {
        debug!("Skipping cleanup - PR poll not yet initialized");
        return None;
    }

    // Check stale branch cleanup cooldown (in-memory state, not I/O)
    let stale_branch_cleanup_due = {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        if cooldowns.check("stale_branch_cleanup", "global", Duration::from_secs(300)) {
            cooldowns.record("stale_branch_cleanup", "global");
            true
        } else {
            false
        }
    };

    Some(StaleBranchCleanupData {
        stale_branch_cleanup_due,
    })
}

/// Build effects for stale branch cleanup based on gathered data.
///
/// Pure function: takes immutable data, returns effects. All I/O flows through
/// Effect variants executed by `effects::execute_effects`.
pub fn decide_stale_branch_cleanup(data: &StaleBranchCleanupData) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Note: Legacy coworker-name-based orphan detection has been removed.
    // Reviewer assignment clearing was driven by that detection and is no longer
    // triggered here. Task-based worktrees handle cleanup via
    // CleanupMergedWorktree / CleanupStaleWorktree effects.

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

/// Dispatches pending tasks in two phases:
/// 1. Owned pending tasks — spawn/nudge the assigned coworker if not running
/// 2. Unowned pending tasks — resolve a coworker name, assign ownership, and spawn
///
/// `excluded_task_ids`: Task IDs already claimed by orphan recovery in this tick.
/// Pending dispatch skips these to avoid dual-spawn when a task appears in both
/// `in_progress_tasks` (orphaned) and `pending_tasks_without_owners` simultaneously.
pub(super) fn spawn_for_pending_tasks_excluding(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
    excluded_task_ids: &std::collections::HashSet<String>,
) -> Vec<effects::Effect> {
    if state.draining.load(std::sync::atomic::Ordering::SeqCst) {
        debug!("Daemon is draining, skipping task assignment");
        return Vec::new();
    }

    debug!(
        "Task assignment state: active={}",
        snap.coworkers.running_coworkers.len()
    );

    let (mut effects, coworkers_dispatched_this_tick) = dispatch_owned_pending_tasks(snap, state);

    effects.extend(dispatch_unowned_pending_tasks(
        snap,
        state,
        excluded_task_ids,
        &coworkers_dispatched_this_tick,
    ));

    effects
}

// ============================================================================
// Owned pending tasks (Case 1)
// ============================================================================

/// Handle pending tasks that already have an owner assigned but whose coworker
/// is not running. Spawns or nudges the assigned coworker as appropriate.
///
/// With the daemon-managed task.claim flow, this case is rare (claims set
/// both owner and in_progress directly). It mainly handles backward compatibility
/// with pre-existing tasks or tasks where the Lead manually set an owner.
///
/// Returns effects and the set of coworker names dispatched (for cross-case
/// deduplication with `dispatch_unowned_pending_tasks`).
fn dispatch_owned_pending_tasks(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> (Vec<effects::Effect>, HashSet<String>) {
    let mut effects = Vec::new();
    let mut coworkers_dispatched_this_tick: HashSet<String> = HashSet::new();

    for (task_id, task_subject, owner) in snap.pending_tasks_with_owners.iter() {
        let action =
            crate::rules::decide_owned_pending_dispatch(task_id, task_subject, owner, snap);

        // Post-decision live-state guards: close the TOCTOU window where a
        // concurrent RPC dispatcher (e.g. daemon.check-pending) claims the
        // same task between snapshot collection and effect execution.
        // Check the *current* in-flight set and cooldowns, not the snapshot copy.
        if matches!(
            action,
            crate::rules::PendingTaskAction::NudgeOwner { .. }
                | crate::rules::PendingTaskAction::SpawnOwner { .. }
        ) {
            if state.is_task_spawn_in_flight(task_id) {
                debug!(
                    "task !{}: skipping owned pending dispatch — in-flight spawn (live check)",
                    task_id
                );
                continue;
            }
            // Live nudge-cooldown check: an RPC dispatcher may have nudged
            // this task after our snapshot was collected, recording a cooldown
            // that isn't in snap.task_nudge_cooldown_ids.
            if matches!(action, crate::rules::PendingTaskAction::NudgeOwner { .. }) {
                let task_key = format!("pending-{}", task_id);
                let on_cooldown = {
                    let cooldowns = state.cooldowns.lock().unwrap();
                    !cooldowns.check(
                        "task_nudge",
                        &task_key,
                        super::constants::TASK_NUDGE_COOLDOWN,
                    )
                };
                if on_cooldown {
                    debug!(
                        "task !{}: skipping owned pending dispatch — nudge cooldown (live check)",
                        task_id
                    );
                    continue;
                }
            }
        }

        match action {
            crate::rules::PendingTaskAction::AutoComplete {
                ref task_id,
                pr_num,
            } => {
                info!(
                    "Auto-completing stale task !{}: PR #{} has been merged",
                    task_id, pr_num
                );
                effects.push(Effect::CompleteTask {
                    task_id: task_id.clone(),
                    dir_key: snap.dir_key.clone(),
                });
                effects.push(Effect::ClearBlockedBy {
                    completed_task_id: task_id.clone(),
                    dir_key: snap.dir_key.clone(),
                });
            }
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                let task_key = format!("pending-{}", tid);
                let session_id = snap
                    .name_session_map
                    .get(&o.to_lowercase())
                    .cloned()
                    .unwrap_or_default();
                effects.push(Effect::NudgeSessionWithCallbacks {
                    session_id,
                    reason: super::wake_reason::WakeReason::TaskAssigned {
                        task_id: tid.clone(),
                        subject: subj.clone(),
                    },
                    on_success: vec![
                        Effect::RecordCooldown {
                            category: "task_nudge".to_string(),
                            key: task_key,
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
                task_subject: ref _subj,
            } => {
                // Post-decision spawn guards: these depend on loop-accumulation
                // state or spawn-specific context that can't be in the pure function.
                if coworkers_dispatched_this_tick.contains(&o.to_lowercase()) {
                    debug!(
                        "Already spawned {} this tick — skipping duplicate spawn for task !{}",
                        o, tid
                    );
                    continue;
                }

                if snap
                    .spawn_failure_cooldown_names
                    .contains(&o.to_lowercase())
                {
                    debug!(
                        "Spawn failure cooldown active for {} — skipping pending task !{}",
                        o, tid
                    );
                    continue;
                }

                info!(
                    "Pending task !{} is assigned to {} but coworker not running - spawning",
                    tid, o
                );

                let decision = SpawnDecision {
                    task_id: tid.clone(),
                    session_mode: crate::launch::SessionMode::Fresh,
                    preferred_name: Some(o.clone()),
                    cooldown_category: "task_dispatch".to_string(),
                };
                effects.extend(build_spawn_effects(&decision, snap));

                coworkers_dispatched_this_tick.insert(o.to_lowercase());
            }
            crate::rules::PendingTaskAction::Skip(ref reason) => {
                debug!(
                    "task !{}: skipping owned pending dispatch — {}",
                    task_id, reason
                );
            }
        }
    }

    (effects, coworkers_dispatched_this_tick)
}

// ============================================================================
// Unowned pending tasks (Case 2)
// ============================================================================

/// Resolve a coworker name for an unowned task by checking grouping strategies.
///
/// Priority: in-memory PR map > in-memory blockedBy map > session-based PR owner >
///           disk blockedBy relationship > None (allocate fresh name).
fn resolve_grouped_name(
    task: &crate::tasks::Task,
    snap: &snapshot::WorldSnapshot,
    pr_coworker_map: &HashMap<String, String>,
    task_coworker_map: &HashMap<String, String>,
) -> Option<String> {
    // Strategy A: Extract PR number from subject or description
    if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(task) {
        if let Some(name) = pr_coworker_map.get(&pr_num) {
            info!(
                "Task !{} references PR #{} - assigning to in-memory owner {}",
                task.id, pr_num, name
            );
            return Some(name.clone());
        }
        // Look up via session: PR number → task with that PR → session → current_name
        if let Ok(pr_number_u64) = pr_num.parse::<u64>()
            && let Some(pr_task) = snap.all_tasks.iter().find(|t| {
                t.pr == Some(pr_number_u64)
                    && (t.status == crate::tasks::TaskStatus::InProgress
                        || t.status == crate::tasks::TaskStatus::Pending)
            })
            && let Some(session) = snap.find_session_for_task(&pr_task.id)
            && !session.name.is_empty()
        {
            let name = &session.name;
            info!(
                "Task !{} references PR #{} - assigning to session owner {}",
                task.id, pr_num, name
            );
            return Some(name.clone());
        }
        // Fallback: scan task subjects/descriptions for PR pattern (covers tasks
        // without the explicit `pr` field set) and resolve via session.
        let pr_pattern = format!("PR #{}", pr_num);
        for t in snap.all_tasks.iter().filter(|t| {
            (t.status == crate::tasks::TaskStatus::InProgress
                || t.status == crate::tasks::TaskStatus::Pending)
                && (t.subject.contains(&pr_pattern)
                    || t.description
                        .as_ref()
                        .is_some_and(|d| d.contains(&pr_pattern)))
        }) {
            // Session-based lookup (source of truth).
            if let Some(session) = snap.find_session_for_task(&t.id)
                && !session.name.is_empty()
            {
                let name = &session.name;
                info!(
                    "Task !{} references PR #{} - assigning to session owner {} (text match)",
                    task.id, pr_num, name
                );
                return Some(name.clone());
            }
        }
    }

    // Strategy B: Check blockedBy relationships
    for blocked_by_id in &task.blocked_by {
        if let Some(name) = task_coworker_map.get(blocked_by_id) {
            info!(
                "Task !{} blocked by #{} - assigning to same owner {}",
                task.id, blocked_by_id, name
            );
            return Some(name.clone());
        }
    }
    if let Some(owner) = crate::tasks::find_owner_via_blocked_by(task, &snap.all_tasks) {
        info!(
            "Task !{} blocked by owned task - assigning to {}",
            task.id, owner
        );
        return Some(owner);
    }

    None
}

/// Handle pending tasks that have no owner. Resolves a coworker name (via PR/blockedBy
/// grouping or fresh allocation), assigns ownership atomically, and spawns.
///
/// `owned_dispatched`: Coworker names already dispatched by `dispatch_owned_pending_tasks`,
/// used to prevent the same coworker from being targeted by both phases in a single tick.
fn dispatch_unowned_pending_tasks(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
    excluded_task_ids: &std::collections::HashSet<String>,
    owned_dispatched: &HashSet<String>,
) -> Vec<effects::Effect> {
    let mut effects = Vec::new();

    // Log PR review priority state for diagnostics, but never block task dispatch.
    let active_review_count = snap.reviewer.active_reviewers.len();
    let prs_with_reviewers = snap
        .reviewer
        .reviewer_pr_assignments
        .values()
        .collect::<HashSet<_>>()
        .len();
    let unserved_prs = snap
        .reviewer
        .prs_needing_review
        .saturating_sub(prs_with_reviewers);
    if unserved_prs > 0 {
        debug!(
            "PR review state: {} unserved PR(s) need review ({} total, {} already have reviewers), {} active reviewers — task dispatch proceeds independently",
            unserved_prs, snap.reviewer.prs_needing_review, prs_with_reviewers, active_review_count
        );
    }

    // Track PR# -> coworker and task_id -> coworker assignments made during this loop.
    // Prevents assigning different coworkers to sub-tasks of the same PR review.
    let mut pr_coworker_map: HashMap<String, String> = HashMap::new();
    let mut task_coworker_map: HashMap<String, String> = HashMap::new();
    // Track coworker names assigned within this phase to prevent duplicate assignments.
    let mut names_assigned_this_tick: HashSet<String> = HashSet::new();
    // Track NEW spawns queued (for task limit enforcement). Nudges to already-running
    // coworkers (grouped tasks) don't count — only fresh spawns.
    let mut spawns_queued_this_tick: usize = 0;
    let in_progress_count = snap.in_progress_tasks.len();
    let task_cap = snap.max_in_progress_tasks;
    let channel_lead_names = snap.channel_lead_names();

    // Order pending tasks by dispatch priority before iterating.
    let in_progress_ids: std::collections::HashSet<String> = snap
        .in_progress_tasks
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect();
    let prioritized_ids = crate::daemon::dispatch_priority::prioritize_pending_tasks(
        &snap.pending_tasks_without_owners,
        &in_progress_ids,
        &snap.task_parent_map,
        &snap.blocks_map,
    );

    for task_id in prioritized_ids.iter() {
        let Some(task) = snap
            .pending_tasks_without_owners
            .iter()
            .find(|t| &t.id == task_id)
        else {
            continue;
        };

        // ── Stage 1: Unconditional skip/cleanup (no task limit gate) ─────────
        // These operations don't consume slots and should run regardless of capacity.

        // Skip tasks already claimed by orphan recovery in this tick.
        if excluded_task_ids.contains(&task.id) {
            debug!(
                "Task !{} already claimed by orphan recovery this tick, skipping pending dispatch",
                task.id
            );
            continue;
        }

        // Skip tasks that already have an in-flight spawn effect.
        if state.is_task_spawn_in_flight(&task.id) {
            debug!(
                "Task !{} already has in-flight spawn, skipping duplicate",
                task.id
            );
            continue;
        }

        // Skip tasks whose explicit PR field references a merged PR.
        // IMPORTANT: This must run before the lead-driven check so merged-PR
        // auto-complete works regardless of channel mode.
        // We have the full Task struct here, so check task.pr directly (O(1))
        // instead of scanning all_tasks by ID like dispatch_owned_pending_tasks does.
        if let Some(pr_num) = task.pr.filter(|pr| snap.pr.merged_pr_numbers.contains(pr)) {
            info!(
                "Auto-completing stale task !{}: PR #{} has been merged",
                task.id, pr_num
            );
            effects.push(Effect::CompleteTask {
                task_id: task.id.clone(),
                dir_key: snap.dir_key.clone(),
            });
            effects.push(Effect::ClearBlockedBy {
                completed_task_id: task.id.clone(),
                dir_key: snap.dir_key.clone(),
            });
            continue;
        }

        // NOTE: We intentionally do NOT check pr_protected_tasks here.
        // PR-protection only applies to in_progress tasks during orphan recovery
        // (see dispatch_orphaned_in_progress_tasks). Pending unowned tasks must
        // remain dispatchable even if they have an associated open PR — e.g., a
        // task created as "rebase and land PR #X" needs to be assigned to someone.

        // Skip tasks in lead-driven channels — the lead manages dispatch manually.
        if task
            .channel
            .as_ref()
            .is_some_and(|ch| snap.lead_driven_channels.contains(ch))
        {
            debug!(
                "Task !{}: skipping unowned pending dispatch — channel is lead-driven",
                task.id
            );
            continue;
        }

        // Session-aware dispatch: if this pending task has a stopped session
        // from a previous attempt, resume it instead of spawning fresh.
        if let Some(record) = snap.find_session_for_task(&task.id) {
            if !record.is_running {
                if snap
                    .recently_recovered_session_ids
                    .contains(&record.session_id)
                {
                    debug!(
                        "Pending task !{} has recently-recovered session {} — skipping (cooldown)",
                        task.id, record.session_id
                    );
                    continue;
                }

                info!(
                    "Pending task !{} has stopped session {} — resuming instead of spawning fresh",
                    task.id, record.session_id
                );

                let preferred_name = if record.name.is_empty() {
                    None
                } else {
                    Some(record.name.clone())
                };
                let session_id = record.session_id.clone();
                let decision = SpawnDecision {
                    task_id: task.id.clone(),
                    session_mode: crate::launch::SessionMode::ResumeSession(session_id),
                    preferred_name,
                    cooldown_category: "session_dispatch".to_string(),
                };
                effects.extend(build_spawn_effects(&decision, snap));
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

        // ── Stage 2: Name resolution + task limit gate ────────────────────────

        // Check if this is a reviewer task — reviewers must always be fresh spawns
        // on isolated worktrees, never grouped with the implementation coworker.
        let is_reviewer_task = snap
            .task_agent_type_map
            .get(&task.id)
            .is_some_and(|at| at == "midtown-code-reviewer");

        // Step 1: Determine the coworker name by checking grouping strategies.
        // Reviewer tasks skip grouping entirely — they share a PR number with the
        // implementation task, so grouping would route them to the author's session.
        let grouped_name = if is_reviewer_task {
            None
        } else {
            resolve_grouped_name(task, snap, &pr_coworker_map, &task_coworker_map)
        };

        // Use grouped name if found, otherwise allocate a fresh coworker.
        // When at task limit and no grouping match, try to reuse an idle coworker.
        let was_grouped = grouped_name.is_some();
        let effective_count = in_progress_count + spawns_queued_this_tick;
        let at_task_limit = effective_count >= task_cap;

        let coworker_name = if let Some(name) = grouped_name {
            name
        } else if at_task_limit {
            // At task limit — can't start new in-progress tasks. Check for an idle
            // coworker (running session, no in-progress task) to reuse via nudge.
            // Reviewer tasks always need a fresh spawn on an isolated worktree,
            // so they cannot reuse idle coworkers.
            if is_reviewer_task {
                debug!(
                    "Task limit reached ({}+{} >= {}), reviewer task !{} deferred",
                    in_progress_count, spawns_queued_this_tick, task_cap, task.id
                );
                continue;
            }
            if let Some(name) = find_idle_coworker(
                snap,
                channel_lead_names,
                &names_assigned_this_tick,
                owned_dispatched,
            ) {
                debug!(
                    "Task limit reached but found idle coworker {} for task !{}",
                    name, task.id
                );
                name
            } else {
                debug!(
                    "Task limit reached ({}+{} >= {}) and no idle coworker, deferring task !{}",
                    in_progress_count, spawns_queued_this_tick, task_cap, task.id
                );
                continue;
            }
        } else {
            let mut excluded_names = snap.channel_lead_names().clone();
            // Exclude all names with active sessions to prevent name collisions.
            // CoworkerManager only knows about registered coworkers, but a session
            // may still be running after its coworker was cleaned up from the manager.
            // active_names (from WorldSnapshot) tracks all names with live sessions.
            for name in &snap.coworkers.active_names {
                excluded_names.insert(name.clone());
            }
            // For reviewer tasks, exclude the PR author to prevent self-review.
            // The author is the owner of the parent implementation task.
            if is_reviewer_task
                && let Some(parent_id) = snap.task_parent_map.get(&task.id)
                && let Some(parent_task) = snap.all_tasks.iter().find(|t| t.id == *parent_id)
                && let Some(ref author) = parent_task.owner
            {
                excluded_names.insert(author.to_lowercase());
            }
            let name = generate_task_session_name(&task.id, &task.subject, &excluded_names);
            debug!("Task !{}: allocated fresh coworker name {}", task.id, name,);
            name
        };

        // Check per-coworker spawn failure cooldown (pre-evaluated in snapshot)
        if snap
            .spawn_failure_cooldown_names
            .contains(&coworker_name.to_lowercase())
        {
            debug!(
                "Task !{}: skipping {} (spawn failure cooldown active)",
                task.id, coworker_name
            );
            continue;
        }

        // For grouped names, the coworker may already be running — we nudge it.
        // For freshly allocated names, this is always false (they were excluded from
        // active_names during allocation), so they always take the spawn path.
        let already_running = snap
            .coworkers
            .active_names
            .contains(&coworker_name.to_lowercase());
        let is_coworker_reviewer = snap
            .reviewer
            .active_reviewers
            .contains(&coworker_name.to_lowercase());
        let is_busy_from_snapshot = snap.busy_coworkers.contains(&coworker_name.to_lowercase());
        let assigned_this_tick = names_assigned_this_tick.contains(&coworker_name.to_lowercase());

        // Skip if owned-task dispatch already dispatched this coworker.
        if owned_dispatched.contains(&coworker_name.to_lowercase()) {
            debug!(
                "Task !{}: skipping {} (already dispatched by owned pending tasks)",
                task.id, coworker_name
            );
            continue;
        }

        // Skip if this coworker is already assigned to THIS SPECIFIC TASK.
        if snap
            .name_task_assignments
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
        // However, always skip if already assigned *this tick*.
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

        // For not-yet-running coworkers, prevent assigning multiple tasks to the
        // same coworker within the same tick. One spawn per coworker per tick is
        // sufficient — grouped tasks are allowed to bypass the busy check for
        // *already-running* coworkers (nudge path above) but not for fresh spawns.
        if !already_running && (assigned_this_tick || is_busy_from_snapshot) {
            debug!(
                "Task !{}: skipping {} (not running, already assigned this tick or busy)",
                task.id, coworker_name
            );
            continue;
        }

        info!(
            "Proposing task !{} for {} (already_running={})",
            task.id, coworker_name, already_running
        );

        // Record this assignment in in-memory maps for same-tick grouping.
        task_coworker_map.insert(task.id.clone(), coworker_name.clone());
        if let Some(pr_num) = crate::tasks::extract_pr_number_from_task(task) {
            pr_coworker_map.insert(pr_num, coworker_name.clone());
        }
        names_assigned_this_tick.insert(coworker_name.to_lowercase());

        // Build plan section before branching — both paths may need it.
        let plan_section = build_plan_prompt_section(&task.id, snap);

        if already_running {
            // Coworker is already running (grouped task) — nudge to claim.
            // Reviewer tasks skip grouping, so they should never reach this path.
            debug_assert!(
                !is_reviewer_task,
                "reviewer task !{} reached already_running path",
                task.id
            );
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
            let mut assign_callbacks = vec![
                Effect::RecordTaskAssignment {
                    coworker: coworker_name.clone(),
                    task_id: task.id.clone(),
                },
                Effect::post_to_ops(channel_msg),
            ];
            if let Some(ch) = &task.channel {
                assign_callbacks.push(Effect::EmitWorkflowEvent(
                    crate::workflow::WorkflowEvent::TaskAssigned {
                        channel: ch.clone(),
                        task_id: task.id.clone(),
                        coworker: coworker_name.clone(),
                        subject: task.subject.clone(),
                        description: task.description.clone(),
                        thread_id: snap.task_thread_id_map.get(&task.id).cloned(),
                        message_id: snap.task_message_id_map.get(&task.id).cloned(),
                    },
                ));
            }
            effects.push(Effect::NudgeSessionWithCallbacks {
                session_id,
                reason: super::wake_reason::WakeReason::TaskClaimed {
                    task_id: task.id.clone(),
                    subject: task.subject.clone(),
                    plan_section: plan_section.clone(),
                },
                on_success: assign_callbacks,
            });
        } else if is_reviewer_task {
            // Reviewer task: use review-specific worktree and launch config.
            let pr_number = task.pr.unwrap_or(0);
            if pr_number == 0 {
                warn!("Reviewer task !{} has no PR number, skipping", task.id);
                continue;
            }

            let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);
            let wt_path = crate::paths::worktrees_dir_for_repo(&snap.dir_key).join(&worktree_id);

            if let Some(bound_coworker) = snap.worktree_collision(&worktree_id, &coworker_name) {
                debug!(
                    "Review task !{}: skipping {} because worktree {} is bound to active coworker {}",
                    task.id, coworker_name, worktree_id, bound_coworker
                );
                continue;
            }

            let auth_provider = crate::config::get_execution_provider_for_role(
                &snap.dir_key,
                crate::config::ExecutionRole::Reviewer,
            );
            let mut config = crate::launch::LaunchConfig::reviewer(
                coworker_name.clone(),
                &snap.dir_key,
                pr_number,
                0,
                auth_provider,
            );
            config.model = super::helpers::normalize_model_for_provider_role(
                &config.model,
                config.auth_provider,
                &config.agent_type,
            );
            config.working_dir = Some(wt_path.clone());
            config.channel = task.channel.clone();
            config.task_id = Some(task.id.clone());

            // Route escalation to channel lead if available
            let channel_lead_names = snap.channel_lead_names();
            if let Some(ref channel_name) = config.channel
                && channel_lead_names.contains(channel_name)
            {
                config.escalation_target = Some(channel_name.clone());
                config.initial_prompt = Some(crate::agents::reviewer_launch_prompt(
                    pr_number,
                    0,
                    auth_provider,
                    Some(channel_name),
                ));
            }

            effects.push(effects::Effect::EnsureWorktree {
                worktree_id: worktree_id.clone(),
                path: wt_path.clone(),
            });

            effects.push(effects::Effect::SpawnForTask {
                task_id: task.id.clone(),
                dir_key: snap.dir_key.clone(),
                preferred_name: Some(coworker_name.clone()),
                config: Box::new(config),
                worktree_id: worktree_id.clone(),
                success_message: daemon_messages::called_in_reviewer(&coworker_name, pr_number),
                failure_message: format!(
                    "⚠️ Spawn failed for review task !{} (reviewer {}) — backing off for {}s",
                    task.id,
                    coworker_name,
                    SPAWN_FAILURE_COOLDOWN.as_secs()
                ),
                cooldown_category: "task_dispatch".to_string(),
                extra_success_cooldowns: vec![],
                reviewer: Some(effects::ReviewerSpawnInfo {
                    pr_number,
                    pr_comment_body: format!(
                        "{}\n## Review Status\n\n\
                         🔍 Review in progress...\n\n---\n\
                         > [!NOTE]\n> This comment will be updated with the review results when complete.\n\n\
                         🌃 Co-built with [Midtown](https://github.com/btucker/midtown)",
                        crate::daemon::helpers::format_placeholder_frontmatter(&task.id)
                    ),
                    restart_count: 0,
                    agent_type: "midtown-code-reviewer".to_string(),
                }),
            });
            spawns_queued_this_tick += 1;
        } else {
            // Regular coworker task — use SpawnDecision for normalized spawn
            let decision = SpawnDecision {
                task_id: task.id.clone(),
                session_mode: crate::launch::SessionMode::Fresh,
                preferred_name: Some(coworker_name.clone()),
                cooldown_category: "task_dispatch".to_string(),
            };
            effects.extend(build_spawn_effects(&decision, snap));
            spawns_queued_this_tick += 1;
        }
    }

    effects
}

/// Find an idle coworker — one with a running session but no in-progress task.
///
/// Returns the name of an idle coworker that can be nudged to pick up a new task,
/// or `None` if no idle coworker is available.
fn find_idle_coworker(
    snap: &snapshot::WorldSnapshot,
    channel_lead_names: &HashSet<String>,
    names_assigned_this_tick: &HashSet<String>,
    owned_dispatched: &HashSet<String>,
) -> Option<String> {
    snap.coworkers
        .active_names
        .iter()
        .find(|name| {
            !snap.busy_coworkers.contains(*name)
                && !snap.reviewer.active_reviewers.contains(*name)
                && !names_assigned_this_tick.contains(*name)
                && !owned_dispatched.contains(*name)
                && !snap.spawn_failure_cooldown_names.contains(*name)
                && !channel_lead_names.contains(*name)
                && super::helpers::is_non_lead_coworker(
                    name,
                    &snap.project_name,
                    channel_lead_names,
                )
        })
        .cloned()
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
/// Context for enriching task workflow events with thread/message/description data.
#[derive(Default)]
pub(super) struct TaskEventContext {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub thread_id: Option<String>,
    pub message_id: Option<String>,
}

pub(super) fn build_task_completion_effects(
    pr_title: &str,
    pr_number: u64,
    dir_key: &str,
    project_name: &str,
    channel: Option<String>,
    ctx: Option<TaskEventContext>,
) -> Vec<Effect> {
    let Some(task_id) = crate::tasks::extract_task_id_from_pr_title(pr_title) else {
        return vec![];
    };

    let mut ctx = ctx.unwrap_or_default();

    // Build deep-link URL for the push notification
    let push_url = channel.as_ref().map(|ch| {
        build_push_deep_link(
            project_name,
            ch,
            ctx.message_id.as_deref(),
            ctx.thread_id.as_deref(),
        )
    });

    // Use the actual task subject when available; fall back to PR title.
    let task_subject = ctx.subject.take().unwrap_or_else(|| pr_title.to_string());

    let mut effects = task_completed_effects(
        &task_id.to_string(),
        dir_key,
        &task_subject,
        format!(
            "✅ Auto-completed task !{} (PR #{} merged)",
            task_id, pr_number
        ),
        channel.clone(),
        None,
        ctx,
        push_url,
    );

    // Emit PrMerged alongside task completion when channel is known.
    if let Some(ch) = channel {
        effects.push(Effect::EmitWorkflowEvent(
            crate::workflow::WorkflowEvent::PrMerged {
                channel: ch,
                task_id: task_id.to_string(),
                pr_number,
            },
        ));
    }

    effects
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

        let task_channel = snap.task_channel.get(&task.id).cloned();

        // Build deep-link URL from task channel + message metadata.
        // Use task_message_id as the scroll target. Don't combine thread_id
        // with message_id — they can refer to different threads when a task
        // was dispatched from a user-created thread, causing a silent scroll
        // failure (message Y doesn't exist in thread X).
        let task_msg_id = snap.task_message_id_map.get(&task.id).map(|s| s.as_str());

        if let Some(pr_number) = task.pr {
            // Path 1: Task has explicit PR association
            // This prevents false positives (e.g., task mentions "PR #940 fix insufficient" as context)
            if snap.pr.merged_pr_numbers.contains(&pr_number) {
                let push_url = task_channel
                    .as_ref()
                    .map(|ch| build_push_deep_link(&snap.project_name, ch, task_msg_id, None));
                effects.extend(task_completed_effects(
                    &task.id,
                    &snap.dir_key,
                    &task.subject,
                    format!(
                        "✅ Auto-completed task !{} (PR #{} merged)",
                        task.id, pr_number
                    ),
                    task_channel,
                    task.owner.clone(),
                    TaskEventContext {
                        subject: None,
                        description: task.description.clone(),
                        thread_id: snap.task_thread_id_map.get(&task.id).cloned(),
                        message_id: snap.task_message_id_map.get(&task.id).cloned(),
                    },
                    push_url,
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
                .all(|pr_num| snap.pr.merged_pr_numbers.contains(pr_num));

            if all_merged {
                let pr_list = pr_numbers
                    .iter()
                    .map(|n| format!("#{}", n))
                    .collect::<Vec<_>>()
                    .join(", ");
                let push_url = task_channel
                    .as_ref()
                    .map(|ch| build_push_deep_link(&snap.project_name, ch, task_msg_id, None));
                effects.extend(task_completed_effects(
                    &task.id,
                    &snap.dir_key,
                    &task.subject,
                    format!(
                        "✅ Auto-completed task !{} (all referenced PRs merged: {})",
                        task.id, pr_list
                    ),
                    task_channel,
                    task.owner.clone(),
                    TaskEventContext {
                        subject: None,
                        description: task.description.clone(),
                        thread_id: snap.task_thread_id_map.get(&task.id).cloned(),
                        message_id: snap.task_message_id_map.get(&task.id).cloned(),
                    },
                    push_url,
                ));
            }
        }
    }

    effects
}

// ============================================================================
// Task unassignment for PRs in review
// ============================================================================

// Test helper function exposed for integration tests.
// `repo_path` is kept in the signature for backward compatibility but unused —
// the direct GitHub API safety net (`is_pr_merged`) has been removed.
#[doc(hidden)]
pub fn should_recover_task_test_helper(
    task: &crate::tasks::Task,
    merged_pr_numbers: &HashSet<u64>,
    _repo_path: &std::path::Path,
    tasks_with_open_prs: &HashMap<String, u64>,
    github_open_pr_task_ids: &HashMap<String, u64>,
) -> bool {
    // Test helper: assume owner is active (preserves existing test behavior)
    let mut active_names = HashSet::new();
    if let Some(owner) = &task.owner {
        active_names.insert(owner.to_lowercase());
    }
    let pr_task_index = snapshot::PrTaskIndex::from_task_maps(
        tasks_with_open_prs.clone(),
        github_open_pr_task_ids.clone(),
    );
    !is_task_pr_protected(task, merged_pr_numbers, &pr_task_index, &active_names)
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

    let recently_stopped = compute_recently_stopped(snap);

    for (task_id, subject, owner) in &snap.in_progress_tasks {
        let owner_clean = owner.trim().trim_matches('"').to_lowercase();

        // Only consider tasks WITHOUT an associated open PR
        // (tasks with PRs are handled by reconcile_tasks_in_review)
        // Check both sources: SessionRecord (tasks_with_open_prs) and GitHub API
        // (github_open_pr_task_ids). After a daemon restart, SessionRecord data may be stale
        // but github_open_pr_task_ids is repopulated from the GitHub API — tasks must be
        // protected from reset even when only the GitHub source has them.
        // NOTE: This guard must fire before the ownerless check so that ownerless tasks
        // with open PRs are also protected.
        if snap.pr.pr_task_index.task_has_pr(task_id) {
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
                .pr
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
                dir_key: snap.dir_key.clone(),
            });
            continue;
        }

        // Only reset if the owner is NOT active (already shut down / on break)
        if snap.coworkers.active_names.contains(&owner_clean) {
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
            dir_key: snap.dir_key.clone(),
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

#[path = "dispatch_name_collision_tests.rs"]
#[cfg(test)]
mod name_collision_tests;
