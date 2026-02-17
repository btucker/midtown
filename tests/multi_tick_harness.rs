//! Multi-tick test harness for testing cross-tick behavior.
//!
//! This harness enables testing daemon behavior across multiple ticks
//! to catch bugs like:
//! - Task dispatched on tick 1 → re-dispatched on tick 2 (duplicate spawn)
//! - Reviewer spawned on tick 1 → re-spawned on tick 2 (double spawn)
//! - Orphan recovered on tick 1 → re-recovered on tick 2
//!
//! ## Usage
//!
//! ```rust
//! # use tests::multi_tick_harness::MultiTickHarness;
//! # use midtown::daemon::snapshot::WorldSnapshot;
//! # use midtown::daemon::DaemonEvent;
//!
//! // Load initial snapshot
//! let fixture = include_str!("fixtures/snapshot/some-snapshot.json");
//! let mut harness = MultiTickHarness::from_json(fixture).unwrap();
//!
//! // Tick 1: Get initial effects
//! let effects1 = harness.tick(&DaemonEvent::TaskDispatchTick);
//!
//! // Tick 2: Apply effects and tick again
//! let effects2 = harness.tick(&DaemonEvent::TaskDispatchTick);
//!
//! // Verify no duplicates
//! assert!(effects2.iter().all(|e| !matches!(e, Effect::AssignAndSpawn { .. })));
//! ```

use chrono::Utc;

use midtown::daemon::Effect;
use midtown::daemon::snapshot::{ProcessHealth, WorldSnapshot};
use midtown::tasks::TaskStatus;

/// A test harness for simulating multiple daemon ticks.
///
/// The harness maintains a mutable WorldSnapshot and provides methods to:
/// 1. Load an initial snapshot from JSON
/// 2. Apply effects to mutate the snapshot
/// 3. Generate a new snapshot for the next tick
///
/// This enables testing cross-tick behavior without running a full daemon.
pub struct MultiTickHarness {
    /// Current snapshot state (mutated by effect application)
    snapshot: WorldSnapshot,
}

impl MultiTickHarness {
    /// Create a new harness from a JSON snapshot fixture.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let snapshot: WorldSnapshot = serde_json::from_str(json)?;
        Ok(Self { snapshot })
    }

    /// Get a reference to the current snapshot.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> &WorldSnapshot {
        &self.snapshot
    }

    /// Simulate a daemon tick by calling pure decision functions.
    ///
    /// This calls only the pure decision functions that don't require DaemonState.
    /// Functions that need DaemonState are skipped for now.
    ///
    /// Returns the effects produced by this tick.
    pub fn tick(&mut self, event: &midtown::daemon::DaemonEvent) -> Vec<Effect> {
        use midtown::daemon::DaemonEvent;

        // Call pure decision functions based on event type
        let effects = match event {
            DaemonEvent::SessionMonitorTick => {
                // Health checks that are pure
                let mut effects = Vec::new();
                effects.extend(midtown::daemon::check_and_shutdown_idle_coworkers(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::check_and_restart_stuck_reviewers(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::check_for_usage_limits(&self.snapshot));
                effects
            }
            DaemonEvent::TaskDispatchTick => {
                let mut effects = Vec::new();
                effects.extend(midtown::daemon::reset_orphaned_tasks(&self.snapshot));
                // Note: check_and_recover_orphans requires DaemonState - skipped in harness
                // Note: spawn_for_pending_tasks requires DaemonState - skipped in harness
                effects
            }
            DaemonEvent::PrPollTick => {
                let mut effects = Vec::new();
                effects.extend(midtown::daemon::collect_merged_pr_cleanup_effects(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::reconcile_orphaned_prs(&self.snapshot));
                effects
            }
            DaemonEvent::RateLimitCheckTick => {
                // Rate limit checks are mostly side effects - skip
                vec![]
            }
        };

        // Apply effects to mutate the snapshot for the next tick
        self.apply_effects(&effects);

        effects
    }

    /// Apply effects to the current snapshot, simulating their execution.
    ///
    /// This is a simplified simulation that only handles the effects most
    /// relevant to cross-tick testing. Many effects (channel posts, nudges)
    /// don't affect the snapshot state and are ignored.
    fn apply_effects(&mut self, effects: &[Effect]) {
        for effect in effects {
            match effect {
                Effect::AssignAndSpawn { task_id, owner, .. } => {
                    // Mark task as in_progress with owner
                    self.assign_task(task_id, owner);
                }
                Effect::RecordTaskAssignment { coworker, task_id } => {
                    // Track the assignment
                    self.snapshot
                        .coworker_task_assignments
                        .insert(coworker.to_lowercase(), task_id.clone());
                    self.snapshot.busy_coworkers.insert(coworker.to_lowercase());
                }
                Effect::SpawnCoworker(config) => {
                    // Add coworker to active set
                    self.spawn_coworker(&config.name);
                }
                Effect::SpawnCoworkerWithCallbacks { config, .. } => {
                    self.spawn_coworker(&config.name);
                }
                Effect::ShutdownCoworker { name, .. } => {
                    // Remove coworker from active set
                    self.remove_coworker(name);
                }
                Effect::ShutdownCoworkerWithCallbacks { name, .. } => {
                    self.remove_coworker(name);
                }
                Effect::AssignReviewer {
                    pr_number,
                    reviewer_name,
                    ..
                } => {
                    // Track reviewer assignment
                    self.snapshot
                        .active_reviewers
                        .insert(reviewer_name.to_lowercase());
                    self.snapshot
                        .reviewer_pr_assignments
                        .insert(reviewer_name.to_lowercase(), *pr_number);
                }
                Effect::RemoveReviewerAssignment { pr_number } => {
                    // Remove reviewer assignment
                    self.snapshot
                        .reviewer_pr_assignments
                        .retain(|_, pr| pr != pr_number);
                }
                Effect::CompleteTask { task_id, .. } => {
                    // Mark task as completed
                    self.complete_task(task_id);
                }
                Effect::ResetTaskToPending { task_id, .. } => {
                    // Reset task to pending (clear owner)
                    self.reset_task(task_id);
                }
                Effect::RecordCooldown { .. } => {
                    // Cooldowns are tracked in DaemonState, not WorldSnapshot
                    // We can't simulate this without DaemonState
                }
                _ => {
                    // Other effects (PostToChannel, NudgeCoworker, etc.) don't affect
                    // the snapshot state in ways that matter for cross-tick testing
                }
            }
        }
    }

    /// Simulate assigning a task to a coworker.
    fn assign_task(&mut self, task_id: &str, owner: &str) {
        // Find the task and update its status/owner
        for task in &mut self.snapshot.all_tasks {
            if task.id == task_id {
                task.status = TaskStatus::InProgress;
                task.owner = Some(owner.to_string());
                break;
            }
        }

        // Update in_progress_tasks
        if let Some(task) = self.snapshot.all_tasks.iter().find(|t| t.id == task_id) {
            self.snapshot.in_progress_tasks.push((
                task.id.clone(),
                task.subject.clone(),
                owner.to_string(),
            ));
        }

        // Remove from pending lists
        self.snapshot
            .pending_tasks_without_owners
            .retain(|t| t.id != task_id);
        self.snapshot
            .pending_tasks_with_owners
            .retain(|(id, _, _)| id != task_id);

        // Mark coworker as busy
        self.snapshot.busy_coworkers.insert(owner.to_lowercase());
        self.snapshot
            .coworker_task_assignments
            .insert(owner.to_lowercase(), task_id.to_string());
    }

    /// Simulate spawning a coworker.
    fn spawn_coworker(&mut self, name: &str) {
        let name_lower = name.to_lowercase();

        // Add to active sets
        self.snapshot.active_names.insert(name_lower.clone());

        // Add start time
        self.snapshot
            .coworker_start_times
            .insert(name_lower.clone(), Utc::now());

        // Add process health (alive, no issues)
        self.snapshot.headless_process_health.insert(
            name_lower,
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(Utc::now()),
                ..Default::default()
            },
        );
    }

    /// Simulate removing a coworker.
    fn remove_coworker(&mut self, name: &str) {
        let name_lower = name.to_lowercase();

        // Remove from active sets
        self.snapshot.active_names.remove(&name_lower);
        self.snapshot.busy_coworkers.remove(&name_lower);

        // Add stop time
        self.snapshot
            .coworker_stop_times
            .insert(name_lower.clone(), Utc::now());

        // Mark process as dead
        if let Some(health) = self.snapshot.headless_process_health.get_mut(&name_lower) {
            health.is_alive = false;
            health.exit_code = Some(0);
        }

        // Clear task assignment
        self.snapshot.coworker_task_assignments.remove(&name_lower);
        self.snapshot.active_reviewers.remove(&name_lower);
    }

    /// Simulate completing a task.
    fn complete_task(&mut self, task_id: &str) {
        // Update task status
        for task in &mut self.snapshot.all_tasks {
            if task.id == task_id {
                task.status = TaskStatus::Completed;
                break;
            }
        }

        // Remove from in_progress
        self.snapshot
            .in_progress_tasks
            .retain(|(id, _, _)| id != task_id);
    }

    /// Simulate resetting a task to pending.
    fn reset_task(&mut self, task_id: &str) {
        // Find the task and reset it
        for task in &mut self.snapshot.all_tasks {
            if task.id == task_id {
                task.status = TaskStatus::Pending;
                let owner = task.owner.clone();
                task.owner = None;

                // Move to pending_tasks_without_owners
                self.snapshot
                    .pending_tasks_without_owners
                    .push(task.clone());

                // Remove from in_progress
                self.snapshot
                    .in_progress_tasks
                    .retain(|(id, _, _)| id != task_id);

                // Clear busy state if this was the only task
                if let Some(owner_name) = owner {
                    let owner_lower = owner_name.to_lowercase();
                    if !self
                        .snapshot
                        .in_progress_tasks
                        .iter()
                        .any(|(_, _, o)| o.to_lowercase() == owner_lower)
                    {
                        self.snapshot.busy_coworkers.remove(&owner_lower);
                        self.snapshot.coworker_task_assignments.remove(&owner_lower);
                    }
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use midtown::daemon::DaemonEvent;

    /// Smoke test: verify harness can load a snapshot and run ticks.
    #[test]
    fn test_harness_basic() {
        let fixture =
            include_str!("fixtures/snapshot/snapshot-reviewer-not-spawning-20260214-003545.json");
        let mut harness = MultiTickHarness::from_json(fixture).unwrap();

        // Tick 1
        let effects1 = harness.tick(&DaemonEvent::TaskDispatchTick);
        println!("Tick 1: {} effects", effects1.len());

        // Tick 2
        let effects2 = harness.tick(&DaemonEvent::TaskDispatchTick);
        println!("Tick 2: {} effects", effects2.len());

        // The test passes if we can run multiple ticks without crashing
    }
}
