//! Auth-related RPC handlers.
//!
//! Extracted from `rpc.rs` to keep that file focused on dispatch and
//! simpler handlers. The `auth.switch` flow is the most complex single
//! handler—it validates, switches profiles, shuts down and relaunches
//! coworkers, and optionally restarts the lead window.

use std::collections::{HashMap, HashSet};

use tracing::{info, warn};

use crate::message::Message;
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::constants::OPS_CHANNEL;

// ============================================================================
// Auth helper types
// ============================================================================

fn filter_coworkers_by_provider(
    coworkers: &[crate::coworker::Coworker],
    provider: crate::auth::AuthProvider,
) -> Vec<crate::coworker::Coworker> {
    coworkers
        .iter()
        .filter(|cw| cw.provider == provider)
        .cloned()
        .collect()
}

fn build_coworker_relaunch_config(
    coworker: &crate::coworker::Coworker,
    repo_name: &str,
) -> crate::launch::LaunchConfig {
    let mut config = crate::launch::LaunchConfig::coworker(
        coworker.name.clone(),
        repo_name.to_string(),
        crate::launch::SessionMode::Resume,
        None,
    );
    config.model = coworker.model.clone();
    config.auth_provider = coworker.provider;
    config
}

fn build_fresh_coworker_relaunch_config(
    coworker: &crate::coworker::Coworker,
    repo_name: &str,
    task_id: Option<&str>,
) -> crate::launch::LaunchConfig {
    let initial_prompt = task_id.map(|task_id| {
        format!(
            "You've been assigned task !{}. Run `midtown task view {}` for full details.",
            task_id, task_id
        )
    });
    let mut config = crate::launch::LaunchConfig::coworker(
        coworker.name.clone(),
        repo_name.to_string(),
        crate::launch::SessionMode::Fresh,
        initial_prompt,
    );
    config.model = coworker.model.clone();
    config
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPlatform {
    ClaudeCli,
    CodexCli,
}

fn platform_for_provider(provider: crate::auth::AuthProvider) -> SessionPlatform {
    match provider {
        crate::auth::AuthProvider::Claude | crate::auth::AuthProvider::Zai => {
            SessionPlatform::ClaudeCli
        }
        crate::auth::AuthProvider::Codex => SessionPlatform::CodexCli,
    }
}

fn can_resume_between_providers(
    from: crate::auth::AuthProvider,
    to: crate::auth::AuthProvider,
) -> bool {
    platform_for_provider(from) == platform_for_provider(to)
}

fn execution_role_for_coworker(
    coworker: &crate::coworker::Coworker,
    reviewer_pr_by_name: &HashMap<String, u64>,
    channel_lead_session_names: &HashSet<String>,
) -> crate::config::ExecutionRole {
    if coworker.name.eq_ignore_ascii_case("lead") {
        crate::config::ExecutionRole::Lead
    } else if reviewer_pr_by_name.contains_key(&coworker.name) {
        crate::config::ExecutionRole::Reviewer
    } else if channel_lead_session_names.contains(&coworker.name) {
        crate::config::ExecutionRole::ChannelLead
    } else {
        crate::config::ExecutionRole::Coworker
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeadRelaunchStatus {
    Relaunched,
    Failed,
    Unchanged,
}

impl LeadRelaunchStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Relaunched => "relaunched",
            Self::Failed => "failed",
            Self::Unchanged => "unchanged",
        }
    }

    fn summary(self) -> &'static str {
        match self {
            Self::Relaunched => "re-launched lead",
            Self::Failed => "lead re-launch failed",
            Self::Unchanged => "lead unchanged",
        }
    }

    fn relaunched(self) -> bool {
        matches!(self, Self::Relaunched)
    }

    fn attempted(self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

// ============================================================================
// Handler
// ============================================================================

/// Handle auth.switch RPC method.
///
/// Switches the active auth profile.
///
/// Also re-launches active sessions for the switched provider:
/// 1. Validates and switches the profile on disk (project or global)
/// 2. Shuts down all running coworkers (daemon will re-spawn for pending tasks)
/// 3. Re-launches the lead window with the new credentials
pub(super) async fn handle_auth_switch(
    id: RequestId,
    profile: &str,
    all: bool,
    provider: crate::auth::AuthProvider,
    state: &DaemonState,
) -> Response {
    // Validate the profile name format (defense-in-depth — CLI also validates)
    if let Err(e) = crate::auth::validate_profile_name(profile) {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Invalid profile name: {}", e)),
        );
    }

    // Validate the profile exists
    if !crate::auth::profile_exists_for(provider, profile) {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!(
                    "Profile '{}' does not exist for {}. Create it with: midtown auth --provider {} login {}",
                    profile, provider, provider, profile
                ),
            ),
        );
    }

    // Check if already on this profile
    if !all {
        // For per-project switch, check the project config's auth_profile (not the effective profile).
        // This is distinct from `active_profile_for_project_with_provider()`, which falls back to the
        // global profile. Using the wrong function here would create false positive "already on profile".
        let path = crate::config::project_config_path(&state.repo_name);
        if let Some(config) = crate::config::FullProjectConfig::load_from(&path)
            && crate::auth::project_profile_override(&config.project, provider) == Some(profile)
        {
            return Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Already on {} profile '{}'", provider, profile),
                    "switched": false,
                }),
            );
        }
    }

    // Switch the profile on disk
    if all {
        // Global switch: update global current profile and clear per-project overrides.
        // Even when the global profile already matches, we must still clear overrides
        // so projects stop shadowing the global setting.
        let current = crate::auth::current_profile_for(provider);
        let cleared = crate::config::clear_all_project_auth_profiles_for(provider);
        if current != profile
            && let Err(e) = crate::auth::set_current_profile_for(provider, profile)
        {
            return Response::error(
                id,
                RpcError::new(-32603, format!("Failed to switch profile: {}", e)),
            );
        }
        if current == profile && cleared == 0 {
            return Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Already on {} profile '{}'", provider, profile),
                    "switched": false,
                }),
            );
        }
    } else {
        // Per-project switch: update this project's config
        let path = crate::config::project_config_path(&state.repo_name);
        let mut config = crate::config::FullProjectConfig::load_from(&path).unwrap_or_default();
        crate::auth::set_project_profile_override(
            &mut config.project,
            provider,
            profile.to_string(),
        );
        if let Err(e) = config.save_to(&path) {
            return Response::error(
                id,
                RpcError::new(-32603, format!("Failed to save project config: {}", e)),
            );
        }
    }

    info!(
        "Auth profile switched to '{}' for {} ({})",
        profile,
        provider,
        if all { "global" } else { "project" }
    );

    // Shut down all running non-lead coworkers for this provider.
    let running_coworkers: Vec<crate::coworker::Coworker> =
        filter_coworkers_by_provider(&state.coworkers.list(), provider)
            .into_iter()
            .filter(|cw| !cw.name.eq_ignore_ascii_case("lead"))
            .collect();

    let current_lead_provider = state
        .coworkers
        .list()
        .into_iter()
        .find(|cw| cw.name.eq_ignore_ascii_case("lead"))
        .map(|cw| cw.provider);

    let shutdown_count = running_coworkers.len();
    for coworker in &running_coworkers {
        let name = &coworker.name;

        // Shut down the headless session (async), then deregister from tracking (sync).
        // This matches the correct shutdown sequence in rpc_coworker.rs (handle_coworker_break).
        // Use session_manager.shutdown() to properly stop headless sessions.
        if let Err(e) = state.session_manager.shutdown(name).await {
            warn!("Failed to shut down headless session for {}: {}", name, e);
        }
        state.coworkers.deregister(name);

        state.record_coworker_stop_time(name);
        // Only clear records on successful shutdown
        {
            let mut records = state.coworker_records.write().await;
            records.remove(name);
        }
        state.broadcast_coworker_update(name, "stopped", None);
    }

    // Capture reviewer + channel-lead role context before relaunch so role-aware
    // provider resolution can decide between resume and fresh spawn.
    let (reviewer_pr_by_name, channel_lead_session_names): (HashMap<String, u64>, HashSet<String>) = {
        let persistent = state.persistent_state.lock().await;
        let reviewers = running_coworkers
            .iter()
            .filter_map(|cw| {
                persistent
                    .github
                    .pr_for_reviewer(&cw.name)
                    .map(|pr| (cw.name.clone(), pr))
            })
            .collect();
        let channel_leads = persistent
            .channel_lead_sessions
            .keys()
            .map(|channel| crate::launch::channel_lead_session_name(channel))
            .collect();
        (reviewers, channel_leads)
    };
    let task_id_by_coworker: HashMap<String, String> = {
        let assignments = state.coworker_task_assignments.lock().unwrap();
        assignments
            .iter()
            .map(|(coworker_name, assignment)| (coworker_name.clone(), assignment.task_id.clone()))
            .collect()
    };

    // Re-launch lead only if it currently runs on the switched provider.
    // Target provider always comes from role-based config (lead provider).
    let configured_lead_provider = crate::config::get_execution_provider_for_role(
        &state.repo_name,
        crate::config::ExecutionRole::Lead,
    );
    let lead_relaunch_status = if current_lead_provider == Some(provider) {
        // Shut down the existing headless lead session if running
        if state.session_manager.is_alive("lead").await {
            let _ = state.session_manager.shutdown("lead").await;
            state.coworkers.deregister("lead");
        }
        let mut lead_config = crate::launch::LaunchConfig::lead(&state.repo_name, None);
        lead_config.auth_provider = configured_lead_provider;
        lead_config.model = super::helpers::default_model_for_provider_role(
            lead_config.auth_provider,
            &lead_config.role,
        )
        .to_string();
        lead_config.auth_profile_dir =
            Some(crate::auth::active_profile_dir_for_project_with_provider(
                &state.repo_name,
                configured_lead_provider,
            ));
        match state.spawn_coworker(&lead_config).await {
            Ok(()) => {
                info!(
                    "Re-launched lead with {} auth profile '{}'",
                    configured_lead_provider, profile
                );
                LeadRelaunchStatus::Relaunched
            }
            Err(e) => {
                warn!("Failed to re-launch lead: {}", e);
                LeadRelaunchStatus::Failed
            }
        }
    } else {
        LeadRelaunchStatus::Unchanged
    };

    let mut relaunch_count = 0usize;
    let mut resumed_count = 0usize;
    let mut fresh_count = 0usize;
    for coworker in &running_coworkers {
        let role = execution_role_for_coworker(
            coworker,
            &reviewer_pr_by_name,
            &channel_lead_session_names,
        );
        let target_provider =
            crate::config::get_execution_provider_for_role(&state.repo_name, role);
        let resume_compatible = can_resume_between_providers(coworker.provider, target_provider);

        let mut config = match role {
            crate::config::ExecutionRole::Reviewer => {
                if let Some(pr_number) = reviewer_pr_by_name.get(&coworker.name).copied() {
                    let mut reviewer =
                        // restart_count=0: auth rotation is not a restart, fresh context
                        crate::launch::LaunchConfig::reviewer(coworker.name.clone(), pr_number, 0);
                    reviewer.session_mode = if resume_compatible {
                        crate::launch::SessionMode::Resume
                    } else {
                        crate::launch::SessionMode::Fresh
                    };
                    reviewer.model = coworker.model.clone();
                    reviewer
                } else if resume_compatible {
                    build_coworker_relaunch_config(coworker, &state.repo_name)
                } else {
                    build_fresh_coworker_relaunch_config(coworker, &state.repo_name, None)
                }
            }
            crate::config::ExecutionRole::ChannelLead => {
                let mut channel_lead = crate::launch::LaunchConfig::channel_lead(
                    coworker.name.clone(),
                    &state.repo_name,
                    if resume_compatible {
                        crate::launch::SessionMode::Resume
                    } else {
                        crate::launch::SessionMode::Fresh
                    },
                    "",
                );
                channel_lead.model = coworker.model.clone();
                channel_lead
            }
            _ if resume_compatible => build_coworker_relaunch_config(coworker, &state.repo_name),
            _ => build_fresh_coworker_relaunch_config(
                coworker,
                &state.repo_name,
                task_id_by_coworker
                    .get(&coworker.name.to_lowercase())
                    .map(String::as_str),
            ),
        };
        if !coworker.working_dir.is_empty() {
            config.working_dir = Some(std::path::PathBuf::from(&coworker.working_dir));
        }
        if config.auth_provider != target_provider {
            config.auth_provider = target_provider;
            config.model =
                super::helpers::default_model_for_provider_role(target_provider, &config.role)
                    .to_string();
        } else {
            config.auth_provider = target_provider;
        }
        config.auth_profile_dir = Some(crate::auth::active_profile_dir_for_project_with_provider(
            &state.repo_name,
            target_provider,
        ));

        match state.spawn_coworker(&config).await {
            Ok(()) => {
                relaunch_count += 1;
                if resume_compatible {
                    resumed_count += 1;
                } else {
                    fresh_count += 1;
                }
            }
            Err(e) => warn!(
                "Failed to relaunch coworker '{}' after {} auth switch: {}",
                coworker.name, provider, e
            ),
        }
    }

    // Post to ops channel — auth switch is daemon operational info
    let mut msg = Message::system(format!(
        "Switched to {} auth profile '{}' - restarted {}/{} coworker(s) (resumed {}, fresh {}), {}",
        provider,
        profile,
        relaunch_count,
        shutdown_count,
        resumed_count,
        fresh_count,
        lead_relaunch_status.summary()
    ));
    msg.channel = Some(OPS_CHANNEL.to_string());
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post auth switch message: {}", e);
    }

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "message": format!(
                "Switched to {} profile '{}'. Restarted {}/{} coworker(s), {}.",
                provider,
                profile,
                relaunch_count,
                shutdown_count,
                lead_relaunch_status.summary()
            ),
            "switched": true,
            "coworkers_shutdown": shutdown_count,
            "coworkers_relaunched": relaunch_count,
            "coworkers_relaunched_resumed": resumed_count,
            "coworkers_relaunched_fresh": fresh_count,
            "lead_relaunched": lead_relaunch_status.relaunched(),
            "lead_relaunch_attempted": lead_relaunch_status.attempted(),
            "lead_relaunch_status": lead_relaunch_status.as_str(),
        }),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[path = "rpc_auth_tests.rs"]
#[cfg(test)]
mod tests;
