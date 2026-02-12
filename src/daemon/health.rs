//! Health check functions for coworker lifecycle monitoring.
//!
//! These functions detect and respond to coworker health issues:
//! idle shutdown, stuck processes, usage limits, and reminder firing.
//! Health state is read from structured `ProcessHealth` data (populated
//! by the session management layer from headless stream events) instead
//! of parsing raw tmux pane content.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::{config, daemon_messages, web};

use super::constants::*;
use super::effects::Effect;
use super::helpers::format_task_prompt;
use super::{DaemonState, snapshot};

/// Check if the lead's tmux pane has changed and broadcast typing status.
///
/// Captures the lead's Claude Code pane (`lead.0`), hashes the content, and
/// compares against the previous hash. If content changed, the lead is working.
/// Uses a grace period so brief pauses (reading, thinking) don't prematurely
/// clear the indicator. Only broadcasts when the working state transitions.
pub(super) async fn check_lead_typing(state: &DaemonState) {
    let tx = match state.web_updates_tx {
        Some(ref tx) => tx,
        None => return,
    };

    let session = format!("{}{}", crate::tmux::SESSION_PREFIX, state.repo_name);
    let target = format!("{}:lead.0", session);

    let content =
        match tokio::task::spawn_blocking(move || crate::tmux::capture_pane(&target)).await {
            Ok(Some(text)) => text,
            _ => return,
        };

    // Hash the pane content for cheap comparison
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut hasher);
    let new_hash = hasher.finish();

    let now = Instant::now();

    // Single lock for all lead typing state — `working` is derived, not stored
    let (is_working, prev_working) = {
        let mut lt = state.lead_typing.lock().unwrap();
        let pane_changed = lt.pane_hash != 0 && new_hash != lt.pane_hash;
        lt.pane_hash = new_hash;

        // Derive previous working state from old last_activity (before update)
        let prev_working =
            determine_lead_working(false, lt.last_activity, now, LEAD_TYPING_GRACE_PERIOD);

        if pane_changed {
            lt.last_activity = Some(now);
        }

        let is_working = determine_lead_working(
            pane_changed,
            lt.last_activity,
            now,
            LEAD_TYPING_GRACE_PERIOD,
        );

        (is_working, prev_working)
    };

    if is_working != prev_working {
        web::broadcast_lead_typing(tx, is_working);
    }
}

/// Check if the lead tmux window is still alive and respawn it if not.
///
/// This runs on a blocking thread since it calls tmux commands.
/// If the tmux session still exists but the lead window is gone, recreates
/// the lead window using `spawn_lead` (which handles --resume fallback).
///
/// Returns `true` if the tmux server is gone (daemon should shut down),
/// `false` otherwise.
pub(super) fn check_and_respawn_lead(
    session: &str,
    workdir: &Path,
    project_name: &str,
    additional_dirs: &[PathBuf],
) -> bool {
    // First check if the tmux session itself exists.
    let session_check = std::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .output();
    match session_check {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            // Session check failed - check if it's because the server is gone
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("no server running") {
                // Tmux server died unexpectedly - signal daemon to shut down
                error!(
                    "Tmux server is not running. The daemon cannot operate without tmux. \
                     Run `midtown start` to restart."
                );
                return true;
            }
            // Session was killed by user (intentional) - don't interfere
            return false;
        }
        Err(e) => {
            // Failed to run tmux command at all
            error!("Failed to check tmux session: {}", e);
            return false;
        }
    }

    // Session exists — check how many lead windows are present.
    // Using count_windows_by_name instead of window_exists to detect
    // duplicates that can accumulate from restart races.
    let lead_check = crate::tmux::count_windows_by_name(session, "lead");
    let all_windows = crate::tmux::list_windows(session).unwrap_or_default();
    debug!(
        session = %session,
        lead_check = ?lead_check,
        all_windows = ?all_windows,
        window_count = all_windows.len(),
        "LEAD_HEALTH: checking lead window status"
    );

    match lead_check {
        Ok((0, _)) => {
            warn!(
                session = %session,
                all_windows = ?all_windows,
                "LEAD_HEALTH: Lead window missing, will respawn"
            );
            match crate::tmux::spawn_lead(
                session,
                &workdir.to_string_lossy(),
                project_name,
                additional_dirs,
            ) {
                Ok(()) => info!("LEAD_HEALTH: Successfully respawned lead window"),
                Err(e) => error!("LEAD_HEALTH: Failed to respawn lead window: {}", e),
            }
        }
        Ok((1, ids)) => {
            debug!(
                session = %session,
                lead_id = ?ids.first(),
                "LEAD_HEALTH: exactly one lead window, all good"
            );
        }
        Ok((n, ids)) => {
            // Multiple lead windows detected — kill all but the first one
            warn!(
                session = %session,
                lead_count = n,
                lead_ids = ?ids,
                all_windows = ?all_windows,
                "LEAD_HEALTH: Found duplicate lead windows, cleaning up extras"
            );
            for id in ids.iter().skip(1) {
                let target = format!("{}:{}", session, id);
                info!(target = %target, "LEAD_HEALTH: Killing duplicate lead window");
                let _ = crate::tmux::kill_window_by_target(&target);
            }
        }
        Err(e) => {
            warn!(error = %e, "LEAD_HEALTH: Failed to check lead window status");
        }
    }

    false // Tmux server is running normally
}

/// Pure decision function: is the lead still working?
///
/// Returns `true` if the pane just changed, or if the last activity was within
/// the grace period. Returns `false` only after sustained inactivity.
pub(super) fn determine_lead_working(
    pane_changed: bool,
    last_activity: Option<Instant>,
    now: Instant,
    grace_period: Duration,
) -> bool {
    if pane_changed {
        return true;
    }
    match last_activity {
        Some(last) => now.duration_since(last) < grace_period,
        None => false,
    }
}

/// Check for idle coworkers and send them on a break after the idle timeout.
///
/// A coworker is considered idle if they have no tasks in "in_progress" status
/// with their name as owner. After 30 seconds of continuous idle, they are
/// automatically sent on a break.
///
/// IMPORTANT: Coworkers are NEVER sent on a break if any of these apply:
/// - They have open unmerged PRs (must stay available for review feedback)
/// - They have active review assignments
/// - They have unblocked dependent tasks
/// - They are usage-limited (waiting for usage limit reset)
/// - They have API errors (will be nudged to retry instead)
///
/// Also enforces a minimum lifetime check - coworkers must be alive for at least
/// 5 minutes before they can be sent on a break. This prevents spawn storms where
/// coworkers are rapidly sent on breaks.
pub(super) fn check_and_shutdown_idle_coworkers(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
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
        let idle_ctx = crate::rules::IdleShutdownContext {
            coworkers: &snap.coworker_snapshots,
            busy_coworkers: &snap.busy_coworkers,
            coworkers_with_open_prs: &snap.coworkers_with_open_prs,
            active_reviewers: &snap.active_reviewers,
            coworkers_with_unblocked_deps: &snap.coworkers_with_unblocked_deps,
            ci_passed_pr_coworkers: &snap.ci_passed_pr_coworkers,
            usage_limited_coworkers: &snap.usage_limited_coworkers,
            api_error_coworkers: &snap.api_error_coworkers,
            pending_task_owners: &snap.pending_task_owners,
            review_feedback_pr_coworkers: &snap.review_feedback_pr_coworkers,
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
                (
                    true,
                    daemon_messages::break_review_complete(name, pr, config::get_personality()),
                )
            } else {
                warn!(
                    "Reviewer {} is idle but no review found for PR #{} - keeping alive",
                    name, pr
                );
                // Don't shutdown - post a warning to the channel so the team knows
                effects.push(Effect::PostToChannel {
                    sender: "system".to_string(),
                    message: format!(
                        "⚠️ Reviewer {} is idle but hasn't posted review for PR #{} yet",
                        name, pr
                    ),
                    channel: None,
                });
                (false, String::new())
            }
        } else if snap.coworkers_with_merged_prs.contains(name) {
            info!("Sending idle coworker {} on a break (PR merged)", name);
            (
                true,
                daemon_messages::break_work_merged(name, config::get_personality()),
            )
        } else {
            info!(
                "Sending idle coworker {} on a break (idle for 30+ seconds)",
                name
            );
            (
                true,
                daemon_messages::break_idle(name, config::get_personality()),
            )
        };

        if !should_shutdown {
            continue;
        }

        // Post system message, broadcast status, and shut down
        effects.push(Effect::PostToChannel {
            sender: "system".to_string(),
            message: shutdown_msg,
            channel: None,
        });
        effects.push(Effect::BroadcastCoworkerUpdate {
            name: name.clone(),
            status: "stopped".to_string(),
            current_task: None,
        });
        effects.push(Effect::ShutdownCoworker {
            name: name.clone(),
            message: String::new(),
            session_id: None,
        });
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
        attached: &snap.attached_coworkers,
    };
    let restarts = crate::rules::decide_stuck_coworker_restarts(
        &snap.headless_process_health,
        &snap.in_progress_tasks,
        &exemptions,
        snap.now_utc,
        COWORKER_STUCK_DURATION,
    );

    let mut effects = Vec::new();
    for restart in restarts {
        info!(
            "Coworker {} no events for {}s — restarting for task !{}",
            restart.name,
            COWORKER_STUCK_DURATION.as_secs(),
            restart.task_id
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

        effects.push(Effect::ShutdownCoworker {
            name: restart.name.clone(),
            message: String::new(),
            session_id: None,
        });
        effects.push(Effect::SpawnCoworker(config));
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔄 Restarted stuck coworker {} (no events for {}s) — resuming task !{}",
                restart.name,
                COWORKER_STUCK_DURATION.as_secs(),
                restart.task_id
            ),
            channel,
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
pub(super) fn check_and_restart_stuck_reviewers(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    let exemptions = crate::rules::StuckExemptions {
        usage_limited: &snap.usage_limited_coworkers,
        api_error: &snap.api_error_coworkers,
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
    );

    let mut effects = Vec::new();
    for restart in restarts {
        let new_restart_count = restart.restart_count + 1;

        info!(
            "Reviewer {} stuck reviewing PR #{} (no events for {}s, restart {}/{})",
            restart.name,
            restart.pr_number,
            REVIEWER_STUCK_DURATION.as_secs(),
            new_restart_count,
            MAX_REVIEWER_RESTARTS,
        );

        // Shut down the stuck reviewer
        effects.push(Effect::ShutdownCoworker {
            name: restart.name.clone(),
            message: String::new(),
            session_id: None,
        });

        // Respawn with incremented restart count
        let worktree_id = crate::worktree_registry::review_slug_for_pr(restart.pr_number);
        let wt_path = crate::paths::worktrees_dir_for_repo(&snap.repo_name).join(&worktree_id);

        let mut config =
            crate::launch::LaunchConfig::reviewer(restart.name.clone(), restart.pr_number);
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
            channel: None,
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
            channel: None,
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
        // Check if actually stuck (same criteria as the pure function)
        let last_event = match health.last_event_at {
            Some(t) => t,
            None => continue,
        };
        if snap.now_utc.signed_duration_since(last_event) < stuck_threshold {
            continue;
        }
        // Skip if already excluded
        if snap.usage_limited_coworkers.contains(&name.to_lowercase())
            || snap.api_error_coworkers.contains(&name.to_lowercase())
            || snap.attached_coworkers.contains(&name.to_lowercase())
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
            channel: None,
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

/// Check headless coworker process health for usage/rate limit detection.
/// If detected, schedule a nudge for when the limit expires.
///
/// Usage limits are account-wide, so when one coworker hits it, all of them
/// will be stuck. We detect it from any coworker's ProcessHealth flag and
/// schedule a nudge based on the parsed reset time (if available) or a default
/// of 15 minutes.
pub(super) fn check_for_usage_limits(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
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
            sender: "system".to_string(),
            message,
            channel: None,
        },
    ]
}

/// Check if a scheduled usage limit nudge is due, and if so, nudge all running coworkers.
pub(super) fn maybe_nudge_usage_limit_expiry(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
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
            sender: "system".to_string(),
            message: format!(
                "🔔 Usage limit expired — nudging {} coworkers to resume work",
                snap.running_coworkers.len()
            ),
            channel: None,
        },
    ];

    // Only nudge Running coworkers — Stopping/Starting coworkers have no tmux window.
    for cw in &snap.running_coworkers {
        effects.push(Effect::NudgeCoworker {
            name: cw.name.clone(),
            message: "continue".to_string(),
            session_id: None,
        });
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
            session_id: None,
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
                sender: "system".to_string(),
                message: format!(
                    "⚠️ Widespread API errors affecting {} coworkers: {}. Will periodically nudge to retry.",
                    affected_count,
                    names.join(", ")
                ),
                channel: None,
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
pub(super) fn check_and_restart_tool_name_conflicts(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
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
            session_id: None,
        });
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔧 Coworker {} hit 'Tool names must be unique' error — restarting with fresh session",
                name
            ),
            channel: None,
        });
    }

    effects
}

/// Detect headless coworkers whose process has exited unexpectedly and restart them.
///
/// Unlike tmux-based zombie detection (blank pane), this checks if the headless
/// process has terminated (exit_code is set, is_alive is false) while the coworker
/// still has work assigned.
pub(super) async fn check_and_respawn_dead_processes(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for (name, health) in &snap.headless_process_health {
        // Only care about processes that died (not alive, has exit code)
        if health.is_alive || health.exit_code.is_none() {
            continue;
        }

        // Check if this coworker has an in-progress task
        let task = snap
            .in_progress_tasks
            .iter()
            .find(|(_id, _subject, owner)| owner.eq_ignore_ascii_case(name));

        let Some((task_id, task_subject, _owner)) = task else {
            continue;
        };

        // Per-coworker cooldown to prevent respawn loops
        let should_check = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("process_respawn", name, ZOMBIE_RESPAWN_COOLDOWN)
        };
        if !should_check {
            debug!("Process respawn cooldown active for {}", name);
            continue;
        }

        let exit_code = health.exit_code.unwrap_or(-1);
        warn!(
            "Coworker {} process died (exit code {}) — restarting for task !{}",
            name, exit_code, task_id
        );

        let prompt = format_task_prompt(
            task_id,
            &format!(
                "You've been assigned task !{}: {}. Your previous session crashed (exit code {}). Check your git status and continue where you left off.",
                task_id, task_subject, exit_code
            ),
        );

        // Look up the task's channel from the snapshot
        let channel = snap
            .all_tasks
            .iter()
            .find(|t| t.id == *task_id)
            .and_then(|t| t.channel.clone());

        let mut config = crate::launch::LaunchConfig::coworker(
            name.clone(),
            state.repo_name.clone(),
            crate::launch::SessionMode::Fresh,
            Some(prompt),
        );
        config.channel = channel.clone();

        // Apply task model if available (sets both provider and model)
        config.apply_task_model(&snap.task_model_map, task_id);

        effects.push(Effect::ShutdownCoworker {
            name: name.clone(),
            message: String::new(),
            session_id: None,
        });
        effects.push(Effect::SpawnCoworker(config));
        effects.push(Effect::RecordCooldown {
            category: "process_respawn".to_string(),
            key: name.clone(),
        });
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "💀 Coworker {} process died (exit {}) — restarting for task !{}",
                name, exit_code, task_id
            ),
            channel,
        });
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
            sender: "system".to_string(),
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

        // Schedule cleanup with channel notification
        let message = if let Some(ref task_id) = assignment.task_id {
            format!(
                "🧹 Cleaned up stale worktree {} (task !{}, completed {}h ago)",
                assignment.worktree_id,
                task_id,
                age.num_hours()
            )
        } else {
            format!(
                "🧹 Cleaned up stale worktree {} (completed {}h ago)",
                assignment.worktree_id,
                age.num_hours()
            )
        };

        effects.push(Effect::CleanupStaleWorktree {
            worktree_id: assignment.worktree_id.clone(),
        });
        effects.push(Effect::PostSystemMessage { message });
    }

    if !effects.is_empty() {
        info!("Scheduled cleanup of {} stale worktree(s)", effects.len());
    }

    effects
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_lead_working_pane_changed() {
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        assert!(determine_lead_working(true, None, now, grace));
        assert!(determine_lead_working(true, Some(now), now, grace));
    }

    #[test]
    fn test_determine_lead_working_within_grace_period() {
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        let last_activity = now - Duration::from_secs(10);
        assert!(determine_lead_working(
            false,
            Some(last_activity),
            now,
            grace
        ));
    }

    #[test]
    fn test_determine_lead_working_grace_period_expired() {
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        let last_activity = now - Duration::from_secs(31);
        assert!(!determine_lead_working(
            false,
            Some(last_activity),
            now,
            grace
        ));
    }

    #[test]
    fn test_determine_lead_working_no_activity_ever() {
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        assert!(!determine_lead_working(false, None, now, grace));
    }

    #[test]
    fn test_determine_lead_working_exactly_at_grace_boundary() {
        let now = Instant::now();
        let grace = Duration::from_secs(30);
        let last_activity = now - Duration::from_secs(30);
        assert!(!determine_lead_working(
            false,
            Some(last_activity),
            now,
            grace
        ));
    }

    /// Test that usage limit expiry nudges only target Running coworkers.
    ///
    /// Regression test: the function previously iterated `snap.active_coworkers`
    /// (all statuses) to generate NudgeCoworker effects. Nudges target tmux
    /// windows via send-keys, so Stopping/Starting coworkers (no window) would
    /// cause "can't find window" errors.
    #[test]
    fn test_usage_limit_nudge_only_targets_running_coworkers() {
        use crate::coworker::{Coworker, CoworkerStatus};
        use std::collections::{HashMap, HashSet};

        let running = Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "lexington".to_string(),
            status: CoworkerStatus::Running,
            working_dir: "/tmp/test".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "sonnet".to_string(),
            provider: crate::auth::AuthProvider::Claude,
        };
        let stopping = Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "park".to_string(),
            status: CoworkerStatus::Stopping,
            working_dir: "/tmp/test".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "sonnet".to_string(),
            provider: crate::auth::AuthProvider::Claude,
        };

        // Build a snapshot where the nudge should fire
        let snap = snapshot::WorldSnapshot {
            active_coworkers: vec![running.clone(), stopping.clone()],
            running_coworkers: vec![running.clone()],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: true,
            // Set nudge time in the past so it fires
            usage_limit_nudge_at: Some(tokio::time::Instant::now() - Duration::from_secs(10)),
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let effects = maybe_nudge_usage_limit_expiry(&snap);

        // Should have effects: ClearUsageLimitNudge + PostToChannel + 1 NudgeCoworker
        let nudge_names: Vec<&str> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::NudgeCoworker { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();

        // Only the Running coworker should be nudged
        assert!(
            nudge_names.contains(&"lexington"),
            "Running coworker should be nudged"
        );
        assert!(
            !nudge_names.contains(&"park"),
            "Stopping coworker must NOT be nudged"
        );
        assert_eq!(nudge_names.len(), 1, "Only 1 coworker should be nudged");
    }

    #[test]
    fn test_fired_reminder_nudges_lead() {
        use crate::reminders::{Reminder, ReminderTrigger};

        let reminder = Reminder {
            id: "abc123".to_string(),
            trigger: ReminderTrigger::AllWorkMerged,
            message: "Cut new release".to_string(),
            created_at: chrono::Utc::now(),
            fired: false,
        };
        let fired = vec![&reminder];

        let effects = effects_for_fired_reminders(&fired, "test-repo");

        // Should have: PostToChannel, NudgeLead, MarkRemindersFired
        assert_eq!(effects.len(), 3, "Expected 3 effects");
        assert!(
            matches!(&effects[0], Effect::PostToChannel { .. }),
            "First effect should be PostToChannel"
        );
        assert!(
            matches!(&effects[1], Effect::NudgeLead { .. }),
            "Second effect should be NudgeLead"
        );
        assert!(
            matches!(&effects[2], Effect::MarkRemindersFired { .. }),
            "Third effect should be MarkRemindersFired"
        );
    }

    #[test]
    fn test_fired_reminder_no_reminders_produces_no_effects() {
        let fired: Vec<&crate::reminders::Reminder> = vec![];
        let effects = effects_for_fired_reminders(&fired, "test-repo");
        assert!(
            effects.is_empty(),
            "No fired reminders should produce no effects"
        );
    }

    #[test]
    fn test_check_for_usage_limits_with_reset_time() {
        use crate::coworker::{Coworker, CoworkerStatus};
        use std::collections::{HashMap, HashSet};

        // Create a ProcessHealth with usage limit and a specific reset time
        let reset_time = chrono::Utc::now() + chrono::Duration::hours(2);
        let mut health = HashMap::new();
        health.insert(
            "amsterdam".to_string(),
            snapshot::ProcessHealth {
                is_alive: true,
                last_event_at: Some(chrono::Utc::now()),
                has_usage_limit: true,
                usage_limit_reset_at: Some(reset_time),
                has_api_error: false,
                has_running_subagent: false,
                has_pending_tool: false,
                has_tool_name_conflict: false,
                exit_code: None,
            },
        );

        let coworker = Coworker {
            slot_id: uuid::Uuid::new_v4().to_string(),
            name: "amsterdam".to_string(),
            status: CoworkerStatus::Running,
            working_dir: "/tmp/test".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            model: "sonnet".to_string(),
            provider: crate::auth::AuthProvider::Claude,
        };

        // Create a minimal snapshot
        let snap = snapshot::WorldSnapshot {
            active_coworkers: vec![coworker],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::from(["amsterdam".to_string()]),
            active_session_ids: HashSet::new(),
            session_name: "test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: health,
            attached_coworkers: HashSet::new(),
            busy_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            pending_tasks_without_owners: vec![],
            pending_tasks_with_owners: vec![],
            all_tasks: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: false,
            usage_limit_nudge_at: None,
            usage_limited_coworkers: HashSet::from(["amsterdam".to_string()]),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let effects = check_for_usage_limits(&snap);

        // Should have SetUsageLimitNudge and PostToChannel effects
        assert!(!effects.is_empty(), "Should produce effects");

        // Check that a nudge is scheduled
        let has_set_nudge = effects
            .iter()
            .any(|e| matches!(e, Effect::SetUsageLimitNudge { .. }));
        assert!(has_set_nudge, "Should schedule a usage limit nudge");

        // Check that a message is posted
        let has_post = effects
            .iter()
            .any(|e| matches!(e, Effect::PostToChannel { .. }));
        assert!(has_post, "Should post a channel message");
    }

    #[test]
    fn test_check_for_usage_limits_already_scheduled() {
        use std::collections::{HashMap, HashSet};

        // Create a snapshot with usage_limit_nudge_scheduled = true
        let snap = snapshot::WorldSnapshot {
            active_coworkers: vec![],
            running_coworkers: vec![],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            active_session_ids: HashSet::new(),
            session_name: "test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            headless_process_health: HashMap::new(),
            attached_coworkers: HashSet::new(),
            busy_coworkers: HashSet::new(),
            in_progress_tasks: vec![],
            pending_tasks_without_owners: vec![],
            pending_tasks_with_owners: vec![],
            all_tasks: vec![],
            task_channel: HashMap::new(),
            task_model_map: HashMap::new(),
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            open_prs_data: vec![],
            pending_task_owners: HashSet::new(),
            tasks_with_open_prs: HashMap::new(),
            pr_task_associations: HashMap::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            reviewer_restart_counts: HashMap::new(),
            reviewer_escalations_posted: HashSet::new(),
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: true, // Already scheduled
            usage_limit_nudge_at: Some(tokio::time::Instant::now()),
            usage_limited_coworkers: HashSet::from(["amsterdam".to_string()]),
            api_error_coworkers: HashSet::new(),
            tool_name_conflict_coworkers: HashSet::new(),
            coworker_task_assignments: HashMap::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            tasks_with_worktrees: HashSet::new(),
            task_worktree_map: HashMap::new(),
            worktree_branch_owners: HashMap::new(),
            worktree_registry: crate::worktree_registry::WorktreeRegistry::default(),
            merged_pr_branches: HashMap::new(),
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
            github_rate_limit: crate::github_rate_limit::GitHubRateLimit::default(),
            freshly_fetched_rate_limit: None,
        };

        let effects = check_for_usage_limits(&snap);

        // Should not schedule another nudge
        assert!(effects.is_empty(), "Should not schedule duplicate nudge");
    }

    #[test]
    fn check_for_stale_worktrees_generates_cleanup_and_channel_message() {
        use std::collections::HashSet;

        let mut registry = crate::worktree_registry::WorktreeRegistry::new();
        // Stale worktree with task ID, completed 48 hours ago
        registry
            .assign_worktree(crate::worktree_registry::WorktreeAssignment {
                worktree_id: "task-99-fix-bug".to_string(),
                branch_name: "task-99-fix-bug".to_string(),
                task_id: Some("99".to_string()),
                current_coworker: None,
                pr_number: Some(200),
                created_at: chrono::Utc::now() - chrono::Duration::hours(72),
                completed_at: Some(chrono::Utc::now() - chrono::Duration::hours(48)),
            })
            .unwrap();
        // Non-stale worktree (within retention period)
        registry
            .assign_worktree(crate::worktree_registry::WorktreeAssignment {
                worktree_id: "task-100-add-test".to_string(),
                branch_name: "task-100-add-test".to_string(),
                task_id: Some("100".to_string()),
                current_coworker: None,
                pr_number: None,
                created_at: chrono::Utc::now() - chrono::Duration::hours(2),
                completed_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
            })
            .unwrap();

        let active_coworkers = HashSet::new();
        let retention = chrono::Duration::hours(24);

        let effects = check_for_stale_worktrees(&registry, &active_coworkers, retention);

        // Only the 48h-old worktree should be cleaned up (2 effects: cleanup + message)
        assert_eq!(
            effects.len(),
            2,
            "should generate 1 cleanup + 1 channel message effect"
        );
        assert!(
            matches!(&effects[0], Effect::CleanupStaleWorktree { worktree_id } if worktree_id == "task-99-fix-bug"),
            "first effect should be CleanupStaleWorktree"
        );
        assert!(
            matches!(&effects[1], Effect::PostSystemMessage { message } if message.contains("task-99-fix-bug") && message.contains("task !99") && message.contains('🧹')),
            "second effect should be PostSystemMessage with task ID"
        );
    }
}
