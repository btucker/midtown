//! Daemon event dispatch — the central coordination point for the state machine.
//!
//! Each event source (timer tick, webhook, RPC, signal) maps to a `DaemonEvent`
//! variant. The `evaluate_tick` function dispatches the event to the appropriate
//! set of check functions, collecting all effects into a single `Vec<Effect>`.
//!
//! ```text
//! Timer/Webhook/RPC → DaemonEvent
//!                   → prepare_tick(state) → Vec<Task>
//!                   → evaluate_tick(event, tasks, state) → Vec<Effect>
//!                   → execute_effects(effects)
//! ```

use std::collections::HashSet;

use super::DaemonState;
use super::effects::Effect;

/// Events that drive the daemon's state machine.
///
/// Currently covers the periodic tick groups. Future phases will add
/// variants for webhook events, RPC requests, and signals — converting
/// the remaining inline side effects to the evaluate/execute pattern.
#[derive(Debug)]
#[allow(clippy::enum_variant_names)] // All tick events share the "Tick" suffix by design
pub enum DaemonEvent {
    /// Periodic session-monitor tick: idle shutdown, stuck detection, usage limits.
    SessionMonitorTick,
    /// Periodic task-dispatch tick: duplicate detection, orphan recovery, task spawning,
    /// zombie respawning, reminders.
    TaskDispatchTick,
    /// Periodic PR poll tick: check open PRs for issues, spawn reviewers.
    ///
    /// Previously ran in a separate `tokio::spawn` task, now integrated into the
    /// main select! loop to prevent spawn races with TaskDispatchTick.
    PrPollTick,
    /// Periodic rate limit check: fetch GitHub API quotas and update state.
    ///
    /// Runs every 2 minutes to monitor GraphQL and REST API usage.
    /// Used by adaptive throttling to reduce PR polling when quotas run low.
    RateLimitCheckTick,
    /// Periodic note review tick: check for stale channel notes and nudge leads.
    ///
    /// Runs every hour. Scans channel note directories for notes whose
    /// `reviewed_at` frontmatter is older than the staleness threshold (3 days)
    /// or missing. Nudges the channel lead to review or delete stale notes.
    NoteReviewTick,
}

/// Evaluate a daemon event against the current world snapshot, returning effects.
///
/// This is the central dispatch point. Each event variant maps to a batch of
/// pure (or near-pure) check functions that read from the snapshot and return
/// effects. The caller executes all returned effects via `execute_effects`.
///
/// Some check functions still take `&DaemonState` for mutable tracker state
/// (coworker_records, cooldowns, etc.) and inline spawns that cannot yet be
/// expressed as pure effects (spawn success/failure determines follow-up effects).
pub async fn evaluate_tick(
    event: &DaemonEvent,
    _tasks: &[crate::task_store::Task],
    state: &DaemonState,
) -> Vec<Effect> {
    match event {
        DaemonEvent::SessionMonitorTick => {
            let mut effects = Vec::new();
            let ps = state.persistent_state.lock().await;
            let tasks = state.task_store.load_all();
            // Health checks: auth errors first (require user intervention),
            // then usage limits and dead process detection.
            effects.extend(super::health::check_and_handle_auth_errors(&ps, state));
            effects.extend(super::health::check_and_restart_dead_reviewers(&ps, &tasks));
            effects.extend(super::health::check_for_usage_limits(&ps));
            effects.extend(super::health::maybe_nudge_usage_limit_expiry(&ps));
            effects.extend(super::health::check_and_nudge_api_errors(&ps, state));
            effects.extend(super::health::check_and_restart_tool_name_conflicts(&ps));
            effects.extend(super::health::maybe_refresh_lead_session(&ps));
            effects.extend(super::health::check_channel_lead_worktree_freshness(&ps));
            effects.extend(super::health::check_and_shutdown_idle_coworkers(&ps));
            drop(ps);
            effects
        }
        DaemonEvent::TaskDispatchTick => {
            let mut effects = Vec::new();
            // Bug #1317 fix: Removed reconcile_tasks_in_review().
            // Previously, when a coworker opened a PR and went idle, this function would
            // unassign the task (clear owner). This broke the ownership chain — if CI failed
            // or review feedback arrived, orphan recovery would spawn a DIFFERENT coworker
            // instead of the original author who has context.
            //
            // New behavior: Task stays assigned to the original coworker even when they're idle.
            // If the PR needs more work, the daemon respawns the SAME coworker who has the full
            // session context for that PR. The coworker going idle already frees the process slot
            // — we don't need to strip task ownership too.
            //
            // effects.extend(super::dispatch::reconcile_tasks_in_review(&ps, &tasks));
            {
                let ps = state.persistent_state.lock().await;
                let tasks = state.task_store.load_all();
                let auto_close_effects = super::dispatch::auto_close_completed_tasks(&ps, &tasks);
                let auto_closed_ids =
                    super::effects::extract_completed_task_ids_from_effects(&auto_close_effects);
                effects.extend(auto_close_effects);
                // Stop running sessions whose task is already completed (e.g., PR merged)
                // or being completed this tick (auto_closed_ids).
                effects.extend(super::dispatch::stop_sessions_for_completed_tasks(
                    &ps,
                    &tasks,
                    &auto_closed_ids,
                ));
                effects.extend(super::dispatch::reset_orphaned_tasks(&ps, &tasks));
                effects.extend(super::dispatch::check_for_duplicate_task_workers(
                    &ps, &tasks,
                ));
                // Session-aware dispatch handles in_progress tasks with session records:
                // resume stopped sessions, skip running ones.
                let session_effects = super::dispatch::dispatch_via_sessions(&ps, &tasks, state);
                let mut claimed_ids =
                    super::effects::extract_claimed_task_ids_from_effects(&session_effects);
                effects.extend(session_effects);
                // Orphan recovery handles all orphaned in-progress tasks (with or without sessions).
                // Exclude tasks already being auto-closed to prevent conflicting effects.
                let orphan_effects = super::dispatch::check_and_recover_orphans(
                    &ps,
                    &tasks,
                    state,
                    &auto_closed_ids,
                );
                let orphan_claimed =
                    super::effects::extract_claimed_task_ids_from_effects(&orphan_effects);
                claimed_ids.extend(orphan_claimed);
                effects.extend(orphan_effects);
                effects.extend(super::dispatch::spawn_for_pending_tasks_excluding(
                    &ps,
                    &tasks,
                    state,
                    &claimed_ids,
                ));
            }
            {
                let ps = state.persistent_state.lock().await;
                let tasks = state.task_store.load_all();
                effects.extend(
                    super::health::check_and_respawn_dead_processes(&ps, &tasks, state).await,
                );
                // Dead forks stay dead — no auto-respawn. Thread replies to dead
                // forks fall through to the channel lead.
                // Auto-detach sessions attached longer than ATTACH_TIMEOUT. Both this function and
                // ensure_lead_alive read the same immutable snapshot, so AutoDetachCoworker only
                // removes the entry from DaemonState when executed in effects.rs. The lead respawn
                // happens on the next tick, once ensure_lead_alive sees the entry is gone.
                effects.extend(super::health::detect_stale_attached_sessions(&ps));
                effects.extend(super::health::ensure_lead_alive(&ps));
                effects.extend(super::health::ensure_channel_leads_alive(&ps));
                effects.extend(super::health::check_and_fire_reminders(&ps, state).await);
            }
            dedup_spawn_effects(effects)
        }
        DaemonEvent::PrPollTick => {
            // PR polling: check open PRs for issues, spawn reviewers, clean up merged worktrees.
            // When GitHub API quota is critically low (< 5%), skip API-calling polls but still
            // run pure cleanup logic (doesn't make API calls, just reads tick state).
            //
            // Two-phase evaluation:
            // 1. Lock persistent_state and run pure decision functions
            // 2. Drop lock, then run async functions (which re-lock internally)
            let mut effects = Vec::new();
            let tasks = state.task_store.load_all();

            // Phase 1: Pure decision functions under a single lock
            let rate_limit_critical = {
                let ps = state.persistent_state.lock().await;

                if ps.tick_rate_limit.is_critical() {
                    tracing::warn!(
                        "Skipping PR poll (GitHub API quota critical: {})",
                        ps.tick_rate_limit.summary()
                    );
                }

                // Always run merged PR cleanup (pure function, no API calls)
                effects.extend(super::pr::collect_merged_pr_cleanup_effects(&ps));

                // Nudge coworkers with open PRs to rebase after a merge
                effects.extend(super::pr::collect_merge_rebase_nudge_effects(&ps));

                // Polling fallback for PR→task auto-link: repair missing SetTaskPr links
                // that webhooks may have missed (no API calls, pure tick state comparison)
                effects.extend(super::pr::collect_pr_task_link_effects(&ps, &tasks));

                // Clean up stale worktrees and daemon state
                {
                    let daemon_config =
                        crate::config::get_project_daemon_config(state.paths.dir_key());
                    let retention_hours =
                        daemon_config.worktree_cleanup_retention_hours.unwrap_or(24);
                    if retention_hours > 0 {
                        let retention_period = chrono::Duration::hours(retention_hours as i64);
                        effects.extend(super::health::check_for_stale_worktrees(
                            &ps.worktree_registry,
                            &ps.tick_active_session_names,
                            retention_period,
                        ));
                        effects.push(super::effects::Effect::CleanupOrphanedWorktrees {
                            retention_hours,
                        });
                    }

                    let gc_retention = chrono::Duration::hours(retention_hours as i64);
                    // Task metadata lives in TaskStore now; GC only removes dead sessions.
                    let task_metadata_keys: std::collections::HashSet<String> =
                        std::collections::HashSet::new();
                    let active_task_ids: std::collections::HashSet<String> =
                        tasks.iter().map(|t| t.id.clone()).collect();
                    effects.extend(super::health::check_for_state_gc(
                        &ps.sessions,
                        &ps.tick_active_session_ids,
                        &task_metadata_keys,
                        &active_task_ids,
                        gc_retention,
                    ));
                }

                // Reconcile orphaned PRs: nudge lead for reviewed + CI green PRs
                effects.extend(super::pr::reconcile_orphaned_prs(&ps, &tasks));

                // Auto-complete tasks whose subjects reference only merged PRs
                effects.extend(super::dispatch::build_subject_based_completion_effects(
                    &ps, &tasks,
                ));

                ps.tick_rate_limit.is_critical()
                // ps lock dropped here
            };

            // Phase 2: Async functions that lock persistent_state internally.
            // poll_prs_for_issues extracts tick state under its own lock.
            if !rate_limit_critical {
                match super::pr::poll_prs_for_issues(state).await {
                    Ok(pr_effects) => effects.extend(pr_effects),
                    Err(e) => {
                        tracing::warn!("PR poll error: {}", e);
                    }
                }
            }

            // Check for post-rebase regressions (reads tick state, spawns blocking git)
            {
                let ps = state.persistent_state.lock().await;
                effects.extend(super::pr::check_for_rebase_regressions(&ps).await);
            }

            dedup_spawn_effects(effects)
        }
        DaemonEvent::RateLimitCheckTick => {
            // Evaluate freshly fetched GitHub API rate limits against previous state.
            let mut effects = Vec::new();
            let ps = state.persistent_state.lock().await;
            if let Some(rate_limit) = &ps.tick_fresh_rate_limit {
                // Check if state changed (low → critical, critical → recovered, etc.)
                let was_critical = ps.tick_rate_limit.is_critical();
                let was_low = ps.tick_rate_limit.is_low();
                let now_critical = rate_limit.is_critical();
                let now_low = rate_limit.is_low();

                // Warn when entering critical state (< 5%)
                if now_critical && !was_critical {
                    effects.push(Effect::system_message_to_ops(format!(
                        "⚠️ GitHub API quota critical ({}). PR polling paused until reset at {}.",
                        rate_limit.summary(),
                        rate_limit.graphql.reset_time().format("%H:%M UTC")
                    )));
                    effects.push(Effect::RecordCooldown {
                        category: "rate_limit_critical".to_string(),
                        key: "throttle_warning".to_string(),
                    });
                }
                // Warn when entering low state (< 20%)
                else if now_low && !was_low && !now_critical {
                    effects.push(Effect::system_message_to_ops(format!(
                        "⚠️ GitHub API quota low ({}). Consider reducing manual gh commands.",
                        rate_limit.summary()
                    )));
                    effects.push(Effect::RecordCooldown {
                        category: "rate_limit_low".to_string(),
                        key: "throttle_warning".to_string(),
                    });
                }
                // Post recovery/improvement messages for state transitions
                else if was_critical && !now_critical {
                    if now_low {
                        // Critical → low (improved but still low)
                        effects.push(Effect::system_message_to_ops(format!(
                            "⬆️ GitHub API quota improved ({}) — PR polling resumed, but quota still low.",
                            rate_limit.summary()
                        )));
                    } else {
                        // Critical → normal (fully recovered)
                        effects.push(Effect::system_message_to_ops(format!(
                            "✅ GitHub API quota recovered ({}). PR polling resumed.",
                            rate_limit.summary()
                        )));
                    }
                } else if was_low && !was_critical && !now_low && !now_critical {
                    // Low → normal (recovered from low state)
                    effects.push(Effect::system_message_to_ops(format!(
                        "✅ GitHub API quota recovered ({}).",
                        rate_limit.summary()
                    )));
                }

                effects.push(Effect::UpdateRateLimit(rate_limit.clone()));
            }
            effects
        }
        DaemonEvent::NoteReviewTick => {
            let ps = state.persistent_state.lock().await;
            super::health::check_for_stale_notes(&ps)
        }
    }
}

/// Deduplicate spawn effects by coworker name and task ID.
///
/// Multiple independent decision functions (orphan recovery, pending task spawn,
/// dead process respawn, PR call-in) can each decide to spawn the same coworker
/// in a single tick. Without deduplication, the first spawn succeeds but subsequent
/// ones trigger `on_success` callbacks (since the idempotent guard returns Ok),
/// posting duplicate "Called in" messages.
///
/// Also acts as defense-in-depth against double-spawn for the same task: orphan recovery
/// might spawn "amsterdam" for task 123, while task dispatch spawns "york" for the same
/// task in the same tick. The primary guard is the exclusion set passed to
/// `spawn_for_pending_tasks_excluding`; this deduplication by task ID is a backstop.
///
/// Handles all spawn-like effect variants: `SpawnCoworker`,
/// `SpawnCoworkerWithCallbacks`, `SpawnForTask`, and `ResumeCoworker`.
/// Keeps the first spawn effect for each coworker name and task ID, drops duplicates.
/// Non-spawn effects are always preserved.
fn dedup_spawn_effects(effects: Vec<Effect>) -> Vec<Effect> {
    let mut seen_coworker_names: HashSet<String> = HashSet::new();
    let mut seen_task_ids: HashSet<String> = HashSet::new();
    let mut result: Vec<Effect> = Vec::new();
    let mut registry_effects: Vec<Effect> = Vec::new();

    for effect in effects {
        let spawn_name = match &effect {
            Effect::SpawnCoworker(config) => Some(config.name.to_lowercase()),
            Effect::SpawnCoworkerWithCallbacks { config, .. } => Some(config.name.to_lowercase()),
            Effect::SpawnForTask {
                preferred_name,
                config,
                ..
            } => Some(
                preferred_name
                    .as_deref()
                    .unwrap_or(&config.name)
                    .to_lowercase(),
            ),
            Effect::ResumeCoworker { name, .. } => Some(name.to_lowercase()),
            _ => None,
        };

        // Extract task ID if this is a task-related spawn
        let task_id = match &effect {
            Effect::SpawnForTask { task_id, .. } => Some(task_id.clone()),
            Effect::SpawnCoworkerWithCallbacks { on_success, .. } => {
                // Look for RecordTaskAssignment in on_success callbacks
                on_success.iter().find_map(|e| {
                    if let Effect::RecordTaskAssignment { task_id, .. } = e {
                        Some(task_id.clone())
                    } else {
                        None
                    }
                })
            }
            _ => None,
        };

        if let Some(name) = spawn_name {
            // Check if we've already seen this coworker name
            let duplicate_by_name = seen_coworker_names.contains(&name);
            // Check if we've already seen this task ID (if task-related)
            let duplicate_by_task = task_id
                .as_ref()
                .is_some_and(|tid| seen_task_ids.contains(tid));

            if duplicate_by_name || duplicate_by_task {
                let reason = if duplicate_by_name && duplicate_by_task {
                    format!(
                        "coworker '{}' and task '{}'",
                        name,
                        task_id.as_ref().unwrap()
                    )
                } else if duplicate_by_name {
                    format!("coworker '{}'", name)
                } else {
                    format!("task '{}'", task_id.as_ref().unwrap())
                };
                tracing::debug!("Deduplicated duplicate spawn effect for {}", reason);

                // Extract and preserve registry effects from the dropped spawn.
                // SpawnForTask no longer carries on_success callbacks — its bookkeeping
                // (including BindCoworkerToWorktree) is inlined in the executor after the
                // real name is known, so there is nothing to hoist here.
                let on_success_effects = match effect {
                    Effect::SpawnCoworkerWithCallbacks { on_success, .. } => on_success,
                    _ => vec![],
                };

                for extracted_effect in on_success_effects {
                    if is_registry_effect(&extracted_effect) {
                        registry_effects.push(extracted_effect);
                    }
                }
                continue;
            }

            // First spawn for this coworker - extract its registry effects and add to
            // the top level, then reconstruct the spawn without those effects.
            // SpawnForTask no longer carries on_success callbacks, so it passes through as-is.
            let (modified_effect, extracted_registry) = match effect {
                Effect::SpawnCoworkerWithCallbacks {
                    config,
                    on_success,
                    on_failure,
                } => {
                    let (registry, other): (Vec<_>, Vec<_>) =
                        on_success.into_iter().partition(is_registry_effect);
                    (
                        Effect::SpawnCoworkerWithCallbacks {
                            config,
                            on_success: other,
                            on_failure,
                        },
                        registry,
                    )
                }
                other => (other, vec![]),
            };

            registry_effects.extend(extracted_registry);
            result.push(modified_effect);
            seen_coworker_names.insert(name);
            if let Some(tid) = task_id {
                seen_task_ids.insert(tid);
            }
        } else {
            result.push(effect);
        }
    }

    // Append all collected registry effects at the end
    result.extend(registry_effects);
    result
}

/// Check if an effect is a registry-related effect that should be preserved
/// when spawn effects are deduplicated.
fn is_registry_effect(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::RegisterWorktreeAssignment { .. } | Effect::BindCoworkerToWorktree { .. }
    )
}

#[path = "events_tests.rs"]
#[cfg(test)]
mod tests;
