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
use super::helpers::{get_merged_task_pr, is_non_lead_coworker, is_project_lead};
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

/// Returns true if a task belongs to a lead-driven channel.
///
/// Lead-driven channels skip automatic coworker dispatch — the channel lead
/// manages coworker lifecycle manually.
fn is_task_in_lead_driven_channel(task_id: &str, snap: &snapshot::WorldSnapshot) -> bool {
    snap.task_channel
        .get(task_id)
        .is_some_and(|ch| snap.lead_driven_channels.contains(ch))
}

// ============================================================================
// Spawn callback helpers
// ============================================================================

/// Standard spawn-failure callback: record cooldown, reset task to pending, post to ops.
fn spawn_failure_effects(
    cooldown_key: impl Into<String>,
    task_id: impl Into<String>,
    dir_key: impl Into<String>,
    message: impl Into<String>,
) -> Vec<Effect> {
    vec![
        Effect::RecordCooldown {
            category: "spawn_failure".to_string(),
            key: cooldown_key.into(),
        },
        Effect::ResetTaskToPending {
            task_id: task_id.into(),
            dir_key: dir_key.into(),
        },
        Effect::post_to_ops(message),
    ]
}

/// Common spawn-success effects: assign task, bind worktree, broadcast status, post to ops.
fn spawn_success_effects(
    coworker: impl Into<String>,
    task_id: impl Into<String>,
    worktree_id: impl Into<String>,
    message: impl Into<String>,
) -> Vec<Effect> {
    let coworker = coworker.into();
    vec![
        Effect::RecordTaskAssignment {
            coworker: coworker.clone(),
            task_id: task_id.into(),
        },
        Effect::BindCoworkerToWorktree {
            worktree_id: worktree_id.into(),
            coworker: coworker.clone(),
        },
        Effect::BroadcastCoworkerUpdate {
            name: coworker,
            status: "running".to_string(),
            current_task: None,
        },
        Effect::post_to_ops(message),
    ]
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
// Session-centric helpers
// ============================================================================

/// Look up the session record for a task, if one exists.
/// Returns None if no session is associated with this task.
///
/// Delegates to [`snapshot::WorldSnapshot::find_session_for_task`].
#[cfg(test)]
fn find_session_for_task<'a>(
    task_id: &str,
    snap: &'a snapshot::WorldSnapshot,
) -> Option<&'a crate::daemon::state::SessionRecord> {
    snap.find_session_for_task(task_id)
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
// Backward-compat test infrastructure: testable version with injectable task lookup.
#[cfg(test)]
fn check_and_recover_orphans_with_task_lookup<F>(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
    _task_lookup: F,
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
            // Skip tasks that are PR-protected (pre-computed in snapshot)
            if snap.pr_protected_tasks.contains(task_id) {
                debug!(
                    "Orphan recovery skipping task !{} — PR-protected",
                    task_id
                );
                return false;
            }
            true
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
        .coworkers
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect();

    // Decide which orphan (if any) to recover using pure decision function
    let channel_lead_names = snap.channel_lead_names();
    let orphan_ctx = crate::rules::OrphanRecoveryContext {
        in_progress: &in_progress_tasks_active,
        active_names: &snap.coworkers.active_names,
        at_dev_limit: snap.is_at_dev_limit,
        coworkers_with_open_prs: &snap.pr.coworkers_with_open_prs,
        review_feedback_pr_coworkers: &snap.pr.review_feedback_pr_coworkers,
        recently_stopped: &recently_stopped,
        attached_coworkers: &snap.coworkers.attached_coworkers,
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
            state.paths.dir_key(),
            snap,
        );

        let mut config = crate::launch::LaunchConfig::coworker(
            recovery.owner.clone(),
            state.paths.dir_key().to_string(),
            crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
            Some(prompt),
            Some(recovery.task_id.clone()),
        );
        config.working_dir = Some(wt.path);
        config.channel = channel.clone();
        config.apply_task_model(&snap.task_model_map, &recovery.task_id);

        let mut on_success = spawn_success_effects(
            recovery.owner.clone(),
            recovery.task_id.clone(),
            wt.worktree_id,
            format!(
                "♻️ Resumed session {} for orphaned task !{} (coworker {})",
                record.session_id, recovery.task_id, recovery.owner
            ),
        );
        on_success.insert(
            on_success.len() - 1,
            Effect::RecordCooldown {
                category: "orphan_spawn".to_string(),
                key: "global".to_string(),
            },
        );

        let mut effects = wt.pre_spawn_effects;
        effects.push(Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure: spawn_failure_effects(
                recovery.owner.clone(),
                recovery.task_id.clone(),
                snap.dir_key.clone(),
                format!(
                    "🔄 Task !{} reset to pending - session resume for {} failed (backing off for {}s)",
                    recovery.task_id,
                    recovery.owner,
                    SPAWN_FAILURE_COOLDOWN.as_secs()
                ),
            ),
        });

        return effects;
    }

    // ── Fresh spawn path (legacy / no session record) ──────────────────
    // Prepare worktree (reuse existing or create new)
    let wt = prepare_task_worktree(
        &recovery.task_id,
        &recovery.task_subject,
        state.paths.dir_key(),
        snap,
    );

    let mut config = crate::launch::LaunchConfig::coworker(
        recovery.owner.clone(),
        state.paths.dir_key().to_string(),
        crate::launch::SessionMode::Fresh,
        Some(prompt),
        Some(recovery.task_id.clone()),
    );
    config.working_dir = Some(wt.path);
    config.channel = channel.clone();

    // Apply task model if available (sets both provider and model)
    config.apply_task_model(&snap.task_model_map, &recovery.task_id);

    // Pre-spawn effects: create worktree and register assignment BEFORE spawning.
    let mut pre_spawn = wt.pre_spawn_effects;

    // Post-spawn success effects
    let mut on_success = spawn_success_effects(
        recovery.owner.clone(),
        recovery.task_id.clone(),
        wt.worktree_id,
        format!(
            "♻️ Recovered coworker {} for orphaned task !{}",
            recovery.owner, recovery.task_id
        ),
    );
    on_success.insert(
        on_success.len() - 1,
        Effect::RecordCooldown {
            category: "orphan_spawn".to_string(),
            key: "global".to_string(),
        },
    );

    // EnsureWorktree + RegisterWorktreeAssignment run first, then spawn
    pre_spawn.push(Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success,
        on_failure: spawn_failure_effects(
            recovery.owner.clone(),
            recovery.task_id.clone(),
            snap.dir_key.clone(),
            format!(
                "🔄 Task !{} reset to pending - {} could not be respawned (backing off for {}s)",
                recovery.task_id,
                recovery.owner,
                SPAWN_FAILURE_COOLDOWN.as_secs()
            ),
        ),
    });
    pre_spawn
}

/// Session-aware dispatch for all in_progress tasks.
///
/// Pre-filter: skips tasks owned by empty owners, the Lead, channel leads
/// (looked up via `channel_lead_sessions`), or tasks in lead-driven channels.
/// These are not managed by the coworker dispatch loop and must not be
/// recovered as regular coworkers.
///
/// For remaining tasks, handles three cases:
/// 1. Task has running session -> skip (being worked on)
/// 2. Task has stopped session -> resume via SpawnCoworkerWithCallbacks,
///    unless the coworker is an active reviewer (skip to avoid interrupting
///    their review work) or the session was recently recovered (per-session
///    cooldown prevents re-recovery spam when sessions die quickly)
/// 3. Task has no session record -> apply recovery filtering (PR merge checks,
///    dev limit, grace period) and fresh spawn if eligible
///
/// Replaces the former `check_and_recover_orphans` which handled case 3 separately.
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

        // Skip tasks in lead-driven channels — the lead manages dispatch manually.
        if is_task_in_lead_driven_channel(task_id, snap) {
            debug!(
                "Task !{}: skipping session recovery — channel is lead-driven",
                task_id
            );
            continue;
        }

        // Check if this task has a session record.
        let record = match snap.find_session_for_task(task_id) {
            Some(r) => r,
            None => {
                if snap.session_task_map.contains_key(task_id) {
                    // session_task_map has the entry but sessions map is stale
                    warn!(
                        "Session for task !{} referenced in session_task_map but not found in sessions map",
                        task_id
                    );
                } else {
                    // No session record — collect for legacy fallback path below.
                    tasks_without_sessions.push((
                        task_id.clone(),
                        task_subject.clone(),
                        owner.clone(),
                    ));
                }
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
        if record.is_running
            || snap
                .coworkers
                .active_session_ids
                .contains(&record.session_id)
        {
            debug!(
                "Task !{} has running session {} -- no recovery needed",
                task_id, record.session_id
            );
            continue;
        }

        // Skip if a recovery was recently attempted for this session (per-session cooldown).
        //
        // Without this guard, when a session dies within a single tick window (5s) after a
        // successful recovery spawn, the next tick sees is_running=false and active_session_ids
        // empty — and fires recovery again. The global SESSION_DISPATCH_COOLDOWN (2s) always
        // expires before the next 5s tick, providing no protection between ticks.
        //
        // The "session_recovered" cooldown (SESSION_RECOVERED_COOLDOWN) is set per-session-id
        // in on_success. If the session_id is in recently_recovered_session_ids, a recovery
        // was already attempted recently — skip to prevent the log spam described in !1709.
        if snap
            .recently_recovered_session_ids
            .contains(&record.session_id)
        {
            debug!(
                "Task !{} has recently-recovered session {} -- skipping re-recovery (cooldown active)",
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
            .reviewer
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
        let wt = prepare_task_worktree(task_id, task_subject, &snap.dir_key, snap);
        if let Some(bound_coworker) = snap.worktree_collision(&wt.worktree_id, coworker_name) {
            debug!(
                "Session dispatch: skipping task !{} because worktree {} is bound to active coworker {}",
                task_id, wt.worktree_id, bound_coworker
            );
            continue;
        }

        let mut config = crate::launch::LaunchConfig::coworker(
            coworker_name.to_string(),
            snap.dir_key.clone(),
            crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
            Some(prompt),
            Some(task_id.clone()),
        );
        // Prefer the session's recorded working_dir (actual location on disk).
        // Fall back to the computed worktree path from the registry.
        // Staleness is pre-evaluated in WorldSnapshot::stale_working_dir_sessions
        // during collect_world_snapshot() — no filesystem I/O here.
        let working_dir = if !record.working_dir.is_empty()
            && !snap.stale_working_dir_sessions.contains(&record.session_id)
        {
            std::path::PathBuf::from(&record.working_dir)
        } else if !record.working_dir.is_empty() {
            warn!(
                "Session {}: recorded working_dir {:?} no longer exists; \
                 falling back to fresh worktree for task !{}",
                record.session_id, record.working_dir, task_id
            );
            effects.push(effects::Effect::ClearSessionWorkingDir {
                session_id: record.session_id.clone(),
            });
            wt.path.clone()
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

        let mut on_success = spawn_success_effects(
            coworker_name.to_string(),
            task_id.clone(),
            wt.worktree_id,
            format!(
                "Session dispatch: recovered task !{} via session {} (coworker {})",
                task_id, record.session_id, coworker_name
            ),
        );
        let insert_pos = on_success.len() - 1;
        on_success.insert(
            insert_pos,
            Effect::RecordCooldown {
                category: "session_dispatch".to_string(),
                key: "global".to_string(),
            },
        );
        // Per-session-id cooldown: prevents re-recovery on the next tick even if the
        // session dies quickly. The recently_recovered_session_ids snapshot field checks
        // this cooldown and skips recovery while it's active (see !1709 fix).
        on_success.insert(
            insert_pos + 1,
            Effect::RecordCooldown {
                category: "session_recovered".to_string(),
                key: record.session_id.clone(),
            },
        );

        // Prepend worktree setup effects (EnsureWorktree + optional registration)
        let mut pre_spawn = wt.pre_spawn_effects;
        pre_spawn.push(Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure: {
                let mut v = vec![Effect::ClearSessionForTask {
                    task_id: task_id.clone(),
                }];
                v.extend(spawn_failure_effects(
                    coworker_name.to_string(),
                    task_id.clone(),
                    snap.dir_key.clone(),
                    format!(
                        "Task !{} reset to pending - session dispatch for {} failed (backing off for {}s)",
                        task_id,
                        coworker_name,
                        SPAWN_FAILURE_COOLDOWN.as_secs()
                    ),
                ));
                v
            },
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
        .coworkers
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect();

    // Use the same pure decision function from rules.rs that orphan recovery used.
    // This ensures identical filtering behavior (active check, attached check,
    // recently-stopped grace period, open PR without feedback check).
    let channel_lead_names = snap.channel_lead_names();
    let orphan_ctx = crate::rules::OrphanRecoveryContext {
        in_progress: &tasks_without_sessions,
        active_names: &snap.coworkers.active_names,
        at_dev_limit: snap.is_at_dev_limit,
        coworkers_with_open_prs: &snap.pr.coworkers_with_open_prs,
        review_feedback_pr_coworkers: &snap.pr.review_feedback_pr_coworkers,
        recently_stopped: &recently_stopped,
        attached_coworkers: &snap.coworkers.attached_coworkers,
        channel_lead_names: &channel_lead_names,
    };
    let recovery = match crate::rules::decide_orphan_recovery(&orphan_ctx) {
        Some(r) => r,
        None => return effects,
    };

    // Check pre-computed PR protection (snapshot-level filtering).
    if snap.pr_protected_tasks.contains(&recovery.task_id) {
        debug!(
            "Task !{} is PR-protected — skipping fresh spawn",
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
        &snap.dir_key,
        snap,
    );
    if let Some(bound_coworker) = snap.worktree_collision(&wt.worktree_id, &recovery.owner) {
        debug!(
            "Session dispatch: skipping fallback spawn for task !{} because worktree {} is bound to active coworker {}",
            recovery.task_id, wt.worktree_id, bound_coworker
        );
        return vec![];
    }

    let mut config = crate::launch::LaunchConfig::coworker(
        recovery.owner.clone(),
        snap.dir_key.clone(),
        crate::launch::SessionMode::Fresh,
        Some(prompt),
        Some(recovery.task_id.clone()),
    );
    config.working_dir = Some(wt.path);
    config.channel = channel;

    // Apply task model if available (sets both provider and model)
    config.apply_task_model(&snap.task_model_map, &recovery.task_id);

    // Pre-spawn effects: create worktree and register assignment BEFORE spawning.
    let mut pre_spawn = wt.pre_spawn_effects;

    // Post-spawn success effects
    let mut on_success = spawn_success_effects(
        recovery.owner.clone(),
        recovery.task_id.clone(),
        wt.worktree_id,
        format!(
            "Session dispatch: fresh spawn for orphaned task !{} (coworker {})",
            recovery.task_id, recovery.owner
        ),
    );
    on_success.insert(
        on_success.len() - 1,
        Effect::RecordCooldown {
            category: "session_dispatch".to_string(),
            key: "global".to_string(),
        },
    );

    // EnsureWorktree + RegisterWorktreeAssignment run first, then spawn
    pre_spawn.push(Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success,
        on_failure: spawn_failure_effects(
            recovery.owner.clone(),
            recovery.task_id.clone(),
            snap.dir_key.clone(),
            format!(
                "Task !{} reset to pending - {} could not be spawned (backing off for {}s)",
                recovery.task_id,
                recovery.owner,
                SPAWN_FAILURE_COOLDOWN.as_secs()
            ),
        ),
    });
    effects.extend(pre_spawn);

    effects
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
                reason: super::wake_reason::WakeReason::SessionRecovery {
                    task_id: task_id.clone(),
                    subject: task_subject.clone(),
                },
            });
            effects.push(Effect::post_to_ops(format!(
                "♻️ Nudged discovered coworker {} to resume task !{}",
                name, task_id
            )));
        } else if let Some(pr_number) = reviewer_prs.get(&name_lower) {
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
                reason: super::wake_reason::WakeReason::ReviewAssigned {
                    pr_number: *pr_number,
                },
            });
            effects.push(Effect::post_to_ops(format!(
                "♻️ Nudged discovered reviewer {} to resume PR #{} review",
                name, pr_number
            )));
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
        let cache = state.pr_coworker_cache.read().unwrap();
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
        // Skip tasks whose explicit PR field references a merged PR.
        // IMPORTANT: This must run before the lead-driven check so merged-PR
        // auto-complete works regardless of channel mode.
        if let Some(pr_num) =
            get_merged_task_pr(task_id, &snap.all_tasks, &snap.pr.merged_pr_numbers)
        {
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
            continue;
        }

        // Skip tasks in lead-driven channels — the lead manages dispatch manually.
        if is_task_in_lead_driven_channel(task_id, snap) {
            debug!(
                "Task !{}: skipping owned pending dispatch — channel is lead-driven",
                task_id
            );
            continue;
        }

        // Skip tasks that already have an in-flight spawn from a previous tick.
        if state.is_task_spawn_in_flight(task_id) {
            debug!(
                "Task !{} already has in-flight spawn, skipping duplicate",
                task_id
            );
            continue;
        }

        // Skip if this owner is already assigned to THIS SPECIFIC TASK.
        // Prevents nudge loops where the same pending-with-owner task gets
        // re-nudged every time the 300s cooldown expires.
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

        let task_key = format!("pending-{}", task_id);
        let on_nudge_cooldown = {
            let cooldowns = state.cooldowns.lock().unwrap();
            !cooldowns.check("task_nudge", &task_key, Duration::from_secs(300))
        };

        let is_owner_reviewer = snap
            .reviewer
            .active_reviewers
            .contains(&owner.to_lowercase());
        let has_in_progress_task = snap.busy_coworkers.contains(&owner.to_lowercase());
        let is_channel_lead = snap
            .channel_lead_sessions
            .contains_key(&owner.to_lowercase());

        let action = crate::rules::decide_pending_task_action(
            task_id,
            task_subject,
            owner,
            &snap.coworkers.active_names,
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
                if coworkers_dispatched_this_tick.contains(&o.to_lowercase()) {
                    debug!(
                        "Already spawned {} this tick — skipping duplicate spawn for task !{}",
                        o, tid
                    );
                    continue;
                }

                // Skip if a previous spawn failure put this coworker on cooldown.
                // Without this check, a missing worktree causes an infinite retry
                // loop every 5s (see !2172).
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
                let plan_section = build_plan_prompt_section(tid, snap);
                let prompt = crate::agents::coworker_task_prompt(tid, subj, &plan_section);

                let wt = prepare_task_worktree(tid, subj, state.paths.dir_key(), snap);

                if let Some(bound_coworker) = snap.worktree_collision(&wt.worktree_id, o) {
                    debug!(
                        "Pending owned task !{}: skipping {} because worktree {} is bound to active coworker {}",
                        tid, o, wt.worktree_id, bound_coworker
                    );
                    continue;
                }

                let mut config = crate::launch::LaunchConfig::coworker(
                    o.clone(),
                    state.paths.dir_key().to_string(),
                    crate::launch::SessionMode::Resume,
                    Some(prompt),
                    Some(tid.clone()),
                );
                config.working_dir = Some(wt.path);
                config.apply_task_model(&snap.task_model_map, tid);

                effects.extend(wt.pre_spawn_effects);

                // Include RecordTaskAssignment so mark_in_flight_spawns_from_effects()
                // can track this spawn across ticks and prevent duplicate spawns if
                // the spawn takes longer than one tick interval to complete.
                let on_success = spawn_success_effects(
                    o.clone(),
                    tid.clone(),
                    wt.worktree_id,
                    daemon_messages::called_in_pending_task(o, &tid.to_string()),
                );

                effects.push(Effect::SpawnCoworkerWithCallbacks {
                    config,
                    on_success,
                    on_failure: spawn_failure_effects(
                        o.clone(),
                        tid.clone(),
                        snap.dir_key.clone(),
                        format!(
                            "⚠️ Spawn failed for pending task !{} (coworker {}) — backing off for {}s",
                            tid, o, SPAWN_FAILURE_COOLDOWN.as_secs()
                        ),
                    ),
                });

                coworkers_dispatched_this_tick.insert(o.to_lowercase());
            }
            crate::rules::PendingTaskAction::Skip { ref reason } => {
                debug!("{}", reason);
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
/// Priority: in-memory PR map > in-memory blockedBy map > disk PR owner >
///           disk blockedBy relationship > None (allocate fresh name).
fn resolve_grouped_name(
    task: &crate::tasks::Task,
    all_tasks: &[crate::tasks::Task],
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
        if let Some(existing_owner) = crate::tasks::find_pr_owner_in_tasks(&pr_num, all_tasks) {
            info!(
                "Task !{} references PR #{} - assigning to existing owner {}",
                task.id, pr_num, existing_owner
            );
            return Some(existing_owner);
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
    if let Some(owner) = crate::tasks::find_owner_via_blocked_by(task, all_tasks) {
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
    // Track NEW spawns queued (for dev limit enforcement). Nudges to already-running
    // coworkers (grouped tasks) don't count — only fresh spawns.
    let mut spawns_queued_this_tick: usize = 0;
    // Dev cap = max_coworkers (REVIEW_HEADROOM does NOT reduce dev slots).
    let dev_cap = state.max_coworkers;
    // Use running coworkers from snapshot (excludes lead and channel leads).
    let channel_lead_names = snap.channel_lead_names();
    let current_coworker_count = snap
        .coworkers
        .running_coworkers
        .iter()
        .filter(|cw| is_non_lead_coworker(&cw.name, &snap.project_name, &channel_lead_names))
        .count();

    for task in snap.pending_tasks_without_owners.iter() {
        // Re-check dev limit after each spawn decision, accounting for spawns queued this tick.
        let effective_count = current_coworker_count + spawns_queued_this_tick;
        if effective_count >= dev_cap {
            debug!(
                "Dev coworkers limit reached ({}+{} >= {}), deferring unowned task !{}",
                current_coworker_count, spawns_queued_this_tick, dev_cap, task.id
            );
            break;
        }

        // Skip tasks already claimed by orphan recovery in this tick.
        if excluded_task_ids.contains(&task.id) {
            debug!(
                "Task !{} already claimed by orphan recovery this tick, skipping pending dispatch",
                task.id
            );
            continue;
        }

        // Skip tasks that already have an in-flight AssignAndSpawn effect.
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
                let plan_section = build_plan_prompt_section(&task.id, snap);
                let prompt =
                    crate::agents::coworker_recovery_prompt(&task.id, &task.subject, &plan_section);
                let wt = prepare_task_worktree(&task.id, &task.subject, &snap.dir_key, snap);
                let coworker_name = record.preferred_name.clone().unwrap_or_default();

                if let Some(bound_coworker) =
                    snap.worktree_collision(&wt.worktree_id, &coworker_name)
                {
                    debug!(
                        "Pending task !{}: skipping resume spawn for {} because worktree {} is bound to active coworker {}",
                        task.id, coworker_name, wt.worktree_id, bound_coworker
                    );
                    continue;
                }

                // Staleness is pre-evaluated in WorldSnapshot::stale_working_dir_sessions.
                let working_dir = if !record.working_dir.is_empty()
                    && !snap.stale_working_dir_sessions.contains(&record.session_id)
                {
                    std::path::PathBuf::from(&record.working_dir)
                } else if !record.working_dir.is_empty() {
                    warn!(
                        "Session {}: recorded working_dir {:?} no longer exists; \
                         falling back to fresh worktree for task !{}",
                        record.session_id, record.working_dir, task.id
                    );
                    effects.push(effects::Effect::ClearSessionWorkingDir {
                        session_id: record.session_id.clone(),
                    });
                    wt.path.clone()
                } else {
                    wt.path.clone()
                };
                let mut config = crate::launch::LaunchConfig::coworker(
                    coworker_name,
                    snap.dir_key.clone(),
                    crate::launch::SessionMode::ResumeSession(record.session_id.clone()),
                    Some(prompt),
                    Some(task.id.clone()),
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
            resolve_grouped_name(task, &snap.all_tasks, &pr_coworker_map, &task_coworker_map)
        };

        // Use grouped name if found, otherwise allocate a fresh coworker.
        let was_grouped = grouped_name.is_some();
        let coworker_name = if let Some(name) = grouped_name {
            name
        } else {
            let mut excluded_names = snap.channel_lead_names();
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
            let Some(name) = state
                .coworkers
                .next_available_name_excluding(&excluded_names)
            else {
                debug!("No available coworker slots for unowned task !{}", task.id);
                break;
            };
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
                &config.role,
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

            let on_success = vec![
                effects::Effect::RegisterWorktreeAssignment {
                    assignment: crate::worktree_registry::WorktreeAssignment {
                        worktree_id: worktree_id.clone(),
                        branch_name: worktree_id.clone(),
                        task_id: Some(task.id.clone()),
                        current_coworker: None,
                        pr_number: Some(pr_number),
                        created_at: chrono::Utc::now(),
                        completed_at: None,
                    },
                },
                effects::Effect::BindCoworkerToWorktree {
                    worktree_id: worktree_id.clone(),
                    coworker: coworker_name.clone(),
                },
                effects::Effect::BroadcastCoworkerUpdate {
                    name: coworker_name.clone(),
                    status: "running".to_string(),
                    current_task: Some(format!("reviewing PR #{}", pr_number)),
                },
                effects::Effect::post_to_ops(daemon_messages::called_in_reviewer(
                    &coworker_name,
                    pr_number,
                )),
                effects::Effect::PostSystemMessage {
                    message: format!("─── Reviewing PR #{} ───", pr_number),
                    channel: Some(format!("dm-{}", coworker_name)),
                },
                // CreateTaskSessionSpan must come before PostPrComment so the
                // span exists when post_pr_comment() stores the placeholder_comment_id.
                effects::Effect::CreateTaskSessionSpan {
                    task_id: task.id.clone(),
                    agent_name: coworker_name.clone(),
                    agent_type: "reviewer".to_string(),
                    session_id: String::new(),
                    pr_number: Some(pr_number),
                    restart_count: 0,
                },
                effects::Effect::PostPrComment {
                    pr_number,
                    reviewer_name: coworker_name.clone(),
                    body: format!(
                        "<!-- midtown-placeholder -->\n## Review Status\n\n\
                             🔍 Review in progress by {}...\n\n---\n\
                             > [!NOTE]\n> This comment will be updated with the review results when complete.\n\n\
                             🌃 Co-built with [Midtown](https://github.com/btucker/midtown)",
                        coworker_name
                    ),
                },
            ];

            effects.push(effects::Effect::AssignAndSpawn {
                task_id: task.id.clone(),
                owner: coworker_name.clone(),
                dir_key: snap.dir_key.clone(),
                config,
                on_success,
                on_failure: spawn_failure_effects(
                    coworker_name.clone(),
                    task.id.clone(),
                    snap.dir_key.clone(),
                    format!(
                        "⚠️ Spawn failed for review task !{} (reviewer {}) — backing off for {}s",
                        task.id,
                        coworker_name,
                        SPAWN_FAILURE_COOLDOWN.as_secs()
                    ),
                ),
            });
            spawns_queued_this_tick += 1;
        } else {
            // Regular coworker task — assign ownership atomically with spawn
            let wt = prepare_task_worktree(&task.id, &task.subject, state.paths.dir_key(), snap);
            if let Some(bound_coworker) = snap.worktree_collision(&wt.worktree_id, &coworker_name) {
                debug!(
                    "Pending task !{}: skipping fresh spawn for {} because worktree {} is bound to active coworker {}",
                    task.id, coworker_name, wt.worktree_id, bound_coworker
                );
                continue;
            }
            let prompt =
                crate::agents::coworker_task_prompt(&task.id, &task.subject, &plan_section);

            let mut config = crate::launch::LaunchConfig::coworker(
                coworker_name.clone(),
                state.paths.dir_key().to_string(),
                crate::launch::SessionMode::Fresh,
                Some(prompt),
                Some(task.id.clone()),
            );
            config.working_dir = Some(wt.path);
            config.channel = task.channel.clone();
            config.apply_task_model(&snap.task_model_map, &task.id);

            let channel_msg = daemon_messages::called_in_assigned_task(
                &coworker_name,
                &task.id.to_string(),
                &task.subject,
            );

            effects.extend(wt.pre_spawn_effects);

            let mut on_success = vec![
                Effect::BindCoworkerToWorktree {
                    worktree_id: wt.worktree_id,
                    coworker: coworker_name.clone(),
                },
                Effect::BroadcastCoworkerUpdate {
                    name: coworker_name.clone(),
                    status: "running".to_string(),
                    current_task: None,
                },
                Effect::post_to_ops(channel_msg),
            ];
            if let Some(ch) = &task.channel {
                on_success.push(Effect::EmitWorkflowEvent(
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

            effects.push(Effect::AssignAndSpawn {
                task_id: task.id.clone(),
                owner: coworker_name.clone(),
                dir_key: snap.dir_key.clone(),
                config,
                on_success,
                on_failure: spawn_failure_effects(
                    coworker_name.clone(),
                    task.id.clone(),
                    snap.dir_key.clone(),
                    format!(
                        "⚠️ Spawn failed for task !{} (coworker {}) — backing off for {}s",
                        task.id,
                        coworker_name,
                        SPAWN_FAILURE_COOLDOWN.as_secs()
                    ),
                ),
            });
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

    // Compute recently-stopped coworkers (within grace period).
    // This matches the logic in check_and_recover_orphans() to prevent
    // conflicting effects for the same task in the same tick.
    let grace_period = chrono::Duration::seconds(ORPHAN_RECOVERY_GRACE_PERIOD.as_secs() as i64);
    let recently_stopped: HashSet<String> = snap
        .coworkers
        .coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| snap.now_utc.signed_duration_since(**stop_time) < grace_period)
        .map(|(name, _)| name.clone())
        .collect();

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
