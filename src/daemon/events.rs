//! Daemon event dispatch — the central coordination point for the state machine.
//!
//! Each event source (timer tick, webhook, RPC, signal) maps to a `DaemonEvent`
//! variant. The `evaluate_tick` function dispatches the event to the appropriate
//! set of check functions, collecting all effects into a single `Vec<Effect>`.
//!
//! ```text
//! Timer/Webhook/RPC → DaemonEvent
//!                   → collect WorldSnapshot (immutable)
//!                   → evaluate_tick(event, snapshot, state) → Vec<Effect>
//!                   → execute_effects(effects)
//! ```

use std::collections::HashSet;

use super::DaemonState;
use super::effects::Effect;
use super::snapshot::WorldSnapshot;

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
    snap: &WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    match event {
        DaemonEvent::SessionMonitorTick => {
            let mut effects = Vec::new();
            // Health checks: idle shutdown, stuck detection, usage limits.
            effects.extend(super::health::check_and_shutdown_idle_coworkers(snap, state).await);
            effects.extend(super::health::check_and_restart_stuck_coworkers(snap, state).await);
            effects.extend(super::health::check_and_restart_stuck_reviewers(snap));
            effects.extend(super::health::check_for_usage_limits(snap));
            effects.extend(super::health::maybe_nudge_usage_limit_expiry(snap));
            effects.extend(super::health::check_and_nudge_api_errors(snap, state));
            effects.extend(super::health::check_and_restart_tool_name_conflicts(snap));
            effects
        }
        DaemonEvent::TaskDispatchTick => {
            let mut effects = Vec::new();
            effects.extend(super::dispatch::reconcile_tasks_in_review(snap));
            effects.extend(super::dispatch::reset_orphaned_tasks(snap));
            effects.extend(super::dispatch::check_for_duplicate_task_workers(snap));
            effects.extend(super::dispatch::check_and_recover_orphans(snap, state));
            effects.extend(super::dispatch::spawn_for_pending_tasks(snap, state));
            effects.extend(super::health::check_and_respawn_dead_processes(snap, state).await);
            effects.extend(super::health::check_and_fire_reminders(snap, state).await);
            dedup_spawn_effects(effects)
        }
        DaemonEvent::PrPollTick => {
            // PR polling: check open PRs for issues, spawn reviewers, clean up merged worktrees.
            // When GitHub API quota is critically low (< 5%), skip API-calling polls but still
            // run pure cleanup logic (doesn't make API calls, just reads snapshot state).
            let mut effects = Vec::new();

            if snap.github_rate_limit.is_critical() {
                tracing::warn!(
                    "Skipping PR poll (GitHub API quota critical: {})",
                    snap.github_rate_limit.summary()
                );
            } else {
                // Normal PR polling when quota is not critical
                match super::pr::poll_prs_for_issues(snap, state).await {
                    Ok(pr_effects) => effects.extend(pr_effects),
                    Err(e) => {
                        tracing::warn!("PR poll error: {}", e);
                    }
                }
            }

            // Always run merged PR cleanup (pure function, no API calls)
            effects.extend(super::pr::collect_merged_pr_cleanup_effects(snap));

            // Reconcile orphaned PRs: create tasks for reviewed + CI green PRs with no active task
            effects.extend(super::pr::reconcile_orphaned_prs(snap));

            // Auto-complete tasks whose descriptions reference only merged PRs
            // (handles meta-tasks, sub-tasks, and fix-PR tasks)
            effects.extend(super::dispatch::build_description_based_completion_effects(
                snap,
            ));

            dedup_spawn_effects(effects)
        }
        DaemonEvent::RateLimitCheckTick => {
            // Evaluate freshly fetched GitHub API rate limits against previous state.
            // The rate limit data was fetched before snapshot collection and passed in
            // via snap.freshly_fetched_rate_limit.
            let mut effects = Vec::new();
            if let Some(rate_limit) = &snap.freshly_fetched_rate_limit {
                // Check if state changed (low → critical, critical → recovered, etc.)
                let was_critical = snap.github_rate_limit.is_critical();
                let was_low = snap.github_rate_limit.is_low();
                let now_critical = rate_limit.is_critical();
                let now_low = rate_limit.is_low();

                // Warn when entering critical state (< 5%)
                if now_critical && !was_critical {
                    effects.push(Effect::PostSystemMessage {
                        message: format!(
                            "⚠️ GitHub API quota critical ({}). PR polling paused until reset at {}.",
                            rate_limit.summary(),
                            rate_limit.graphql.reset_time().format("%H:%M UTC")
                        ),
                    });
                    effects.push(Effect::RecordCooldown {
                        category: "rate_limit_critical".to_string(),
                        key: "throttle_warning".to_string(),
                    });
                }
                // Warn when entering low state (< 20%)
                else if now_low && !was_low && !now_critical {
                    effects.push(Effect::PostSystemMessage {
                        message: format!(
                            "⚠️ GitHub API quota low ({}). Consider reducing manual gh commands.",
                            rate_limit.summary()
                        ),
                    });
                    effects.push(Effect::RecordCooldown {
                        category: "rate_limit_low".to_string(),
                        key: "throttle_warning".to_string(),
                    });
                }
                // Post recovery/improvement messages for state transitions
                else if was_critical && !now_critical {
                    if now_low {
                        // Critical → low (improved but still low)
                        effects.push(Effect::PostSystemMessage {
                            message: format!(
                                "⬆️ GitHub API quota improved ({}) — PR polling resumed, but quota still low.",
                                rate_limit.summary()
                            ),
                        });
                    } else {
                        // Critical → normal (fully recovered)
                        effects.push(Effect::PostSystemMessage {
                            message: format!(
                                "✅ GitHub API quota recovered ({}). PR polling resumed.",
                                rate_limit.summary()
                            ),
                        });
                    }
                } else if was_low && !was_critical && !now_low && !now_critical {
                    // Low → normal (recovered from low state)
                    effects.push(Effect::PostSystemMessage {
                        message: format!(
                            "✅ GitHub API quota recovered ({}).",
                            rate_limit.summary()
                        ),
                    });
                }

                effects.push(Effect::UpdateRateLimit(rate_limit.clone()));
            }
            effects
        }
    }
}

/// Deduplicate spawn effects by coworker name.
///
/// Multiple independent decision functions (orphan recovery, pending task spawn,
/// dead process respawn, PR call-in) can each decide to spawn the same coworker
/// in a single tick. Without deduplication, the first spawn succeeds but subsequent
/// ones trigger `on_success` callbacks (since the idempotent guard returns Ok),
/// posting duplicate "Called in" messages.
///
/// Handles all spawn-like effect variants: `SpawnCoworker`,
/// `SpawnCoworkerWithCallbacks`, `AssignAndSpawn`, and `ResumeCoworker`.
/// Keeps the first spawn effect for each coworker name, drops duplicates.
/// Non-spawn effects are always preserved.
fn dedup_spawn_effects(effects: Vec<Effect>) -> Vec<Effect> {
    let mut seen_spawns: HashSet<String> = HashSet::new();
    let mut result: Vec<Effect> = Vec::new();
    let mut registry_effects: Vec<Effect> = Vec::new();

    for effect in effects {
        let spawn_name = match &effect {
            Effect::SpawnCoworker(config) => Some(config.name.to_lowercase()),
            Effect::SpawnCoworkerWithCallbacks { config, .. } => Some(config.name.to_lowercase()),
            Effect::AssignAndSpawn { config, .. } => Some(config.name.to_lowercase()),
            Effect::ResumeCoworker { name, .. } => Some(name.to_lowercase()),
            _ => None,
        };

        if let Some(name) = spawn_name {
            if seen_spawns.contains(&name) {
                tracing::debug!("Deduplicated duplicate spawn effect for '{}'", name);

                // Extract and preserve registry effects from the dropped spawn
                let on_success_effects = match effect {
                    Effect::SpawnCoworkerWithCallbacks { on_success, .. } => on_success,
                    Effect::AssignAndSpawn { on_success, .. } => on_success,
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
            // the top level, then reconstruct the spawn without those effects
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
                Effect::AssignAndSpawn {
                    task_id,
                    owner,
                    repo_name,
                    config,
                    on_success,
                    on_failure,
                } => {
                    let (registry, other): (Vec<_>, Vec<_>) =
                        on_success.into_iter().partition(is_registry_effect);
                    (
                        Effect::AssignAndSpawn {
                            task_id,
                            owner,
                            repo_name,
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
            seen_spawns.insert(name);
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
        Effect::RegisterWorktreeAssignment { .. }
            | Effect::BindCoworkerToWorktree { .. }
            | Effect::SetWorktreePrNumber { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{CoworkerRole, LaunchConfig, SessionMode};

    fn make_spawn(name: &str) -> Effect {
        Effect::SpawnCoworker(LaunchConfig {
            name: name.to_string(),
            session_mode: SessionMode::Fresh,
            role: CoworkerRole::Coworker,
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: false,
            pr_number: None,
            team_name: None,
            working_dir: None,
            model: "sonnet".to_string(),
            channel: None,
            auth_profile_dir: None,
            auth_provider: crate::auth::AuthProvider::Claude,
        })
    }

    #[test]
    fn dedup_removes_duplicate_spawn_for_same_coworker() {
        let effects = vec![
            make_spawn("lexington"),
            Effect::NudgeLead {
                message: "hello".to_string(),
            },
            make_spawn("lexington"), // duplicate — should be removed
            make_spawn("park"),      // different coworker — should be kept
        ];

        let deduped = dedup_spawn_effects(effects);

        let spawn_names: Vec<&str> = deduped
            .iter()
            .filter_map(|e| {
                if let Effect::SpawnCoworker(config) = e {
                    Some(config.name.as_str())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(spawn_names, vec!["lexington", "park"]);
        // NudgeLead preserved
        assert_eq!(deduped.len(), 3);
    }

    #[test]
    fn dedup_preserves_all_when_no_duplicates() {
        let effects = vec![
            make_spawn("lexington"),
            make_spawn("park"),
            Effect::NudgeLead {
                message: "hello".to_string(),
            },
        ];

        let deduped = dedup_spawn_effects(effects);
        assert_eq!(deduped.len(), 3);
    }

    #[test]
    fn dedup_is_case_insensitive() {
        let effects = vec![make_spawn("Lexington"), make_spawn("lexington")];

        let deduped = dedup_spawn_effects(effects);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn dedup_empty_effects_returns_empty() {
        let deduped = dedup_spawn_effects(vec![]);
        assert!(deduped.is_empty());
    }

    fn make_spawn_with_callbacks(name: &str) -> Effect {
        let config = LaunchConfig {
            name: name.to_string(),
            session_mode: SessionMode::Fresh,
            role: CoworkerRole::Coworker,
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: false,
            pr_number: None,
            team_name: None,
            working_dir: None,
            model: "sonnet".to_string(),
            channel: None,
            auth_profile_dir: None,
            auth_provider: crate::auth::AuthProvider::Claude,
        };
        Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success: vec![],
            on_failure: vec![],
        }
    }

    fn make_assign_and_spawn(name: &str) -> Effect {
        let config = LaunchConfig {
            name: name.to_string(),
            session_mode: SessionMode::Fresh,
            role: CoworkerRole::Coworker,
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: false,
            pr_number: None,
            team_name: None,
            working_dir: None,
            model: "sonnet".to_string(),
            channel: None,
            auth_profile_dir: None,
            auth_provider: crate::auth::AuthProvider::Claude,
        };
        Effect::AssignAndSpawn {
            task_id: "1".to_string(),
            owner: name.to_string(),
            repo_name: "test".to_string(),
            config,
            on_success: vec![],
            on_failure: vec![],
        }
    }

    #[test]
    fn dedup_removes_duplicate_spawn_with_callbacks() {
        let effects = vec![
            make_spawn_with_callbacks("lexington"),
            make_spawn_with_callbacks("lexington"), // duplicate
            make_spawn_with_callbacks("park"),
        ];

        let deduped = dedup_spawn_effects(effects);
        assert_eq!(deduped.len(), 2, "Should keep one lexington + one park");
    }

    #[test]
    fn dedup_across_spawn_variants() {
        // AssignAndSpawn and SpawnCoworkerWithCallbacks for the same coworker
        // should deduplicate (first one wins).
        let effects = vec![
            make_assign_and_spawn("lexington"),
            make_spawn_with_callbacks("lexington"), // same coworker, different variant
            make_spawn("park"),
        ];

        let deduped = dedup_spawn_effects(effects);
        assert_eq!(deduped.len(), 2, "Should keep first lexington + park");
        // First effect should be the AssignAndSpawn (it came first)
        assert!(
            matches!(&deduped[0], Effect::AssignAndSpawn { config, .. } if config.name == "lexington"),
            "First effect should be AssignAndSpawn for lexington"
        );
    }

    #[test]
    fn dedup_preserves_registry_effects_from_dropped_spawns() {
        // Issue #8 from PR #752 review: When two tasks are assigned to the same
        // coworker in one tick, the second AssignAndSpawn is dropped entirely,
        // losing its RegisterWorktreeAssignment effect.
        use crate::worktree_registry::WorktreeAssignment;

        let config1 = LaunchConfig {
            name: "lexington".to_string(),
            session_mode: SessionMode::Fresh,
            role: CoworkerRole::Coworker,
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: false,
            pr_number: None,
            team_name: None,
            working_dir: None,
            model: "sonnet".to_string(),
            channel: None,
            auth_profile_dir: None,
            auth_provider: crate::auth::AuthProvider::Claude,
        };

        let config2 = config1.clone();

        // First spawn with task-123 worktree assignment
        let spawn1 = Effect::AssignAndSpawn {
            task_id: "123".to_string(),
            owner: "lexington".to_string(),
            repo_name: "test".to_string(),
            config: config1,
            on_success: vec![Effect::RegisterWorktreeAssignment {
                assignment: WorktreeAssignment {
                    worktree_id: "task-123-foo".to_string(),
                    branch_name: "task-123-foo".to_string(),
                    task_id: Some("123".to_string()),
                    current_coworker: None,
                    pr_number: None,
                    created_at: chrono::Utc::now(),
                },
            }],
            on_failure: vec![],
        };

        // Second spawn with task-456 worktree assignment (different task, same coworker)
        let spawn2 = Effect::AssignAndSpawn {
            task_id: "456".to_string(),
            owner: "lexington".to_string(),
            repo_name: "test".to_string(),
            config: config2,
            on_success: vec![Effect::RegisterWorktreeAssignment {
                assignment: WorktreeAssignment {
                    worktree_id: "task-456-bar".to_string(),
                    branch_name: "task-456-bar".to_string(),
                    task_id: Some("456".to_string()),
                    current_coworker: None,
                    pr_number: None,
                    created_at: chrono::Utc::now(),
                },
            }],
            on_failure: vec![],
        };

        let effects = vec![spawn1, spawn2];
        let deduped = dedup_spawn_effects(effects);

        // The spawn should be deduplicated (only one spawn for lexington)
        let spawn_count = deduped
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    Effect::AssignAndSpawn { .. }
                        | Effect::SpawnCoworker(_)
                        | Effect::SpawnCoworkerWithCallbacks { .. }
                )
            })
            .count();
        assert_eq!(spawn_count, 1, "Should have only one spawn for lexington");

        // BUT: Both RegisterWorktreeAssignment effects should be preserved
        let registry_assignments: Vec<&str> = deduped
            .iter()
            .filter_map(|e| {
                if let Effect::RegisterWorktreeAssignment { assignment } = e {
                    Some(assignment.worktree_id.as_str())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            registry_assignments.len(),
            2,
            "Both registry assignments should be preserved"
        );
        assert!(
            registry_assignments.contains(&"task-123-foo"),
            "First task's worktree should be registered"
        );
        assert!(
            registry_assignments.contains(&"task-456-bar"),
            "Second task's worktree should be registered"
        );
    }
}
