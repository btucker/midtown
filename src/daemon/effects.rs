use tracing::{info, warn};

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
    SpawnCoworker(crate::tmux::ClaudeLaunchConfig),
    /// Shut down a running coworker with a message.
    ShutdownCoworker { name: String, message: String },
    /// Nudge a coworker by sending a message to their tmux pane.
    NudgeCoworker { name: String, message: String },
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
    /// Kill a zombie coworker (blank pane) and respawn with --continue.
    RespawnZombieCoworker { name: String },
    /// Spawn a coworker with conditional follow-up effects.
    ///
    /// On success, `on_success` effects are executed. On failure, `on_failure`
    /// effects are executed. This allows decision functions to express
    /// spawn-dependent branching as data without calling spawn inline.
    SpawnCoworkerWithCallbacks {
        config: crate::tmux::ClaudeLaunchConfig,
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
    /// Records an in-memory task assignment for busy tracking but does NOT
    /// write ownership to disk. The coworker claims the task after starting
    /// via `midtown task claim`, which nudges the Lead to set ownership
    /// through TaskUpdate.
    AssignAndSpawn {
        task_id: String,
        owner: String,
        #[allow(dead_code)]
        repo_name: String,
        config: crate::tmux::ClaudeLaunchConfig,
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
    /// Nudge the Lead with a message (via tmux send-keys to the lead pane).
    NudgeLead { message: String },
    /// Directly write task ownership to disk as a fallback.
    ///
    /// Used when the Lead fails to process a task.claim nudge after max retries.
    /// Sets the task owner on disk. The in-memory assignment already exists.
    AssignTaskOwnerDirect { task_id: String, owner: String },
    /// Increment the nudge retry counter for a stale claim assignment.
    IncrementClaimRetry { coworker: String },
    /// Clear a saved PR break session after successful resume.
    ClearPrBreakSession { name: String },
    /// Send raw tmux keys to a coworker (e.g., Escape, Enter) without the
    /// nudge text mechanism. Used for recovering stuck states like compaction
    /// whirlpools or queued prompts.
    SendRawKeys { name: String, keys: String },
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
    /// Create a new task for a PR author to address review feedback.
    ///
    /// Emitted when a PR has CI green + review feedback but no existing task
    /// for the author. Prevents the spawn→idle→break loop by giving the author
    /// concrete work to do.
    CreateReviewFeedbackTask {
        pr_number: u64,
        pr_title: String,
        owner: String,
        repo_name: String,
    },
}

/// Execute a list of effects against the daemon state.
///
/// This is the imperative shell — the only place where side effects happen.
/// Each effect variant maps to a call on `DaemonState` or its subsystems.
pub async fn execute_effects(effects: Vec<Effect>, state: &DaemonState) {
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
                // Log the shutdown with current window state for debugging
                let session = state.coworkers.session_name();
                let windows = crate::tmux::list_windows(session).unwrap_or_default();
                info!(
                    coworker = %name,
                    session = %session,
                    window_count = windows.len(),
                    windows = ?windows,
                    message_preview = %message.chars().take(50).collect::<String>(),
                    "SHUTDOWN_COWORKER: executing shutdown effect"
                );

                // Nudge the goodbye message first, then shut down
                if !message.is_empty()
                    && let Err(e) = state.coworkers.nudge(&name, &message)
                {
                    warn!("Failed to send shutdown message to {}: {}", name, e);
                }
                if let Err(e) = state.coworkers.shutdown(&name) {
                    warn!("Failed to shut down coworker {}: {}", name, e);
                } else {
                    info!(coworker = %name, "SHUTDOWN_COWORKER: shutdown completed");
                }
                // Record stop time for workflow features that need to track coworker lifecycle
                {
                    let mut stop_times = state.coworker_stop_times.write().unwrap();
                    stop_times.insert(name.to_lowercase(), chrono::Utc::now());
                }
                // Clear state file so next session doesn't read stale phase
                crate::coworker_state::clear_state(&state.repo_name, &name);
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
            }
            Effect::NudgeCoworker { name, message } => {
                match state.coworkers.nudge(&name, &message) {
                    Ok(()) => {
                        // Record pending nudge for attribution tracking
                        state.record_pending_nudge(&name, &message);
                    }
                    Err(e) => {
                        warn!("Failed to nudge coworker {}: {}", name, e);
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
                    warn!("Failed to reset task #{} to pending: {}", task_id, e);
                }
                // Clear task assignment tracking (task is no longer assigned)
                state.clear_task_assignment_by_task(&task_id);
            }
            Effect::RespawnZombieCoworker { name } => {
                // Shut down properly (kills window + removes from internal registry)
                if let Err(e) = state.coworkers.shutdown(&name) {
                    warn!("Failed to shutdown zombie coworker {}: {}", name, e);
                }
                // Record stop time for workflow features that need to track coworker lifecycle
                {
                    let mut stop_times = state.coworker_stop_times.write().unwrap();
                    stop_times.insert(name.to_lowercase(), chrono::Utc::now());
                }
                // Clear state file so respawned session doesn't read stale phase
                crate::coworker_state::clear_state(&state.repo_name, &name);
                // Clean up unified coworker record
                {
                    let mut records = state.coworker_records.write().await;
                    records.remove(&name);
                }
                // Clear cooldown entries for this coworker (prevents stale state on respawn)
                {
                    let mut cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.clear_for_key(&name);
                }
                // Clear any pending nudge for this coworker (prevents stale attribution)
                state.clear_pending_nudge(&name);
                // Brief delay to let tmux clean up
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                // Respawn with --continue to resume the coworker's conversation
                let config = crate::tmux::ClaudeLaunchConfig::coworker(
                    name.clone(),
                    state.repo_name.clone(),
                    crate::tmux::SessionMode::Resume,
                    None,
                );
                match state.spawn_coworker(&config).await {
                    Ok(_) => {
                        info!("Respawned zombie coworker {} successfully", name);
                    }
                    Err(e) => {
                        warn!("Failed to respawn zombie coworker {}: {}", name, e);
                    }
                }
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
            } => match state.coworkers.nudge(&name, &message) {
                Ok(()) => {
                    info!("Nudged coworker {} successfully", name);
                    // Record pending nudge for attribution tracking
                    state.record_pending_nudge(&name, &message);
                    Box::pin(execute_effects(on_success, state)).await;
                }
                Err(e) => {
                    warn!("Failed to nudge coworker {}: {}", name, e);
                }
            },
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
                // which nudges the Lead to confirm ownership via TaskUpdate.
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
                                "Failed to set task #{} owner to {} after spawn: {}",
                                task_id, owner, e
                            );
                        }
                        // Transition task from pending to in_progress now that the coworker is running
                        if let Err(e) =
                            crate::tasks::set_task_in_progress_for_repo(&task_id, &repo_name)
                        {
                            warn!(
                                "Failed to set task #{} to in_progress after spawn: {}",
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
            Effect::NudgeLead { message } => {
                if let Err(e) = state.coworkers.nudge_lead(&message) {
                    warn!("Failed to nudge Lead: {}", e);
                }
            }
            Effect::AssignTaskOwnerDirect { task_id, owner } => {
                if let Err(e) = crate::tasks::update_task_owner(&task_id, &owner) {
                    warn!(
                        "Failed to directly assign task #{} to {}: {}",
                        task_id, owner, e
                    );
                } else {
                    info!(
                        "Directly assigned task #{} to {} (Lead nudge fallback)",
                        task_id, owner
                    );
                }
            }
            Effect::IncrementClaimRetry { coworker } => {
                state.increment_claim_retry(&coworker);
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
            Effect::SendRawKeys { name, keys } => {
                if let Err(e) =
                    crate::tmux::send_keys_raw(state.coworkers.session_name(), &name, &keys)
                {
                    warn!("Failed to send raw keys to {}: {}", name, e);
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
                    warn!("Failed to complete task #{}: {}", task_id, e);
                } else {
                    info!("Auto-completed task #{}", task_id);
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
                        "Failed to clear blockedBy for task #{}: {}",
                        completed_task_id, e
                    );
                } else {
                    info!(
                        "Cleared blockedBy references to completed task #{}",
                        completed_task_id
                    );
                }
            }
            Effect::CreateReviewFeedbackTask {
                pr_number,
                pr_title,
                owner,
                repo_name,
            } => {
                let subject = format!("Address review feedback on PR #{}", pr_number);
                let description = format!(
                    "PR #{} ({}) has review feedback that needs to be addressed.\n\n\
                     Please review the comments, make the requested changes, and push updates.\n\
                     Once feedback is addressed, the reviewer will re-check and approve.",
                    pr_number, pr_title
                );
                let active_form = format!("Addressing review feedback on PR #{}", pr_number);
                match crate::tasks::create_task_for_repo(
                    &subject,
                    &description,
                    &active_form,
                    &owner,
                    &repo_name,
                ) {
                    Ok(task_id) => {
                        info!(
                            "Created review feedback task #{} for {} (PR #{})",
                            task_id, owner, pr_number
                        );
                    }
                    Err(e) => {
                        warn!(
                            "Failed to create review feedback task for {} (PR #{}): {}",
                            owner, pr_number, e
                        );
                    }
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
