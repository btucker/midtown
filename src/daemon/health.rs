//! Health check functions for coworker lifecycle monitoring.
//!
//! These functions detect and respond to coworker health issues:
//! idle shutdown, stuck panes, usage limits, zombie processes, and
//! reminder firing. Pane scraping is used exclusively for health
//! detection — workflow state is reported via RPC.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, trace, warn};

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
/// - They have running subagents (Task tool agents)
/// - They are usage-limited (waiting for usage limit reset)
/// - They have API errors (will be nudged to retry instead)
///
/// Also enforces a minimum lifetime check - coworkers must be alive for at least
/// 5 minutes before they can be sent on a break. This prevents spawn storms where
/// coworkers are rapidly sent on breaks.
pub(super) async fn check_and_shutdown_idle_coworkers(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
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
        let mut records = state.coworker_records.write().await;
        let (decisions, transitions) = crate::rules::decide_idle_shutdowns(
            &snap.coworker_snapshots,
            &snap.busy_coworkers,
            &snap.coworkers_with_open_prs,
            &snap.active_reviewers,
            &snap.coworkers_with_unblocked_deps,
            &snap.coworkers_with_running_subagents,
            &snap.ci_passed_pr_coworkers,
            &snap.usage_limited_coworkers,
            &snap.api_error_coworkers,
            &snap.pending_task_owners,
            &snap.review_feedback_pr_coworkers,
            &records,
            snap.now_utc,
            MINIMUM_COWORKER_LIFETIME,
        );
        crate::rules::apply_health_transitions(&mut records, transitions);
        decisions
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
            let has_subagent = snap
                .coworkers_with_running_subagents
                .iter()
                .any(|s| s.eq_ignore_ascii_case(name));
            let ci_passed = snap
                .ci_passed_pr_coworkers
                .iter()
                .any(|c| c.eq_ignore_ascii_case(name));
            warn!(
                "IDLE_SHUTDOWN: {} - is_busy={}, has_open_pr={}, is_reviewing={}, has_subagent={}, ci_passed={}",
                name, is_busy, has_open_pr, is_reviewing, has_subagent, ci_passed,
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
        });
        effects.push(Effect::BroadcastCoworkerUpdate {
            name: name.clone(),
            status: "stopped".to_string(),
            current_task: None,
        });
        effects.push(Effect::ShutdownCoworker {
            name: name.clone(),
            message: String::new(),
        });
    }

    effects
}

/// Detect coworkers whose tmux pane content has not changed for `COWORKER_STUCK_DURATION`,
/// kill them, and respawn with their current task prompt.
///
/// Uses the same pane-hashing approach as lead typing detection. Each tick we hash
/// every coworker's captured pane content and compare to the previous hash. If the
/// hash has been unchanged for 5 minutes, the coworker is assumed stuck (hung process,
/// infinite loop, etc.) and is restarted.
pub(super) async fn check_and_restart_stuck_coworkers(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    // Extract pane hashes from unified records, run pure decision, write back
    let mut records = state.coworker_records.write().await;
    let hashes: HashMap<String, (u64, Instant)> = records
        .iter()
        .filter_map(|(name, r): (&String, &crate::rules::CoworkerRecord)| {
            r.pane_hash.map(|h| (name.clone(), h))
        })
        .collect();
    let result = crate::rules::decide_stuck_coworker_restarts(
        &hashes,
        &snap.pane_contents,
        &snap.in_progress_tasks,
        &snap.usage_limited_coworkers,
        &snap.api_error_coworkers,
        snap.now,
        COWORKER_STUCK_DURATION,
    );

    // Write updated hashes back into records
    for (name, hash_entry) in &result.updated_hashes {
        records
            .entry(name.clone())
            .or_insert_with(crate::rules::CoworkerRecord::new_spawn)
            .pane_hash = Some(*hash_entry);
    }
    // Clear hashes for coworkers no longer tracked
    for (name, record) in records.iter_mut() {
        if !result.updated_hashes.contains_key(name) {
            record.pane_hash = None;
        }
    }
    drop(records);

    // Generate effects from pure decisions
    let mut effects = Vec::new();
    for restart in result.restarts {
        info!(
            "Coworker {} pane unchanged for {}s — restarting for task #{}",
            restart.name,
            COWORKER_STUCK_DURATION.as_secs(),
            restart.task_id
        );

        let prompt = format_task_prompt(
            &restart.task_id,
            &format!(
                "You've been assigned task #{}: {}. Your previous session appeared stuck so you were restarted. Check your git status and continue where you left off.",
                restart.task_id, restart.task_subject
            ),
        );

        effects.push(Effect::ShutdownCoworker {
            name: restart.name.clone(),
            message: String::new(),
        });
        effects.push(Effect::SpawnCoworker(
            crate::tmux::ClaudeLaunchConfig::coworker(
                restart.name.clone(),
                state.repo_name.clone(),
                crate::tmux::SessionMode::Fresh,
                Some(prompt),
            ),
        ));
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🔄 Restarted stuck coworker {} (pane unchanged for {}s) — resuming task #{}",
                restart.name,
                COWORKER_STUCK_DURATION.as_secs(),
                restart.task_id
            ),
        });
    }

    effects
}

// Usage limit patterns and parse_usage_limit_duration moved to crate::rules

/// Check all active coworkers' tmux panes for usage/rate limit messages.
/// If detected, schedule a nudge for when the limit expires.
///
/// Usage limits are account-wide, so when one coworker hits it, all of them
/// will be stuck. We detect it from any coworker, parse the expiry, and
/// schedule a single nudge time for everyone.
pub(super) fn check_for_usage_limits(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    // If we already have a nudge scheduled, don't re-detect
    if snap.usage_limit_nudge_scheduled {
        return vec![];
    }

    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    // Pure decision: detect usage limit
    let decision = crate::rules::decide_usage_limit_detection(&snap.pane_contents);

    let detected_coworker = match decision {
        crate::rules::UsageLimitDecision::Detected { coworker } => coworker,
        _ => return vec![],
    };

    // Find the pane content for the detected coworker to parse duration
    let pane_content = snap
        .pane_contents
        .get(&detected_coworker)
        .map(|s| s.as_str())
        .unwrap_or("");

    let wait_duration = crate::rules::parse_usage_limit_duration(pane_content);
    let nudge_time = tokio::time::Instant::now() + wait_duration + USAGE_LIMIT_NUDGE_BUFFER;

    let human_duration = if wait_duration.as_secs() >= 3600 {
        format!(
            "{}h {}m",
            wait_duration.as_secs() / 3600,
            (wait_duration.as_secs() % 3600) / 60
        )
    } else {
        format!("{}m", wait_duration.as_secs() / 60)
    };

    info!(
        "Usage limit detected via coworker {} — scheduling nudge in {} + 30s buffer",
        detected_coworker, human_duration
    );

    vec![
        Effect::SetUsageLimitNudge { at: nudge_time },
        Effect::PostToChannel {
            sender: "system".to_string(),
            message: format!(
                "⏳ Usage limit detected (via {}). All coworkers will be nudged in ~{} when it resets.",
                detected_coworker, human_duration
            ),
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
        },
    ];

    // Only nudge Running coworkers — Stopping/Starting coworkers have no tmux window.
    for cw in &snap.running_coworkers {
        effects.push(Effect::NudgeCoworker {
            name: cw.name.clone(),
            message: "continue".to_string(),
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
            },
        );
    }

    effects
}

/// Detect coworkers stuck in compaction (whirlpool) or with queued prompts,
/// and send the appropriate recovery keypress (Escape or Enter).
///
/// Uses per-coworker cooldowns to avoid spamming keys on every tick.
///
/// `exclude_names` allows the caller to skip coworkers that are already being
/// shut down (e.g., from idle shutdown effects in the same tick), preventing
/// race conditions where we interrupt a coworker that's about to terminate.
pub(super) fn check_and_recover_stuck_ui(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
    exclude_names: &HashSet<String>,
) -> Vec<Effect> {
    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    let recoveries = crate::rules::decide_stuck_ui_recoveries(
        &snap.pane_contents,
        MIN_COMPACTION_STUCK_DURATION,
        &snap.coworker_start_times,
        snap.now_utc,
        chrono::Duration::seconds(QUEUED_NUDGE_MIN_AGE_SECS),
    );

    let mut effects = Vec::new();

    for recovery in recoveries {
        match recovery {
            crate::rules::StuckUiRecovery::InterruptCompaction { name } => {
                // Skip coworkers being shut down
                if exclude_names.contains(&name) {
                    debug!(
                        "Skipping compaction recovery for {} (being shut down)",
                        name
                    );
                    continue;
                }

                let should_act = {
                    let cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.check("compaction_recovery", &name, COMPACTION_RECOVERY_COOLDOWN)
                };
                if !should_act {
                    debug!("Compaction recovery cooldown active for {}", name);
                    continue;
                }

                info!(
                    "Coworker {} stuck in compaction — sending Escape to interrupt",
                    name
                );
                // Trace log pane content for debugging false positives
                // Guard with enabled! to avoid expensive string operations when trace is disabled
                if tracing::enabled!(tracing::Level::TRACE)
                    && let Some(pane_content) = snap.pane_contents.get(&name)
                {
                    // Log last 20 lines to see what triggered detection
                    let last_lines: Vec<&str> = pane_content.lines().rev().take(20).collect();
                    trace!(
                        coworker = %name,
                        pane_tail = ?last_lines,
                        "Compaction detection triggered - last 20 pane lines (reversed)"
                    );
                }
                effects.push(Effect::SendRawKeys {
                    name: name.clone(),
                    keys: "Escape".to_string(),
                });
                effects.push(Effect::RecordCooldown {
                    category: "compaction_recovery".to_string(),
                    key: name.clone(),
                });
                effects.push(Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!("🌀 Interrupted stuck compaction for {} (sent Escape)", name),
                });
            }
            crate::rules::StuckUiRecovery::InterruptQueuedNudges { name } => {
                // Skip coworkers being shut down (effect coordination logic stays here)
                if exclude_names.contains(&name) {
                    debug!(
                        "Skipping queued nudge recovery for {} (being shut down)",
                        name
                    );
                    continue;
                }

                // Age-based protection is handled in the pure decision function
                // (decide_stuck_ui_recoveries filters out young coworkers)

                let should_act = {
                    let cooldowns = state.cooldowns.lock().unwrap();
                    cooldowns.check(
                        "queued_prompt_recovery",
                        &name,
                        QUEUED_PROMPT_RECOVERY_COOLDOWN,
                    )
                };
                if !should_act {
                    debug!("Queued prompt recovery cooldown active for {}", name);
                    continue;
                }

                // Extract the queued text and check if it matches a daemon-sent nudge
                let pane_content = snap.pane_contents.get(&name);
                let queued_text = pane_content
                    .and_then(|content| crate::rules::extract_queued_nudge_text(content));

                let is_daemon_nudge = queued_text
                    .as_ref()
                    .is_some_and(|text| state.matches_pending_nudge(&name, text));

                if is_daemon_nudge {
                    // Daemon-sent nudge: send Enter to auto-submit
                    info!(
                        "Coworker {} has daemon-sent nudge stuck in queue — sending Enter to submit",
                        name
                    );
                    effects.push(Effect::SendRawKeys {
                        name: name.clone(),
                        keys: "Enter".to_string(),
                    });
                    effects.push(Effect::RecordCooldown {
                        category: "queued_prompt_recovery".to_string(),
                        key: name.clone(),
                    });
                    effects.push(Effect::PostToChannel {
                        sender: "midtown".to_string(),
                        message: format!("📨 Auto-submitted stuck nudge for {} (sent Enter)", name),
                    });
                    // Clear the pending nudge since we're submitting it
                    state.clear_pending_nudge(&name);
                } else {
                    // Not a daemon-sent nudge (user input): don't auto-submit
                    // Just log and record cooldown to avoid spamming
                    debug!(
                        "Coworker {} has queued text that doesn't match pending nudge — leaving alone",
                        name
                    );
                    effects.push(Effect::RecordCooldown {
                        category: "queued_prompt_recovery".to_string(),
                        key: name.clone(),
                    });
                }
            }
        }
    }

    effects
}

pub(super) async fn check_and_respawn_zombies(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    let zombies = crate::rules::detect_blank_pane_zombies(
        &snap.blank_pane_coworkers,
        &snap.coworker_start_times,
        snap.now_utc,
        chrono::Duration::seconds(ZOMBIE_MIN_AGE_SECS),
    );

    let mut effects = Vec::new();
    for name in zombies {
        // Skip active reviewers — they were spawned with a specific review prompt.
        // Respawning with --continue would produce a confused coworker that doesn't
        // know which PR to review. Just shut them down and alert.
        if snap.active_reviewers.contains(&name.to_lowercase()) {
            warn!(
                "Blank-pane zombie {} is an active reviewer — shutting down instead of respawning",
                name
            );
            effects.push(Effect::ShutdownCoworker {
                name: name.clone(),
                message: String::new(),
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "⚠️ Reviewer {} crashed on startup (blank pane). \
                     Isolated reviewers cannot be respawned — shutting down. \
                     The PR will be picked up for review on the next poll cycle.",
                    name
                ),
            });
            continue;
        }

        // Per-coworker cooldown to prevent respawn loops
        let should_check = {
            let cooldowns = state.cooldowns.lock().unwrap();
            cooldowns.check("zombie_respawn", &name, ZOMBIE_RESPAWN_COOLDOWN)
        };
        if !should_check {
            debug!("Zombie respawn cooldown active for {}", name);
            continue;
        }

        // Check respawn attempt count — give up after MAX_ZOMBIE_RESPAWN_ATTEMPTS
        let attempt_count = {
            let mut records = state.coworker_records.write().await;
            let record = records
                .entry(name.clone())
                .or_insert_with(crate::rules::CoworkerRecord::new_spawn);
            record.zombie_respawn_count += 1;
            record.zombie_respawn_count
        };

        if attempt_count > MAX_ZOMBIE_RESPAWN_ATTEMPTS {
            warn!(
                "Zombie {} has failed {} respawn attempts (max {}), giving up",
                name, attempt_count, MAX_ZOMBIE_RESPAWN_ATTEMPTS
            );
            effects.push(Effect::ShutdownCoworker {
                name: name.clone(),
                message: String::new(),
            });
            effects.push(Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: format!(
                    "⚠️ Coworker {} failed to start after {} attempts — giving up. \
                     Check daemon logs for BLANK PANE DIAGNOSTIC details.",
                    name, MAX_ZOMBIE_RESPAWN_ATTEMPTS
                ),
            });
            // Clean up the counter
            {
                let mut records = state.coworker_records.write().await;
                if let Some(record) = records.get_mut(&name) {
                    record.zombie_respawn_count = 0;
                }
            }
            continue;
        }

        // Capture diagnostics before respawning
        let target = format!("{}:{}", snap.session_name, name);
        let pane_pid = std::process::Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &target,
                "-F",
                "#{pane_pid} #{pane_width}x#{pane_height}",
            ])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        let raw_content = crate::tmux::capture_pane(&target)
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>();
        let age = snap
            .coworker_start_times
            .get(&name)
            .map(|t| snap.now_utc.signed_duration_since(*t).num_seconds())
            .unwrap_or(-1);

        warn!(
            "BLANK PANE ZOMBIE {} — age={}s, attempt={}/{}, pane_info=[{}], running_coworkers={}, raw={:?}",
            name,
            age,
            attempt_count,
            MAX_ZOMBIE_RESPAWN_ATTEMPTS,
            pane_pid,
            snap.running_coworkers.len(),
            raw_content,
        );

        effects.push(Effect::RespawnZombieCoworker { name: name.clone() });
        effects.push(Effect::RecordCooldown {
            category: "zombie_respawn".to_string(),
            key: name.clone(),
        });
        effects.push(Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "🧟 Detected blank-pane zombie {} — respawning (attempt {}/{})",
                name, attempt_count, MAX_ZOMBIE_RESPAWN_ATTEMPTS
            ),
        });
    }

    effects
}

pub(super) async fn check_and_fire_reminders(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    // Convert snapshot HashSet to Vec for evaluate_trigger compatibility
    let open_pr_coworkers: Vec<String> = snap.coworkers_with_open_prs.iter().cloned().collect();

    let ps = state.persistent_state.lock().await;
    let mut fired_ids = Vec::new();
    let mut effects = Vec::new();

    for reminder in &ps.reminders.reminders {
        if reminder.fired {
            continue;
        }
        if crate::reminders::evaluate_trigger(&reminder.trigger, &open_pr_coworkers) {
            info!(
                "Reminder {} should fire (trigger: {}): {}",
                reminder.id, reminder.trigger, reminder.message
            );
            effects.push(Effect::PostToChannel {
                sender: "system".to_string(),
                message: format!(
                    "\u{23f0} Reminder ({}): {}",
                    reminder.trigger, reminder.message
                ),
            });
            fired_ids.push(reminder.id.clone());
        }
    }

    if !fired_ids.is_empty() {
        effects.push(Effect::MarkRemindersFired {
            fired_ids,
            repo_name: snap.repo_name.clone(),
        });
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
            name: "lexington".to_string(),
            status: CoworkerStatus::Running,
            working_dir: "/tmp/test".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            isolated_tasks: false,
        };
        let stopping = Coworker {
            name: "park".to_string(),
            status: CoworkerStatus::Stopping,
            working_dir: "/tmp/test".to_string(),
            started_at: chrono::Utc::now(),
            current_task: None,
            session_id: None,
            isolated_tasks: false,
        };

        // Build a snapshot where the nudge should fire
        let snap = snapshot::WorldSnapshot {
            active_coworkers: vec![running.clone(), stopping.clone()],
            running_coworkers: vec![running.clone()],
            coworker_snapshots: vec![],
            active_names: HashSet::new(),
            session_name: "midtown-test".to_string(),
            coworker_start_times: HashMap::new(),
            coworker_stop_times: HashMap::new(),
            pane_contents: HashMap::new(),
            blank_pane_coworkers: HashSet::new(),
            coworkers_with_running_subagents: HashSet::new(),
            in_progress_tasks: vec![],
            busy_coworkers: HashSet::new(),
            all_tasks: vec![],
            pending_tasks_with_owners: vec![],
            pending_tasks_without_owners: vec![],
            coworkers_with_open_prs: HashSet::new(),
            coworkers_with_merged_prs: HashSet::new(),
            merged_pr_numbers: HashSet::new(),
            ci_passed_pr_coworkers: HashSet::new(),
            review_feedback_pr_coworkers: HashSet::new(),
            pending_task_owners: HashSet::new(),
            active_reviewers: HashSet::new(),
            reviewer_pr_assignments: HashMap::new(),
            reviewed_prs: HashSet::new(),
            prs_needing_review: 0,
            coworkers_with_unblocked_deps: HashSet::new(),
            usage_limit_nudge_scheduled: true,
            // Set nudge time in the past so it fires
            usage_limit_nudge_at: Some(tokio::time::Instant::now() - Duration::from_secs(10)),
            usage_limited_coworkers: HashSet::new(),
            api_error_coworkers: HashSet::new(),
            channel_messages: vec![],
            daemon_logs: vec![],
            is_at_coworker_limit: false,
            is_at_dev_limit: false,
            now: Instant::now(),
            now_utc: chrono::Utc::now(),
            repo_name: "test-repo".to_string(),
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
}
