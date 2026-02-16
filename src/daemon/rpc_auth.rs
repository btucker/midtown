//! Auth-related RPC handlers.
//!
//! Extracted from `rpc.rs` to keep that file focused on dispatch and
//! simpler handlers. The `auth.switch` flow is the most complex single
//! handler—it validates, switches profiles, shuts down and relaunches
//! coworkers, and optionally restarts the lead window.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::message::Message;
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

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
/// For Claude, also re-launches active sessions:
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

    // Shut down all running coworkers for this provider
    let running_coworkers: Vec<crate::coworker::Coworker> =
        filter_coworkers_by_provider(&state.coworkers.list(), provider);

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

    // Capture reviewer assignments before relaunch so reviewer coworkers can
    // be re-spawned with the reviewer role/prompt.
    let reviewer_pr_by_name: HashMap<String, u64> = {
        let persistent = state.persistent_state.lock().await;
        running_coworkers
            .iter()
            .filter_map(|cw| {
                persistent
                    .github
                    .pr_for_reviewer(&cw.name)
                    .map(|pr| (cw.name.clone(), pr))
            })
            .collect()
    };

    // Re-launch lead only when switching the provider backing the interactive
    // lead session. Today lead is Claude-backed; other providers leave lead
    // untouched instead of reporting a relaunch failure.
    let lead_relaunch_status = if provider == crate::auth::AuthProvider::Claude {
        // Shut down the existing headless lead session if running
        if state.session_manager.is_alive("lead").await {
            let _ = state.session_manager.shutdown("lead").await;
            state.coworkers.deregister("lead");
        }
        let mut lead_config = crate::launch::LaunchConfig::lead(&state.repo_name);
        lead_config.auth_profile_dir = Some(crate::auth::active_profile_dir_for_project(
            &state.repo_name,
        ));
        match state.spawn_coworker(&lead_config).await {
            Ok(()) => {
                info!("Re-launched lead with auth profile '{}'", profile);
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

    // Re-launch all sessions for this provider using the updated auth profile.
    // Get the new profile's directory (the config switch at lines 142-181 has
    // already updated the active profile, so this reads the NEW profile's dir).
    let provider_auth_dir =
        crate::auth::active_profile_dir_for_project_with_provider(&state.repo_name, provider);
    let mut relaunch_count = 0usize;
    for coworker in &running_coworkers {
        let mut config = if let Some(pr_number) = reviewer_pr_by_name.get(&coworker.name).copied() {
            let mut reviewer =
                crate::launch::LaunchConfig::reviewer(coworker.name.clone(), pr_number);
            reviewer.session_mode = crate::launch::SessionMode::Resume;
            reviewer.model = coworker.model.clone();
            reviewer
        } else {
            build_coworker_relaunch_config(coworker, &state.repo_name)
        };
        config.auth_profile_dir = Some(provider_auth_dir.clone());
        // CRITICAL: Set the NEW provider on the launch config. Without this line,
        // coworkers would restart with the wrong provider:
        // - Reviewers get AuthProvider::Claude from LaunchConfig::reviewer() (line 280)
        // - Non-reviewers get their old provider from build_coworker_relaunch_config() (line 285)
        // Either way, they'd use the old provider's auth env var, causing credential failures.
        config.auth_provider = provider;

        match state.spawn_coworker(&config).await {
            Ok(()) => relaunch_count += 1,
            Err(e) => warn!(
                "Failed to relaunch coworker '{}' after {} auth switch: {}",
                coworker.name, provider, e
            ),
        }
    }

    // Post to channel
    let msg = Message::system(format!(
        "Switched to {} auth profile '{}' - restarted {}/{} coworker(s), {}",
        provider,
        profile,
        relaunch_count,
        shutdown_count,
        lead_relaunch_status.summary()
    ));
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
