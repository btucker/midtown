//! Health check functions for coworker lifecycle monitoring.
//!
//! These functions detect and respond to coworker health issues:
//! idle shutdown, stuck panes, usage limits, zombie processes, and
//! reminder firing. Pane scraping is used exclusively for health
//! detection — workflow state is reported via RPC.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};

use crate::{config, daemon_messages, web};

use super::constants::*;
use super::effects::Effect;
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
pub(super) fn check_and_respawn_lead(
    session: &str,
    workdir: &Path,
    project_name: &str,
    additional_dirs: &[PathBuf],
) {
    // First check if the tmux session itself exists. If the entire session
    // is gone (e.g., user killed it), don't try to recreate — that's intentional.
    let session_check = std::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .output();
    match session_check {
        Ok(o) if o.status.success() => {}
        _ => return, // session gone entirely, don't interfere
    }

    // Session exists — check how many lead windows are present.
    // Using count_windows_by_name instead of window_exists to detect
    // duplicates that can accumulate from restart races.
    match crate::tmux::count_windows_by_name(session, "lead") {
        Ok((0, _)) => {
            warn!("Lead window missing in session {}, respawning...", session);
            match crate::tmux::spawn_lead(
                session,
                &workdir.to_string_lossy(),
                project_name,
                additional_dirs,
            ) {
                Ok(()) => info!("Successfully respawned lead window"),
                Err(e) => error!("Failed to respawn lead window: {}", e),
            }
        }
        Ok((1, _)) => {} // exactly one lead window, all good
        Ok((n, ids)) => {
            // Multiple lead windows detected — kill all but the first one
            warn!(
                "Found {} duplicate lead windows in session {}, cleaning up extras",
                n, session
            );
            for id in ids.iter().skip(1) {
                let target = format!("{}:{}", session, id);
                info!("Killing duplicate lead window {}", target);
                let _ = crate::tmux::kill_window_by_target(&target);
            }
        }
        Err(e) => {
            warn!("Failed to check lead window status: {}", e);
        }
    }
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
/// - Their tmux pane content changed recently (actively working)
/// - They have unblocked dependent tasks
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
            &records,
            snap.now,
            snap.now_utc,
            IDLE_BREAK_DURATION,
            MINIMUM_COWORKER_LIFETIME,
            PANE_ACTIVITY_GRACE,
        );
        crate::rules::apply_health_transitions(&mut records, transitions);
        decisions
    };

    let mut effects = Vec::new();

    // Determine effects for idle coworkers
    for decision in to_shutdown {
        let name = &decision.name;

        // For isolated coworkers (reviewers), verify the review was actually posted
        let (should_shutdown, shutdown_msg) = if decision.is_isolated {
            // Look up the PR this reviewer was assigned to (from snapshot)
            let pr_number = snap.reviewer_pr_assignments.get(name).copied();

            match pr_number {
                Some(pr) => {
                    // Check if review was actually posted (from snapshot, no API call)
                    if snap.reviewed_prs.contains(&pr) {
                        info!(
                            "Sending reviewer {} on a break (review verified for PR #{})",
                            name, pr
                        );
                        (
                            true,
                            daemon_messages::break_review_complete(
                                name,
                                pr,
                                config::get_personality(),
                            ),
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
                }
                None => {
                    // Can't find PR assignment — check if their work already merged
                    if snap.coworkers_with_merged_prs.contains(name) {
                        info!(
                            "Isolated coworker {} has no PR assignment but has merged PR, sending on a break",
                            name
                        );
                        (
                            true,
                            daemon_messages::break_work_merged(name, config::get_personality()),
                        )
                    } else {
                        warn!(
                            "Isolated coworker {} has no PR assignment found, sending on a break",
                            name
                        );
                        (
                            true,
                            daemon_messages::break_no_pr(name, config::get_personality()),
                        )
                    }
                }
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

        let prompt = format!(
            "You've been assigned task #{}: {}. Your previous session appeared stuck so you were restarted. Check your git status and continue where you left off.",
            restart.task_id, restart.task_subject
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

/// Check if a scheduled usage limit nudge is due, and if so, nudge all active coworkers.
pub(super) fn maybe_nudge_usage_limit_expiry(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    // Pure decision: should we nudge?
    let decision = crate::rules::decide_usage_limit_expiry(
        snap.usage_limit_nudge_at,
        tokio::time::Instant::now(),
    );

    if decision != crate::rules::UsageLimitExpiryDecision::NudgeNow {
        return vec![];
    }

    if snap.active_coworkers.is_empty() {
        return vec![];
    }

    info!(
        "Usage limit expired — nudging {} active coworkers",
        snap.active_coworkers.len()
    );

    let mut effects = vec![
        Effect::ClearUsageLimitNudge,
        Effect::PostToChannel {
            sender: "system".to_string(),
            message: format!(
                "🔔 Usage limit expired — nudging {} coworkers to resume work",
                snap.active_coworkers.len()
            ),
        },
    ];

    for cw in &snap.active_coworkers {
        effects.push(Effect::NudgeCoworker {
            name: cw.name.clone(),
            message: "continue".to_string(),
        });
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

                info!(
                    "Coworker {} has queued nudges not being processed — sending Escape to interrupt",
                    name
                );
                effects.push(Effect::SendRawKeys {
                    name: name.clone(),
                    keys: "Escape".to_string(),
                });
                effects.push(Effect::RecordCooldown {
                    category: "queued_prompt_recovery".to_string(),
                    key: name.clone(),
                });
                effects.push(Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "📨 Interrupted {} to process queued nudges (sent Escape)",
                        name
                    ),
                });
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

    // Build a set of isolated (reviewer) coworker names for fast lookup
    let isolated_coworkers: HashSet<&str> = snap
        .coworker_snapshots
        .iter()
        .filter(|cw| cw.isolated_tasks)
        .map(|cw| cw.name.as_str())
        .collect();

    let mut effects = Vec::new();
    for name in zombies {
        // Skip isolated (reviewer) coworkers — they were one-shot tasks spawned
        // with a specific review prompt. Respawning with --continue and no prompt
        // would produce a confused coworker that joins the shared task list without
        // knowing which PR to review. Just shut them down and alert.
        if isolated_coworkers.contains(name.as_str()) {
            warn!(
                "Blank-pane zombie {} is an isolated reviewer — shutting down instead of respawning",
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
}
