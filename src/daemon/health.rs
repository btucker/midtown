//! Health check functions for coworker lifecycle monitoring.
//!
//! These functions detect and respond to coworker health issues:
//! idle shutdown, stuck processes, usage limits, and reminder firing.
//! Health state is read from structured `ProcessHealth` data (populated
//! by the session management layer from headless stream events).

use std::path::PathBuf;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::daemon_messages;

use super::constants::*;
use super::effects::Effect;
use super::helpers::format_task_prompt;
use super::{DaemonState, snapshot};

/// Check for idle coworkers and send them on a break after the idle timeout.
///
/// A coworker is considered idle if they have no tasks in "in_progress" status
/// with their name as owner. After 30 seconds of continuous idle, they are
/// automatically sent on a break.
///
/// IMPORTANT: Coworkers are NEVER sent on a break if any of these apply:
/// - They are a channel lead (long-lived domain expert session, like "lead")
/// - They have open unmerged PRs (must stay available for review feedback)
/// - They have active review assignments
/// - They have unblocked dependent tasks
/// - They are usage-limited (waiting for usage limit reset)
/// - They have API errors (will be nudged to retry instead)
/// - They have auth errors (waiting for re-authentication)
///
/// Also enforces a minimum lifetime check - coworkers must be alive for at least
/// 5 minutes before they can be sent on a break. This prevents spawn storms where
/// coworkers are rapidly sent on breaks.
pub fn check_and_shutdown_idle_coworkers(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    debug!(
        "Idle shutdown check: active={}, busy=[{}], open_prs=[{}], reviewers=[{}], unblocked_deps=[{}]",
        snap.active_coworkers.len(),
        snap.busy_coworkers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        snap.coworkers_with_open_prs
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        snap.active_reviewers
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
        snap.coworkers_with_unblocked_deps
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", "),
    );

    // Pure decision: who should be shut down?
    let to_shutdown = {
        let channel_lead_names: std::collections::HashSet<String> =
            snap.channel_lead_sessions.keys().cloned().collect();
        let idle_ctx = crate::rules::IdleShutdownContext {
            coworkers: &snap.coworker_snapshots,
            busy_coworkers: &snap.busy_coworkers,
            coworkers_with_open_prs: &snap.coworkers_with_open_prs,
            active_reviewers: &snap.active_reviewers,
            coworkers_with_unblocked_deps: &snap.coworkers_with_unblocked_deps,
            ci_passed_pr_coworkers: &snap.ci_passed_pr_coworkers,
            usage_limited_coworkers: &snap.usage_limited_coworkers,
            api_error_coworkers: &snap.api_error_coworkers,
            auth_error_coworkers: &snap.auth_error_coworkers,
            pending_task_owners: &snap.pending_task_owners,
            review_feedback_pr_coworkers: &snap.review_feedback_pr_coworkers,
            channel_lead_names: &channel_lead_names,
            now_utc: snap.now_utc,
            minimum_lifetime: MINIMUM_COWORKER_LIFETIME,
        };
        crate::rules::decide_idle_shutdowns(&idle_ctx)
    };

    // Log all shutdown decisions for debugging the mass-shutdown issue
    if !to_shutdown.is_empty() {
        warn!(
            "IDLE_SHUTDOWN: {} coworkers flagged for shutdown: {:?}",
            to_shutdown.len(),
            to_shutdown.iter().map(|d| &d.name).collect::<Vec<_>>()
        );
        // Log protection state for each coworker being shut down
        for decision in &to_shutdown {
            let name = &decision.name;
            let is_busy = snap
                .busy_coworkers
                .iter()
                .any(|b| b.eq_ignore_ascii_case(name));
            let has_open_pr = snap
                .coworkers_with_open_prs
                .iter()
                .any(|c| c.eq_ignore_ascii_case(name));
            let is_reviewing = snap
                .active_reviewers
                .iter()
                .any(|r| r.eq_ignore_ascii_case(name));
            let ci_passed = snap
                .ci_passed_pr_coworkers
                .iter()
                .any(|c| c.eq_ignore_ascii_case(name));
            warn!(
                "IDLE_SHUTDOWN: {} - is_busy={}, has_open_pr={}, is_reviewing={}, ci_passed={}",
                name, is_busy, has_open_pr, is_reviewing, ci_passed,
            );
        }
    }

    let mut effects = Vec::new();

    // Determine effects for idle coworkers
    for decision in to_shutdown {
        let name = &decision.name;

        // For reviewers (identified by having a PR assignment), verify the review
        // was actually posted before shutting down. All other coworkers can be shut
        // down normally.
        let reviewer_pr = snap.reviewer_pr_assignments.get(name).copied();
        let (should_shutdown, shutdown_msg) = if let Some(pr) = reviewer_pr {
            // Check if review was actually posted (from snapshot, no API call)
            if snap.reviewed_prs.contains(&pr) {
                info!(
                    "Sending reviewer {} on a break (review verified for PR #{})",
                    name, pr
                );
                (true, daemon_messages::break_review_complete(name, pr))
            } else {
                warn!(
                    "Reviewer {} is idle but no review found for PR #{} - keeping alive",
                    name, pr
                );
                // Don't shutdown - post a warning to the ops channel so the team knows
                effects.push(Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "⚠️ Reviewer {} is idle but hasn't posted review for PR #{} yet",
                        name, pr
                    ),
                    channel: Some(OPS_CHANNEL.to_string()),
                });
                (false, String::new())
            }
        } else if snap.coworkers_with_merged_prs.contains(name) {
            info!("Sending idle coworker {} on a break (PR merged)", name);
            (true, daemon_messages::break_work_merged(name))
        } else {
            info!(
                "Sending idle coworker {} on a break (idle for 30+ seconds)",
                name
            );
            (true, daemon_messages::break_idle(name))
        };

        if !should_shutdown {
            continue;
        }

        // Post to ops channel, broadcast status, and shut down
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: shutdown_msg,
            channel: Some(OPS_CHANNEL.to_string()),
        });
        effects.push(Effect::BroadcastCoworkerUpdate {
            name: name.clone(),
            status: "stopped".to_string(),
            current_task: None,
        });
        // Prefer session-centric shutdown when session mapping exists.
        if let Some(session_id) = snap.name_session_map.get(name) {
            effects.push(Effect::ShutdownSession {
                session_id: session_id.clone(),
                reason: format!("idle shutdown: {}", name),
            });
        } else {
            effects.push(Effect::ShutdownCoworker {
                name: name.clone(),
                message: String::new(),
            });
        }
        // Clean the coworker's target/ directory to reclaim disk space.
        // Resolve working_dir from the snapshot so we target the actual
        // directory (task-based worktree), not the legacy coworker-named path.
        if let Some(cw) = snap
            .active_coworkers
            .iter()
            .find(|cw| cw.name.eq_ignore_ascii_case(name))
        {
            effects.push(Effect::CleanWorktreeTarget {
                name: name.clone(),
                working_dir: PathBuf::from(&cw.working_dir),
            });
        } else {
            debug!(
                "Coworker {} not found in snapshot, skipping target/ cleanup",
                name
            );
        }
    }

    effects
}

/// Detect coworkers whose headless process has not produced events for
/// `COWORKER_STUCK_DURATION`, kill them, and respawn with their current task.
///
/// Uses `ProcessHealth.last_event_at` from the headless session stream.
/// A coworker is stuck if it's alive but hasn't emitted any stream events
/// for the stuck duration, and it has an in-progress task.
pub(super) async fn check_and_restart_stuck_coworkers(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
        auth_error: &snap.auth_error_coworkers,
        attached: &snap.attached_coworkers,
    };
    let restarts = crate::rules::decide_stuck_coworker_restarts(
        &snap.headless_process_health,
        &snap.in_progress_tasks,
        &exemptions,
        snap.now_utc,
        COWORKER_STUCK_DURATION,
        &snap.name_session_map,
        &snap.coworker_start_times,
    );

    let mut effects = Vec::new();
    for restart in restarts {
        info!(
            "Coworker {} no events for {}s — restarting for task !{} (session: {:?})",
            restart.name,
            COWORKER_STUCK_DURATION.as_secs(),
            restart.task_id,
            restart.session_id
        );

        let prompt = format_task_prompt(
            &restart.task_id,
            &format!(
                "You've been assigned task !{}: {}. Your previous session appeared stuck so you were restarted. Check your git status and continue where you left off.",
                restart.task_id, restart.task_subject
            ),
        );

        // Look up the task's channel from the snapshot
        let channel = snap
            .all_tasks
            .iter()
            .find(|t| t.id == restart.task_id)
            .and_then(|t| t.channel.clone());

        let mut config = crate::launch::LaunchConfig::coworker(
            restart.name.clone(),
            state.repo_name.clone(),
            crate::launch::SessionMode::Fresh,
            Some(prompt),
        );
        config.channel = channel.clone();

        // Apply task model if available (sets both provider and model)
        config.apply_task_model(&snap.task_model_map, &restart.task_id);

        if let Some(session_id) = snap.name_session_map.get(&restart.name) {
            effects.push(Effect::ShutdownSession {
                session_id: session_id.clone(),
                reason: format!(
                    "stuck coworker: {} (task !{})",
                    restart.name, restart.task_id
                ),
            });
        } else {
            effects.push(Effect::ShutdownCoworker {
                name: restart.name.clone(),
                message: String::new(),
            });
        }
        effects.push(Effect::SpawnCoworker(config));
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔄 Restarted stuck coworker {} (no events for {}s) — resuming task !{}",
                restart.name,
                COWORKER_STUCK_DURATION.as_secs(),
                restart.task_id
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        });
    }

    effects
}

/// Detect reviewers whose headless process has been stuck (no events for
/// `REVIEWER_STUCK_DURATION`), kill them, and respawn with the same PR assignment.
///
/// Uses the same exclusion logic as task stuck detection but checks reviewer
/// PR assignments instead of in-progress tasks. Implements backoff via
/// `restart_count` — after `MAX_REVIEWER_RESTARTS`, posts an escalation
/// warning and stops retrying.
pub fn check_and_restart_stuck_reviewers(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
        auth_error: &snap.auth_error_coworkers,
        attached: &snap.attached_coworkers,
    };
    let restarts = crate::rules::decide_stuck_reviewer_restarts(
        &snap.headless_process_health,
        &snap.reviewer_pr_assignments,
        &snap.reviewer_restart_counts,
        &exemptions,
        snap.now_utc,
        REVIEWER_STUCK_DURATION,
        MAX_REVIEWER_RESTARTS,
        &snap.name_session_map,
        &snap.coworker_start_times,
    );

    let mut effects = Vec::new();
    for restart in restarts {
        let new_restart_count = restart.restart_count + 1;

        info!(
            "Reviewer {} stuck reviewing PR #{} (no events for {}s, restart {}/{}, session: {:?})",
            restart.name,
            restart.pr_number,
            REVIEWER_STUCK_DURATION.as_secs(),
            new_restart_count,
            MAX_REVIEWER_RESTARTS,
            restart.session_id,
        );

        // Shut down the stuck reviewer
        if let Some(session_id) = snap.name_session_map.get(&restart.name) {
            effects.push(Effect::ShutdownSession {
                session_id: session_id.clone(),
                reason: format!(
                    "stuck reviewer: {} (PR #{})",
                    restart.name, restart.pr_number
                ),
            });
        } else {
            effects.push(Effect::ShutdownCoworker {
                name: restart.name.clone(),
                message: String::new(),
            });
        }

        // Respawn with incremented restart count
        let worktree_id = crate::worktree_registry::review_slug_for_pr(restart.pr_number);
        let wt_path = crate::paths::worktrees_dir_for_repo(&snap.repo_name).join(&worktree_id);

        let mut config =
            crate::launch::LaunchConfig::reviewer(restart.name.clone(), restart.pr_number);
        config.auth_provider = crate::config::get_execution_provider_for_role(
            &snap.repo_name,
            crate::config::ExecutionRole::Reviewer,
        );
        config.model =
            super::helpers::default_model_for_provider_role(config.auth_provider, &config.role)
                .to_string();
        config.working_dir = Some(wt_path.clone());

        effects.push(Effect::EnsureWorktree {
            worktree_id: worktree_id.clone(),
            path: wt_path,
        });

        let on_success = vec![
            Effect::BindCoworkerToWorktree {
                worktree_id,
                coworker: restart.name.clone(),
            },
            Effect::BroadcastCoworkerUpdate {
                name: restart.name.clone(),
                status: "running".to_string(),
                current_task: Some(format!("reviewing PR #{}", restart.pr_number)),
            },
            Effect::AssignReviewer {
                pr_number: restart.pr_number,
                reviewer_name: restart.name.clone(),
                source: crate::github_state::AssignmentSource::Manual,
                restart_count: new_restart_count,
                reviewer_session_id: None,
            },
        ];

        let on_failure = vec![Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "⚠️ Failed to respawn reviewer {} for PR #{} (attempt {}/{})",
                restart.name, restart.pr_number, new_restart_count, MAX_REVIEWER_RESTARTS,
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        }];

        effects.push(Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure,
        });

        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔄 Restarted stuck reviewer {} for PR #{} (no events for {}s, attempt {}/{})",
                restart.name,
                restart.pr_number,
                REVIEWER_STUCK_DURATION.as_secs(),
                new_restart_count,
                MAX_REVIEWER_RESTARTS,
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        });
    }

    // Check for reviewers that have hit the restart limit and emit escalation warnings.
    // These are reviewers whose restart_count >= MAX_REVIEWER_RESTARTS that were
    // filtered out by decide_stuck_reviewer_restarts(). We detect them by checking
    // for alive, stuck reviewers with maxed-out restart counts.
    //
    // The escalation is only posted once per PR (tracked via reviewer_escalations_posted
    // in WorldSnapshot) to prevent spamming the channel/lead on every tick.
    let stuck_threshold = chrono::Duration::from_std(REVIEWER_STUCK_DURATION).unwrap_or_default();
    for (name, health) in &snap.headless_process_health {
        if !health.is_alive {
            continue;
        }
        let pr_number = match snap.reviewer_pr_assignments.get(name) {
            Some(pr) => *pr,
            None => continue,
        };
        // Skip if we've already posted an escalation for this PR
        if snap.reviewer_escalations_posted.contains(&pr_number) {
            continue;
        }
        let restart_count = snap
            .reviewer_restart_counts
            .get(&pr_number)
            .copied()
            .unwrap_or(0);
        if restart_count < MAX_REVIEWER_RESTARTS {
            continue;
        }
        // Check if actually stuck (same criteria as the pure function).
        // Fall back to spawn time if no events were ever received.
        let reference_time = health
            .last_event_at
            .or_else(|| snap.coworker_start_times.get(&name.to_lowercase()).copied());
        let reference_time = match reference_time {
            Some(t) => t,
            None => continue,
        };
        if snap.now_utc.signed_duration_since(reference_time) < stuck_threshold {
            continue;
        }
        // Skip if already excluded
        if snap.usage_limited_coworkers.contains(&name.to_lowercase())
            || snap.api_error_coworkers.contains(&name.to_lowercase())
            || snap.attached_coworkers.contains_key(&name.to_lowercase())
            || health.has_running_subagent
            || health.has_pending_tool
        {
            continue;
        }

        warn!(
            "Reviewer {} stuck on PR #{} after {} restarts — escalating to lead",
            name, pr_number, restart_count
        );

        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🚨 Reviewer {} is stuck on PR #{} after {} restart attempts. \
                 Manual intervention needed — the reviewer keeps getting stuck on this PR.",
                name, pr_number, restart_count
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        });
        effects.push(Effect::NudgeLead {
            message: format!(
                "Reviewer {} is stuck on PR #{} after {} restarts. Please investigate.",
                name, pr_number, restart_count
            ),
        });
        effects.push(Effect::RecordReviewerEscalation { pr_number });
    }

    effects
}

/// Check for reviewer processes that exited without posting their review.
///
/// When a reviewer's Claude Code session ends naturally (max turns, rate limit,
/// context window full) before posting the review, the process dies while the
/// reviewer assignment remains. Unlike stuck reviewers (alive but unresponsive),
/// these reviewers are dead — so `decide_stuck_reviewer_restarts` won't catch them
/// because it exempts dead processes.
///
/// This function detects dead reviewers with unposted reviews and respawns them,
/// up to `MAX_REVIEWER_RESTARTS` attempts per PR. When a dead reviewer exhausts
/// the restart budget, it escalates to the ops channel instead.
pub fn check_and_restart_dead_reviewers(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let respawns = crate::rules::decide_dead_reviewer_respawns(
        &snap.headless_process_health,
        &snap.reviewer_pr_assignments,
        &snap.reviewed_prs,
        &snap.reviewer_restart_counts,
        MAX_REVIEWER_RESTARTS,
        &snap.name_session_map,
        &snap.usage_limited_coworkers,
    );

    let escalations = crate::rules::decide_dead_reviewer_escalations(
        &snap.headless_process_health,
        &snap.reviewer_pr_assignments,
        &snap.reviewed_prs,
        &snap.reviewer_restart_counts,
        &snap.reviewer_escalations_posted,
        MAX_REVIEWER_RESTARTS,
    );

    if respawns.is_empty() && escalations.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();
    for restart in respawns {
        let new_restart_count = restart.restart_count + 1;

        warn!(
            "Reviewer {} exited without posting review for PR #{} (attempt {}/{})",
            restart.name, restart.pr_number, new_restart_count, MAX_REVIEWER_RESTARTS,
        );

        // Respawn the reviewer with an incremented restart count.
        let worktree_id = crate::worktree_registry::review_slug_for_pr(restart.pr_number);
        let wt_path = crate::paths::worktrees_dir_for_repo(&snap.repo_name).join(&worktree_id);

        let mut config =
            crate::launch::LaunchConfig::reviewer(restart.name.clone(), restart.pr_number);
        config.auth_provider = crate::config::get_execution_provider_for_role(
            &snap.repo_name,
            crate::config::ExecutionRole::Reviewer,
        );
        config.model =
            super::helpers::default_model_for_provider_role(config.auth_provider, &config.role)
                .to_string();
        config.working_dir = Some(wt_path.clone());

        effects.push(Effect::EnsureWorktree {
            worktree_id: worktree_id.clone(),
            path: wt_path,
        });

        let on_success = vec![
            Effect::BindCoworkerToWorktree {
                worktree_id,
                coworker: restart.name.clone(),
            },
            Effect::BroadcastCoworkerUpdate {
                name: restart.name.clone(),
                status: "running".to_string(),
                current_task: Some(format!("reviewing PR #{}", restart.pr_number)),
            },
            Effect::AssignReviewer {
                pr_number: restart.pr_number,
                reviewer_name: restart.name.clone(),
                source: crate::github_state::AssignmentSource::Manual,
                restart_count: new_restart_count,
                reviewer_session_id: None,
            },
        ];

        let on_failure = vec![Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "⚠️ Failed to respawn reviewer {} for PR #{} (attempt {}/{})",
                restart.name, restart.pr_number, new_restart_count, MAX_REVIEWER_RESTARTS,
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        }];

        effects.push(Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure,
        });

        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔄 Respawning reviewer {} for PR #{} — exited without posting review (attempt {}/{})",
                restart.name,
                restart.pr_number,
                new_restart_count,
                MAX_REVIEWER_RESTARTS,
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        });
    }

    for escalation in escalations {
        warn!(
            "Reviewer {} exited without posting review for PR #{} after {} restarts — escalating to ops",
            escalation.name, escalation.pr_number, escalation.restart_count
        );

        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "@ops PR #{} has hit max reviewer restarts — needs manual intervention. \
                 Reviewer {} exited without posting a review {} times.",
                escalation.pr_number, escalation.name, escalation.restart_count,
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        });
        effects.push(Effect::NudgeLead {
            message: format!(
                "Reviewer {} failed to post a review for PR #{} after {} attempts. \
                 Escalated to ops — please investigate.",
                escalation.name, escalation.pr_number, escalation.restart_count,
            ),
        });
        effects.push(Effect::RecordReviewerEscalation {
            pr_number: escalation.pr_number,
        });
    }

    effects
}

/// Check headless coworker process health for usage/rate limit detection.
/// If detected, schedule a nudge for when the limit expires.
///
/// Usage limits are account-wide, so when one coworker hits it, all of them
/// will be stuck. We detect it from any coworker's ProcessHealth flag and
/// schedule a nudge based on the parsed reset time (if available) or a default
/// of 15 minutes.
pub fn check_for_usage_limits(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    if snap.usage_limit_nudge_scheduled {
        return vec![];
    }

    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    // Find the first coworker with a usage limit flag and extract reset time
    let (detected_coworker, reset_time_utc) = match snap
        .headless_process_health
        .iter()
        .find(|(_, health)| health.has_usage_limit)
    {
        Some((name, health)) => (name.clone(), health.usage_limit_reset_at),
        None => return vec![],
    };

    // Calculate nudge time based on reset time or default to 15 minutes
    let nudge_time = if let Some(reset_utc) = reset_time_utc {
        // Convert reset_time_utc (DateTime<Utc>) to tokio::time::Instant
        let now = chrono::Utc::now();
        let duration_until_reset = reset_utc.signed_duration_since(now);

        if duration_until_reset.num_seconds() > 0 {
            tokio::time::Instant::now()
                + Duration::from_secs(duration_until_reset.num_seconds() as u64)
                + USAGE_LIMIT_NUDGE_BUFFER
        } else {
            // Reset time is in the past or now — nudge immediately (with small buffer)
            tokio::time::Instant::now() + USAGE_LIMIT_NUDGE_BUFFER
        }
    } else {
        // Fallback: default wait of 15 minutes
        tokio::time::Instant::now() + Duration::from_secs(15 * 60) + USAGE_LIMIT_NUDGE_BUFFER
    };

    let message = if reset_time_utc.is_some() {
        format!(
            "⏳ Usage limit detected (via {}). All coworkers will be nudged when it resets.",
            detected_coworker
        )
    } else {
        format!(
            "⏳ Usage limit detected (via {}). All coworkers will be nudged in ~15m when it resets.",
            detected_coworker
        )
    };

    info!(
        "Usage limit detected via coworker {} — scheduling nudge at {:?}",
        detected_coworker, nudge_time
    );

    vec![
        Effect::SetUsageLimitNudge { at: nudge_time },
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message,
            channel: Some(OPS_CHANNEL.to_string()),
        },
    ]
}

/// Check if a scheduled usage limit nudge is due, and if so, nudge all running coworkers.
pub fn maybe_nudge_usage_limit_expiry(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    // Pure decision: should we nudge?
    let decision = crate::rules::decide_usage_limit_expiry(
        snap.usage_limit_nudge_at,
        tokio::time::Instant::now(),
    );

    if decision != crate::rules::UsageLimitExpiryDecision::NudgeNow {
        return vec![];
    }

    if snap.running_coworkers.is_empty() {
        return vec![];
    }

    info!(
        "Usage limit expired — nudging {} running coworkers",
        snap.running_coworkers.len()
    );

    let mut effects = vec![
        Effect::ClearUsageLimitNudge,
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔔 Usage limit expired — nudging {} coworkers to resume work",
                snap.running_coworkers.len()
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        },
    ];

    // Only nudge Running coworkers — Stopping/Starting coworkers have no active session.
    for cw in &snap.running_coworkers {
        effects.push(Effect::NudgeCoworker {
            name: cw.name.clone(),
            message: "continue".to_string(),
        });
    }

    effects
}

/// Check for coworkers experiencing authentication errors and notify the user.
///
/// Unlike usage limits (which reset automatically) and API errors (which may clear on
/// retry), auth errors require user intervention to re-authenticate. When detected:
/// 1. Shut down the affected coworker (no point retrying with an expired token)
/// 2. Post a clear message to the channel with re-auth instructions
/// 3. Nudge the lead so the user sees the notification immediately
///
/// Uses a cooldown to prevent spamming when multiple coworkers hit the same auth error.
pub(super) fn check_and_handle_auth_errors(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    if snap.auth_error_coworkers.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();
    let mut newly_detected = Vec::new();

    for name in &snap.auth_error_coworkers {
        // Check cooldown - only act if we haven't already handled this coworker
        let should_handle = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("auth_error_shutdown", name, AUTH_ERROR_SHUTDOWN_COOLDOWN)
        };

        if !should_handle {
            debug!("Auth error shutdown cooldown active for {}", name);
            continue;
        }

        newly_detected.push(name.clone());

        info!(
            "Coworker {} hit auth error (OAuth token expired) — shutting down",
            name
        );

        // Shut down the coworker - no point retrying with expired token
        effects.push(Effect::ShutdownCoworker {
            name: name.clone(),
            message: String::new(),
        });

        // Record the cooldown so we don't repeatedly shut down the same coworker
        effects.push(Effect::RecordCooldown {
            category: "auth_error_shutdown".to_string(),
            key: name.clone(),
        });
    }

    // Post a channel message and nudge the lead on first detection
    if !newly_detected.is_empty() {
        let names_str = newly_detected.join(", ");

        let message = format!(
            "🔐 OAuth token expired — coworkers {} shut down. Re-authenticate with: midtown auth login\n\
             Coworkers with pending tasks will be respawned after re-authentication.",
            names_str
        );

        effects.insert(
            0,
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: message.clone(),
                channel: Some(OPS_CHANNEL.to_string()),
            },
        );

        // Nudge the lead so the user sees this immediately
        effects.push(Effect::NudgeLead { message });
    }

    effects
}

/// Check for coworkers experiencing API errors and periodically nudge them to retry.
///
/// Unlike usage limits (which have a known reset time and get a single scheduled nudge),
/// API errors are transient and may resolve at any moment. We periodically nudge
/// coworkers with API errors to encourage them to retry, using a cooldown to avoid
/// spamming.
///
/// First detection: posts a channel message about the API error.
/// Subsequent detections: nudges the coworker with a cooldown (does not re-post).
pub(super) fn check_and_nudge_api_errors(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    if snap.api_error_coworkers.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();
    let mut first_detection = false;

    for name in &snap.api_error_coworkers {
        // Check cooldown - only nudge if the cooldown has expired
        let should_nudge = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("api_error_nudge", name, API_ERROR_NUDGE_COOLDOWN)
        };

        if !should_nudge {
            debug!("API error nudge cooldown active for {}", name);
            continue;
        }

        // Check if this is the first time we're seeing this coworker with API error.
        // First detection = no prior cooldown entry exists.
        // Note: entries persist until cleanup (2× cooldown duration), so if an error
        // clears briefly and recurs within that window, it won't be considered "first".
        // This is acceptable because nudging continues regardless, and the channel
        // message is only for widespread outages (2+ coworkers) anyway.
        let is_first = {
            let cooldowns = state.cooldowns.lock().unwrap();
            !cooldowns.has_entry("api_error_nudge", name)
        };

        if is_first {
            first_detection = true;
        }

        info!(
            "Nudging coworker {} to retry after API error (cooldown: {}s)",
            name,
            API_ERROR_NUDGE_COOLDOWN.as_secs()
        );

        effects.push(Effect::NudgeCoworker {
            name: name.clone(),
            message: "The API error may have cleared. Try continuing your work.".to_string(),
        });
        effects.push(Effect::RecordCooldown {
            category: "api_error_nudge".to_string(),
            key: name.clone(),
        });
    }

    // Post a channel message when API errors are widespread (2+ coworkers affected)
    // Only post on first detection of a widespread outage to avoid spam.
    let affected_count = snap.api_error_coworkers.len();
    if first_detection && affected_count >= 2 {
        let names: Vec<&str> = snap
            .api_error_coworkers
            .iter()
            .map(|s| s.as_str())
            .collect();
        effects.insert(
            0,
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "⚠️ Widespread API errors affecting {} coworkers: {}. Will periodically nudge to retry.",
                    affected_count,
                    names.join(", ")
                ),
                channel: Some(OPS_CHANNEL.to_string()),
            },
        );
    }

    effects
}

/// Detect coworkers with tool name conflicts and shut them down for fresh restart.
///
/// "Tool names must be unique" is an unrecoverable API error caused by duplicate
/// tool registrations (e.g., from session resume loading saved tools + plugin
/// re-registration). The affected session loops on 400 errors indefinitely.
///
/// The primary fix is in `headless.rs` (skip `--settings` on resume), but this
/// serves as defense in depth: detect the error via stderr, shut down the session,
/// and let normal task dispatch respawn it.
pub fn check_and_restart_tool_name_conflicts(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    if snap.tool_name_conflict_coworkers.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();

    for name in &snap.tool_name_conflict_coworkers {
        warn!(
            "Coworker {} has tool name conflict — shutting down for fresh restart",
            name
        );

        effects.push(Effect::ShutdownCoworker {
            name: name.clone(),
            message: String::new(),
        });
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔧 Coworker {} hit 'Tool names must be unique' error — restarting with fresh session",
                name
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        });
    }

    effects
}

/// Detect headless coworkers whose process has exited unexpectedly and restart them.
///
/// This checks if the headless
/// process has terminated (exit_code is set, is_alive is false) while the coworker
/// still has work assigned.
pub(super) async fn check_and_respawn_dead_processes(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    // Pure decision: which processes need respawning?
    let respawns = crate::rules::decide_dead_process_respawns(
        &snap.headless_process_health,
        &snap.in_progress_tasks,
        &snap.name_session_map,
    );

    let mut effects = Vec::new();
    for respawn in respawns {
        // Per-coworker cooldown to prevent respawn loops
        let should_check = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("process_respawn", &respawn.name, ZOMBIE_RESPAWN_COOLDOWN)
        };
        if !should_check {
            debug!("Process respawn cooldown active for {}", respawn.name);
            continue;
        }

        warn!(
            "Coworker {} process died (exit code {}) — restarting for task !{} (session: {:?})",
            respawn.name, respawn.exit_code, respawn.task_id, respawn.session_id
        );

        let prompt = format_task_prompt(
            &respawn.task_id,
            &format!(
                "You've been assigned task !{}: {}. Your previous session crashed (exit code {}). Check your git status and continue where you left off.",
                respawn.task_id, respawn.task_subject, respawn.exit_code
            ),
        );

        // Look up the task's channel from the snapshot
        let channel = snap
            .all_tasks
            .iter()
            .find(|t| t.id == respawn.task_id)
            .and_then(|t| t.channel.clone());

        let mut config = crate::launch::LaunchConfig::coworker(
            respawn.name.clone(),
            state.repo_name.clone(),
            crate::launch::SessionMode::Fresh,
            Some(prompt),
        );
        config.channel = channel.clone();

        // Apply task model if available (sets both provider and model)
        config.apply_task_model(&snap.task_model_map, &respawn.task_id);

        effects.push(Effect::ShutdownCoworker {
            name: respawn.name.clone(),
            message: String::new(),
        });
        effects.push(Effect::SpawnCoworker(config));
        effects.push(Effect::RecordCooldown {
            category: "process_respawn".to_string(),
            key: respawn.name.clone(),
        });
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "💀 Coworker {} process died (exit {}) — restarting for task !{}",
                respawn.name, respawn.exit_code, respawn.task_id
            ),
            channel: Some(OPS_CHANNEL.to_string()),
        });
    }

    effects
}

/// Ensure the lead session is always running.
///
/// The lead is the human-facing session that should never be permanently down.
/// If the lead is not in `active_coworkers` (dead and deregistered), respawn it.
/// Uses `coworker_stop_times` as a cooldown to prevent rapid respawn loops.
pub fn ensure_lead_alive(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    // Check if lead is already registered (any status)
    let lead_registered = snap
        .active_coworkers
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case("lead"));

    if lead_registered {
        return vec![];
    }

    // Check if lead is currently attached interactively — if so, the daemon
    // shouldn't spawn a headless lead that would conflict.
    if snap.attached_coworkers.contains_key("lead") {
        return vec![];
    }

    // Cooldown: if the lead was recently stopped (within 5 minutes), don't
    // respawn yet to prevent crash loops. The lead may have been stopped for
    // a good reason (e.g., auth error, attach/detach cycle).
    if let Some(stop_time) = snap.coworker_stop_times.get("lead") {
        let since_stop = snap.now_utc.signed_duration_since(*stop_time);
        if since_stop < chrono::Duration::from_std(LEAD_RESPAWN_COOLDOWN).unwrap_or_default() {
            debug!(
                "Lead respawn cooldown: stopped {}s ago (need {}s)",
                since_stop.num_seconds(),
                LEAD_RESPAWN_COOLDOWN.as_secs()
            );
            return vec![];
        }
    }

    warn!("Lead session is not running — respawning");

    let mut config = crate::launch::LaunchConfig::lead(&snap.repo_name, None);
    config.model =
        super::helpers::default_model_for_provider_role(config.auth_provider, &config.role)
            .to_string();
    let lead_wt = crate::paths::lead_worktree_path(&snap.repo_name);
    if lead_wt.exists() {
        config.working_dir = Some(lead_wt);
    }

    vec![Effect::SpawnCoworker(config)]
}

/// Periodically refresh the lead session to prevent context drift.
///
/// Long lead sessions accumulate context and the LLM starts forgetting
/// system prompt instructions. This function shuts down the lead session
/// when it has been running longer than `lead_session_refresh_interval_secs`.
/// The existing `ensure_lead_alive()` respawns it on the next tick.
///
/// Returns no effects if:
/// - The refresh interval is 0 (disabled)
/// - The lead is not running (already handled by ensure_lead_alive)
/// - The lead has been running for less than the refresh interval
/// - The lead is attached interactively (don't cycle an interactive session)
///
/// Pure function — no I/O, no `.await`, no mutex locks.
pub fn maybe_refresh_lead_session(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let interval_secs = snap.lead_session_refresh_interval_secs;
    if interval_secs == 0 {
        return vec![];
    }

    // Don't refresh an interactively attached session
    if snap.attached_coworkers.contains_key("lead") {
        return vec![];
    }

    // Find the lead in active coworkers
    let lead = snap
        .active_coworkers
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case("lead"));

    let lead = match lead {
        Some(l) => l,
        None => return vec![],
    };

    // Check how long the lead has been running
    let start_time = match snap.coworker_start_times.get("lead") {
        Some(t) => t,
        None => return vec![],
    };

    let age = snap.now_utc.signed_duration_since(*start_time);
    let threshold = chrono::Duration::seconds(interval_secs as i64);

    if age < threshold {
        return vec![];
    }

    info!(
        age_secs = age.num_seconds(),
        interval_secs = interval_secs,
        "Lead session has been running too long — scheduling periodic refresh"
    );

    vec![
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: "Restarting lead session for a fresh context.".to_string(),
            channel: Some(OPS_CHANNEL.to_string()),
        },
        Effect::ShutdownCoworker {
            name: lead.name.clone(),
            message: "Time for a fresh session. Restarting now — will be back shortly.".to_string(),
        },
    ]
}

/// Detect attached sessions that have exceeded `ATTACH_TIMEOUT` without receiving a detach.
///
/// If an interactive session ends without `midtown session detach` (terminal crash,
/// SSH disconnect, wrapper bug), the entry persists in `attached_coworkers` forever.
/// `ensure_lead_alive()` sees the lead as "attached" and skips respawn, leaving
/// the lead permanently stuck.
///
/// This function emits `AutoDetachCoworker` for each stale entry so the next tick
/// clears the entry and allows `ensure_lead_alive()` to respawn the lead.
///
/// Pure function — no I/O, no `.await`, no mutex locks. Takes `now_utc` from the
/// snapshot so tests can control time.
pub fn detect_stale_attached_sessions(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let timeout = chrono::Duration::from_std(ATTACH_TIMEOUT).unwrap_or_default();
    snap.attached_coworkers
        .iter()
        .filter_map(|(name, attached_at)| {
            let age = snap.now_utc.signed_duration_since(*attached_at);
            if age >= timeout {
                info!(
                    "Stale attached session for '{}' (attached {}s ago, timeout {}s) — auto-detaching",
                    name,
                    age.num_seconds(),
                    ATTACH_TIMEOUT.as_secs()
                );
                Some(Effect::AutoDetachCoworker {
                    name: name.clone(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Ensure all registered channel lead sessions are running.
///
/// Channel leads are spawned at startup and on first user message to a channel.
/// This function closes the gap: if a channel lead crashes mid-session, it won't be
/// respawned until someone posts to its channel. By running this on every
/// `SessionMonitorTick`, crashed channel leads are automatically recovered.
///
/// Only channels already registered in `channel_lead_sessions` are checked —
/// new channels are added at startup or on first message (rpc_channel.rs).
///
/// Uses `coworker_stop_times` as a cooldown to prevent rapid respawn loops.
///
/// Pure function — no I/O, no `.await`, no mutex locks.
pub fn ensure_channel_leads_alive(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    for (channel_name, session_id) in &snap.channel_lead_sessions {
        // Skip archived channels — their leads should not be respawned.
        // This is defense-in-depth; handle_channel_archive and ArchiveChannel
        // both clean up channel_lead_sessions, but if a stale entry persists
        // (e.g., CLI archive without daemon restart), this prevents respawning.
        if snap.archived_channels.contains(channel_name.as_str()) {
            debug!(
                "Channel lead '{}': channel is archived, skipping respawn",
                channel_name
            );
            continue;
        }

        let session_name = crate::launch::channel_lead_session_name(channel_name);

        // Skip if this channel lead is already registered as an active coworker.
        let is_registered = snap
            .active_coworkers
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(&session_name));

        if is_registered {
            continue;
        }

        // If session_id is empty, distinguish in-flight spawns from crash recovery:
        // - In-flight (first spawn, no previous stop): skip to avoid double-spawning.
        // - Crash recovery (death handler cleared session_id, stop_time exists): fall through.
        if session_id.is_empty() {
            let had_previous_stop = snap
                .coworker_stop_times
                .contains_key(session_name.to_lowercase().as_str());
            if !had_previous_stop {
                debug!(
                    "Channel lead '{}': in-flight spawn (no previous stop), skipping",
                    channel_name
                );
                continue;
            }
        }

        // Apply cooldown to prevent crash loops.
        if let Some(stop_time) = snap
            .coworker_stop_times
            .get(session_name.to_lowercase().as_str())
        {
            let since_stop = snap.now_utc.signed_duration_since(*stop_time);
            if since_stop < chrono::Duration::from_std(LEAD_RESPAWN_COOLDOWN).unwrap_or_default() {
                debug!(
                    "Channel lead '{}' respawn cooldown: stopped {}s ago (need {}s)",
                    channel_name,
                    since_stop.num_seconds(),
                    LEAD_RESPAWN_COOLDOWN.as_secs()
                );
                continue;
            }
        }

        warn!(
            "Channel lead '{}' is not running — respawning",
            channel_name
        );

        // Use the stored session_id if available (resume); fall back to Fresh.
        // The death handler clears channel_lead_sessions[channel] on crash, so
        // an empty session_id here typically means a fresh spawn is needed.
        let session_mode = if !session_id.is_empty() {
            crate::launch::SessionMode::ResumeSession(session_id.clone())
        } else {
            crate::launch::SessionMode::Fresh
        };

        let config = crate::launch::LaunchConfig::channel_lead(
            channel_name.clone(),
            &snap.repo_name,
            session_mode,
            "", // domain_context: accumulates via session persistence
        );
        effects.push(Effect::SpawnCoworker(config));
    }

    effects
}

pub(super) async fn check_and_fire_reminders(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    let open_pr_coworkers: Vec<String> = snap.coworkers_with_open_prs.iter().cloned().collect();
    let ps = state.persistent_state.lock().await;
    build_reminder_effects(&ps.reminders.reminders, &open_pr_coworkers, &snap.repo_name)
}

/// Pure function: evaluate reminders and build effects (PostToChannel + NudgeLead + MarkFired).
fn build_reminder_effects(
    reminders: &[crate::reminders::Reminder],
    open_pr_coworkers: &[String],
    repo_name: &str,
) -> Vec<Effect> {
    let fired: Vec<&crate::reminders::Reminder> = reminders
        .iter()
        .filter(|r| !r.fired && crate::reminders::evaluate_trigger(&r.trigger, open_pr_coworkers))
        .collect();
    effects_for_fired_reminders(&fired, repo_name)
}

/// Build effects for reminders that have already been evaluated as firing.
fn effects_for_fired_reminders(
    fired: &[&crate::reminders::Reminder],
    repo_name: &str,
) -> Vec<Effect> {
    let mut effects = Vec::new();
    let mut fired_ids = Vec::new();

    for reminder in fired {
        info!(
            "Reminder {} should fire (trigger: {}): {}",
            reminder.id, reminder.trigger, reminder.message
        );
        let message = format!(
            "\u{23f0} Reminder ({}): {}",
            reminder.trigger, reminder.message
        );
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: message.clone(),
            channel: None,
        });
        effects.push(Effect::NudgeLead { message });
        fired_ids.push(reminder.id.clone());
    }

    if !fired_ids.is_empty() {
        effects.push(Effect::MarkRemindersFired {
            fired_ids,
            repo_name: repo_name.to_string(),
        });
    }

    effects
}

/// Check for stale worktrees that can be cleaned up.
///
/// Worktrees are considered stale if:
/// - They have a `completed_at` timestamp (task completed or PR merged)
/// - The completion was more than `retention_period` ago
/// - They are not currently bound to an active coworker
///
/// Returns `CleanupStaleWorktree` effects for each stale worktree.
pub(super) fn check_for_stale_worktrees(
    worktree_registry: &crate::worktree_registry::WorktreeRegistry,
    active_coworkers: &std::collections::HashSet<String>,
    retention_period: chrono::Duration,
) -> Vec<Effect> {
    let now = chrono::Utc::now();
    let mut effects = Vec::new();

    for (_, assignment) in worktree_registry.all_assignments().iter() {
        // Skip if not completed
        let Some(completed_at) = assignment.completed_at else {
            continue;
        };

        // Skip if within retention period
        let age = now.signed_duration_since(completed_at);
        if age < retention_period {
            continue;
        }

        // Skip if actively in use
        if let Some(ref coworker) = assignment.current_coworker
            && active_coworkers.contains(coworker)
        {
            continue;
        }

        debug!(
            "Worktree {} is stale (completed {}h ago), scheduling cleanup",
            assignment.worktree_id,
            age.num_hours()
        );

        // Schedule cleanup (message posting happens in effects.rs when cleanup executes)
        effects.push(Effect::CleanupStaleWorktree {
            worktree_id: assignment.worktree_id.clone(),
        });
    }

    if !effects.is_empty() {
        info!("Scheduled cleanup of {} stale worktree(s)", effects.len());
    }

    effects
}

#[path = "health_tests.rs"]
#[cfg(test)]
mod tests;
