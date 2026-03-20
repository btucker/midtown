//! Task dispatch — session-aware in_progress recovery, duplicate detection, pending task spawning.
//!
//! These functions run on the `TaskDispatchTick` event and coordinate coworker
//! lifecycle around the shared task list. They read from `DaemonPersistentState`
//! (with `tick_*` ephemeral fields) and `&[Task]`, returning `Vec<Effect>` for
//! execution by the effect runner.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::daemon::state::DaemonPersistentState;
use crate::daemon_messages;
use crate::task_store::Task;

use super::DaemonState;
use super::constants::*;
use super::effects::{self, Effect};
use super::helpers::is_project_lead;

/// Look up a task by ID in a task slice.
fn task_by_id<'a>(tasks: &'a [Task], id: &str) -> Option<&'a Task> {
    tasks.iter().find(|t| t.id == id)
}

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
// Recently-stopped coworker helper
// ============================================================================

/// Compute the set of coworker names that stopped within the orphan recovery grace period.
fn compute_recently_stopped(ps: &DaemonPersistentState) -> HashSet<String> {
    let grace_period = chrono::Duration::seconds(ORPHAN_RECOVERY_GRACE_PERIOD.as_secs() as i64);
    ps.tick_coworker_stop_times
        .iter()
        .filter(|(_, stop_time)| ps.tick_now.signed_duration_since(**stop_time) < grace_period)
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
    ps: &DaemonPersistentState,
) -> WorktreeSetup {
    let (worktree_id, needs_registration) =
        if let Some(existing) = ps.worktree_registry.get_by_task(task_id) {
            (existing.worktree_id.clone(), false)
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
/// Looks up plan and execution skill from the task directly.
fn build_plan_prompt_section(task_id: &str, tasks: &[Task]) -> String {
    let task = task_by_id(tasks, task_id);
    build_plan_prompt_section_from_parts(
        task_id,
        task.and_then(|t| t.plan.as_deref()),
        task.and_then(|t| t.execution_skill.as_deref()),
    )
}

/// Build plan and execution skill prompt sections from raw values.
///
/// Standalone version of `build_plan_prompt_section` that doesn't require
/// tasks. Used by the `coworker.spawn` RPC handler (which reads
/// plan/skill data directly from persistent state) and by the task-based
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
/// metadata from the persistent state and task list.
fn build_spawn_effects(
    decision: &SpawnDecision,
    ps: &DaemonPersistentState,
    tasks: &[Task],
) -> Vec<effects::Effect> {
    // Look up task from tasks slice
    let task = task_by_id(tasks, &decision.task_id);
    let task_subject = task.map(|t| t.subject.as_str()).unwrap_or("(unknown)");

    let channel = task.and_then(|t| t.channel.clone());

    // Build prompt — includes resume context when session is being resumed
    let plan_section = build_plan_prompt_section(&decision.task_id, tasks);
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
    let wt = prepare_task_worktree(&decision.task_id, task_subject, &ps.tick_dir_key, ps);

    // Check for worktree collision — skip if bound to a different active coworker
    let preferred = decision.preferred_name.as_deref().unwrap_or("");
    if let Some(bound_coworker) = ps.worktree_collision(&wt.worktree_id, preferred) {
        debug!(
            "SpawnDecision for task !{}: skipping because worktree {} is bound to active coworker {}",
            decision.task_id, wt.worktree_id, bound_coworker
        );
        return vec![];
    }

    // Build launch config
    let mut config = crate::launch::LaunchConfig::coworker(
        String::new(), // name allocated at execution time by SpawnForTask
        ps.tick_dir_key.clone(),
        decision.session_mode.clone(),
        Some(prompt),
        Some(decision.task_id.clone()),
    );
    config.working_dir = Some(wt.path);
    config.channel = channel;

    // Apply model from task
    if let Some(model) = task.and_then(|t| t.model.as_deref()) {
        config.model = model.to_string();
    }

    // For session resume, clear stale working_dir if needed
    let mut all_effects = Vec::new();
    if let crate::launch::SessionMode::ResumeSession(ref session_id) = decision.session_mode
        && ps.tick_stale_working_dir_sessions.contains(session_id)
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
        dir_key: ps.tick_dir_key.clone(),
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

/// Build the standard effects for completing a task.
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
pub(crate) fn is_task_pr_protected(
    task: &crate::task_store::Task,
    merged_pr_numbers: &HashSet<u64>,
    pr_task_index: &super::snapshot::PrTaskIndex,
    active_names: &HashSet<String>,
) -> bool {
    if task.status == crate::task_store::TaskStatus::Completed {
        debug!("Skipping recovery for task !{}: already completed", task.id);
        return true;
    }

    if let Some(pr_number) = pr_task_index.session_pr_for_task(&task.id)
        && merged_pr_numbers.contains(&pr_number)
    {
        debug!(
            "Task !{} is in pr_task_index (session) and PR #{} is merged — protected",
            task.id, pr_number
        );
        return true;
    }

    if let Some(pr_number) = task.pr
        && merged_pr_numbers.contains(&pr_number)
    {
        debug!(
            "Skipping recovery for task !{}: explicit PR #{} is in merged cache",
            task.id, pr_number
        );
        return true;
    }

    let owner_is_active =
        !task.agent_name.is_empty() && active_names.contains(&task.agent_name.to_lowercase());
    if !owner_is_active {
        debug!(
            "Task !{} has no active owner session — open-PR protection does not apply",
            task.id
        );
        return false;
    }

    if let Some(pr_number) = pr_task_index.session_pr_for_task(&task.id) {
        debug!(
            "Skipping recovery for task !{}: has open PR via session data (PR #{})",
            task.id, pr_number
        );
        return true;
    }

    if let Some(open_pr) = pr_task_index.github_pr_for_task(&task.id) {
        debug!(
            "Skipping recovery for task !{}: found open PR #{} via GitHub PR title pattern",
            task.id, open_pr
        );
        return true;
    }

    false
}

/// Check for orphaned tasks and recover coworkers.
pub(super) fn check_and_recover_orphans(
    ps: &DaemonPersistentState,
    tasks: &[Task],
    _state: &DaemonState,
) -> Vec<effects::Effect> {
    check_and_recover_orphans_impl(ps, tasks)
}

fn check_and_recover_orphans_impl(
    ps: &DaemonPersistentState,
    tasks: &[Task],
) -> Vec<effects::Effect> {
    if ps.tick_orphan_spawn_cooldown_active {
        debug!("Orphan recovery cooldown active");
        return vec![];
    }

    if ps.tick_in_progress_tasks.is_empty() {
        return vec![];
    }

    let in_progress_tasks_active: Vec<(String, String, String)> = ps
        .tick_in_progress_tasks
        .iter()
        .filter(|(task_id, _task_subject, _owner)| {
            if ps.tick_pr_protected_tasks.contains(task_id) {
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

    let recently_stopped = compute_recently_stopped(ps);

    let channel_lead_names = ps.channel_lead_names();
    let orphan_ctx = crate::rules::OrphanRecoveryContext {
        in_progress: &in_progress_tasks_active,
        active_names: &ps.tick_active_session_names,
        recently_stopped: &recently_stopped,
        attached_coworkers: &ps.tick_attached_coworkers,
        channel_lead_names: &channel_lead_names,
    };
    let recovery = crate::rules::decide_orphan_recovery(&orphan_ctx);

    let Some(recovery) = recovery else {
        return vec![];
    };

    if ps
        .tick_spawn_failure_cooldown_names
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

    let (session_mode, preferred_name) = match ps.find_session_for_task(&recovery.task_id) {
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
    build_spawn_effects(&decision, ps, tasks)
}

/// Session-aware dispatch for in_progress tasks that have session records.
pub(super) fn dispatch_via_sessions(
    ps: &DaemonPersistentState,
    tasks: &[Task],
    _state: &DaemonState,
) -> Vec<effects::Effect> {
    dispatch_via_sessions_inner(ps, tasks)
}

fn dispatch_via_sessions_inner(ps: &DaemonPersistentState, tasks: &[Task]) -> Vec<effects::Effect> {
    if ps.tick_session_dispatch_cooldown_active {
        debug!("Session dispatch cooldown active");
        return vec![];
    }

    if ps.tick_in_progress_tasks.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();

    for (task_id, task_subject, owner) in &ps.tick_in_progress_tasks {
        let action = decide_session_recovery(task_id, task_subject, owner, ps, tasks);

        match action {
            crate::rules::SessionRecoveryAction::Skip(ref reason) => {
                if reason.is_stale_session_ref() {
                    warn!("task !{}: skipping session recovery — {}", task_id, reason);
                } else {
                    debug!("task !{}: skipping session recovery — {}", task_id, reason);
                }
                continue;
            }
            crate::rules::SessionRecoveryAction::FallbackToOrphan { .. } => {
                continue;
            }
            crate::rules::SessionRecoveryAction::Recover {
                ref task_id,
                task_subject: _,
                ref coworker_name,
                ref session_id,
            } => {
                let record = match ps.find_session_for_task(task_id) {
                    Some(r) => r,
                    None => continue,
                };

                info!(
                    "Session dispatch: recovering task !{} via stopped session {} (preferred_name: {})",
                    task_id, session_id, coworker_name
                );

                let decision = SpawnDecision {
                    task_id: task_id.clone(),
                    session_mode: crate::launch::SessionMode::ResumeSession(
                        record.session_id.clone(),
                    ),
                    preferred_name: Some(coworker_name.clone()),
                    cooldown_category: "session_dispatch".to_string(),
                };
                let mut spawn_effects = build_spawn_effects(&decision, ps, tasks);
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

                // Only spawn one coworker per tick
                break;
            }
        }
    }

    effects
}

/// Detect and kill duplicate task workers.
pub fn check_for_duplicate_task_workers(
    ps: &DaemonPersistentState,
    _tasks: &[Task],
) -> Vec<effects::Effect> {
    if ps.tick_in_progress_tasks.is_empty() {
        return vec![];
    }

    let mut task_workers: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, _subject, owner) in &ps.tick_in_progress_tasks {
        if owner.is_empty()
            || is_project_lead(owner, &ps.tick_project_name)
            || ps.channel_lead_sessions.contains_key(&owner.to_lowercase())
        {
            continue;
        }
        task_workers
            .entry(task_id.clone())
            .or_default()
            .push(owner.clone());
    }

    let mut effects = Vec::new();

    for (task_id, workers) in task_workers {
        if workers.len() <= 1 {
            continue;
        }

        let task_subject = ps
            .tick_in_progress_tasks
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

        let mut workers_with_times: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = workers
            .into_iter()
            .map(|name| {
                let start_time = ps
                    .tick_coworker_start_times
                    .get(&name.to_lowercase())
                    .copied();
                (name, start_time)
            })
            .collect();

        workers_with_times.sort_by(|a, b| match (&a.1, &b.1) {
            (Some(t1), Some(t2)) => t1.cmp(t2),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

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
pub(super) struct StaleBranchCleanupData {
    pub stale_branch_cleanup_due: bool,
}

/// Gather data needed for periodic cleanup decisions.
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

/// Build effects for stale branch cleanup.
pub fn decide_stale_branch_cleanup(data: &StaleBranchCleanupData) -> Vec<Effect> {
    let mut effects = Vec::new();
    if data.stale_branch_cleanup_due {
        effects.push(Effect::CleanStaleBranches);
    }
    effects
}

/// Convenience wrapper that calls `spawn_for_pending_tasks_excluding` with no exclusions.
#[allow(dead_code)]
pub(super) fn spawn_for_pending_tasks(
    ps: &DaemonPersistentState,
    tasks: &[Task],
    state: &DaemonState,
) -> Vec<effects::Effect> {
    spawn_for_pending_tasks_excluding(ps, tasks, state, &std::collections::HashSet::new())
}

/// Dispatches pending tasks in two phases:
/// 1. Owned pending tasks — spawn/nudge the assigned coworker if not running
/// 2. Unowned pending tasks — resolve a coworker name, assign ownership, and spawn
pub(super) fn spawn_for_pending_tasks_excluding(
    ps: &DaemonPersistentState,
    tasks: &[Task],
    state: &DaemonState,
    excluded_task_ids: &std::collections::HashSet<String>,
) -> Vec<effects::Effect> {
    if state.draining.load(std::sync::atomic::Ordering::SeqCst) {
        debug!("Daemon is draining, skipping task assignment");
        return Vec::new();
    }

    debug!(
        "Task assignment state: active={}",
        ps.tick_running_coworkers.len()
    );

    let (mut effects, coworkers_dispatched_this_tick) =
        dispatch_owned_pending_tasks(ps, tasks, state);

    effects.extend(dispatch_unowned_pending_tasks(
        ps,
        tasks,
        state,
        excluded_task_ids,
        &coworkers_dispatched_this_tick,
    ));

    effects
}

// ============================================================================
// Owned pending tasks (Case 1)
// ============================================================================

fn dispatch_owned_pending_tasks(
    ps: &DaemonPersistentState,
    tasks: &[Task],
    state: &DaemonState,
) -> (Vec<effects::Effect>, HashSet<String>) {
    let mut effects = Vec::new();
    let mut coworkers_dispatched_this_tick: HashSet<String> = HashSet::new();

    for (task_id, task_subject, owner) in ps.tick_pending_tasks_with_owners.iter() {
        let action = decide_owned_pending_dispatch(task_id, task_subject, owner, ps, tasks);

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
                    dir_key: ps.tick_dir_key.clone(),
                });
                effects.push(Effect::ClearBlockedBy {
                    completed_task_id: task_id.clone(),
                    dir_key: ps.tick_dir_key.clone(),
                });
            }
            crate::rules::PendingTaskAction::NudgeOwner {
                owner: ref o,
                task_id: ref tid,
                task_subject: ref subj,
            } => {
                let task_key = format!("pending-{}", tid);
                let session_id = ps
                    .tick_name_session_map
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
                if coworkers_dispatched_this_tick.contains(&o.to_lowercase()) {
                    debug!(
                        "Already spawned {} this tick — skipping duplicate spawn for task !{}",
                        o, tid
                    );
                    continue;
                }

                if ps
                    .tick_spawn_failure_cooldown_names
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
                effects.extend(build_spawn_effects(&decision, ps, tasks));

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

fn resolve_grouped_name(
    task: &crate::task_store::Task,
    ps: &DaemonPersistentState,
    tasks: &[Task],
    pr_coworker_map: &HashMap<String, String>,
    task_coworker_map: &HashMap<String, String>,
) -> Option<String> {
    if let Some(pr_num) = crate::task_store::extract_pr_number_from_task(task) {
        if let Some(name) = pr_coworker_map.get(&pr_num) {
            info!(
                "Task !{} references PR #{} - assigning to in-memory owner {}",
                task.id, pr_num, name
            );
            return Some(name.clone());
        }
        if let Ok(pr_number_u64) = pr_num.parse::<u64>()
            && let Some(pr_task) = tasks.iter().find(|t| {
                t.pr == Some(pr_number_u64)
                    && (t.status == crate::task_store::TaskStatus::InProgress
                        || t.status == crate::task_store::TaskStatus::Pending)
            })
            && let Some(session) = ps.find_session_for_task(&pr_task.id)
            && !session.name.is_empty()
        {
            let name = &session.name;
            info!(
                "Task !{} references PR #{} - assigning to session owner {}",
                task.id, pr_num, name
            );
            return Some(name.clone());
        }
        let pr_pattern = format!("PR #{}", pr_num);
        for t in tasks.iter().filter(|t| {
            (t.status == crate::task_store::TaskStatus::InProgress
                || t.status == crate::task_store::TaskStatus::Pending)
                && (t.subject.contains(&pr_pattern)
                    || t.description
                        .as_ref()
                        .is_some_and(|d| d.contains(&pr_pattern)))
        }) {
            if let Some(session) = ps.find_session_for_task(&t.id)
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

    for blocked_by_id in &task.blocked_by {
        if let Some(name) = task_coworker_map.get(blocked_by_id) {
            info!(
                "Task !{} blocked by #{} - assigning to same owner {}",
                task.id, blocked_by_id, name
            );
            return Some(name.clone());
        }
    }
    if let Some(owner) = crate::task_store::find_owner_via_blocked_by(task, tasks) {
        info!(
            "Task !{} blocked by owned task - assigning to {}",
            task.id, owner
        );
        return Some(owner);
    }

    None
}

// ── Pending task precondition result ──────────────────────────────────────────

/// Result of evaluating whether a pending unowned task is ready for dispatch.
enum PendingTaskCheck {
    /// Skip — do not dispatch (already excluded, in-flight, lead-driven, etc.)
    Skip,
    /// Auto-complete because the task's PR was merged.
    AutoComplete,
    /// Resume a stopped session rather than spawning fresh.
    ResumeSession {
        session_id: String,
        preferred_name: Option<String>,
    },
    /// Proceed to coworker name resolution and dispatch.
    ReadyToDispatch,
}

/// Stage 1: Evaluate whether a pending unowned task should be dispatched,
/// skipped, auto-completed, or resumed from a stopped session.
///
/// These checks don't consume task slots and run regardless of capacity.
fn check_pending_task_preconditions(
    task: &Task,
    ps: &DaemonPersistentState,
    state: &DaemonState,
    excluded_task_ids: &std::collections::HashSet<String>,
) -> PendingTaskCheck {
    // Skip tasks already claimed by orphan recovery in this tick.
    if excluded_task_ids.contains(&task.id) {
        debug!(
            "Task !{} already claimed by orphan recovery this tick, skipping pending dispatch",
            task.id
        );
        return PendingTaskCheck::Skip;
    }

    // Skip tasks that already have an in-flight spawn effect.
    if state.is_task_spawn_in_flight(&task.id) {
        debug!(
            "Task !{} already has in-flight spawn, skipping duplicate",
            task.id
        );
        return PendingTaskCheck::Skip;
    }

    // Skip tasks whose explicit PR field references a merged PR.
    // IMPORTANT: This must run before the lead-driven check so merged-PR
    // auto-complete works regardless of channel mode.
    // We have the full Task struct here, so check task.pr directly (O(1))
    // instead of scanning all_tasks by ID like dispatch_owned_pending_tasks does.
    if task
        .pr
        .filter(|pr| ps.tick_merged_pr_numbers.contains(pr))
        .is_some()
    {
        info!(
            "Auto-completing stale task !{}: PR #{} has been merged",
            task.id,
            task.pr.unwrap()
        );
        return PendingTaskCheck::AutoComplete;
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
        .is_some_and(|ch| ps.lead_driven_channels.contains(ch))
    {
        debug!(
            "Task !{}: skipping unowned pending dispatch — channel is lead-driven",
            task.id
        );
        return PendingTaskCheck::Skip;
    }

    // Session-aware dispatch: if this pending task has a stopped session
    // from a previous attempt, resume it instead of spawning fresh.
    if let Some(record) = ps.find_session_for_task(&task.id) {
        if !record.is_running {
            if ps
                .tick_recently_recovered_session_ids
                .contains(&record.session_id)
            {
                debug!(
                    "Pending task !{} has recently-recovered session {} — skipping (cooldown)",
                    task.id, record.session_id
                );
                return PendingTaskCheck::Skip;
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
            return PendingTaskCheck::ResumeSession {
                session_id: record.session_id.clone(),
                preferred_name,
            };
        }
        // Session is running — task is already being worked on. Skip.
        if record.is_running {
            debug!(
                "Pending task !{} has running session {} — skipping dispatch",
                task.id, record.session_id
            );
            return PendingTaskCheck::Skip;
        }
    }

    PendingTaskCheck::ReadyToDispatch
}

// ── Coworker name selection ──────────────────────────────────────────────────

/// Mutable state tracked across the dispatch loop for same-tick deduplication.
struct DispatchLoopState {
    /// PR# → coworker name assignments made during this loop.
    pr_coworker_map: HashMap<String, String>,
    /// task_id → coworker name assignments made during this loop.
    task_coworker_map: HashMap<String, String>,
    /// Coworker names assigned within this phase to prevent duplicate assignments.
    names_assigned_this_tick: HashSet<String>,
    /// NEW spawns queued (for task limit enforcement). Nudges to already-running
    /// coworkers (grouped tasks) don't count — only fresh spawns.
    spawns_queued_this_tick: usize,
}

/// Select a coworker name for a pending unowned task.
///
/// Tries grouping strategies first (PR, blockedBy), then falls back to fresh
/// name allocation. At task limit, defers the task.
///
/// Returns `Some((name, was_grouped))` or `None` if the task should be deferred.
fn select_coworker_name(
    task: &Task,
    ps: &DaemonPersistentState,
    tasks: &[Task],
    loop_state: &DispatchLoopState,
) -> Option<(String, bool)> {
    let is_reviewer_task = task.agent_type == "midtown-code-reviewer";

    // Step 1: Determine the coworker name by checking grouping strategies.
    // Reviewer tasks skip grouping entirely — they share a PR number with the
    // implementation task, so grouping would route them to the author's session.
    let grouped_name = if is_reviewer_task {
        None
    } else {
        resolve_grouped_name(
            task,
            ps,
            tasks,
            &loop_state.pr_coworker_map,
            &loop_state.task_coworker_map,
        )
    };

    // Use grouped name if found, otherwise allocate a fresh coworker.
    let was_grouped = grouped_name.is_some();
    let in_progress_count = ps.tick_in_progress_tasks.len();
    let task_cap = ps.tick_max_in_progress_tasks;
    let effective_count = in_progress_count + loop_state.spawns_queued_this_tick;
    let at_task_limit = effective_count >= task_cap;

    let name = if let Some(name) = grouped_name {
        name
    } else if at_task_limit {
        debug!(
            "Task limit reached ({}+{} >= {}), deferring task !{}",
            in_progress_count, loop_state.spawns_queued_this_tick, task_cap, task.id
        );
        return None;
    } else {
        let name = allocate_fresh_coworker_name(task, ps, tasks, is_reviewer_task);
        debug!("Task !{}: allocated coworker name {}", task.id, name);
        name
    };

    Some((name, was_grouped))
}

/// Allocate a fresh coworker name, excluding channel leads, active sessions,
/// and (for reviewer tasks) the PR author to prevent self-review.
fn allocate_fresh_coworker_name(
    task: &Task,
    ps: &DaemonPersistentState,
    tasks: &[Task],
    is_reviewer_task: bool,
) -> String {
    let mut excluded_names = ps.channel_lead_names();
    for name in &ps.tick_active_session_names {
        excluded_names.insert(name.clone());
    }
    // For reviewer tasks, exclude the PR author to prevent self-review.
    if is_reviewer_task
        && let Some(parent_id) = task.parent.as_ref()
        && let Some(parent_task) = task_by_id(tasks, parent_id)
        && !parent_task.agent_name.is_empty()
    {
        excluded_names.insert(parent_task.agent_name.to_lowercase());
    }
    if !task.agent_name.is_empty() {
        task.agent_name.clone()
    } else {
        generate_task_session_name(&task.id, &task.subject, &excluded_names)
    }
}

// ── Coworker assignment validation ───────────────────────────────────────────

/// Validate that a coworker can be assigned to a task. Checks spawn failure
/// cooldowns, owned-task dispatch conflicts, duplicate assignments, and
/// busy/reviewer status.
///
/// Returns `true` if the assignment is valid, `false` if it should be skipped.
fn validate_coworker_assignment(
    coworker_name: &str,
    was_grouped: bool,
    task: &Task,
    ps: &DaemonPersistentState,
    loop_state: &DispatchLoopState,
    owned_dispatched: &HashSet<String>,
    name_task_assignments: &HashMap<String, String>,
) -> bool {
    let name_lower = coworker_name.to_lowercase();

    // Check per-coworker spawn failure cooldown (pre-evaluated in snapshot)
    if ps.tick_spawn_failure_cooldown_names.contains(&name_lower) {
        debug!(
            "Task !{}: skipping {} (spawn failure cooldown active)",
            task.id, coworker_name
        );
        return false;
    }

    // Skip if owned-task dispatch already dispatched this coworker.
    if owned_dispatched.contains(&name_lower) {
        debug!(
            "Task !{}: skipping {} (already dispatched by owned pending tasks)",
            task.id, coworker_name
        );
        return false;
    }

    // Skip if this coworker is already assigned to THIS SPECIFIC TASK.
    if name_task_assignments
        .get(&name_lower)
        .is_some_and(|assigned_task_id| assigned_task_id == &task.id)
    {
        debug!(
            "Task !{}: skipping {} (already assigned to this task)",
            task.id, coworker_name
        );
        return false;
    }

    let already_running = ps.tick_active_session_names.contains(&name_lower);
    let is_coworker_reviewer = ps.tick_active_reviewers.contains(&name_lower);
    let is_busy_from_snapshot = ps.tick_busy_coworkers.contains(&name_lower);
    let assigned_this_tick = loop_state.names_assigned_this_tick.contains(&name_lower);

    // Skip running coworkers that are busy or reviewing.
    if already_running
        && (is_coworker_reviewer || assigned_this_tick || (is_busy_from_snapshot && !was_grouped))
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
        return false;
    }

    if !already_running && (assigned_this_tick || is_busy_from_snapshot) {
        debug!(
            "Task !{}: skipping {} (not running, already assigned this tick or busy)",
            task.id, coworker_name
        );
        return false;
    }

    true
}

// ── Effect builders for dispatch branches ────────────────────────────────────

/// Build effects to nudge an already-running coworker to claim a grouped task.
fn build_grouped_nudge_effects(
    task: &Task,
    coworker_name: &str,
    ps: &DaemonPersistentState,
    tasks: &[Task],
) -> Vec<Effect> {
    let channel_msg = daemon_messages::called_in_assigned_task(
        coworker_name,
        &task.id.to_string(),
        &task.subject,
    );
    let session_id = ps
        .tick_name_session_map
        .get(&coworker_name.to_lowercase())
        .cloned()
        .unwrap_or_default();
    let mut assign_callbacks = vec![
        Effect::RecordTaskAssignment {
            coworker: coworker_name.to_string(),
            task_id: task.id.clone(),
        },
        Effect::post_to_ops(channel_msg),
    ];
    if let Some(ref ch) = task.channel {
        assign_callbacks.push(Effect::EmitWorkflowEvent(
            crate::workflow::WorkflowEvent::TaskAssigned {
                channel: ch.clone(),
                task_id: task.id.clone(),
                coworker: coworker_name.to_string(),
                subject: task.subject.clone(),
                description: task.description.clone(),
                thread_id: task.thread_id.clone(),
                message_id: task.message_id.clone(),
            },
        ));
    }
    let plan_section = build_plan_prompt_section(&task.id, tasks);
    vec![Effect::NudgeSessionWithCallbacks {
        session_id,
        reason: super::wake_reason::WakeReason::TaskClaimed {
            task_id: task.id.clone(),
            subject: task.subject.clone(),
            plan_section,
        },
        on_success: assign_callbacks,
    }]
}

/// Build effects to spawn a reviewer on an isolated worktree for a PR review task.
///
/// Returns `None` if the task has no PR number or the worktree is already bound
/// to another active coworker.
fn build_reviewer_spawn_effects(
    task: &Task,
    coworker_name: &str,
    ps: &DaemonPersistentState,
) -> Option<Vec<Effect>> {
    let pr_number = task.pr.unwrap_or(0);
    if pr_number == 0 {
        warn!("Reviewer task !{} has no PR number, skipping", task.id);
        return None;
    }

    let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);
    let wt_path = crate::paths::worktrees_dir_for_repo(&ps.tick_dir_key).join(&worktree_id);

    if let Some(bound_coworker) = ps.worktree_collision(&worktree_id, coworker_name) {
        debug!(
            "Review task !{}: skipping {} because worktree {} is bound to active coworker {}",
            task.id, coworker_name, worktree_id, bound_coworker
        );
        return None;
    }

    let auth_provider = crate::config::get_execution_provider_for_role(
        &ps.tick_dir_key,
        crate::config::ExecutionRole::Reviewer,
    );
    let mut config = crate::launch::LaunchConfig::reviewer(
        coworker_name.to_string(),
        &ps.tick_dir_key,
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
    let channel_lead_names = ps.channel_lead_names();
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

    let mut effects = vec![effects::Effect::EnsureWorktree {
        worktree_id: worktree_id.clone(),
        path: wt_path.clone(),
    }];

    effects.push(effects::Effect::SpawnForTask {
        task_id: task.id.clone(),
        dir_key: ps.tick_dir_key.clone(),
        preferred_name: Some(coworker_name.to_string()),
        config: Box::new(config),
        worktree_id: worktree_id.clone(),
        success_message: daemon_messages::called_in_reviewer(coworker_name, pr_number),
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

    Some(effects)
}

// ── Main orchestrator ────────────────────────────────────────────────────────

/// Handle pending tasks that have no owner. Resolves a coworker name (via PR/blockedBy
/// grouping or fresh allocation), assigns ownership atomically, and spawns.
///
/// `owned_dispatched`: Coworker names already dispatched by `dispatch_owned_pending_tasks`,
/// used to prevent the same coworker from being targeted by both phases in a single tick.
fn dispatch_unowned_pending_tasks(
    ps: &DaemonPersistentState,
    tasks: &[Task],
    state: &DaemonState,
    excluded_task_ids: &std::collections::HashSet<String>,
    owned_dispatched: &HashSet<String>,
) -> Vec<effects::Effect> {
    let mut effects = Vec::new();

    // Log PR review priority state for diagnostics, but never block task dispatch.
    let active_review_count = ps.tick_active_reviewers.len();
    let prs_with_reviewers = ps
        .tick_reviewer_pr_assignments
        .values()
        .collect::<HashSet<_>>()
        .len();
    let unserved_prs = ps
        .tick_prs_needing_review
        .saturating_sub(prs_with_reviewers);
    if unserved_prs > 0 {
        debug!(
            "PR review state: {} unserved PR(s) need review ({} total, {} already have reviewers), {} active reviewers — task dispatch proceeds independently",
            unserved_prs, ps.tick_prs_needing_review, prs_with_reviewers, active_review_count
        );
    }

    let mut loop_state = DispatchLoopState {
        pr_coworker_map: HashMap::new(),
        task_coworker_map: HashMap::new(),
        names_assigned_this_tick: HashSet::new(),
        spawns_queued_this_tick: 0,
    };
    let pending_tasks_without_owners =
        crate::task_store::filter_pending_tasks_without_owners(tasks, 45);

    let in_progress_ids: std::collections::HashSet<String> = ps
        .tick_in_progress_tasks
        .iter()
        .map(|(id, _, _)| id.clone())
        .collect();

    let task_parent_map: HashMap<String, String> = tasks
        .iter()
        .filter_map(|t| t.parent.as_ref().map(|p| (t.id.clone(), p.clone())))
        .collect();

    let prioritized_ids = crate::daemon::dispatch_priority::prioritize_pending_tasks(
        &pending_tasks_without_owners,
        &in_progress_ids,
        &task_parent_map,
        &ps.tick_blocks_map,
    );

    let name_task_assignments = ps.name_task_assignments();

    for task_id in prioritized_ids.iter() {
        let Some(task) = pending_tasks_without_owners
            .iter()
            .find(|t| &t.id == task_id)
        else {
            continue;
        };

        // Stage 1: Precondition checks (skip, auto-complete, or resume).
        match check_pending_task_preconditions(task, ps, state, excluded_task_ids) {
            PendingTaskCheck::Skip => continue,
            PendingTaskCheck::AutoComplete => {
                effects.push(Effect::CompleteTask {
                    task_id: task.id.clone(),
                    dir_key: ps.tick_dir_key.clone(),
                });
                effects.push(Effect::ClearBlockedBy {
                    completed_task_id: task.id.clone(),
                    dir_key: ps.tick_dir_key.clone(),
                });
                continue;
            }
            PendingTaskCheck::ResumeSession {
                session_id,
                preferred_name,
            } => {
                let decision = SpawnDecision {
                    task_id: task.id.clone(),
                    session_mode: crate::launch::SessionMode::ResumeSession(session_id),
                    preferred_name,
                    cooldown_category: "session_dispatch".to_string(),
                };
                effects.extend(build_spawn_effects(&decision, ps, tasks));
                loop_state.spawns_queued_this_tick += 1;
                continue;
            }
            PendingTaskCheck::ReadyToDispatch => {}
        }

        // Stage 2: Coworker name resolution.
        let Some((coworker_name, was_grouped)) = select_coworker_name(task, ps, tasks, &loop_state)
        else {
            continue;
        };

        // Stage 3: Validate the coworker assignment.
        if !validate_coworker_assignment(
            &coworker_name,
            was_grouped,
            task,
            ps,
            &loop_state,
            owned_dispatched,
            &name_task_assignments,
        ) {
            continue;
        }

        let is_reviewer_task = task.agent_type == "midtown-code-reviewer";

        info!(
            "Proposing task !{} for {} (already_running={})",
            task.id,
            coworker_name,
            ps.tick_active_session_names
                .contains(&coworker_name.to_lowercase())
        );

        // Record this assignment in loop state for same-tick grouping.
        loop_state
            .task_coworker_map
            .insert(task.id.clone(), coworker_name.clone());
        if let Some(pr_num) = crate::task_store::extract_pr_number_from_task(task) {
            loop_state
                .pr_coworker_map
                .insert(pr_num, coworker_name.clone());
        }
        loop_state
            .names_assigned_this_tick
            .insert(coworker_name.to_lowercase());

        // Stage 4: Build dispatch effects based on task type and coworker state.
        let already_running = ps
            .tick_active_session_names
            .contains(&coworker_name.to_lowercase());

        if already_running {
            // Grouped task → nudge the already-running coworker to claim it.
            debug_assert!(
                !is_reviewer_task,
                "reviewer task !{} reached already_running path",
                task.id
            );
            effects.extend(build_grouped_nudge_effects(task, &coworker_name, ps, tasks));
        } else if is_reviewer_task {
            // Reviewer task → spawn on isolated review worktree.
            if let Some(reviewer_effects) = build_reviewer_spawn_effects(task, &coworker_name, ps) {
                effects.extend(reviewer_effects);
                loop_state.spawns_queued_this_tick += 1;
            }
        } else {
            // Regular coworker task → fresh spawn via SpawnDecision.
            let decision = SpawnDecision {
                task_id: task.id.clone(),
                session_mode: crate::launch::SessionMode::Fresh,
                preferred_name: Some(coworker_name.clone()),
                cooldown_category: "task_dispatch".to_string(),
            };
            effects.extend(build_spawn_effects(&decision, ps, tasks));
            loop_state.spawns_queued_this_tick += 1;
        }
    }

    effects
}

// ============================================================================
// Task completion for PR merged
// ============================================================================

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
    let Some(task_id) = crate::task_store::extract_task_id_from_pr_title(pr_title) else {
        return vec![];
    };

    let mut ctx = ctx.unwrap_or_default();

    let push_url = channel.as_ref().map(|ch| {
        build_push_deep_link(
            project_name,
            ch,
            ctx.message_id.as_deref(),
            ctx.thread_id.as_deref(),
        )
    });

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
pub fn build_subject_based_completion_effects(
    ps: &DaemonPersistentState,
    tasks: &[Task],
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for task in tasks {
        if task.status != crate::task_store::TaskStatus::InProgress {
            continue;
        }

        let task_channel = task.channel.clone();

        let task_msg_id = task.message_id.as_deref();

        if let Some(pr_number) = task.pr {
            if ps.tick_merged_pr_numbers.contains(&pr_number) {
                let push_url = task_channel
                    .as_ref()
                    .map(|ch| build_push_deep_link(&ps.tick_project_name, ch, task_msg_id, None));
                let thread_id = task.thread_id.clone();
                let message_id = task.message_id.clone();
                effects.extend(task_completed_effects(
                    &task.id,
                    &ps.tick_dir_key,
                    &task.subject,
                    format!(
                        "✅ Auto-completed task !{} (PR #{} merged)",
                        task.id, pr_number
                    ),
                    task_channel,
                    if task.agent_name.is_empty() {
                        None
                    } else {
                        Some(task.agent_name.clone())
                    },
                    TaskEventContext {
                        subject: None,
                        description: task.description.clone(),
                        thread_id,
                        message_id,
                    },
                    push_url,
                ));
            }
        } else {
            let pr_numbers = crate::task_store::extract_pr_numbers_from_text(&task.subject);

            if pr_numbers.is_empty() {
                continue;
            }

            let all_merged = pr_numbers
                .iter()
                .all(|pr_num| ps.tick_merged_pr_numbers.contains(pr_num));

            if all_merged {
                let pr_list = pr_numbers
                    .iter()
                    .map(|n| format!("#{}", n))
                    .collect::<Vec<_>>()
                    .join(", ");
                let push_url = task_channel
                    .as_ref()
                    .map(|ch| build_push_deep_link(&ps.tick_project_name, ch, task_msg_id, None));
                let thread_id = task.thread_id.clone();
                let message_id = task.message_id.clone();
                effects.extend(task_completed_effects(
                    &task.id,
                    &ps.tick_dir_key,
                    &task.subject,
                    format!(
                        "✅ Auto-completed task !{} (all referenced PRs merged: {})",
                        task.id, pr_list
                    ),
                    task_channel,
                    if task.agent_name.is_empty() {
                        None
                    } else {
                        Some(task.agent_name.clone())
                    },
                    TaskEventContext {
                        subject: None,
                        description: task.description.clone(),
                        thread_id,
                        message_id,
                    },
                    push_url,
                ));
            }
        }
    }

    effects
}

// ============================================================================
// Test helpers
// ============================================================================

#[doc(hidden)]
pub fn should_recover_task_test_helper(
    task: &crate::task_store::Task,
    merged_pr_numbers: &HashSet<u64>,
    _repo_path: &std::path::Path,
    tasks_with_open_prs: &HashMap<String, u64>,
    github_open_pr_task_ids: &HashMap<String, u64>,
) -> bool {
    let mut active_names = HashSet::new();
    if !task.agent_name.is_empty() {
        active_names.insert(task.agent_name.to_lowercase());
    }
    let pr_task_index = super::snapshot::PrTaskIndex::from_task_maps(
        tasks_with_open_prs.clone(),
        github_open_pr_task_ids.clone(),
    );
    !is_task_pr_protected(task, merged_pr_numbers, &pr_task_index, &active_names)
}

// ============================================================================
// Task reset for orphaned tasks
// ============================================================================

/// Reset tasks that are orphaned — either ownerless or their owner went on break.
pub fn reset_orphaned_tasks(ps: &DaemonPersistentState, _tasks: &[Task]) -> Vec<Effect> {
    let mut effects = vec![];

    let recently_stopped = compute_recently_stopped(ps);

    for (task_id, subject, owner) in &ps.tick_in_progress_tasks {
        let owner_clean = owner.trim().trim_matches('"').to_lowercase();

        if ps.tick_pr_task_index.task_has_pr(task_id) {
            continue;
        }

        if let Some(pr_num_str) = crate::task_store::extract_pr_number(subject)
            && let Ok(pr_num) = pr_num_str.parse::<u64>()
        {
            let pr_is_open = ps
                .tick_open_prs
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

        if owner_clean.is_empty() {
            debug!(
                "Task !{} is in_progress with no owner — resetting to pending",
                task_id
            );
            effects.push(Effect::ResetTaskToPending {
                task_id: task_id.clone(),
                dir_key: ps.tick_dir_key.clone(),
            });
            continue;
        }

        if ps.tick_active_session_names.contains(&owner_clean) {
            continue;
        }

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
            dir_key: ps.tick_dir_key.clone(),
        });
    }

    effects
}

// ============================================================================
// Session recovery decision (local to dispatch)
// ============================================================================

fn decide_session_recovery(
    task_id: &str,
    task_subject: &str,
    owner: &str,
    ps: &DaemonPersistentState,
    tasks: &[Task],
) -> crate::rules::SessionRecoveryAction {
    if owner.is_empty()
        || is_project_lead(owner, &ps.tick_project_name)
        || ps.channel_lead_sessions.contains_key(&owner.to_lowercase())
    {
        return crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::LeadOrChannelLead,
        );
    }

    let task_channel = task_by_id(tasks, task_id).and_then(|t| t.channel.as_deref());
    if task_channel.is_some_and(|ch| ps.lead_driven_channels.contains(ch)) {
        return crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::LeadDrivenChannel,
        );
    }

    let record = match ps.find_session_for_task(task_id) {
        Some(r) => r,
        None => {
            if ps.tick_session_task_map.contains_key(task_id) {
                return crate::rules::SessionRecoveryAction::Skip(
                    crate::rules::SessionRecoverySkipReason::StaleSessionRef,
                );
            }
            return crate::rules::SessionRecoveryAction::FallbackToOrphan {
                task_id: task_id.to_string(),
                task_subject: task_subject.to_string(),
                owner: owner.to_string(),
            };
        }
    };

    if record.is_running || ps.tick_active_session_ids.contains(&record.session_id) {
        return crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::SessionRunning,
        );
    }

    if ps
        .tick_recently_recovered_session_ids
        .contains(&record.session_id)
    {
        return crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::RecentlyRecovered,
        );
    }

    let coworker_name = if !record.name.is_empty() {
        record.name.clone()
    } else {
        owner.to_string()
    };

    if ps
        .tick_active_reviewers
        .contains(&coworker_name.to_lowercase())
    {
        return crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::ActiveReviewer,
        );
    }

    if ps
        .tick_spawn_failure_cooldown_names
        .contains(&coworker_name.to_lowercase())
    {
        return crate::rules::SessionRecoveryAction::Skip(
            crate::rules::SessionRecoverySkipReason::SpawnFailureCooldown,
        );
    }

    crate::rules::SessionRecoveryAction::Recover {
        task_id: task_id.to_string(),
        task_subject: task_subject.to_string(),
        coworker_name,
        session_id: record.session_id.clone(),
    }
}

fn decide_owned_pending_dispatch(
    task_id: &str,
    task_subject: &str,
    owner: &str,
    ps: &DaemonPersistentState,
    tasks: &[Task],
) -> crate::rules::PendingTaskAction {
    if let Some(pr_num) =
        crate::daemon::helpers::get_merged_task_pr(task_id, tasks, &ps.tick_merged_pr_numbers)
    {
        return crate::rules::PendingTaskAction::AutoComplete {
            task_id: task_id.to_string(),
            pr_num,
        };
    }

    let task_channel = task_by_id(tasks, task_id).and_then(|t| t.channel.as_deref());
    if task_channel.is_some_and(|ch| ps.lead_driven_channels.contains(ch)) {
        return crate::rules::PendingTaskAction::Skip(
            crate::rules::OwnedPendingSkipReason::LeadDrivenChannel,
        );
    }

    if ps.tick_in_flight_task_spawns.contains(task_id) {
        return crate::rules::PendingTaskAction::Skip(
            crate::rules::OwnedPendingSkipReason::InFlightSpawn,
        );
    }

    let name_task_assignments = ps.name_task_assignments();
    if name_task_assignments
        .get(&owner.to_lowercase())
        .is_some_and(|assigned_task_id| assigned_task_id == task_id)
    {
        return crate::rules::PendingTaskAction::Skip(
            crate::rules::OwnedPendingSkipReason::AlreadyAssigned,
        );
    }

    let on_nudge_cooldown = ps.tick_task_nudge_cooldown_ids.contains(task_id);
    let is_owner_reviewer = ps.tick_active_reviewers.contains(&owner.to_lowercase());
    let has_in_progress_task = ps.tick_busy_coworkers.contains(&owner.to_lowercase());
    let is_channel_lead = ps.channel_lead_sessions.contains_key(&owner.to_lowercase());

    crate::rules::decide_pending_task_action(
        task_id,
        task_subject,
        owner,
        &ps.tick_active_session_names,
        ps.tick_is_at_task_limit,
        on_nudge_cooldown,
        is_owner_reviewer,
        has_in_progress_task,
        is_channel_lead,
    )
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
