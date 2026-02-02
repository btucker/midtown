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

use super::DaemonState;
use super::effects::Effect;
use super::snapshot::WorldSnapshot;

/// Events that drive the daemon's state machine.
///
/// Currently covers the two periodic tick groups. Future phases will add
/// variants for webhook events, RPC requests, and signals — converting
/// the remaining inline side effects to the evaluate/execute pattern.
#[derive(Debug)]
pub enum DaemonEvent {
    /// Periodic session-monitor tick: idle shutdown, interrupt/prompt nudge, stuck
    /// detection, usage limits.
    SessionMonitorTick,
    /// Periodic task-dispatch tick: duplicate detection, orphan recovery, task spawning,
    /// zombie respawning, reminders.
    TaskDispatchTick,
}

/// Evaluate a daemon event against the current world snapshot, returning effects.
///
/// This is the central dispatch point. Each event variant maps to a batch of
/// pure (or near-pure) check functions that read from the snapshot and return
/// effects. The caller executes all returned effects via `execute_effects`.
///
/// Some check functions still take `&DaemonState` for mutable tracker state
/// (coworker_lifecycles, cooldowns, etc.) and inline spawns that cannot yet be
/// expressed as pure effects (spawn success/failure determines follow-up effects).
pub async fn evaluate_tick(
    event: &DaemonEvent,
    snap: &WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    match event {
        DaemonEvent::SessionMonitorTick => {
            let mut effects = Vec::new();
            // Order matters: later calls can override phase transitions from earlier
            // calls. For example, a prompt nudge can supersede an idle shutdown
            // decision for the same coworker.
            effects.extend(super::health::check_and_shutdown_idle_coworkers(snap, state).await);
            effects.extend(super::health::check_and_restart_stuck_coworkers(
                snap, state,
            ));
            effects.extend(super::health::check_for_usage_limits(snap));
            effects.extend(super::health::maybe_nudge_usage_limit_expiry(snap));
            effects
        }
        DaemonEvent::TaskDispatchTick => {
            let mut effects = Vec::new();
            effects.extend(super::dispatch::check_for_duplicate_task_workers(snap));
            effects.extend(super::dispatch::check_and_recover_orphans(snap, state));
            effects.extend(super::dispatch::spawn_for_pending_tasks(snap, state));
            effects.extend(super::health::check_and_respawn_zombies(snap, state));
            effects.extend(super::health::check_and_fire_reminders(snap, state));
            effects
        }
    }
}
