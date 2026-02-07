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
            effects.extend(super::health::check_for_usage_limits(snap));
            effects.extend(super::health::maybe_nudge_usage_limit_expiry(snap));
            effects.extend(super::health::check_and_nudge_api_errors(snap, state));
            effects
        }
        DaemonEvent::TaskDispatchTick => {
            let mut effects = Vec::new();
            effects.extend(super::dispatch::check_for_duplicate_task_workers(snap));
            effects.extend(super::dispatch::check_and_recover_orphans(snap, state));
            effects.extend(super::dispatch::spawn_for_pending_tasks(snap, state));
            effects.extend(super::health::check_and_respawn_dead_processes(snap, state).await);
            effects.extend(super::health::check_and_fire_reminders(snap, state).await);
            dedup_spawn_effects(effects)
        }
        DaemonEvent::PrPollTick => {
            // PR polling: check open PRs for issues, spawn reviewers.
            match super::pr::poll_prs_for_issues(snap, state).await {
                Ok(effects) => dedup_spawn_effects(effects),
                Err(e) => {
                    tracing::warn!("PR poll error: {}", e);
                    Vec::new()
                }
            }
        }
    }
}

/// Deduplicate SpawnCoworker effects by coworker name.
///
/// Multiple independent decision functions (orphan recovery, pending task spawn,
/// dead process respawn, PR call-in) can each decide to spawn the same coworker
/// in a single tick. Without deduplication, the first spawn succeeds but subsequent
/// ones fail with "session already exists", and the failure handler destructively
/// resets the task to pending — undoing the successful first spawn.
///
/// Keeps the first SpawnCoworker effect for each name, drops duplicates.
/// Non-spawn effects are always preserved.
fn dedup_spawn_effects(effects: Vec<Effect>) -> Vec<Effect> {
    let mut seen_spawns: HashSet<String> = HashSet::new();
    effects
        .into_iter()
        .filter(|effect| {
            if let Effect::SpawnCoworker(config) = effect {
                let name = config.name.to_lowercase();
                if seen_spawns.contains(&name) {
                    tracing::debug!("Deduplicated duplicate SpawnCoworker for '{}'", config.name);
                    return false;
                }
                seen_spawns.insert(name);
            }
            true
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{CoworkerRole, LaunchConfig, SessionMode, TaskMode};

    fn make_spawn(name: &str) -> Effect {
        Effect::SpawnCoworker(LaunchConfig {
            name: name.to_string(),
            session_mode: SessionMode::Fresh,
            task_mode: TaskMode::Isolated,
            role: CoworkerRole::Coworker,
            initial_prompt: None,
            additional_dirs: vec![],
            restrict_setting_sources: false,
            pr_number: None,
            team_name: None,
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
}
