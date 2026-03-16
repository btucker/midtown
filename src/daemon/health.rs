//! Health check functions for coworker lifecycle monitoring.
//!
//! These functions detect and respond to coworker health issues:
//! usage limits, dead reviewer detection, and reminder firing.
//! Health state is read from structured `ProcessHealth` data (populated
//! by the session management layer from headless stream events).

use std::time::Duration;

use tracing::{debug, info, warn};

use super::constants::*;
use super::effects::Effect;
use super::helpers::format_task_prompt;
use super::{DaemonState, snapshot};

/// Check for reviewer processes that exited without posting their review.
///
/// Dead reviewers (process exited without posting a review) need respawning.
/// When a reviewer's Claude Code session ends naturally (max turns, rate limit,
/// context window full) before posting the review, the process dies while the
/// reviewer assignment remains.
///
/// This function detects dead reviewers with unposted reviews and respawns them,
/// up to `MAX_REVIEWER_RESTARTS` attempts per PR. When a dead reviewer exhausts
/// the restart budget, it escalates to the ops channel instead.
pub fn check_and_restart_dead_reviewers(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let respawns = crate::rules::decide_dead_reviewer_respawns(
        &snap.health.headless_process_health,
        &snap.reviewer.reviewer_pr_assignments,
        &snap.reviewer.reviewed_prs,
        &snap.reviewer.reviewer_restart_counts,
        MAX_REVIEWER_RESTARTS,
        &snap.name_session_map,
        &snap.health.usage_limited_coworkers,
    );

    let escalations = crate::rules::decide_dead_reviewer_escalations(
        &snap.health.headless_process_health,
        &snap.reviewer.reviewer_pr_assignments,
        &snap.reviewer.reviewed_prs,
        &snap.reviewer.reviewer_restart_counts,
        &snap.reviewer.reviewer_escalations_posted,
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

        // Emit a workflow event so channel scripts can react to the dead reviewer.
        let reviewer_task_id = snap
            .pr
            .pr_task_associations
            .get(&restart.pr_number)
            .cloned();
        let reviewer_channel = snap.channel_for_pr_or_default(restart.pr_number);
        effects.push(Effect::EmitWorkflowEvent(
            crate::workflow::WorkflowEvent::CoworkerStuck {
                channel: reviewer_channel,
                task_id: reviewer_task_id,
                coworker: restart.name.clone(),
            },
        ));

        // Respawn the reviewer with an incremented restart count.
        effects.extend(build_reviewer_respawn_effects(
            snap,
            &restart.name,
            restart.pr_number,
            new_restart_count,
            "exited without completing the review",
        ));

        effects.push(Effect::post_to_ops(format!(
            "🔄 Respawning reviewer {} for PR #{} — exited without posting review (attempt {}/{})",
            restart.name, restart.pr_number, new_restart_count, MAX_REVIEWER_RESTARTS,
        )));
    }

    for escalation in escalations {
        warn!(
            "Reviewer {} exited without posting review for PR #{} after {} restarts — escalating to ops",
            escalation.name, escalation.pr_number, escalation.restart_count
        );

        effects.push(Effect::post_to_ops(format!(
            "@ops PR #{} has hit max reviewer restarts — needs manual intervention. \
             Reviewer {} exited without posting a review {} times.",
            escalation.pr_number, escalation.name, escalation.restart_count,
        )));
        effects.push(Effect::nudge_channel_lead(
            &snap.project_name,
            format!(
                "Reviewer {} failed to post a review for PR #{} after {} attempts. \
                 Escalated to ops — please investigate.",
                escalation.name, escalation.pr_number, escalation.restart_count,
            ),
        ));
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
    if snap.health.usage_limit_nudge_scheduled {
        return vec![];
    }

    if snap.coworkers.active_coworkers.is_empty() {
        return vec![];
    }

    // Collect all coworkers with a usage limit flag. Usage limits are account-wide
    // so multiple coworkers may hit the limit simultaneously.
    let limited: Vec<_> = snap
        .health
        .headless_process_health
        .iter()
        .filter(|(_, health)| health.has_usage_limit)
        .collect();

    if limited.is_empty() {
        return vec![];
    }

    // Use the first detected coworker for nudge scheduling and the channel message.
    // The reset time is account-wide, so any coworker's value is representative.
    let (detected_coworker, reset_time_utc) = {
        let (name, health) = limited[0];
        (name.clone(), health.usage_limit_reset_at)
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

    let mut effects = vec![
        Effect::SetUsageLimitNudge { at: nudge_time },
        Effect::post_to_ops(message),
    ];

    // Mark pool profiles for ALL usage-limited coworkers, not just the first.
    // Multiple coworkers may hit the limit simultaneously when they share an account.
    for (coworker_name, health) in &limited {
        if let Some(profile_email) = snap.session_profile_map.get(&coworker_name.to_lowercase()) {
            info!(
                "Marking pool profile '{}' as usage-limited (via {})",
                profile_email, coworker_name
            );
            effects.push(Effect::MarkProfileLimited {
                profile_email: profile_email.clone(),
                reset_at: health.usage_limit_reset_at,
            });
        }
    }

    effects
}

/// Check if a scheduled usage limit nudge is due, and if so, nudge all running coworkers.
pub fn maybe_nudge_usage_limit_expiry(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    // Pure decision: should we nudge?
    let decision = crate::rules::decide_usage_limit_expiry(
        snap.health.usage_limit_nudge_at,
        tokio::time::Instant::now(),
    );

    if decision != crate::rules::UsageLimitExpiryDecision::NudgeNow {
        return vec![];
    }

    let eligible_session_ids: Vec<String> = snap
        .coworkers
        .running_coworkers
        .iter()
        .filter_map(|cw| {
            snap.name_session_map
                .get(&cw.name.to_lowercase())
                .cloned()
                .filter(|sid| !sid.is_empty())
        })
        .collect();

    let mut effects = vec![Effect::ClearUsageLimitNudge];

    if eligible_session_ids.is_empty() {
        info!("Usage limit expired — no running sessions to nudge");
    } else {
        info!(
            "Usage limit expired — nudging {} running sessions",
            eligible_session_ids.len()
        );

        effects.push(Effect::post_to_ops(format!(
            "🔔 Usage limit expired — nudging {} running sessions to resume work",
            eligible_session_ids.len()
        )));

        for session_id in eligible_session_ids {
            effects.push(Effect::nudge_session(session_id, "continue"));
        }
    }

    // Clear profile-level limits for ALL pool profiles currently marked
    // is_usage_limited in persistent state. The usage limit has now expired.
    //
    // We iterate `limited_pool_profiles` (from DaemonPersistentState) rather
    // than `session_profile_map` (from DaemonState) because session_profile_map
    // is ephemeral: entries are removed when coworkers stop. If a coworker exited
    // before the nudge timer fired, its profile would stay permanently excluded
    // from pool selection. Persistent state survives both coworker exits and
    // daemon restarts.
    for profile_email in &snap.limited_pool_profiles {
        info!("Clearing usage-limit on pool profile '{}'", profile_email);
        effects.push(Effect::ClearProfileLimit {
            profile_email: profile_email.clone(),
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
    if snap.health.auth_error_coworkers.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();
    let mut newly_detected = Vec::new();

    let channel_lead_names = snap.channel_lead_names();

    for name in &snap.health.auth_error_coworkers {
        // Determine session role for logging
        let is_lead = super::helpers::is_project_lead(name, &snap.project_name);
        let is_channel_lead = channel_lead_names.contains(name);
        let session_role = if is_lead {
            "Lead"
        } else if is_channel_lead {
            "Channel lead"
        } else {
            "Coworker"
        };

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
            "{} {} hit auth error (OAuth token expired) — shutting down",
            session_role, name
        );

        // Shut down the session - no point retrying with expired token.
        // Lead and channel lead sessions will be respawned by
        // ensure_lead_alive() / ensure_channel_leads_alive() after auth recovers.
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
            "🔐 OAuth token expired — sessions {} shut down. Re-authenticate with: midtown auth login\n\
             Sessions with pending tasks will be respawned after re-authentication.",
            names_str
        );

        effects.insert(0, Effect::post_to_ops(message.clone()));

        // Nudge the lead so the user sees this immediately
        effects.push(Effect::nudge_channel_lead(&snap.default_channel, message));
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
    if snap.health.api_error_coworkers.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();
    let mut first_detection = false;

    for name in &snap.health.api_error_coworkers {
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

        let session_id = snap
            .name_session_map
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default();
        effects.push(Effect::nudge_session(
            session_id,
            "The API error may have cleared. Try continuing your work.",
        ));
        effects.push(Effect::RecordCooldown {
            category: "api_error_nudge".to_string(),
            key: name.clone(),
        });
    }

    // Post a channel message when API errors are widespread (2+ coworkers affected)
    // Only post on first detection of a widespread outage to avoid spam.
    let affected_count = snap.health.api_error_coworkers.len();
    if first_detection && affected_count >= 2 {
        let names: Vec<&str> = snap
            .health
            .api_error_coworkers
            .iter()
            .map(|s| s.as_str())
            .collect();
        effects.insert(
            0,
            Effect::post_to_ops(format!(
                "⚠️ Widespread API errors affecting {} coworkers: {}. Will periodically nudge to retry.",
                affected_count,
                names.join(", ")
            )),
        );
    }

    effects
}

/// Detect coworkers with unrecoverable session errors and restart them fresh.
///
/// The `has_tool_name_conflict` flag currently covers unrecoverable conditions:
/// - "Tool names must be unique" registration conflicts.
/// - Stale Codex resume/session IDs (e.g., "no rollout found for thread id ...").
/// - Context exhaustion ("prompt is too long") — the conversation outgrew the model's window.
///
/// Generic API retries cannot fix these. We clear the saved session ID first, then
/// shut down and restart fresh.
pub fn check_and_restart_tool_name_conflicts(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    if snap.health.tool_name_conflict_coworkers.is_empty() {
        return vec![];
    }

    let mut effects = Vec::new();

    for name in &snap.health.tool_name_conflict_coworkers {
        warn!(
            "Coworker {} has unrecoverable session error — restarting with fresh session",
            name
        );

        effects.push(Effect::ClearSavedSessionId { name: name.clone() });
        let session_id = snap
            .name_session_map
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, sid)| sid);
        effects.push(shutdown_effect(
            name,
            session_id,
            "unrecoverable session error".to_string(),
        ));
        // Restart the project lead immediately (don't wait for ensure_lead_alive
        // cooldown), so the user-facing lead recovers within the same tick.
        if name.eq_ignore_ascii_case(&snap.project_name) {
            let mut config = crate::launch::LaunchConfig::lead(&snap.dir_key, None);
            config.model = super::helpers::resolve_model_for_role(
                &snap.dir_key,
                config.auth_provider,
                &config.role,
            );
            let lead_wt = crate::paths::lead_worktree_path(&snap.dir_key);
            if lead_wt.exists() {
                config.working_dir = Some(lead_wt);
            }
            effects.push(Effect::SpawnCoworker(config));
        }
        effects.push(Effect::post_to_ops(format!(
            "🔧 Coworker {} hit an unrecoverable session error — clearing saved session ID and restarting fresh",
            name
        )));
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
        &snap.health.headless_process_health,
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

        // Emit a workflow event so channel scripts can react to the dead process.
        effects.push(Effect::EmitWorkflowEvent(
            crate::workflow::WorkflowEvent::CoworkerStuck {
                channel: channel.clone().unwrap_or_else(|| snap.project_name.clone()),
                task_id: Some(respawn.task_id.clone()),
                coworker: respawn.name.clone(),
            },
        ));

        let mut config = crate::launch::LaunchConfig::coworker(
            respawn.name.clone(),
            state.paths.dir_key().to_string(),
            crate::launch::SessionMode::Fresh,
            Some(prompt),
            Some(respawn.task_id.clone()),
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
        effects.push(Effect::post_to_ops(format!(
            "💀 Coworker {} process died (exit {}) — restarting for task !{}",
            respawn.name, respawn.exit_code, respawn.task_id
        )));
    }

    effects
}

// Note: fork crash recovery is handled in the session_drain handler (mod.rs),
// not via a snapshot-based function. See the "Crash recovery" section in
// docs/architecture.md for the rationale (cleanup ordering).

/// Ensure the lead session is always running.
///
/// The lead is the human-facing session that should never be permanently down.
/// If the lead is not in `active_coworkers` (dead and deregistered), respawn it.
/// Uses `coworker_stop_times` as a cooldown to prevent rapid respawn loops.
pub fn ensure_lead_alive(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    // Check if lead is already registered (any status)
    let lead_registered = snap
        .coworkers
        .active_coworkers
        .iter()
        .any(|c| c.name.eq_ignore_ascii_case(&snap.project_name));

    if lead_registered {
        return vec![];
    }

    // Check if lead is currently attached interactively — if so, the daemon
    // shouldn't spawn a headless lead that would conflict.
    if snap
        .coworkers
        .attached_coworkers
        .contains_key(&snap.project_name.to_lowercase())
    {
        return vec![];
    }

    // Cooldown: if the lead was recently stopped (within 5 minutes), don't
    // respawn yet to prevent crash loops. The lead may have been stopped for
    // a good reason (e.g., auth error, attach/detach cycle).
    if let Some(stop_time) = snap
        .coworkers
        .coworker_stop_times
        .get(&snap.project_name.to_lowercase())
    {
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

    let mut config = crate::launch::LaunchConfig::lead(&snap.dir_key, None);
    config.model =
        super::helpers::resolve_model_for_role(&snap.dir_key, config.auth_provider, &config.role);
    let lead_wt = crate::paths::lead_worktree_path(&snap.dir_key);
    if lead_wt.exists() {
        config.working_dir = Some(lead_wt);
    }

    vec![Effect::SpawnCoworker(config)]
}

/// Ensure channel lead sessions are always running.
///
/// Channel leads are long-lived domain expert sessions that should be respawned
/// when they die unexpectedly. This is the channel lead equivalent of
/// `ensure_lead_alive()` for the project lead.
///
/// For each channel in `channel_lead_sessions`, checks if the session is still
/// alive. If not, emits a `RespawnChannelLead` effect (I/O deferred to executor).
/// Uses `coworker_stop_times` as a cooldown to prevent rapid respawn loops.
pub fn ensure_channel_leads_alive(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    for channel_name in snap.channel_lead_sessions.keys() {
        // Check if the channel lead is already registered (any status)
        let is_registered = snap
            .coworkers
            .active_coworkers
            .iter()
            .any(|c| c.name.eq_ignore_ascii_case(channel_name));

        if is_registered {
            continue;
        }

        // Check if the channel lead is currently attached interactively — if so,
        // the daemon shouldn't spawn a headless session that would conflict.
        if snap
            .coworkers
            .attached_coworkers
            .contains_key(&channel_name.to_lowercase())
        {
            continue;
        }

        // Cooldown: if the channel lead was recently stopped, don't respawn yet
        if let Some(stop_time) = snap
            .coworkers
            .coworker_stop_times
            .get(&channel_name.to_lowercase())
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

        effects.push(Effect::RespawnChannelLead {
            channel_name: channel_name.clone(),
        });
    }

    effects
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
    if snap
        .coworkers
        .attached_coworkers
        .contains_key(&snap.project_name.to_lowercase())
    {
        return vec![];
    }

    // Find the lead in active coworkers
    let lead = snap
        .coworkers
        .active_coworkers
        .iter()
        .find(|c| c.name.eq_ignore_ascii_case(&snap.project_name));

    let lead = match lead {
        Some(l) => l,
        None => return vec![],
    };

    // Check how long the lead has been running
    let start_time = match snap
        .coworkers
        .coworker_start_times
        .get(&snap.project_name.to_lowercase())
    {
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
        Effect::post_to_ops("Restarting lead session for a fresh context."),
        Effect::ShutdownCoworker {
            name: lead.name.clone(),
            message: "Time for a fresh session. Restarting now — will be back shortly.".to_string(),
        },
    ]
}

/// Detect attached sessions that have exceeded `ATTACH_TIMEOUT` without receiving a detach.
///
/// If an interactive session ends without `midtown agent detach` (terminal crash,
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
    snap.coworkers.attached_coworkers
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

pub(super) async fn check_and_fire_reminders(
    snap: &snapshot::WorldSnapshot,
    state: &DaemonState,
) -> Vec<Effect> {
    let open_pr_coworkers: Vec<String> = snap.pr.coworkers_with_open_prs.iter().cloned().collect();
    let ps = state.persistent_state.lock().await;
    build_reminder_effects(
        &ps.reminders.reminders,
        &open_pr_coworkers,
        &snap.dir_key,
        &snap.default_channel,
    )
}

/// Pure function: evaluate reminders and build effects (PostToChannel + NudgeChannelLead + MarkFired).
fn build_reminder_effects(
    reminders: &[crate::reminders::Reminder],
    open_pr_coworkers: &[String],
    dir_key: &str,
    default_channel: &str,
) -> Vec<Effect> {
    let fired: Vec<&crate::reminders::Reminder> = reminders
        .iter()
        .filter(|r| !r.fired && crate::reminders::evaluate_trigger(&r.trigger, open_pr_coworkers))
        .collect();
    effects_for_fired_reminders(&fired, dir_key, default_channel)
}

/// Build effects for reminders that have already been evaluated as firing.
fn effects_for_fired_reminders(
    fired: &[&crate::reminders::Reminder],
    dir_key: &str,
    default_channel: &str,
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
        effects.push(Effect::post_to_channel("midtown", message.clone(), None));
        effects.push(Effect::nudge_channel_lead(default_channel, message));
        fired_ids.push(reminder.id.clone());
    }

    if !fired_ids.is_empty() {
        effects.push(Effect::MarkRemindersFired {
            fired_ids,
            dir_key: dir_key.to_string(),
        });
    }

    effects
}

/// Check for stale worktrees that can be cleaned up.
///
/// Worktrees are considered stale if:
/// - They are older than `retention_period`, based on `completed_at` when set,
///   otherwise `created_at` (abandoned worktree fallback)
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
        // Skip if actively in use
        if let Some(ref coworker) = assignment.current_coworker
            && active_coworkers.contains(coworker)
        {
            continue;
        }

        // Use completion age when available. For abandoned worktrees where
        // completed_at was never set, fall back to created_at age.
        let age =
            now.signed_duration_since(assignment.completed_at.unwrap_or(assignment.created_at));
        if age < retention_period {
            continue;
        }

        debug!(
            "Worktree {} is stale (age {}h, completed_at={}), scheduling cleanup",
            assignment.worktree_id,
            age.num_hours(),
            assignment.completed_at.is_some()
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

/// Garbage-collect stale daemon persistent state.
///
/// Examines session records and task metadata maps to identify data that can
/// be pruned. Returns a single `GarbageCollectState` effect that performs the
/// cleanup atomically.
///
/// **Session pruning:**
/// - Reviewer sessions (`is_reviewer=true`) that are stopped are removed
///   immediately (no retention wait) since they're never resumed.
/// - Channel lead sessions (`coworker_type="channel-lead"`) are never pruned —
///   they are long-lived and should always be available for resume.
/// - Non-reviewer, non-channel-lead sessions where `is_running=false` AND
///   `resume_on_startup=false` AND `last_active` older than `retention_period`
///   are removed entirely (including their `initial_prompt`, which is dropped
///   with the whole record).
///
/// **Task metadata pruning:**
/// - Entries in task_channel, task_model, task_plan, task_execution_skill,
///   task_thread_id, and task_message_id are pruned when their task_id doesn't
///   appear in any surviving session record or active task.
///
/// **Note:** `initial_prompt` is intentionally preserved on stopped sessions
/// within the retention window because `session.clear` uses it to restart
/// sessions with their original context.
pub(super) fn check_for_state_gc(
    sessions: &std::collections::HashMap<String, crate::daemon::state::SessionRecord>,
    active_session_ids: &std::collections::HashSet<String>,
    task_metadata_keys: &std::collections::HashSet<String>,
    active_task_ids: &std::collections::HashSet<String>,
    retention_period: chrono::Duration,
) -> Vec<Effect> {
    let now = chrono::Utc::now();

    let mut dead_session_ids = Vec::new();

    // Collect task IDs referenced by sessions that will survive GC
    let mut surviving_task_ids = std::collections::HashSet::new();

    for (session_id, record) in sessions {
        // Skip running sessions entirely
        if record.is_running || active_session_ids.contains(session_id) {
            if let Some(ref tid) = record.task_id {
                surviving_task_ids.insert(tid.clone());
            }
            continue;
        }

        // Dead reviewer sessions: prune immediately (ephemeral lifecycle).
        // Resume-on-@mention uses PrReviewerAssignment (not session records).
        if record.is_reviewer {
            dead_session_ids.push(session_id.clone());
            continue;
        }

        // Channel lead sessions are long-lived and should always be available
        // for resume — never garbage-collect them.
        if record.coworker_type == "channel-lead" {
            if let Some(ref tid) = record.task_id {
                surviving_task_ids.insert(tid.clone());
            }
            continue;
        }

        // Dead non-reviewer sessions: check retention
        let age = now.signed_duration_since(record.last_active);
        if !record.resume_on_startup && age >= retention_period {
            dead_session_ids.push(session_id.clone());
            continue;
        }

        // Surviving stopped session: preserve for session.clear
        if let Some(ref tid) = record.task_id {
            surviving_task_ids.insert(tid.clone());
        }
    }

    // Orphaned task metadata: keys that aren't referenced by any surviving
    // session or any active task in the task list
    let mut orphaned_task_ids: Vec<String> = task_metadata_keys
        .iter()
        .filter(|tid| !surviving_task_ids.contains(*tid) && !active_task_ids.contains(*tid))
        .cloned()
        .collect();
    orphaned_task_ids.sort(); // deterministic ordering for tests

    if dead_session_ids.is_empty() && orphaned_task_ids.is_empty() {
        return vec![];
    }

    if !dead_session_ids.is_empty() {
        info!(
            "State GC: scheduling removal of {} dead session(s) ({} reviewer)",
            dead_session_ids.len(),
            dead_session_ids
                .iter()
                .filter(|sid| sessions.get(*sid).map(|r| r.is_reviewer).unwrap_or(false))
                .count()
        );
    }

    vec![Effect::GarbageCollectState {
        dead_session_ids,
        orphaned_task_ids,
    }]
}

/// Build the effects needed to respawn a reviewer for a given PR.
///
/// Used by `check_and_restart_dead_reviewers`. Handles the
/// worktree-setup → launch-config → spawn sequence, and optionally updates an
/// abandoned "Review in progress" placeholder comment.
///
/// ## Caller responsibilities
///
/// **Pre-spawn:** `check_and_restart_dead_reviewers` must *not* push a shutdown
/// effect — the process has already exited, so no shutdown is needed.
///
/// **Post-spawn:** the caller appends its own trailing effects after this
/// helper returns (e.g. the ops-channel message for dead reviewers).
fn build_reviewer_respawn_effects(
    snap: &snapshot::WorldSnapshot,
    name: &str,
    pr_number: u64,
    new_restart_count: u32,
    abandoned_reason: &str,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);
    let wt_path = crate::paths::worktrees_dir_for_repo(&snap.dir_key).join(&worktree_id);

    // If there's a dangling "Review in progress" placeholder, mark it as abandoned.
    if let Some(&comment_id) = snap
        .reviewer
        .reviewer_in_progress_comment_ids
        .get(&pr_number)
    {
        let repo_full_name = format!(
            "{}/{}",
            snap.repo_owner.as_deref().unwrap_or("unknown"),
            snap.project_name
        );
        let abandoned_body = format!(
            "<!-- midtown: midtown -->\n\n\
             ## Review Status\n\n\
             ⚠️ Previous reviewer `{}` {} \
             (attempt {}/{}).\n\
             A replacement reviewer has been assigned.\n\n\
             🌃 Co-built with [Midtown](https://github.com/btucker/midtown)",
            name, abandoned_reason, new_restart_count, MAX_REVIEWER_RESTARTS
        );
        effects.push(Effect::UpdatePrComment {
            comment_id,
            repo_full_name,
            new_body: abandoned_body,
        });
    }

    let reviewer_provider = crate::config::get_execution_provider_for_role(
        &snap.dir_key,
        crate::config::ExecutionRole::Reviewer,
    );
    let mut config = crate::launch::LaunchConfig::reviewer(
        name.to_string(),
        &snap.dir_key,
        pr_number,
        new_restart_count,
        reviewer_provider,
    );
    config.model = super::helpers::normalize_model_for_provider_role(
        &config.model,
        config.auth_provider,
        &config.role,
    );
    config.working_dir = Some(wt_path.clone());

    // Route reviewer to the task's topic channel.
    config.channel = snap.channel_for_pr(pr_number);

    // Route escalation mentions to the channel lead when available.
    if let Some(ref channel_name) = config.channel {
        let lead_names = snap.channel_lead_names();
        if lead_names.contains(channel_name) {
            config.escalation_target = Some(channel_name.clone());
            // Belt-and-suspenders: regenerate the initial prompt with the escalation
            // target so the reviewer knows who to address even if the system prompt
            // substitution fails.
            config.initial_prompt = Some(crate::agents::reviewer_launch_prompt(
                pr_number,
                new_restart_count,
                reviewer_provider,
                Some(channel_name),
            ));
        } else {
            warn!(
                "PR #{}: task has channel {:?} but no channel lead registered; \
                 reviewer escalation_target falls back to project name",
                pr_number, channel_name
            );
        }
    }

    effects.push(Effect::EnsureWorktree {
        worktree_id: worktree_id.clone(),
        path: wt_path,
    });

    let on_success = vec![
        Effect::BindCoworkerToWorktree {
            worktree_id,
            coworker: name.to_string(),
        },
        Effect::BroadcastCoworkerUpdate {
            name: name.to_string(),
            status: "running".to_string(),
            current_task: Some(format!("reviewing PR #{}", pr_number)),
        },
        Effect::AssignReviewer {
            pr_number,
            reviewer_name: name.to_string(),
            source: crate::github_state::AssignmentSource::Manual,
            restart_count: new_restart_count,
            reviewer_session_id: None,
            task_id: snap
                .all_tasks
                .iter()
                .find(|t| {
                    t.pr == Some(pr_number)
                        && snap
                            .task_agent_type_map
                            .get(&t.id)
                            .is_some_and(|at| at == "midtown-code-reviewer")
                })
                .map(|t| t.id.clone()),
        },
    ];

    let on_failure = vec![Effect::post_to_ops(format!(
        "⚠️ Failed to respawn reviewer {} for PR #{} (attempt {}/{})",
        name, pr_number, new_restart_count, MAX_REVIEWER_RESTARTS,
    ))];

    effects.push(Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success,
        on_failure,
    });

    effects
}

/// Return the appropriate shutdown [`Effect`] for a coworker.
///
/// Prefers session-centric `ShutdownSession` when `session_id` is provided,
/// falling back to name-based `ShutdownCoworker` otherwise.
fn shutdown_effect(name: &str, session_id: Option<&String>, reason: String) -> Effect {
    if let Some(sid) = session_id {
        Effect::ShutdownSession {
            session_id: sid.clone(),
            reason,
        }
    } else {
        Effect::ShutdownCoworker {
            name: name.to_string(),
            message: String::new(),
        }
    }
}

/// Check for stale notes across all channels and nudge channel leads.
///
/// Scans channel note directories for notes with stale `reviewed_at` frontmatter
/// and emits `NudgeChannelLead` effects. Uses the pre-evaluated cooldown set
/// from the snapshot to avoid repeated nudges within a 24-hour window.
///
/// Note: This function does filesystem I/O (reads note files from disk).
/// It's called only from the low-frequency `NoteReviewTick` (once per hour).
pub fn check_for_stale_notes(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    for (channel_name, stale_notes) in &snap.stale_channel_notes {
        // Only nudge channels that have a channel lead session
        if !snap.channel_lead_sessions.contains_key(channel_name) {
            continue;
        }

        // Skip if recently nudged (pre-evaluated cooldown)
        if snap.note_staleness_cooldown_channels.contains(channel_name) {
            continue;
        }

        // Build the nudge message listing stale notes
        let note_list: Vec<String> = stale_notes
            .iter()
            .map(|name| format!("  - {}", name))
            .collect();

        let message = format!(
            "These notes haven't been reviewed in 3+ days:\n{}\n\n\
             Review each one — confirm it's still accurate with \
             `midtown notes review <path>`, or delete it if no longer relevant.\n\
             Bias toward deleting notes that are outdated or redundant.",
            note_list.join("\n")
        );

        effects.push(Effect::nudge_channel_lead(channel_name.clone(), message));
        effects.push(Effect::RecordCooldown {
            category: "note_staleness".to_string(),
            key: channel_name.clone(),
        });
    }

    effects
}

/// Check channel lead worktrees for staleness and nudge leads to update.
///
/// For each channel name in `stale_channel_lead_worktrees` that is not on
/// cooldown, emits a `NudgeChannelLead` effect telling the lead to update
/// their worktree, plus a `RecordCooldown` to prevent spamming.
///
/// Decision function — no async I/O or mutation. Staleness data is pre-computed
/// during snapshot collection; cooldown state is pre-evaluated into the snapshot.
/// (Uses `info!()` for logging, consistent with other health decision functions.)
pub fn check_channel_lead_worktree_freshness(snap: &snapshot::WorldSnapshot) -> Vec<Effect> {
    let mut effects = Vec::new();

    for channel_name in &snap.stale_channel_lead_worktrees {
        // Skip if recently nudged (pre-evaluated cooldown)
        if snap
            .lead_worktree_freshness_cooldown_channels
            .contains(channel_name)
        {
            continue;
        }

        let branch = &snap.default_branch;
        info!(
            "Channel lead '{}' worktree is behind origin/{} — nudging to update",
            channel_name, branch
        );

        effects.push(Effect::nudge_channel_lead(
            channel_name.clone(),
            format!(
                "Your worktree is behind origin/{}. Run: `git fetch origin && git checkout --detach origin/{}`",
                branch, branch
            ),
        ));
        effects.push(Effect::RecordCooldown {
            category: "lead_worktree_freshness".to_string(),
            key: channel_name.clone(),
        });
    }

    effects
}

#[path = "health_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "health_worktree_freshness_tests.rs"]
#[cfg(test)]
mod worktree_freshness_tests;
