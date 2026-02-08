use tracing::{debug, info, warn};

use super::DaemonState;
use super::trackers::PrIssueType;
use crate::message::Message;

/// A side effect that the daemon should execute.
///
/// Pure evaluation functions return `Vec<Effect>` instead of performing side
/// effects inline. The `execute_effects` function is the single place where
/// effects are carried out. This separation makes the decision logic testable
/// without mocking async infrastructure.
#[derive(Debug)]
pub enum Effect {
    /// Spawn a coworker using a typed launch configuration.
    SpawnCoworker(crate::launch::LaunchConfig),
    /// Shut down a running coworker with a message.
    ShutdownCoworker { name: String, message: String },
    /// Nudge a coworker by sending a message to their headless session.
    NudgeCoworker { name: String, message: String },
    /// Nudge the Lead by sending a message to their tmux pane.
    NudgeLead { message: String },
    /// Resume a stopped headless coworker session.
    ///
    /// Uses `SessionManager::spawn` with `SessionMode::ResumeSession` to
    /// restart a coworker from a previously saved session ID.
    #[allow(dead_code)]
    ResumeCoworker {
        name: String,
        session_id: String,
        config: crate::launch::LaunchConfig,
    },
    /// Deliver a message to a coworker via the agent teams mailbox.
    ///
    /// Uses the filesystem-based inbox (`~/.claude/teams/{team}/inboxes/{name}.json`)
    /// for non-urgent messages like task assignments and PR feedback. The coworker
    /// polls its inbox between turns, so delivery is not immediate but avoids the
    /// terminal corruption risks of tmux send-keys.
    ///
    /// Phase 1: Used alongside tmux nudges for task assignment to idle coworkers.
    /// As mailbox reliability is confirmed, more nudge paths can migrate here.
    DeliverMailboxMessage {
        name: String,
        message: String,
        summary: Option<String>,
    },
    /// Post a message to the IRC-style channel (and broadcast to WebSocket clients).
    PostToChannel { sender: String, message: String },
    /// Post a system message to the channel (and broadcast to WebSocket clients).
    PostSystemMessage { message: String },
    /// Broadcast a coworker status update to WebSocket clients.
    BroadcastCoworkerUpdate {
        name: String,
        status: String,
        current_task: Option<String>,
    },
    /// Record a cooldown entry (category + key).
    RecordCooldown { category: String, key: String },
    /// Schedule a usage-limit nudge at a specific time.
    SetUsageLimitNudge { at: tokio::time::Instant },
    /// Clear the scheduled usage-limit nudge (after it fires).
    ClearUsageLimitNudge,
    /// Reset a task back to pending (e.g. when a coworker can't be respawned).
    ResetTaskToPending { task_id: String, repo_name: String },
    /// Spawn a coworker with conditional follow-up effects.
    ///
    /// On success, `on_success` effects are executed. On failure, `on_failure`
    /// effects are executed. This allows decision functions to express
    /// spawn-dependent branching as data without calling spawn inline.
    SpawnCoworkerWithCallbacks {
        config: crate::launch::LaunchConfig,
        on_success: Vec<Effect>,
        on_failure: Vec<Effect>,
    },
    /// Nudge a coworker with conditional follow-up effects on success.
    ///
    /// On success, `on_success` effects are executed. On failure, nothing extra
    /// happens (the nudge failure is logged). This allows decision functions to
    /// record cooldowns only when nudges succeed.
    NudgeCoworkerWithCallbacks {
        name: String,
        message: String,
        on_success: Vec<Effect>,
    },
    /// Spawn a coworker for a pending task.
    ///
    /// Records an in-memory task assignment for busy tracking and writes
    /// ownership + in_progress status directly to disk.
    AssignAndSpawn {
        task_id: String,
        owner: String,
        #[allow(dead_code)]
        repo_name: String,
        config: crate::launch::LaunchConfig,
        on_success: Vec<Effect>,
        on_failure: Vec<Effect>,
    },
    /// Mark reminders as fired and persist to disk.
    ///
    /// Defers the mutation from the decision phase to the effect executor,
    /// keeping `check_and_fire_reminders` pure.
    MarkRemindersFired {
        fired_ids: Vec<String>,
        repo_name: String,
    },
    /// Record a PR issue nudge in the tracker (prevents repeated nudges).
    RecordPrNudge {
        pr_number: u64,
        issue_type: PrIssueType,
    },
    /// Record an in-memory task assignment for busy tracking.
    ///
    /// Defers the mutation from the decision phase to the effect executor,
    /// keeping decision functions pure.
    RecordTaskAssignment { coworker: String, task_id: String },
    /// Clear a saved PR break session after successful resume.
    ClearPrBreakSession { name: String },
    /// Assign a reviewer to a PR in github_state and persist.
    AssignReviewer {
        pr_number: u64,
        reviewer_name: String,
        source: crate::github_state::AssignmentSource,
    },
    /// Clear reviewer assignments for orphaned coworkers (sessions that ended unexpectedly).
    ClearOrphanedReviewerAssignments { orphaned_coworkers: Vec<String> },
    /// Re-run a GitHub Actions workflow that appears to be stuck.
    ///
    /// Used when a CI check has been pending for > 4x its typical duration.
    ///
    /// Used when a CI check has been pending for > 4x its typical duration.
    RerunWorkflow {
        run_id: u64,
        check_name: String,
        pr_number: u64,
    },
    /// Rebase a PR on main to pick up workflow changes.
    ///
    /// Used when a PR is missing a required CI check because it predates
    /// a workflow change. Rebasing pulls in the new workflow definition.
    /// TODO: Implement missing check detection logic.
    #[allow(dead_code)]
    RebasePrOnMain { pr_number: u64, reason: String },
    /// Store a PR author's session ID for potential handoff.
    ///
    /// When a coworker opens a PR, we store their session ID so any other
    /// coworker can later resume work on that PR with full context preserved.
    StorePrAuthorSession {
        pr_number: u64,
        session_id: String,
        branch: String,
        author: String,
    },
    /// Mark a task as completed (e.g., when its PR is opened).
    ///
    /// Called when a PR is opened with `[Midtown #XX]` in the title.
    CompleteTask { task_id: String, repo_name: String },
    /// Clear a completed task ID from all dependent tasks' `blockedBy` arrays.
    ///
    /// Called after a task is completed to unblock dependent tasks.
    ClearBlockedBy {
        completed_task_id: String,
        repo_name: String,
    },
    /// Force-delete orphaned worktrees whose PRs were merged (squash-merge).
    ///
    /// These worktrees appear to have "unmerged commits" because the squash-merge
    /// changed commit SHAs, but the work is already in main. Safe to force-remove.
    ForceCleanupWorktrees { names: Vec<String> },
    /// Send a push notification to the mobile PWA.
    ///
    /// Fire-and-forget: the push manager runs in a background task.
    SendPushNotification {
        title: String,
        body: String,
        tag: String,
    },
    /// Clean up stale local branches that match coworker naming patterns
    /// and are already merged into the default branch.
    ///
    /// Catches branches left behind after worktree removal.
    CleanStaleBranches,
    /// Clean up a task-based worktree after its PR is merged.
    ///
    /// Looks up the worktree in the registry by PR number or branch name,
    /// removes it from the registry, and deletes the worktree directory.
    CleanupMergedWorktree { pr_number: u64, branch: String },
    /// Ensure a task-based worktree exists at the specified path.
    ///
    /// Creates the worktree if it doesn't exist, or succeeds idempotently
    /// if it already exists. Must be executed BEFORE SpawnCoworker effects
    /// that depend on the worktree.
    ///
    /// This follows the effect-based architecture: worktree creation is a
    /// side effect that must go through the Effect pipeline, not happen
    /// inline in spawn_coworker().
    EnsureWorktree {
        worktree_id: String,
        path: std::path::PathBuf,
    },
    /// Bind a coworker to a worktree in the registry.
    ///
    /// Called when a coworker is assigned to work in an existing task-based
    /// worktree. Updates the registry's reverse indexes.
    BindCoworkerToWorktree {
        worktree_id: String,
        coworker: String,
    },
    /// Unbind a coworker from their worktree in the registry.
    ///
    /// Called when a coworker is shut down. The worktree persists for reuse
    /// by the next coworker assigned to the same task.
    #[allow(dead_code)]
    UnbindCoworkerFromWorktree { coworker: String },
    /// Register a new task-based worktree assignment in the registry.
    ///
    /// Called during task dispatch when a new worktree is allocated for a task.
    RegisterWorktreeAssignment {
        assignment: crate::worktree_registry::WorktreeAssignment,
    },
    /// Set the PR number for a worktree in the registry.
    ///
    /// Called when a coworker opens a PR, linking the worktree to the PR
    /// for automatic cleanup on merge.
    #[allow(dead_code)]
    SetWorktreePrNumber { worktree_id: String, pr_number: u64 },
}

/// Deduplicate nudge effects targeting the same coworker within a single batch.
///
/// When multiple PR issue types (CI green, review complete, merge conflict)
/// each generate a nudge for the same coworker in one tick, only the first
/// nudge is kept. For `NudgeCoworkerWithCallbacks`, subsequent nudges' `on_success`
/// callbacks are merged into the first nudge's callbacks so state recording
/// (e.g., `RecordPrNudge`, `RecordTaskAssignment`) still happens.
///
/// Plain `NudgeCoworker` effects for already-nudged coworkers are dropped entirely.
fn dedup_nudge_effects(effects: Vec<Effect>) -> Vec<Effect> {
    use std::collections::HashSet;

    let mut nudged_coworkers: HashSet<String> = HashSet::new();
    let mut result: Vec<Effect> = Vec::with_capacity(effects.len());

    for effect in effects {
        match effect {
            Effect::NudgeCoworker { ref name, .. } => {
                let key = name.to_lowercase();
                if nudged_coworkers.contains(&key) {
                    debug!(
                        "Deduplicating NudgeCoworker for {} (already nudged in this batch)",
                        name
                    );
                    continue;
                }
                nudged_coworkers.insert(key);
                result.push(effect);
            }
            Effect::NudgeCoworkerWithCallbacks {
                ref name,
                message,
                on_success,
            } => {
                let key = name.to_lowercase();
                if nudged_coworkers.contains(&key) {
                    debug!(
                        "Deduplicating NudgeCoworkerWithCallbacks for {} — \
                         executing on_success callbacks without re-nudging",
                        name
                    );
                    // Merge on_success into the existing nudge's callbacks.
                    // Find the first NudgeCoworkerWithCallbacks for this coworker
                    // and append the callbacks there.
                    let remaining = merge_callbacks_into_existing(&mut result, &key, on_success);
                    if let Some(unmerged) = remaining {
                        // First nudge was a plain NudgeCoworker — promote the
                        // callbacks to standalone effects. These include state-tracking
                        // effects like RecordPrNudge that must fire to prevent the
                        // same nudge from triggering again on the next tick.
                        result.extend(unmerged);
                    }
                    continue;
                }
                nudged_coworkers.insert(key);
                result.push(Effect::NudgeCoworkerWithCallbacks {
                    name: name.clone(),
                    message,
                    on_success,
                });
            }
            _ => {
                result.push(effect);
            }
        }
    }

    result
}

/// Merge `on_success` callbacks into an existing `NudgeCoworkerWithCallbacks` effect
/// for the same coworker. Returns `None` if merged successfully, or `Some(callbacks)`
/// if no matching effect was found (e.g., first nudge was a plain `NudgeCoworker`).
fn merge_callbacks_into_existing(
    effects: &mut [Effect],
    target_key: &str,
    additional_callbacks: Vec<Effect>,
) -> Option<Vec<Effect>> {
    for effect in effects.iter_mut() {
        if let Effect::NudgeCoworkerWithCallbacks {
            name, on_success, ..
        } = effect
            && name.to_lowercase() == target_key
        {
            on_success.extend(additional_callbacks);
            return None;
        }
    }
    Some(additional_callbacks)
}

/// Execute a list of effects against the daemon state.
///
/// This is the imperative shell — the only place where side effects happen.
/// Each effect variant maps to a call on `DaemonState` or its subsystems.
///
/// Before execution, nudge effects targeting the same coworker are deduplicated
/// to prevent rapid-fire nudges within a single tick (e.g., when CI green,
/// review complete, and merge conflict each independently nudge the same coworker).
pub async fn execute_effects(effects: Vec<Effect>, state: &DaemonState) {
    let effects = dedup_nudge_effects(effects);
    for effect in effects {
        match effect {
            Effect::SpawnCoworker(config) => {
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                    }
                }
            }
            Effect::ShutdownCoworker { name, message } => {
                info!(
                    coworker = %name,
                    message_preview = %message.chars().take(50).collect::<String>(),
                    "SHUTDOWN_COWORKER: executing shutdown effect"
                );

                // Send goodbye message via headless stdin, then shut down the session
                if !message.is_empty()
                    && let Err(e) = state.session_manager.send_message(&name, &message).await
                {
                    warn!("Failed to send shutdown message to {}: {}", name, e);
                }
                if let Err(e) = state.session_manager.shutdown(&name).await {
                    warn!("Failed to shut down headless session {}: {}", name, e);
                } else {
                    info!(coworker = %name, "SHUTDOWN_COWORKER: headless session stopped");
                }
                // Remove from CoworkerManager tracking (without touching tmux)
                state.coworkers.deregister(&name);
                // Record stop time for workflow features that need to track coworker lifecycle
                {
                    let mut stop_times = state.coworker_stop_times.write().unwrap();
                    stop_times.insert(name.to_lowercase(), chrono::Utc::now());
                }
                // Clean up unified coworker record (health, workflow phase, etc.)
                {
                    let mut records = state.coworker_records.write().await;
                    records.remove(&name);
                }
                // Clear cooldown entries for this coworker (prevents stale state on respawn)
                {
                    let mut cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.clear_for_key(&name);
                }
                // Clear any pending nudge for this coworker
                state.clear_pending_nudge(&name);
                // Clear task assignment tracking (coworker is no longer active)
                state.clear_coworker_assignments(&name);
                // Unbind from worktree registry (worktree persists for build cache reuse)
                {
                    let mut ps = state.persistent_state.lock().await;
                    ps.worktree_registry.unbind_coworker(&name);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!(
                            "Failed to save daemon state after unbinding coworker: {}",
                            e
                        );
                    }
                }
            }
            Effect::NudgeCoworker { name, message } => {
                match state.session_manager.send_message(&name, &message).await {
                    Ok(()) => {
                        // Record pending nudge for attribution tracking
                        state.record_pending_nudge(&name, &message);
                    }
                    Err(e) => {
                        warn!("Failed to nudge coworker {}: {}", name, e);
                    }
                }
            }
            Effect::NudgeLead { message } => {
                if let Err(e) = state.coworkers.nudge_lead(&message) {
                    warn!("Failed to nudge Lead: {}", e);
                }
            }
            Effect::ResumeCoworker {
                name,
                session_id,
                mut config,
            } => {
                // Override session mode to resume the saved session
                config.session_mode = crate::launch::SessionMode::ResumeSession(session_id);
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Resumed coworker {} successfully", name);
                    }
                    Err(e) => {
                        warn!("Failed to resume coworker {}: {}", name, e);
                    }
                }
            }
            Effect::DeliverMailboxMessage {
                name,
                message,
                summary,
            } => {
                let team_name = crate::mailbox::team_name_for_repo(&state.repo_name);
                let mut msg = crate::mailbox::MailboxMessage::new(&message, "midtown")
                    .with_color("yellow".to_string());
                if let Some(s) = summary {
                    msg = msg.with_summary(s);
                }
                match crate::mailbox::write_to_inbox(&team_name, &name, msg) {
                    Ok(()) => {
                        debug!("Delivered mailbox message to {}", name);
                    }
                    Err(e) => {
                        warn!(
                            "Failed to deliver mailbox message to {}: {} — falling back to headless stdin",
                            name, e
                        );
                        // Fallback: try headless send_message as a last resort
                        if let Err(nudge_err) =
                            state.session_manager.send_message(&name, &message).await
                        {
                            warn!(
                                "Fallback headless nudge also failed for {}: {}",
                                name, nudge_err
                            );
                        }
                    }
                }
            }
            Effect::PostToChannel { sender, message } => {
                let msg = Message::text(&sender, &message);
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post channel message: {}", e);
                }
            }
            Effect::BroadcastCoworkerUpdate {
                name,
                status,
                current_task,
            } => {
                state.broadcast_coworker_update(&name, &status, current_task.as_deref());
            }
            Effect::RecordCooldown { category, key } => {
                let mut cooldowns = state.cooldowns.lock().unwrap();
                cooldowns.record(&category, &key);
            }
            Effect::SetUsageLimitNudge { at } => {
                let mut nudge_at = state.usage_limit_nudge_at.lock().await;
                *nudge_at = Some(at);
            }
            Effect::ClearUsageLimitNudge => {
                let mut nudge_at = state.usage_limit_nudge_at.lock().await;
                *nudge_at = None;
            }
            Effect::ResetTaskToPending { task_id, repo_name } => {
                if let Err(e) = crate::tasks::reset_task_to_pending_for_repo(&task_id, &repo_name) {
                    warn!("Failed to reset task !{} to pending: {}", task_id, e);
                }
                // Clear task assignment tracking (task is no longer assigned)
                state.clear_task_assignment_by_task(&task_id);
            }
            Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            } => {
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                        // Recursively execute success follow-ups
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                        // Recursively execute failure follow-ups
                        Box::pin(execute_effects(on_failure, state)).await;
                    }
                }
            }
            Effect::NudgeCoworkerWithCallbacks {
                name,
                message,
                on_success,
            } => {
                // Extract task IDs from on_success RecordTaskAssignment effects
                // to clear their in-flight markers after the nudge completes.
                let task_ids: Vec<String> = on_success
                    .iter()
                    .filter_map(|e| {
                        if let Effect::RecordTaskAssignment { task_id, .. } = e {
                            Some(task_id.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                match state.session_manager.send_message(&name, &message).await {
                    Ok(()) => {
                        info!("Nudged coworker {} successfully", name);
                        // Record pending nudge for attribution tracking
                        state.record_pending_nudge(&name, &message);
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(e) => {
                        warn!("Failed to nudge coworker {}: {}", name, e);
                    }
                }
                // Clear in-flight markers regardless of success/failure,
                // so these tasks can be retried on the next tick if needed.
                for task_id in &task_ids {
                    state.clear_task_spawn_in_flight(task_id);
                }
            }
            Effect::AssignAndSpawn {
                task_id,
                owner,
                repo_name,
                config,
                on_success,
                on_failure,
            } => {
                // Spawn the coworker and set ownership + in_progress on disk.
                // The coworker also claims the task via `midtown task claim` after starting,
                // which writes ownership directly via the daemon.
                let name = config.name.clone();
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Spawned coworker {} successfully", name);
                        // Clear in-flight marker on success
                        state.clear_task_spawn_in_flight(&task_id);
                        // Record task assignment in-memory for busy tracking
                        state.record_task_assignment(&owner, &task_id);
                        // Set task owner on disk so status and owner are consistent
                        if let Err(e) = crate::tasks::update_task_owner(&task_id, &owner) {
                            warn!(
                                "Failed to set task !{} owner to {} after spawn: {}",
                                task_id, owner, e
                            );
                        }
                        // Transition task from pending to in_progress now that the coworker is running
                        if let Err(e) =
                            crate::tasks::set_task_in_progress_for_repo(&task_id, &repo_name)
                        {
                            warn!(
                                "Failed to set task !{} to in_progress after spawn: {}",
                                task_id, e
                            );
                        }
                        Box::pin(execute_effects(on_success, state)).await;
                    }
                    Err(e) => {
                        warn!("Failed to spawn coworker {}: {}", name, e);
                        // Clear in-flight marker on failure (no disk rollback needed)
                        state.clear_task_spawn_in_flight(&task_id);
                        Box::pin(execute_effects(on_failure, state)).await;
                    }
                }
            }
            Effect::MarkRemindersFired {
                fired_ids,
                repo_name,
            } => {
                let mut ps = state.persistent_state.lock().await;
                for reminder in &mut ps.reminders.reminders {
                    if fired_ids.contains(&reminder.id) {
                        reminder.fired = true;
                    }
                }
                if let Err(e) = ps.save_for_repo(&repo_name) {
                    warn!(
                        "Failed to save daemon-state.json after firing reminders: {}",
                        e
                    );
                }
            }
            Effect::RecordPrNudge {
                pr_number,
                issue_type,
            } => {
                let mut tracker = state.pr_issue_tracker.lock().await;
                tracker.record_nudge(pr_number, issue_type);
            }
            Effect::RecordTaskAssignment { coworker, task_id } => {
                state.record_task_assignment(&coworker, &task_id);
            }
            Effect::ClearPrBreakSession { name } => {
                let mut sessions = state.pr_break_sessions.write().unwrap();
                sessions.remove(&name);
                info!("Cleared PR break session for {}", name);
            }
            Effect::AssignReviewer {
                pr_number,
                reviewer_name,
                source,
            } => {
                let mut ps = state.persistent_state.lock().await;
                ps.github.assign_reviewer(pr_number, &reviewer_name, source);
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!("Failed to save daemon-state.json: {}", e);
                }
            }
            Effect::PostSystemMessage { message } => {
                let msg = Message::system(message);
                if let Err(e) = state.send_and_broadcast(&msg) {
                    warn!("Failed to post system message: {}", e);
                }
            }
            Effect::ClearOrphanedReviewerAssignments { orphaned_coworkers } => {
                if orphaned_coworkers.is_empty() {
                    continue;
                }
                let mut cleared_count = 0;
                let mut ps = state.persistent_state.lock().await;
                for name in &orphaned_coworkers {
                    if let Some(assignment) = ps.github.remove_assignment_by_reviewer(name) {
                        info!(
                            "Cleared stale reviewer assignment: {} was reviewing PR #{}",
                            name, assignment.pr_number
                        );
                        cleared_count += 1;
                    }
                }
                if cleared_count > 0
                    && let Err(e) = ps.save_for_repo(&state.repo_name)
                {
                    warn!(
                        "Failed to save daemon-state.json after clearing orphan reviewer assignments: {}",
                        e
                    );
                }
            }
            Effect::RerunWorkflow {
                run_id,
                check_name,
                pr_number,
            } => {
                rerun_workflow(state, run_id, &check_name, pr_number).await;
            }
            Effect::RebasePrOnMain { pr_number, reason } => {
                rebase_pr_on_main(state, pr_number, &reason).await;
            }
            Effect::StorePrAuthorSession {
                pr_number,
                session_id,
                branch,
                author,
            } => {
                let mut ps = state.persistent_state.lock().await;
                ps.github
                    .store_pr_author_session(pr_number, &session_id, &branch, &author);
                // Link the PR to the worktree if one exists for this coworker
                if let Some(assignment) = ps.worktree_registry.get_by_coworker(&author) {
                    let wt_id = assignment.worktree_id.clone();
                    ps.worktree_registry.set_pr_number(&wt_id, pr_number);
                    debug!(
                        "Linked PR #{} to worktree {} (author: {})",
                        pr_number, wt_id, author
                    );
                }
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!("Failed to persist PR author session: {}", e);
                } else {
                    info!(
                        "Stored author session for PR #{}: session={}, author={}",
                        pr_number, session_id, author
                    );
                }
            }
            Effect::CompleteTask { task_id, repo_name } => {
                if let Err(e) = crate::tasks::complete_task_for_repo(&task_id, &repo_name) {
                    warn!("Failed to complete task !{}: {}", task_id, e);
                } else {
                    info!("Auto-completed task !{}", task_id);
                    // Clear task assignment tracking (coworker is now free)
                    state.clear_task_assignment_by_task(&task_id);
                }
            }
            Effect::ClearBlockedBy {
                completed_task_id,
                repo_name,
            } => {
                if let Err(e) =
                    crate::tasks::clear_blocked_by_for_repo(&completed_task_id, &repo_name)
                {
                    warn!(
                        "Failed to clear blockedBy for task !{}: {}",
                        completed_task_id, e
                    );
                } else {
                    info!(
                        "Cleared blockedBy references to completed task !{}",
                        completed_task_id
                    );
                }
            }
            Effect::ForceCleanupWorktrees { names } => {
                if names.is_empty() {
                    continue;
                }
                let coworkers = state.coworkers.clone();

                // Filter out names where headless sessions are still alive.
                // Check session_manager before entering spawn_blocking since is_alive is async.
                let mut to_cleanup = Vec::new();
                for name in names {
                    // Guard: check SessionManager directly to avoid a race where
                    // the headless session exists but hasn't yet registered in
                    // the daemon's coworkers map.
                    if state.session_manager.is_alive(&name).await {
                        warn!(
                            "Skipping cleanup of worktree for {} — headless session still running",
                            name
                        );
                        continue;
                    }
                    // Double-check: skip if coworker is registered in the manager
                    if coworkers.get(&name).is_some() {
                        warn!(
                            "Skipping cleanup of worktree for {} — coworker still registered",
                            name
                        );
                        continue;
                    }
                    to_cleanup.push(name);
                }

                if to_cleanup.is_empty() {
                    continue;
                }

                tokio::task::spawn_blocking(move || {
                    for name in to_cleanup {
                        match coworkers.force_cleanup_worktree(&name) {
                            Ok(()) => {
                                info!("Cleaned up orphaned worktree for {} (PR was merged)", name);
                            }
                            Err(e) => {
                                warn!("Failed to cleanup merged-PR worktree for {}: {}", name, e);
                            }
                        }
                    }
                })
                .await
                .ok();
            }
            Effect::SendPushNotification { title, body, tag } => {
                state.send_push_notification(&title, &body, &tag);
            }
            Effect::CleanStaleBranches => {
                let coworkers = state.coworkers.clone();
                let cleaned =
                    tokio::task::spawn_blocking(move || coworkers.clean_stale_coworker_branches())
                        .await
                        .unwrap_or_default();
                if !cleaned.is_empty() {
                    info!(
                        "Cleaned up {} stale coworker branch(es): {}",
                        cleaned.len(),
                        cleaned.join(", ")
                    );
                }
            }
            Effect::CleanupMergedWorktree { pr_number, branch } => {
                // Remove from registry
                let removed = {
                    let mut ps = state.persistent_state.lock().await;
                    let removed = ps.worktree_registry.cleanup_for_merged_pr(pr_number);
                    if removed.is_some()
                        && let Err(e) = ps.save_for_repo(&state.repo_name)
                    {
                        warn!("Failed to save daemon state after worktree cleanup: {}", e);
                    }
                    removed
                };
                if let Some(assignment) = removed {
                    // Remove the worktree directory using the primary worktree manager
                    let wt_mgr = state.coworkers.worktree_manager().clone();
                    let wt_id = assignment.worktree_id.clone();
                    tokio::task::spawn_blocking(move || {
                        if let Err(e) = wt_mgr.force_cleanup_task_worktree(&wt_id) {
                            warn!("Failed to remove task worktree {}: {}", wt_id, e);
                        } else {
                            info!(
                                "Cleaned up task worktree {} (PR #{} merged)",
                                wt_id, pr_number
                            );
                        }
                    })
                    .await
                    .ok();
                } else {
                    debug!(
                        "No worktree registered for PR #{} (branch: {}), skipping cleanup",
                        pr_number, branch
                    );
                }
            }
            Effect::BindCoworkerToWorktree {
                worktree_id,
                coworker,
            } => {
                let mut ps = state.persistent_state.lock().await;
                if let Err(e) = ps.worktree_registry.bind_coworker(&worktree_id, &coworker) {
                    warn!(
                        "Failed to bind {} to worktree {}: {}",
                        coworker, worktree_id, e
                    );
                } else {
                    debug!("Bound {} to worktree {}", coworker, worktree_id);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!("Failed to save daemon state after binding coworker: {}", e);
                    }
                }
            }
            Effect::UnbindCoworkerFromWorktree { coworker } => {
                let mut ps = state.persistent_state.lock().await;
                ps.worktree_registry.unbind_coworker(&coworker);
                debug!("Unbound {} from worktree", coworker);
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!(
                        "Failed to save daemon state after unbinding coworker: {}",
                        e
                    );
                }
            }
            Effect::EnsureWorktree { worktree_id, path } => {
                if path.exists() {
                    debug!(
                        "Worktree {} already exists at {}, reusing",
                        worktree_id,
                        path.display()
                    );
                } else {
                    info!("Creating worktree {} at {}", worktree_id, path.display());
                    if let Err(e) = state
                        .coworkers
                        .worktree_manager()
                        .create_task_worktree(&worktree_id)
                    {
                        warn!("Failed to create worktree {}: {}", worktree_id, e);
                    }
                }
            }
            Effect::RegisterWorktreeAssignment { assignment } => {
                let mut ps = state.persistent_state.lock().await;
                let wt_id = assignment.worktree_id.clone();
                if let Err(e) = ps.worktree_registry.assign_worktree(assignment) {
                    warn!("Failed to register worktree assignment {}: {}", wt_id, e);
                } else {
                    debug!("Registered worktree assignment {}", wt_id);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!(
                            "Failed to save daemon state after registering worktree: {}",
                            e
                        );
                    }
                }
            }
            Effect::SetWorktreePrNumber {
                worktree_id,
                pr_number,
            } => {
                let mut ps = state.persistent_state.lock().await;
                ps.worktree_registry.set_pr_number(&worktree_id, pr_number);
                debug!("Set PR #{} for worktree {}", pr_number, worktree_id);
                if let Err(e) = ps.save_for_repo(&state.repo_name) {
                    warn!("Failed to save daemon state after setting PR number: {}", e);
                }
            }
        }
    }
}

/// Re-run a GitHub Actions workflow using `gh run rerun`.
///
/// Posts a channel message on success or failure.
async fn rerun_workflow(state: &DaemonState, run_id: u64, check_name: &str, pr_number: u64) {
    // Record cooldown before attempting (to prevent rapid retries on failure)
    {
        let mut ps = state.persistent_state.lock().await;
        ps.ci_stats.record_rerun(run_id);
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            warn!("Failed to save CI stats after recording rerun: {}", e);
        }
    }

    let output = match tokio::process::Command::new("gh")
        .args(["run", "rerun", &run_id.to_string()])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            warn!("Failed to run gh run rerun for workflow {}: {}", run_id, e);
            return;
        }
    };

    if output.status.success() {
        info!(
            "Re-ran workflow {} (check '{}') for PR #{}",
            run_id, check_name, pr_number
        );
        let msg = Message::new(
            "midtown",
            format!(
                "🔄 Re-running stale CI check '{}' on PR #{} (workflow {})",
                check_name, pr_number, run_id
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post workflow rerun message: {}", e);
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "gh run rerun failed for workflow {}: {}",
            run_id,
            stderr.trim()
        );
    }
}

/// Rebase a PR on main to pick up workflow changes using `gh pr rebase`.
///
/// Posts a channel message on success or failure.
async fn rebase_pr_on_main(state: &DaemonState, pr_number: u64, reason: &str) {
    // Note: There isn't a direct `gh pr rebase` command. We'll use the Git approach
    // via the PR owner's branch. For now, we'll post a nudge to the PR owner instead
    // since rebasing requires pushing to their branch.
    //
    // Alternative: Use GitHub's update branch API if the repo allows it:
    // gh api repos/{owner}/{repo}/pulls/{pr}/update-branch -X PUT

    // Try using GitHub's update branch API first (requires repo to allow it)
    let output = match tokio::process::Command::new("gh")
        .args([
            "api",
            &format!("repos/{{owner}}/{{repo}}/pulls/{}/update-branch", pr_number),
            "-X",
            "PUT",
        ])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            warn!(
                "Failed to run gh api update-branch for PR #{}: {}",
                pr_number, e
            );
            return;
        }
    };

    if output.status.success() {
        info!("Updated PR #{} branch to include latest main", pr_number);
        let msg = Message::new(
            "midtown",
            format!(
                "🔄 Updated PR #{} to include latest main ({})",
                pr_number, reason
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post branch update message: {}", e);
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // If the API fails (e.g., branch protection), log it but don't spam
        info!(
            "Could not auto-update PR #{} branch (may need manual rebase): {}",
            pr_number,
            stderr.trim()
        );
    }
}

/// Auto-merge a PR using `gh pr merge --squash`.
///
/// Posts a channel message on success or failure.
/// TODO: Wire up to Effect::AutoMergePr when auto-merge logic is complete.
#[allow(dead_code)]
async fn auto_merge_pr(state: &DaemonState, pr_number: u64, title: &str) {
    use super::helpers::truncate_str;

    let output = match tokio::process::Command::new("gh")
        .args(["pr", "merge", &pr_number.to_string(), "--squash", "--auto"])
        .output()
        .await
    {
        Ok(output) => output,
        Err(e) => {
            warn!("Failed to run gh pr merge for PR #{}: {}", pr_number, e);
            return;
        }
    };

    if output.status.success() {
        info!("Auto-merge enabled for PR #{} ({})", pr_number, title);
        let msg = Message::new(
            "midtown",
            format!(
                "🤝 Auto-merge enabled for PR #{} ({}) — approved with all checks passing",
                pr_number,
                truncate_str(title, 40)
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post auto-merge message: {}", e);
        }
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!("gh pr merge failed for PR #{}: {}", pr_number, stderr);
        let msg = Message::new(
            "midtown",
            format!(
                "⚠️ Auto-merge failed for PR #{} ({}) — {}",
                pr_number,
                truncate_str(title, 40),
                truncate_str(stderr.trim(), 80)
            ),
            crate::message::MessageType::Text,
        );
        if let Err(e) = state.send_and_broadcast(&msg) {
            warn!("Failed to post auto-merge failure message: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::trackers::PrIssueType;

    /// Helper to count effects of a specific type.
    fn count_nudge_coworker(effects: &[Effect], name: &str) -> usize {
        effects
            .iter()
            .filter(|e| matches!(e, Effect::NudgeCoworker { name: n, .. } if n == name))
            .count()
    }

    fn count_nudge_with_callbacks(effects: &[Effect], name: &str) -> usize {
        effects
            .iter()
            .filter(
                |e| matches!(e, Effect::NudgeCoworkerWithCallbacks { name: n, .. } if n == name),
            )
            .count()
    }

    #[test]
    fn test_dedup_removes_duplicate_nudge_coworker() {
        let effects = vec![
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "first nudge".into(),
            },
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "second nudge".into(),
            },
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "third nudge".into(),
            },
        ];

        let deduped = dedup_nudge_effects(effects);
        assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
        // First message wins
        if let Effect::NudgeCoworker { message, .. } = &deduped[0] {
            assert_eq!(message, "first nudge");
        } else {
            panic!("Expected NudgeCoworker");
        }
    }

    #[test]
    fn test_dedup_removes_duplicate_nudge_with_callbacks() {
        let effects = vec![
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "CI green".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 42,
                    issue_type: PrIssueType::Approved,
                }],
            },
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "review complete".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 42,
                    issue_type: PrIssueType::ReviewComplete,
                }],
            },
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "merge conflict".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 42,
                    issue_type: PrIssueType::MergeConflict,
                }],
            },
        ];

        let deduped = dedup_nudge_effects(effects);
        assert_eq!(
            count_nudge_with_callbacks(&deduped, "riverside"),
            1,
            "Should collapse 3 nudges into 1"
        );
        // First message wins, but all callbacks are merged
        if let Effect::NudgeCoworkerWithCallbacks {
            message,
            on_success,
            ..
        } = &deduped[0]
        {
            assert_eq!(message, "CI green");
            assert_eq!(
                on_success.len(),
                3,
                "All three on_success callbacks should be merged"
            );
        } else {
            panic!("Expected NudgeCoworkerWithCallbacks");
        }
    }

    #[test]
    fn test_dedup_preserves_different_coworkers() {
        let effects = vec![
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "nudge riverside".into(),
            },
            Effect::NudgeCoworker {
                name: "broadway".into(),
                message: "nudge broadway".into(),
            },
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "duplicate riverside".into(),
            },
        ];

        let deduped = dedup_nudge_effects(effects);
        assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
        assert_eq!(count_nudge_coworker(&deduped, "broadway"), 1);
        assert_eq!(deduped.len(), 2);
    }

    #[test]
    fn test_dedup_mixed_nudge_types_promotes_callbacks() {
        // Plain NudgeCoworker first, then NudgeCoworkerWithCallbacks — the nudge
        // is deduped but on_success callbacks are promoted to standalone effects
        // so state tracking (RecordPrNudge) still fires.
        let effects = vec![
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "plain nudge".into(),
            },
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "callback nudge".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 42,
                    issue_type: PrIssueType::Approved,
                }],
            },
        ];

        let deduped = dedup_nudge_effects(effects);
        // 1 NudgeCoworker + 1 promoted RecordPrNudge callback
        assert_eq!(deduped.len(), 2);
        assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
        // Verify the RecordPrNudge callback was promoted as a standalone effect
        assert!(
            deduped
                .iter()
                .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 42, .. })),
            "RecordPrNudge callback should be promoted to standalone effect"
        );
    }

    #[test]
    fn test_dedup_preserves_non_nudge_effects() {
        let effects = vec![
            Effect::PostToChannel {
                sender: "midtown".into(),
                message: "hello".into(),
            },
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "nudge 1".into(),
            },
            Effect::RecordCooldown {
                category: "test".into(),
                key: "key".into(),
            },
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "nudge 2".into(),
            },
            Effect::PostToChannel {
                sender: "midtown".into(),
                message: "world".into(),
            },
        ];

        let deduped = dedup_nudge_effects(effects);
        // 1 nudge + 2 PostToChannel + 1 RecordCooldown = 4
        assert_eq!(deduped.len(), 4);
        assert_eq!(count_nudge_coworker(&deduped, "riverside"), 1);
    }

    #[test]
    fn test_dedup_case_insensitive() {
        let effects = vec![
            Effect::NudgeCoworker {
                name: "Riverside".into(),
                message: "nudge 1".into(),
            },
            Effect::NudgeCoworker {
                name: "riverside".into(),
                message: "nudge 2".into(),
            },
        ];

        let deduped = dedup_nudge_effects(effects);
        assert_eq!(deduped.len(), 1);
    }

    #[test]
    fn test_dedup_quadruple_nudge_scenario() {
        // Reproduces the exact bug: 4 nudges to same coworker in 1 second
        // from different PR issue sources.
        let effects = vec![
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "PR #181 - CI checks passed".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 181,
                    issue_type: PrIssueType::Approved,
                }],
            },
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "PR #181 - Review complete".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 181,
                    issue_type: PrIssueType::ReviewComplete,
                }],
            },
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "PR #181 - Merge conflict".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 181,
                    issue_type: PrIssueType::MergeConflict,
                }],
            },
            Effect::NudgeCoworkerWithCallbacks {
                name: "riverside".into(),
                message: "PR #181 - Green with feedback".into(),
                on_success: vec![Effect::RecordPrNudge {
                    pr_number: 181,
                    issue_type: PrIssueType::GreenWithFeedback,
                }],
            },
        ];

        let deduped = dedup_nudge_effects(effects);

        // Should have: 1 nudge (with merged callbacks)
        assert_eq!(
            count_nudge_with_callbacks(&deduped, "riverside"),
            1,
            "4 nudges should collapse into 1"
        );

        // The merged nudge should have all 4 on_success callbacks
        if let Effect::NudgeCoworkerWithCallbacks {
            on_success,
            message,
            ..
        } = &deduped[0]
        {
            assert_eq!(message, "PR #181 - CI checks passed", "First message wins");
            assert_eq!(on_success.len(), 4, "All 4 callbacks should be merged");
        } else {
            panic!("Expected NudgeCoworkerWithCallbacks");
        }
    }
}
