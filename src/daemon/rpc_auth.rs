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
        task_id.map(|s| s.to_string()),
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
    repo_name: &str,
) -> crate::config::ExecutionRole {
    if super::helpers::is_project_lead(&coworker.name, repo_name) {
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
    force: bool,
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
        let path = crate::config::project_config_path(state.paths.dir_key());
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
        if !force && current == profile && cleared == 0 {
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
        let path = crate::config::project_config_path(state.paths.dir_key());
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
            .filter(|cw| !super::helpers::is_project_lead(&cw.name, &state.project_name))
            .collect();

    let current_lead = state
        .coworkers
        .list()
        .into_iter()
        .find(|cw| super::helpers::is_project_lead(&cw.name, &state.project_name));
    let current_lead_provider = current_lead.as_ref().map(|cw| cw.provider);

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
                    .active_reviewer_sessions()
                    .into_iter()
                    .find(|s| s.name == cw.name)
                    .and_then(|s| s.pr_number.map(|pr| (cw.name.clone(), pr)))
            })
            .collect();
        let channel_leads = persistent
            .channel_lead_sessions
            .keys()
            .map(|channel| crate::launch::channel_lead_session_name(channel))
            .collect();
        (reviewers, channel_leads)
    };
    // Capture channel lead names and reviewer channel assignments for escalation_target resolution.
    let (channel_lead_names, reviewer_channels) = {
        let persistent = state.persistent_state.lock().await;
        let lead_names = persistent.channel_lead_names();
        let channels: HashMap<String, Option<String>> = reviewer_pr_by_name
            .keys()
            .filter_map(|name| {
                persistent
                    .sessions
                    .values()
                    .find(|r| r.name == *name)
                    .map(|r| (name.clone(), r.channel.clone()))
            })
            .collect();
        (lead_names, channels)
    };
    let task_id_by_coworker: HashMap<String, String> = state.get_name_task_assignments().await;

    // Re-launch lead only if it currently runs on the switched provider.
    // Target provider always comes from role-based config (lead provider).
    let configured_lead_provider = crate::config::get_execution_provider_for_role(
        state.paths.dir_key(),
        crate::config::ExecutionRole::Lead,
    );
    let lead_relaunch_status = if current_lead_provider == Some(provider) {
        // Shut down the existing headless lead session if running.
        // Use the actual registered name (canonical repo name or legacy "lead").
        if let Some(lead) = current_lead.as_ref() {
            let lead_name = lead.name.as_str();
            if state.session_manager.is_alive(lead_name).await {
                let _ = state.session_manager.shutdown(lead_name).await;
                state.coworkers.deregister(lead_name);
            }
        }
        let mut lead_config = crate::launch::LaunchConfig::lead(state.paths.dir_key(), None);
        lead_config.auth_provider = configured_lead_provider;
        lead_config.model = super::helpers::resolve_model_for_role(
            state.paths.dir_key(),
            lead_config.auth_provider,
            &lead_config.agent_type,
        );
        lead_config.auth_profile_dir =
            Some(crate::auth::active_profile_dir_for_project_with_provider(
                state.paths.dir_key(),
                configured_lead_provider,
            ));
        match state.spawn_coworker(&lead_config).await {
            Ok(_) => {
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
            state.paths.dir_key(),
        );
        let target_provider =
            crate::config::get_execution_provider_for_role(state.paths.dir_key(), role);
        let resume_compatible = can_resume_between_providers(coworker.provider, target_provider);

        let mut config = match role {
            crate::config::ExecutionRole::Reviewer => {
                if let Some(pr_number) = reviewer_pr_by_name.get(&coworker.name).copied() {
                    let mut reviewer =
                        // restart_count=0: auth rotation is not a restart, fresh context
                        crate::launch::LaunchConfig::reviewer(
                            coworker.name.clone(),
                            state.paths.dir_key(),
                            pr_number,
                            0,
                            target_provider,
                        );
                    reviewer.session_mode = if resume_compatible {
                        crate::launch::SessionMode::Resume
                    } else {
                        crate::launch::SessionMode::Fresh
                    };
                    reviewer.model = coworker.model.clone();
                    // Restore channel and resolve escalation target from session record
                    if let Some(channel) = reviewer_channels
                        .get(&coworker.name)
                        .and_then(|c| c.clone())
                    {
                        if channel_lead_names.contains(&channel) {
                            reviewer.escalation_target = Some(channel.clone());
                            // Belt-and-suspenders: regenerate the initial prompt with the
                            // escalation target so the reviewer knows who to address even
                            // if the system prompt substitution fails.
                            reviewer.initial_prompt = Some(crate::agents::reviewer_launch_prompt(
                                pr_number,
                                0,
                                target_provider,
                                Some(&channel),
                            ));
                        } else {
                            warn!(
                                "Auth rotation for reviewer {}: task has channel {:?} but no \
                                 channel lead registered; reviewer escalation_target falls back \
                                 to project name",
                                coworker.name, channel
                            );
                        }
                        reviewer.channel = Some(channel);
                    }
                    reviewer
                } else if resume_compatible {
                    build_coworker_relaunch_config(coworker, state.paths.dir_key())
                } else {
                    build_fresh_coworker_relaunch_config(coworker, state.paths.dir_key(), None)
                }
            }
            crate::config::ExecutionRole::ChannelLead => {
                let channel_name = coworker.name.clone();
                let notes_base = state.paths.base_dir().to_path_buf();
                let notes_channel = channel_name.clone();
                let dir_key = state.paths.dir_key().to_string();
                let project_root = state.all_repo_paths.first().cloned().unwrap_or_default();
                let discover_channel = channel_name.clone();
                let (wf_name, wf_state_summary) = {
                    let ps = state.persistent_state.lock().await;
                    let wf = ps.channel_workflows.get(&channel_name).cloned();
                    let wfs = ps
                        .workflow_state
                        .get(&channel_name)
                        .map(super::effects::format_workflow_state_summary);
                    (wf, wfs)
                };
                let workflows_dir = state.paths.workflows_dir();
                let (domain_context, agents_md) = tokio::task::spawn_blocking(move || {
                    let notes = crate::channel::load_channel_notes(&notes_base, &notes_channel);
                    let agents = crate::paths::agents_md_for_channel(
                        &discover_channel,
                        &project_root,
                        &dir_key,
                    );

                    // Merge workflow AGENTS.md and state summary
                    let workflow_agents = wf_name.as_deref().and_then(|name| {
                        crate::paths::workflow_agents_md_content(&workflows_dir, name)
                    });
                    let merged_agents = crate::paths::merge_workflow_agents_md(
                        agents,
                        workflow_agents.as_deref(),
                        wf_state_summary.as_deref(),
                    );

                    (notes, merged_agents)
                })
                .await
                .unwrap_or_else(|e| {
                    warn!(
                        "Channel lead discovery task failed for '{}': {}",
                        channel_name, e
                    );
                    (String::new(), None)
                });
                let channel_directory =
                    crate::paths::read_channel_directory(state.paths.dir_key(), &channel_name);
                let mut channel_lead = crate::launch::LaunchConfig::channel_lead(
                    channel_name,
                    state.paths.dir_key(),
                    if resume_compatible {
                        crate::launch::SessionMode::Resume
                    } else {
                        crate::launch::SessionMode::Fresh
                    },
                    domain_context,
                    agents_md,
                );
                channel_lead.model = coworker.model.clone();
                channel_lead.cwd_subdir = channel_directory;
                channel_lead
            }
            _ if resume_compatible => {
                build_coworker_relaunch_config(coworker, state.paths.dir_key())
            }
            _ => build_fresh_coworker_relaunch_config(
                coworker,
                state.paths.dir_key(),
                task_id_by_coworker
                    .get(&coworker.name.to_lowercase())
                    .map(String::as_str),
            ),
        };
        if !coworker.working_dir.is_empty() {
            config.working_dir = Some(std::path::PathBuf::from(&coworker.working_dir));
        }
        config.auth_provider = target_provider;
        config.model = super::helpers::resolve_model_for_role(
            state.paths.dir_key(),
            target_provider,
            &config.agent_type,
        );
        config.auth_profile_dir = Some(crate::auth::active_profile_dir_for_project_with_provider(
            state.paths.dir_key(),
            target_provider,
        ));

        match state.spawn_coworker(&config).await {
            Ok(_) => {
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
// Pool toggle
// ============================================================================

/// Toggle whether an auth profile is included in the coworker spawn pool.
///
/// When `enabled=true`, adds the profile to `execution.coworker_profiles`.
/// When `enabled=false`, removes it. Idempotent — calling multiple times is safe.
///
/// The `provider` parameter is used to validate that the profile exists for that
/// provider before adding it (profile existence is provider-specific). This endpoint
/// always modifies `execution.coworker_profiles` regardless of provider — it manages
/// the coworker spawn pool only, not reviewer or channel-lead pools.
pub(super) async fn handle_auth_pool_toggle(
    id: RequestId,
    provider: crate::auth::AuthProvider,
    profile: &str,
    enabled: bool,
    state: &DaemonState,
) -> Response {
    if let Err(e) = crate::auth::validate_profile_name(profile) {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Invalid profile name: {}", e)),
        );
    }

    // Validate the profile exists before adding it to the pool (P2).
    if enabled && !crate::auth::profile_exists_for(provider, profile) {
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

    let path = crate::config::project_config_path(state.paths.dir_key());
    let mut config = crate::config::FullProjectConfig::load_from(&path).unwrap_or_default();

    // P1: Only initialize the list when enabling. When disabling, operate on
    // the existing list only — if it's None there's nothing to remove, and
    // creating Some([]) would unintentionally shadow any inherited global pool
    // entries via ExecutionSection::merge().
    if enabled {
        let profiles = config
            .execution
            .coworker_profiles
            .get_or_insert_with(Vec::new);
        if !profiles.contains(&profile.to_string()) {
            profiles.push(profile.to_string());
        }
    } else if let Some(profiles) = config.execution.coworker_profiles.as_mut() {
        profiles.retain(|p| p != profile);
    }

    let updated_profiles = config
        .execution
        .coworker_profiles
        .clone()
        .unwrap_or_default();

    if let Err(e) = config.save_to(&path) {
        return Response::error(
            id,
            RpcError::new(-32603, format!("Failed to save project config: {}", e)),
        );
    }

    info!(
        "Pool toggle: profile '{}' for {} -> {}",
        profile, provider, enabled
    );

    // Broadcast to ops channel so web UI clients receive the update without polling.
    let action = if enabled { "added to" } else { "removed from" };
    let mut msg = Message::system(format!(
        "Profile '{}' ({}) {} coworker pool. Pool: [{}]",
        profile,
        provider,
        action,
        updated_profiles.join(", ")
    ));
    msg.channel = Some(OPS_CHANNEL.to_string());
    if let Err(e) = state.send_and_broadcast_async(&msg).await {
        warn!("Failed to post pool toggle message: {}", e);
    }

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "profile": profile,
            "provider": provider.as_str(),
            "enabled": enabled,
            "coworker_profiles": updated_profiles,
        }),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[path = "rpc_auth_tests.rs"]
#[cfg(test)]
mod tests;
