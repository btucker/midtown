//! Per-tick preparation — populates ephemeral fields on DaemonPersistentState.
//!
//! Called once per tick before `evaluate_tick()`. Replaces the snapshot
//! collection from `collect_world_snapshot()` for data that comes from
//! ephemeral DaemonState caches rather than persistent state or Task files.

use std::collections::{HashMap, HashSet};

use chrono::Utc;

use super::DaemonState;
use crate::task_store::Task;

/// Populate tick-scoped ephemeral fields on `DaemonPersistentState` and
/// return the current task list from `TaskStore`.
///
/// This is the replacement for `collect_world_snapshot()`. Decision functions
/// read tick data from `DaemonPersistentState`'s `tick_*` fields instead of
/// `WorldSnapshot` fields.
pub(crate) async fn prepare_tick(state: &DaemonState) -> Vec<Task> {
    // Load tasks once from TaskStore
    let tasks = state.task_store.load_all();

    // ── Coworker state ──────────────────────────────────────────────────
    let active_coworkers = state.coworkers.list();
    let running_coworkers = state.coworkers.list_running();

    let coworker_start_times: HashMap<String, chrono::DateTime<Utc>> = active_coworkers
        .iter()
        .map(|cw| (cw.name.to_lowercase(), cw.started_at))
        .collect();

    let coworker_stop_times: HashMap<String, chrono::DateTime<Utc>> = {
        let stop_times = state.coworker_stop_times.read().unwrap();
        stop_times.clone()
    };

    // ── Active session names ────────────────────────────────────────────
    // Include running coworkers and alive headless sessions
    let mut active_names: HashSet<String> = running_coworkers
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();

    let headless_active_names = state.session_manager.list_names().await;
    for name in headless_active_names {
        if state.session_manager.is_alive(&name).await {
            active_names.insert(name.to_lowercase());
        }
    }

    // ── Active session IDs ──────────────────────────────────────────────
    let mut active_session_ids: HashSet<String> = active_coworkers
        .iter()
        .filter(|cw| active_names.contains(&cw.name.to_lowercase()))
        .filter_map(|cw| cw.session_id.clone())
        .collect();
    for name in &active_names {
        if let Some(sid) = state.session_manager.get_session_id(name).await {
            active_session_ids.insert(sid);
        }
    }

    // ── Process health (headless coworkers) ────────────────────────────
    let headless_process_health: HashMap<String, super::snapshot::ProcessHealth> = {
        let health = state.headless_health.read().unwrap();
        health.clone()
    };

    // ── Attached coworkers ──────────────────────────────────────────────
    let attached_coworkers: HashMap<String, chrono::DateTime<Utc>> = {
        let attached = state.attached_coworkers.lock().unwrap();
        attached.clone()
    };

    // ── PR / GitHub state ───────────────────────────────────────────────
    let (merged_pr_numbers, _merged_prs_data) = super::pr::fetch_merged_pr_data(state);
    let (prs_needing_review, open_prs_data) = {
        let cache = state.pr_poll_data.read().unwrap();
        (cache.prs_needing_review, cache.open_prs_data.clone())
    };

    // Derive task→PR mapping from open_prs_data PR titles for orphan recovery.
    let github_open_pr_task_ids: HashMap<String, u64> = open_prs_data
        .iter()
        .filter_map(|pr| {
            let number = pr.get("number")?.as_u64()?;
            let title = pr.get("title")?.as_str()?;
            let task_id = crate::task_store::extract_task_id_from_pr_title(title)?;
            Some((task_id.to_string(), number))
        })
        .collect();

    // ── Reviewer escalations and orphaned PR nudges ────────────────────
    let reviewer_escalations_posted: HashSet<u64> = {
        let posted = state.reviewer_escalations_posted.lock().unwrap();
        posted.clone()
    };

    let orphaned_pr_nudges_sent: HashSet<u64> = {
        let sent = state.orphaned_pr_lead_nudges_sent.lock().unwrap();
        sent.clone()
    };

    // ── Channel state ──────────────────────────────────────────────────
    let base_dir = state.paths.base_dir().to_path_buf();
    let archived_channels: HashSet<String> = crate::channel::Channel::list_archived(&base_dir)
        .unwrap_or_default()
        .into_iter()
        .collect();

    // ── Fork / topic sessions (derived from SessionRecord) ──────────────
    // Build topic_sessions from SessionRecord.bound_thread_id for fork sessions.
    let topic_sessions: HashMap<String, String> = {
        let ps_guard = state.persistent_state.lock().await;
        ps_guard
            .sessions
            .values()
            .filter(|s| s.is_fork_session() && s.is_running)
            .filter_map(|s| {
                s.bound_thread_id
                    .as_ref()
                    .map(|tid| (tid.clone(), s.session_id.clone()))
            })
            .collect()
    };

    // ── Session profile map ────────────────────────────────────────────
    let session_profile_map: HashMap<String, String> = {
        let map = state.session_profile_map.lock().unwrap();
        map.clone()
    };

    // ── In-flight task spawns ──────────────────────────────────────────
    let in_flight_task_spawns: HashSet<String> =
        state.in_flight_task_spawns.lock().unwrap().clone();

    // ── Config values ──────────────────────────────────────────────────
    let dir_key = state.paths.dir_key().to_string();
    let project_name = state.project_name.clone();
    let default_channel = state.channel_router.default_channel_name().to_string();
    let default_branch = state.default_branch.clone();
    let repo_owner = state.repo_owner.clone();
    let max_in_progress_tasks = state.max_in_progress_tasks;

    // Lead session refresh interval: env var -> config.toml -> default
    let lead_refresh_interval_secs = {
        let cfg = crate::config::get_project_daemon_config(state.paths.dir_key());
        std::env::var("MIDTOWN_LEAD_SESSION_REFRESH_INTERVAL")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(cfg.lead_session_refresh_interval_secs)
            .unwrap_or(super::constants::DEFAULT_LEAD_SESSION_REFRESH_INTERVAL_SECS)
    };

    let session_name = format!("midtown-{}", project_name);
    let now = Utc::now();

    // ── Stale channel lead worktrees ────────────────────────────────────
    // Read channel_lead_sessions and sessions from persistent state for the
    // staleness check. This needs its own scope to avoid holding the lock
    // across the async worktree freshness check.
    let (channel_lead_sessions_clone, sessions_clone) = {
        let ps = state.persistent_state.lock().await;
        (ps.channel_lead_sessions.clone(), ps.sessions.clone())
    };
    let stale_lead_worktrees = super::snapshot::collect_stale_channel_lead_worktrees(
        state,
        &channel_lead_sessions_clone,
        &sessions_clone,
    )
    .await;

    // ── Cooldowns ──────────────────────────────────────────────────────
    let (
        orphan_spawn_cooldown_active,
        session_dispatch_cooldown_active,
        spawn_failure_cooldown_names,
        merge_rebase_nudge_cooldown_names,
        rebase_nudge_processed_prs,
        rebase_regression_cooldown_names,
        note_staleness_cooldown_channels,
        lead_worktree_freshness_cooldown_channels,
        recently_recovered_session_ids,
        task_nudge_cooldown_ids,
    ) = {
        let cooldowns = state.cooldowns.lock().unwrap();
        let ps_locked = state.persistent_state.try_lock();

        let orphan_active = !cooldowns.check(
            "orphan_spawn",
            "global",
            super::constants::ORPHAN_SPAWN_COOLDOWN,
        );
        let session_active = !cooldowns.check(
            "session_dispatch",
            "global",
            super::constants::SESSION_DISPATCH_COOLDOWN,
        );

        // Spawn failure cooldown: check active names
        let spawn_failure: HashSet<String> = active_names
            .iter()
            .filter(|name| {
                !cooldowns.check(
                    "spawn_failure",
                    name,
                    super::constants::SPAWN_FAILURE_COOLDOWN,
                )
            })
            .cloned()
            .collect();

        // Merge-rebase nudge cooldowns
        let merge_rebase: HashSet<String> = active_names
            .iter()
            .filter(|name| {
                !cooldowns.check(
                    "merge_rebase_nudge",
                    name,
                    super::constants::MERGE_REBASE_NUDGE_COOLDOWN,
                )
            })
            .cloned()
            .collect();

        // Rebase nudge processed PRs
        let rebase_processed: HashSet<u64> = merged_pr_numbers
            .iter()
            .filter(|pr_num| {
                !cooldowns.check(
                    "merge_rebase_pr_processed",
                    &pr_num.to_string(),
                    super::constants::MERGE_REBASE_PR_PROCESSED_COOLDOWN,
                )
            })
            .copied()
            .collect();

        // Rebase regression cooldowns
        let rebase_regression: HashSet<String> = active_names
            .iter()
            .filter(|name| {
                !cooldowns.check(
                    "rebase_regression",
                    name,
                    super::constants::REBASE_REGRESSION_COOLDOWN,
                )
            })
            .cloned()
            .collect();

        // Note staleness cooldowns — check against channel lead sessions
        let note_staleness: HashSet<String> = channel_lead_sessions_clone
            .keys()
            .filter(|ch| {
                !cooldowns.check(
                    "note_staleness",
                    ch,
                    std::time::Duration::from_secs(
                        super::constants::NOTE_STALENESS_NUDGE_COOLDOWN_SECS,
                    ),
                )
            })
            .cloned()
            .collect();

        // Lead worktree freshness cooldowns
        let lead_wt_freshness: HashSet<String> = channel_lead_sessions_clone
            .keys()
            .filter(|ch| {
                !cooldowns.check(
                    "lead_worktree_freshness",
                    ch,
                    super::constants::LEAD_WORKTREE_FRESHNESS_COOLDOWN,
                )
            })
            .cloned()
            .collect();

        // Recently recovered session IDs
        let recovered: HashSet<String> = if let Ok(ref ps) = ps_locked {
            ps.sessions
                .keys()
                .filter(|sid| {
                    !cooldowns.check(
                        "session_recovered",
                        sid,
                        super::constants::SESSION_RECOVERED_COOLDOWN,
                    )
                })
                .cloned()
                .collect()
        } else {
            // If we can't get the lock (shouldn't happen since we're the only caller),
            // fall back to checking sessions_clone which we already have.
            sessions_clone
                .keys()
                .filter(|sid| {
                    !cooldowns.check(
                        "session_recovered",
                        sid,
                        super::constants::SESSION_RECOVERED_COOLDOWN,
                    )
                })
                .cloned()
                .collect()
        };

        // Task nudge cooldowns — check pending tasks with owners (session_id set).
        // In the old Task format, "owner" was an explicit field. In TaskStore,
        // session_id serves the same role (set when a coworker claims the task).
        let task_nudge: HashSet<String> = tasks
            .iter()
            .filter(|t| {
                t.status == crate::task_store::TaskStatus::Pending && t.session_id.is_some()
            })
            .filter(|t| {
                let key = format!("pending-{}", t.id);
                !cooldowns.check("task_nudge", &key, super::constants::TASK_NUDGE_COOLDOWN)
            })
            .map(|t| t.id.clone())
            .collect();

        (
            orphan_active,
            session_active,
            spawn_failure,
            merge_rebase,
            rebase_processed,
            rebase_regression,
            note_staleness,
            lead_wt_freshness,
            recovered,
            task_nudge,
        )
    };

    // ── Usage limit nudge state ──────────────────────────────────────
    let (usage_limit_nudge_scheduled, usage_limit_nudge_at) = {
        let nudge_at = state.usage_limit_nudge_at.lock().await;
        (nudge_at.is_some(), *nudge_at)
    };

    // ── Lock persistent state and populate ephemeral fields ────────────
    {
        let mut ps = state.persistent_state.lock().await;

        // Process health
        ps.tick_process_health = headless_process_health;

        // PR poll data
        ps.tick_prs_needing_review = prs_needing_review;
        ps.tick_open_prs = open_prs_data;
        ps.tick_merged_pr_numbers = merged_pr_numbers;

        // Merged PR → branch mapping (from worktree registry)
        ps.tick_merged_pr_branches = ps
            .worktree_registry
            .all_assignments()
            .iter()
            .filter_map(|(_, assignment)| {
                assignment
                    .pr_number
                    .map(|pr| (pr, assignment.branch_name.clone()))
            })
            .collect();

        // Rate limit (from persistent state's own github field)
        ps.tick_rate_limit = ps.github.rate_limit.clone();
        ps.tick_fresh_rate_limit = None; // Set by RateLimitCheckTick handler if needed

        // PR task index
        let session_task_to_pr = super::state::task_to_pr_map_from_sessions(&ps.sessions);
        let pr_to_task = super::state::pr_to_task_map_from_sessions(&ps.sessions);
        ps.tick_pr_task_index = super::snapshot::PrTaskIndex::new(
            session_task_to_pr,
            github_open_pr_task_ids,
            pr_to_task,
        );

        // Cooldowns
        ps.tick_orphan_spawn_cooldown_active = orphan_spawn_cooldown_active;
        ps.tick_session_dispatch_cooldown_active = session_dispatch_cooldown_active;
        ps.tick_spawn_failure_cooldown_names = spawn_failure_cooldown_names;
        ps.tick_merge_rebase_nudge_cooldown_names = merge_rebase_nudge_cooldown_names;
        ps.tick_rebase_nudge_processed_prs = rebase_nudge_processed_prs;
        ps.tick_rebase_regression_cooldown_names = rebase_regression_cooldown_names;
        ps.tick_note_staleness_cooldown_channels = note_staleness_cooldown_channels;
        ps.tick_lead_worktree_freshness_cooldown_channels =
            lead_worktree_freshness_cooldown_channels;
        ps.tick_recently_recovered_session_ids = recently_recovered_session_ids;
        ps.tick_task_nudge_cooldown_ids = task_nudge_cooldown_ids;
        ps.tick_in_flight_task_spawns = in_flight_task_spawns;

        // Coworker times
        ps.tick_coworker_start_times = coworker_start_times;
        ps.tick_coworker_stop_times = coworker_stop_times;

        // Attached coworkers
        ps.tick_attached_coworkers = attached_coworkers;

        // Config
        ps.tick_dir_key = dir_key;
        ps.tick_project_name = project_name;
        ps.tick_default_channel = default_channel;
        ps.tick_default_branch = default_branch;
        ps.tick_repo_owner = repo_owner;
        ps.tick_max_in_progress_tasks = max_in_progress_tasks;
        ps.tick_lead_refresh_interval_secs = lead_refresh_interval_secs;
        ps.tick_now = now;

        // Stale lead worktrees
        ps.tick_stale_lead_worktrees = stale_lead_worktrees;

        // Topic sessions
        ps.tick_topic_sessions = topic_sessions;

        // Session profile map
        ps.tick_session_profile_map = session_profile_map;

        // Limited pool profiles (derived from persistent state's own profile_pool_state)
        ps.tick_limited_pool_profiles = ps
            .profile_pool_state
            .iter()
            .filter(|(_, p)| p.is_usage_limited)
            .map(|(email, _)| email.clone())
            .collect();

        // Reviewer escalations
        ps.tick_reviewer_escalations_posted = reviewer_escalations_posted;

        // Orphaned PR nudges
        ps.tick_orphaned_pr_nudges_sent = orphaned_pr_nudges_sent;

        // Archived channels
        ps.tick_archived_channels = archived_channels;

        // Stale channel notes: populated on-demand only for NoteReviewTick (hourly),
        // matching the pattern in collect_world_snapshot.
        ps.tick_stale_channel_notes = HashMap::new();

        // Channel messages and daemon logs: NOT populated during tick collection
        // (hot path). Only populated on-demand via debug context capture.
        ps.tick_channel_messages = Vec::new();
        ps.tick_daemon_logs = Vec::new();

        // Active session names, IDs, and coworker data
        ps.tick_active_session_names = active_names;
        ps.tick_active_session_ids = active_session_ids;
        ps.tick_active_coworkers = active_coworkers;
        ps.tick_running_coworkers = running_coworkers;
        ps.tick_session_name = session_name;

        // Task limit check
        let active_in_progress_count = tasks
            .iter()
            .filter(|t| {
                t.status == crate::task_store::TaskStatus::InProgress
                    && (t.agent_name.is_empty()
                        || ps
                            .tick_active_session_names
                            .contains(t.agent_name.to_lowercase().as_str()))
            })
            .count();
        ps.tick_is_at_task_limit = active_in_progress_count >= max_in_progress_tasks;

        // Usage limit nudge state
        ps.tick_usage_limit_nudge_scheduled = usage_limit_nudge_scheduled;
        ps.tick_usage_limit_nudge_at = usage_limit_nudge_at;

        // Reviewer PR assignments (from all reviewer sessions)
        ps.tick_reviewer_pr_assignments =
            super::snapshot::build_reviewer_pr_assignments_from_spans(&ps);

        // PR → restart_count for stuck reviewer backoff (from TaskStore).
        ps.tick_reviewer_restart_counts = tasks
            .iter()
            .filter(|t| t.pr.is_some() && t.restart_count > 0)
            .filter_map(|t| t.pr.map(|pr| (pr, t.restart_count)))
            .collect();

        // Placeholder comment IDs for PRs with unupdated "Review in progress" comments.
        ps.tick_reviewer_in_progress_comment_ids = tasks
            .iter()
            .filter_map(|t| {
                let pr = t.pr?;
                let comment_id = t.placeholder_comment_id?;
                Some((pr, comment_id))
            })
            .collect();

        // Name → session ID mapping (lowercase name → session_id)
        ps.tick_name_session_map = ps
            .sessions
            .iter()
            .filter(|(_, r)| !r.name.is_empty())
            .map(|(sid, r)| (r.name.to_lowercase(), sid.clone()))
            .collect();

        // ── Dispatch tick fields ────────────────────────────────────────────

        // Session task map: task_id → session_id
        ps.tick_session_task_map = ps
            .sessions
            .iter()
            .filter(|(sid, _)| !sid.is_empty())
            .filter_map(|(sid, r)| r.task_id.as_ref().map(|tid| (tid.clone(), sid.clone())))
            .collect();

        // Stale working-dir sessions
        ps.tick_stale_working_dir_sessions = ps
            .sessions
            .values()
            .filter(|r| !r.working_dir.is_empty() && !std::path::Path::new(&r.working_dir).exists())
            .map(|r| r.session_id.clone())
            .collect();

        // In-progress tasks
        ps.tick_in_progress_tasks = tasks
            .iter()
            .filter(|t| t.status == crate::task_store::TaskStatus::InProgress)
            .map(|t| (t.id.clone(), t.subject.clone(), t.agent_name.clone()))
            .collect();

        // Pending tasks with owners
        ps.tick_pending_tasks_with_owners = tasks
            .iter()
            .filter(|t| {
                t.status == crate::task_store::TaskStatus::Pending && !t.agent_name.is_empty()
            })
            .map(|t| (t.id.clone(), t.subject.clone(), t.agent_name.clone()))
            .collect();

        // Busy coworkers — inline computation to avoid borrow conflict with MutexGuard
        {
            let in_progress_ids: HashSet<&str> = ps
                .tick_in_progress_tasks
                .iter()
                .map(|(id, _, _)| id.as_str())
                .collect();
            ps.tick_busy_coworkers = ps
                .sessions
                .values()
                .filter(|s| {
                    s.task_id
                        .as_deref()
                        .is_some_and(|tid| in_progress_ids.contains(tid))
                })
                .filter(|s| !s.name.is_empty())
                .map(|s| s.name.to_lowercase())
                .collect();
        }

        // Active reviewers
        ps.tick_active_reviewers = ps
            .sessions
            .values()
            .filter(|s| s.agent_type == "midtown-code-reviewer" && s.is_running)
            .filter(|s| !s.name.is_empty())
            .map(|s| s.name.to_lowercase())
            .collect();

        // PR-protected tasks
        ps.tick_pr_protected_tasks = tasks
            .iter()
            .filter(|t| {
                super::dispatch::is_task_pr_protected(
                    t,
                    &ps.tick_merged_pr_numbers,
                    &ps.tick_pr_task_index,
                    &ps.tick_active_session_names,
                )
            })
            .map(|t| t.id.clone())
            .collect();

        // Blocks map
        let mut blocks_map: HashMap<String, Vec<String>> = HashMap::new();
        for task in &tasks {
            for blocker_id in &task.blocked_by {
                blocks_map
                    .entry(blocker_id.clone())
                    .or_default()
                    .push(task.id.clone());
            }
        }
        ps.tick_blocks_map = blocks_map;

        // Task nudge cooldown IDs — already populated above as task_nudge_cooldown_ids
        // (tick_task_nudge_cooldown_ids was set earlier in the cooldowns block)
    }

    tasks
}
