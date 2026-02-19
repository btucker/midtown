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

use midtown::auth::AuthProvider;
use midtown::coworker::{Coworker, CoworkerStatus};
use midtown::daemon::Effect;
use midtown::daemon::SessionRecord;
use midtown::daemon::snapshot::{ProcessHealth, WorldSnapshot};
use midtown::tasks::{Task, TaskStatus};

/// A test harness for simulating multiple daemon ticks.
///
/// The harness maintains a mutable WorldSnapshot and provides methods to:
/// 1. Load an initial snapshot from JSON
/// 2. Apply effects to mutate the snapshot
/// 3. Generate a new snapshot for the next tick
///
/// This enables testing cross-tick behavior without running a full daemon.
///
/// ## Coverage
///
/// The harness calls only pure decision functions (those taking `&WorldSnapshot`
/// and returning `Vec<Effect>` without I/O). Functions requiring `DaemonState`
/// are skipped:
///
/// ### SessionMonitorTick
/// - Called: `check_and_shutdown_idle_coworkers`, `check_and_restart_stuck_reviewers`,
///   `check_for_usage_limits`, `maybe_nudge_usage_limit_expiry`,
///   `check_and_restart_tool_name_conflicts`, `ensure_channel_leads_alive`
/// - Skipped (needs DaemonState): `check_and_handle_auth_errors`,
///   `check_and_restart_stuck_coworkers`, `check_and_nudge_api_errors`
///
/// ### TaskDispatchTick
/// - Called: `reset_orphaned_tasks`, `check_for_duplicate_task_workers`,
///   `detect_stale_attached_sessions`, `ensure_lead_alive`, `dispatch_via_sessions`
/// - Skipped (needs DaemonState): `check_and_recover_orphans`,
///   `spawn_for_pending_tasks`, `check_and_respawn_dead_processes`,
///   `check_and_fire_reminders`
/// - Skipped (takes individual fields): `collect_auto_archive_effects`
///
/// ### PrPollTick
/// - Called: `collect_merged_pr_cleanup_effects`, `reconcile_orphaned_prs`,
///   `build_description_based_completion_effects`
/// - Skipped (needs DaemonState): `poll_prs_for_issues`
/// - Skipped (takes individual fields): `check_for_stale_worktrees`
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

    /// Create a minimal harness with an empty snapshot suitable for unit tests.
    ///
    /// The snapshot has no coworkers, no tasks, and no PRs. Use `snapshot_mut()`
    /// and the session helpers to add state before calling `tick()`.
    #[allow(dead_code)]
    pub fn new_minimal() -> Self {
        let minimal_json = r#"{
            "active_coworkers": [],
            "running_coworkers": [],
            "coworker_snapshots": [],
            "active_names": [],
            "active_session_ids": [],
            "session_name": "midtown-test",
            "coworker_start_times": {},
            "coworker_stop_times": {},
            "headless_process_health": {},
            "attached_coworkers": {},
            "in_progress_tasks": [],
            "busy_coworkers": [],
            "coworker_task_assignments": {},
            "all_tasks": [],
            "pending_tasks_with_owners": [],
            "pending_tasks_without_owners": [],
            "task_channel": {},
            "task_model_map": {},
            "task_plan_map": {},
            "task_execution_skill_map": {},
            "channel_lead_sessions": {},
            "coworkers_with_open_prs": [],
            "coworkers_with_merged_prs": [],
            "merged_pr_numbers": [],
            "ci_passed_pr_coworkers": [],
            "review_feedback_pr_coworkers": [],
            "open_prs_data": [],
            "github_open_pr_task_ids": {},
            "pending_task_owners": [],
            "tasks_with_open_prs": {},
            "pr_task_associations": {},
            "active_reviewers": [],
            "reviewer_pr_assignments": {},
            "reviewed_prs": [],
            "prs_needing_review": 0,
            "reviewer_restart_counts": {},
            "reviewer_escalations_posted": [],
            "orphaned_pr_lead_nudges_sent": [],
            "github_rate_limit": {
                "core": {"limit": 5000, "remaining": 5000, "reset": 9999999999, "used": 0},
                "graphql": {"limit": 5000, "remaining": 5000, "reset": 9999999999, "used": 0},
                "last_updated": "2026-01-01T00:00:00Z"
            },
            "coworkers_with_unblocked_deps": [],
            "usage_limit_nudge_scheduled": false,
            "usage_limited_coworkers": [],
            "api_error_coworkers": [],
            "auth_error_coworkers": [],
            "tool_name_conflict_coworkers": [],
            "archived_channels": [],
            "channel_messages": [],
            "daemon_logs": [],
            "tasks_with_worktrees": [],
            "task_worktree_map": {},
            "worktree_branch_owners": {},
            "merged_pr_branches": {},
            "lead_session_refresh_interval_secs": 0,
            "is_at_coworker_limit": false,
            "is_at_dev_limit": false,
            "now_utc": "2026-01-01T00:00:00Z",
            "repo_name": "test-repo",
            "orphan_spawn_cooldown_active": false,
            "session_dispatch_cooldown_active": false,
            "spawn_failure_cooldown_names": [],
            "sessions": {},
            "session_task_map": {},
            "session_name_map": {},
            "name_session_map": {}
        }"#;
        Self::from_json(minimal_json).expect("minimal snapshot JSON should always be valid")
    }

    /// Create a `SessionRecord` in the snapshot for a given session.
    ///
    /// Inserts the record into `sessions`, updates `session_task_map`,
    /// `session_name_map`, `name_session_map` (if name provided), and `active_session_ids`.
    #[allow(dead_code)]
    pub fn create_session(&mut self, session_id: &str, task_id: &str, name: Option<&str>) {
        let name_opt = name.map(|n| n.to_string());
        let record = SessionRecord {
            session_id: session_id.to_string(),
            task_id: Some(task_id.to_string()),
            current_name: name_opt.clone(),
            preferred_name: name_opt.clone(),
            working_dir: String::new(),
            branch: None,
            pr_number: None,
            initial_prompt: None,
            is_reviewer: false,
            coworker_type: "dev".to_string(),
            is_running: true,
            created_at: Utc::now(),
            resume_on_startup: true,
        };
        self.snapshot
            .sessions
            .insert(session_id.to_string(), record);
        self.snapshot
            .session_task_map
            .insert(task_id.to_string(), session_id.to_string());
        if let Some(ref name_str) = name_opt {
            self.snapshot
                .session_name_map
                .insert(session_id.to_string(), name_str.clone());
            self.snapshot
                .name_session_map
                .insert(name_str.clone(), session_id.to_string());
        }
        self.snapshot
            .active_session_ids
            .insert(session_id.to_string());
    }

    /// Mark a session as stopped.
    ///
    /// Sets `is_running = false`, removes from `session_name_map`, `name_session_map`,
    /// and `active_session_ids`. The session record itself persists in `sessions`.
    #[allow(dead_code)]
    pub fn stop_session(&mut self, session_id: &str) {
        assert!(
            self.snapshot.sessions.contains_key(session_id),
            "stop_session called with unknown session_id: {}",
            session_id
        );
        if let Some(record) = self.snapshot.sessions.get_mut(session_id) {
            let name = record.current_name.take();
            record.is_running = false;
            if let Some(n) = name {
                self.snapshot.session_name_map.remove(session_id);
                self.snapshot.name_session_map.remove(&n);
            }
        }
        self.snapshot.active_session_ids.remove(session_id);
    }

    /// Mark a stopped session as running again and restore name allocations.
    #[allow(dead_code)]
    pub fn resume_session(&mut self, session_id: &str, name: &str) {
        assert!(
            self.snapshot.sessions.contains_key(session_id),
            "resume_session called with unknown session_id: {}",
            session_id
        );
        if let Some(record) = self.snapshot.sessions.get_mut(session_id) {
            record.is_running = true;
            record.current_name = Some(name.to_string());
            self.snapshot
                .session_name_map
                .insert(session_id.to_string(), name.to_string());
            self.snapshot
                .name_session_map
                .insert(name.to_string(), session_id.to_string());
        }
        self.snapshot
            .active_session_ids
            .insert(session_id.to_string());
    }

    /// Get a mutable reference to the current snapshot for test setup.
    #[allow(dead_code)]
    pub fn snapshot_mut(&mut self) -> &mut WorldSnapshot {
        &mut self.snapshot
    }

    /// Get a reference to the current snapshot.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> &WorldSnapshot {
        &self.snapshot
    }

    /// Simulate a daemon tick by calling pure decision functions.
    ///
    /// This calls only the pure decision functions that don't require DaemonState.
    /// See the struct-level documentation for the full list of called/skipped functions.
    ///
    /// Returns the effects produced by this tick.
    pub fn tick(&mut self, event: &midtown::daemon::DaemonEvent) -> Vec<Effect> {
        use midtown::daemon::DaemonEvent;

        // Call pure decision functions based on event type
        let effects = match event {
            DaemonEvent::SessionMonitorTick => {
                let mut effects = Vec::new();
                effects.extend(midtown::daemon::check_and_shutdown_idle_coworkers(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::check_and_restart_stuck_reviewers(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::check_for_usage_limits(&self.snapshot));
                effects.extend(midtown::daemon::maybe_nudge_usage_limit_expiry(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::check_and_restart_tool_name_conflicts(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::ensure_channel_leads_alive(&self.snapshot));
                effects
            }
            DaemonEvent::TaskDispatchTick => {
                let mut effects = Vec::new();
                effects.extend(midtown::daemon::reset_orphaned_tasks(&self.snapshot));
                effects.extend(midtown::daemon::check_for_duplicate_task_workers(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::detect_stale_attached_sessions(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::ensure_lead_alive(&self.snapshot));
                effects.extend(midtown::daemon::dispatch_via_sessions(&self.snapshot));
                // Skipped (needs DaemonState): check_and_recover_orphans,
                // spawn_for_pending_tasks, check_and_respawn_dead_processes,
                // check_and_fire_reminders
                // Skipped (takes individual fields): collect_auto_archive_effects
                effects
            }
            DaemonEvent::PrPollTick => {
                let mut effects = Vec::new();
                effects.extend(midtown::daemon::collect_merged_pr_cleanup_effects(
                    &self.snapshot,
                ));
                effects.extend(midtown::daemon::reconcile_orphaned_prs(&self.snapshot));
                effects.extend(midtown::daemon::build_subject_based_completion_effects(
                    &self.snapshot,
                ));
                // Skipped (needs DaemonState): poll_prs_for_issues
                // Skipped (takes individual fields): check_for_stale_worktrees
                effects
            }
            DaemonEvent::RateLimitCheckTick => {
                // Rate limit checks are inline in evaluate_tick with no separate
                // pure decision functions to call
                vec![]
            }
        };

        // Apply effects to mutate the snapshot for the next tick
        self.apply_effects(&effects);

        effects
    }

    /// Apply effects to the current snapshot, simulating their execution.
    ///
    /// Handles all effects that modify WorldSnapshot state relevant to cross-tick
    /// testing. Effects that only produce side effects (channel posts, nudges)
    /// are ignored since they don't affect decision function inputs.
    ///
    /// `tick()` calls this automatically after each tick. Call directly when
    /// you need to apply effects between ticks for fine-grained control.
    #[allow(dead_code)]
    pub fn apply_effects(&mut self, effects: &[Effect]) {
        for effect in effects {
            match effect {
                Effect::AssignAndSpawn { task_id, owner, .. } => {
                    self.assign_task(task_id, owner);
                }
                Effect::RecordTaskAssignment { coworker, task_id } => {
                    self.snapshot
                        .coworker_task_assignments
                        .insert(coworker.to_lowercase(), task_id.clone());
                    self.snapshot.busy_coworkers.insert(coworker.to_lowercase());
                }
                Effect::SpawnCoworker(config) => {
                    self.spawn_coworker(&config.name);
                }
                Effect::SpawnCoworkerWithCallbacks { config, .. } => {
                    self.spawn_coworker(&config.name);
                    // If resuming a session, mark it running in sessions map so
                    // dispatch_via_sessions sees it correctly on the next tick.
                    if let midtown::launch::SessionMode::ResumeSession(ref session_id) =
                        config.session_mode
                    {
                        if let Some(record) = self.snapshot.sessions.get_mut(session_id) {
                            record.is_running = true;
                            record.current_name = Some(config.name.clone());
                        }
                        self.snapshot.active_session_ids.insert(session_id.clone());
                        self.snapshot
                            .session_name_map
                            .insert(session_id.clone(), config.name.clone());
                        self.snapshot
                            .name_session_map
                            .insert(config.name.clone(), session_id.clone());
                    }
                }
                Effect::ShutdownCoworker { name, .. } => {
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
                    self.snapshot
                        .active_reviewers
                        .insert(reviewer_name.to_lowercase());
                    self.snapshot
                        .reviewer_pr_assignments
                        .insert(reviewer_name.to_lowercase(), *pr_number);
                }
                Effect::RemoveReviewerAssignment { pr_number } => {
                    self.snapshot
                        .reviewer_pr_assignments
                        .retain(|_, pr| pr != pr_number);
                }
                Effect::CompleteTask { task_id, .. } => {
                    self.complete_task(task_id);
                }
                Effect::ResetTaskToPending { task_id, .. } => {
                    self.reset_task(task_id);
                }
                Effect::SetUsageLimitNudge { .. } => {
                    self.snapshot.usage_limit_nudge_scheduled = true;
                }
                Effect::CleanupMergedWorktree { pr_number, .. } => {
                    self.snapshot.merged_pr_numbers.remove(pr_number);
                    self.snapshot.merged_pr_branches.remove(pr_number);
                }
                Effect::CreateTask { subject, pr, .. } => {
                    self.create_task(subject, *pr);
                }
                Effect::AutoDetachCoworker { name } => {
                    self.snapshot.attached_coworkers.remove(name);
                }
                Effect::RecordOrphanedPrLeadNudge { pr_number } => {
                    self.snapshot
                        .orphaned_pr_lead_nudges_sent
                        .insert(*pr_number);
                }
                Effect::ClearOrphanedPrLeadNudge { pr_number } => {
                    self.snapshot.orphaned_pr_lead_nudges_sent.remove(pr_number);
                }
                Effect::RecordCooldown { .. } => {
                    // Cooldowns are tracked in DaemonState, not WorldSnapshot.
                    // Cannot simulate without DaemonState.
                }
                // Session-centric effects (new model)
                Effect::SpawnSession {
                    session_id,
                    task_id,
                    preferred_name,
                    is_reviewer,
                    ..
                } => {
                    let name = preferred_name.clone().unwrap_or_else(|| {
                        panic!(
                            "SpawnSession in apply_effects requires preferred_name — provide one in the test"
                        )
                    });
                    if let Some(record) = self.snapshot.sessions.get_mut(session_id) {
                        record.is_running = true;
                        record.current_name = Some(name.clone());
                    } else {
                        let record = SessionRecord {
                            session_id: session_id.clone(),
                            task_id: Some(task_id.clone()),
                            current_name: Some(name.clone()),
                            preferred_name: preferred_name.clone(),
                            working_dir: String::new(),
                            branch: None,
                            pr_number: None,
                            initial_prompt: None,
                            is_reviewer: *is_reviewer,
                            coworker_type: if *is_reviewer {
                                "reviewer".to_string()
                            } else {
                                "dev".to_string()
                            },
                            is_running: true,
                            created_at: Utc::now(),
                            resume_on_startup: !is_reviewer,
                        };
                        self.snapshot.sessions.insert(session_id.clone(), record);
                    }
                    self.snapshot
                        .session_task_map
                        .insert(task_id.clone(), session_id.clone());
                    self.snapshot
                        .session_name_map
                        .insert(session_id.clone(), name.clone());
                    self.snapshot
                        .name_session_map
                        .insert(name, session_id.clone());
                    self.snapshot.active_session_ids.insert(session_id.clone());
                }
                Effect::ShutdownSession { session_id, .. } => {
                    if let Some(record) = self.snapshot.sessions.get_mut(session_id) {
                        let name = record.current_name.take();
                        record.is_running = false;
                        if let Some(n) = name {
                            self.snapshot.session_name_map.remove(session_id);
                            self.snapshot.name_session_map.remove(&n);
                        }
                    }
                    self.snapshot.active_session_ids.remove(session_id);
                }
                Effect::RecordSession { record } => {
                    self.snapshot
                        .sessions
                        .insert(record.session_id.clone(), *record.clone());
                }
                Effect::ReleaseName { name } => {
                    if let Some(session_id) = self.snapshot.name_session_map.remove(name) {
                        self.snapshot.session_name_map.remove(&session_id);
                        if let Some(record) = self.snapshot.sessions.get_mut(&session_id) {
                            record.current_name = None;
                        }
                    }
                }
                _ => {
                    // Other effects (PostToChannel, PostSystemMessage, NudgeCoworker,
                    // NudgeLead, etc.) don't affect WorldSnapshot state.
                }
            }
        }
    }

    /// Simulate assigning a task to a coworker.
    fn assign_task(&mut self, task_id: &str, owner: &str) {
        for task in &mut self.snapshot.all_tasks {
            if task.id == task_id {
                task.status = TaskStatus::InProgress;
                task.owner = Some(owner.to_string());
                break;
            }
        }

        if let Some(task) = self.snapshot.all_tasks.iter().find(|t| t.id == task_id) {
            self.snapshot.in_progress_tasks.push((
                task.id.clone(),
                task.subject.clone(),
                owner.to_string(),
            ));
        }

        self.snapshot
            .pending_tasks_without_owners
            .retain(|t| t.id != task_id);
        self.snapshot
            .pending_tasks_with_owners
            .retain(|(id, _, _)| id != task_id);

        self.snapshot.busy_coworkers.insert(owner.to_lowercase());
        self.snapshot
            .coworker_task_assignments
            .insert(owner.to_lowercase(), task_id.to_string());
    }

    /// Simulate spawning a coworker.
    fn spawn_coworker(&mut self, name: &str) {
        let name_lower = name.to_lowercase();
        self.snapshot.active_names.insert(name_lower.clone());
        let now = Utc::now();
        self.snapshot
            .coworker_start_times
            .insert(name_lower.clone(), now);
        self.snapshot.headless_process_health.insert(
            name_lower.clone(),
            ProcessHealth {
                is_alive: true,
                last_event_at: Some(now),
                ..Default::default()
            },
        );

        // Add to active_coworkers so ensure_lead_alive sees it
        let coworker = Coworker {
            slot_id: format!("test-slot-{}", name_lower),
            name: name.to_string(),
            status: CoworkerStatus::Running,
            working_dir: format!("/test/worktree/{}", name_lower),
            started_at: now,
            current_task: None,
            session_id: None,
            model: "opus".to_string(),
            provider: AuthProvider::Claude,
            profile: "default".to_string(),
        };
        self.snapshot.active_coworkers.push(coworker.clone());
        self.snapshot.running_coworkers.push(coworker);
    }

    /// Simulate removing a coworker.
    fn remove_coworker(&mut self, name: &str) {
        let name_lower = name.to_lowercase();
        self.snapshot.active_names.remove(&name_lower);
        self.snapshot.busy_coworkers.remove(&name_lower);
        self.snapshot
            .coworker_stop_times
            .insert(name_lower.clone(), Utc::now());
        if let Some(health) = self.snapshot.headless_process_health.get_mut(&name_lower) {
            health.is_alive = false;
            health.exit_code = Some(0);
        }
        self.snapshot.coworker_task_assignments.remove(&name_lower);
        self.snapshot.active_reviewers.remove(&name_lower);

        // Remove from active_coworkers and running_coworkers
        self.snapshot
            .active_coworkers
            .retain(|c| c.name.to_lowercase() != name_lower);
        self.snapshot
            .running_coworkers
            .retain(|c| c.name.to_lowercase() != name_lower);
    }

    /// Simulate completing a task.
    fn complete_task(&mut self, task_id: &str) {
        for task in &mut self.snapshot.all_tasks {
            if task.id == task_id {
                task.status = TaskStatus::Completed;
                break;
            }
        }
        self.snapshot
            .in_progress_tasks
            .retain(|(id, _, _)| id != task_id);
    }

    /// Simulate resetting a task to pending.
    fn reset_task(&mut self, task_id: &str) {
        for task in &mut self.snapshot.all_tasks {
            if task.id == task_id {
                task.status = TaskStatus::Pending;
                let owner = task.owner.clone();
                task.owner = None;

                self.snapshot
                    .pending_tasks_without_owners
                    .push(task.clone());

                self.snapshot
                    .in_progress_tasks
                    .retain(|(id, _, _)| id != task_id);

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

    /// Simulate creating a task (from reconcile_orphaned_prs).
    fn create_task(&mut self, subject: &str, pr: Option<u64>) {
        let task_id = format!("harness-{}", self.snapshot.all_tasks.len() + 1);
        let task = Task {
            id: task_id,
            subject: subject.to_string(),
            status: TaskStatus::Pending,
            owner: None,
            description: None,
            blocked_by: vec![],
            channel: None,
            pr,
            created_at: None,
        };
        self.snapshot.all_tasks.push(task.clone());
        self.snapshot.pending_tasks_without_owners.push(task);

        // Track the PR → task association so reconcile_orphaned_prs
        // won't create a duplicate task for the same PR on the next tick
        if let Some(pr_number) = pr {
            self.snapshot.pr_task_associations.insert(
                pr_number,
                format!("harness-{}", self.snapshot.all_tasks.len()),
            );
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
