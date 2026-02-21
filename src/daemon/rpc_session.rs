//! Session management RPC handlers.
//!
//! Handles `session.resolve`, `session.attach`, `session.detach`,
//! `session.list`, `session.view`, and `session.clear` methods,
//! allowing interactive terminal sessions to be connected to headless coworker
//! processes.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::message::Message;
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;
use super::constants::OPS_CHANNEL;

// ============================================================================
// Target parsing
// ============================================================================

/// Parsed attach target from a "type:value" string.
#[derive(Debug, PartialEq)]
enum AttachTarget {
    Name(String),
    Task(u32),
    Pr(u64),
    Platform(crate::auth::AuthProvider),
    PlatformSession {
        platform: crate::auth::AuthProvider,
        session_id: String,
    },
}

/// Parse an attach target string into a typed enum.
///
/// Pure function — no state access. Validates format and types.
fn parse_attach_target(target: &str) -> Result<AttachTarget, String> {
    // Platform-only shorthand:
    //   claude, codex
    match target.to_ascii_lowercase().as_str() {
        "claude" | "anthropic" | "antropic" => {
            return Ok(AttachTarget::Platform(crate::auth::AuthProvider::Claude));
        }
        "codex" | "openai" => {
            return Ok(AttachTarget::Platform(crate::auth::AuthProvider::Codex));
        }
        "zai" | "z.ai" => {
            return Err(
                "Invalid platform 'zai'. Use claude for Anthropic/z.ai sessions.".to_string(),
            );
        }
        _ => {}
    }

    // New slash-delimited syntax:
    //   name/<coworker>, task/<id>, pr/<number>, claude/<session_id>, codex/<session_id>
    if let Some((kind, value)) = target.split_once('/') {
        if value.is_empty() {
            return Err(format!("Missing value in attach target '{}'", target));
        }

        return match kind.to_ascii_lowercase().as_str() {
            "name" => Ok(AttachTarget::Name(value.to_lowercase())),
            "task" => {
                let id: u32 = value
                    .parse()
                    .map_err(|_| format!("Invalid task ID: {}", value))?;
                Ok(AttachTarget::Task(id))
            }
            "pr" => {
                let pr_num: u64 = value
                    .parse()
                    .map_err(|_| format!("Invalid PR number: {}", value))?;
                Ok(AttachTarget::Pr(pr_num))
            }
            "claude" | "anthropic" | "antropic" => Ok(AttachTarget::PlatformSession {
                platform: crate::auth::AuthProvider::Claude,
                session_id: value.to_string(),
            }),
            "codex" | "openai" => Ok(AttachTarget::PlatformSession {
                platform: crate::auth::AuthProvider::Codex,
                session_id: value.to_string(),
            }),
            "zai" | "z.ai" => Err(
                "Invalid platform 'zai'. Use claude/<session_id> for Anthropic/z.ai sessions."
                    .to_string(),
            ),
            _ => Err(format!(
                "Invalid target format: '{}'. Use name/<name>, task/<id>, pr/<number>, or <platform>/<session_id> where platform is claude|codex.",
                target
            )),
        };
    }

    // Legacy colon-delimited syntax retained for compatibility:
    //   name:<coworker>, task:<id>, pr:<number>
    if let Some(name) = target.strip_prefix("name:") {
        if name.is_empty() {
            return Err("Coworker name cannot be empty".to_string());
        }
        return Ok(AttachTarget::Name(name.to_lowercase()));
    }

    if let Some(id_str) = target.strip_prefix("task:") {
        let id: u32 = id_str
            .parse()
            .map_err(|_| format!("Invalid task ID: {}", id_str))?;
        return Ok(AttachTarget::Task(id));
    }

    if let Some(pr_str) = target.strip_prefix("pr:") {
        let pr_num: u64 = pr_str
            .parse()
            .map_err(|_| format!("Invalid PR number: {}", pr_str))?;
        return Ok(AttachTarget::Pr(pr_num));
    }

    Err(format!(
        "Invalid target format: '{}'. Use name:<name>, task:<id>, or pr:<number>",
        target
    ))
}

fn platform_for_provider(provider: Option<crate::auth::AuthProvider>) -> crate::auth::AuthProvider {
    match provider {
        Some(crate::auth::AuthProvider::Codex) => crate::auth::AuthProvider::Codex,
        Some(crate::auth::AuthProvider::Claude) | Some(crate::auth::AuthProvider::Zai) | None => {
            crate::auth::AuthProvider::Claude
        }
    }
}

fn monotonic_now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Resolve an attach target to one or more coworker names.
async fn resolve_attach_target_candidates(
    target: &str,
    state: &DaemonState,
) -> Result<Vec<String>, String> {
    let parsed = parse_attach_target(target)?;

    let mut names = match parsed {
        AttachTarget::Name(name) => vec![name],
        AttachTarget::Task(id) => {
            let id_str = id.to_string();
            let mut matches: Vec<String> = {
                let assignments = state.coworker_task_assignments.lock().unwrap();
                assignments
                    .iter()
                    .filter_map(|(coworker, assignment)| {
                        if assignment.task_id == id_str {
                            Some(coworker.to_lowercase())
                        } else {
                            None
                        }
                    })
                    .collect()
            };

            let persistent = state.persistent_state.lock().await;
            matches.extend(
                persistent
                    .headless_sessions
                    .iter()
                    .filter_map(|(name, info)| {
                        if info.task_id == Some(u64::from(id)) {
                            Some(name.to_lowercase())
                        } else {
                            None
                        }
                    }),
            );

            if matches.is_empty() {
                return Err(format!("No coworker is assigned to task !{}", id));
            }
            matches
        }
        AttachTarget::Pr(pr_num) => {
            let mut matches: Vec<String> = Vec::new();
            // Check reviewer assignments
            let persistent = state.persistent_state.lock().await;
            if let Some(reviewer) = persistent.github.get_reviewer(pr_num) {
                matches.push(reviewer.to_lowercase());
            }
            matches.extend(
                persistent
                    .headless_sessions
                    .iter()
                    .filter_map(|(name, info)| {
                        if info.pr_number == Some(pr_num) {
                            Some(name.to_lowercase())
                        } else {
                            None
                        }
                    }),
            );
            drop(persistent);
            // Fall back to branch-name-based mapping via coworker list
            let coworkers = state.coworkers.list();
            for cw in &coworkers {
                if cw
                    .current_task
                    .as_ref()
                    .is_some_and(|t| t.contains(&format!("PR #{}", pr_num)))
                {
                    matches.push(cw.name.to_lowercase());
                }
            }
            if matches.is_empty() {
                return Err(format!("No coworker is working on PR #{}", pr_num));
            }
            matches
        }
        AttachTarget::Platform(platform) => {
            let persistent = state.persistent_state.lock().await;
            let matches: Vec<String> = persistent
                .headless_sessions
                .iter()
                .filter_map(|(name, info)| {
                    if platform_for_provider(info.provider) == platform {
                        Some(name.to_lowercase())
                    } else {
                        None
                    }
                })
                .collect();
            if matches.is_empty() {
                return Err(format!(
                    "No persisted sessions found for platform '{}'",
                    platform.as_str()
                ));
            }
            matches
        }
        AttachTarget::PlatformSession {
            platform,
            session_id,
        } => {
            let persistent = state.persistent_state.lock().await;
            let matches: Vec<String> = persistent
                .headless_sessions
                .iter()
                .filter_map(|(name, info)| {
                    if info.session_id != session_id {
                        return None;
                    }

                    if platform_for_provider(info.provider) == platform {
                        Some(name.to_lowercase())
                    } else {
                        None
                    }
                })
                .collect();
            if matches.is_empty() {
                return Err(format!(
                    "No persisted session found for {}/{}",
                    platform.as_str(),
                    session_id
                ));
            }
            matches
        }
    };

    names.sort();
    names.dedup();
    Ok(names)
}

/// Resolve an attach target to exactly one coworker name.
///
/// `verb` controls the error message wording (e.g., "attach", "clear", "detach").
async fn resolve_attach_target(
    target: &str,
    state: &DaemonState,
    verb: &str,
) -> Result<String, String> {
    let mut names = resolve_attach_target_candidates(target, state).await?;
    match names.len() {
        0 => Err(format!("No sessions for target '{}'", target)),
        1 => Ok(names.remove(0)),
        _ => Err(format!(
            "Multiple sessions match '{}': {}. Choose one via `midtown session {} name/<coworker>`.",
            target,
            names.join(", "),
            verb,
        )),
    }
}

// ============================================================================
// Handlers
// ============================================================================

/// Handle session.resolve RPC method.
///
/// Returns attachable candidates for a target query so the CLI can present an
/// interactive selector before calling `session.attach`.
pub(super) async fn handle_session_resolve(
    id: RequestId,
    target: &str,
    state: &DaemonState,
) -> Response {
    let names = match resolve_attach_target_candidates(target, state).await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    let resolved_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0);
    let resolved_at_monotonic_ms = monotonic_now_ms();

    let persistent = state.persistent_state.lock().await;
    let now = chrono::Utc::now();
    let attached = state.attached_coworkers.lock().unwrap().clone();
    let running_coworkers: std::collections::HashMap<String, crate::coworker::Coworker> = state
        .coworkers
        .list()
        .into_iter()
        .map(|cw| (cw.name.to_lowercase(), cw))
        .collect();
    let mut candidates: Vec<serde_json::Value> = names
        .into_iter()
        .filter_map(|name| {
            let info = persistent.headless_sessions.get(&name)?;
            let coworker = running_coworkers.get(&name);
            let provider = info.provider.unwrap_or(crate::auth::AuthProvider::Claude);
            let platform = platform_for_provider(info.provider);
            let attached_now = attached.contains_key(&name);
            let running = coworker.is_some();
            let cwd = coworker
                .map(|cw| cw.working_dir.clone())
                .or_else(|| info.working_dir.clone())
                .unwrap_or_else(|| {
                    state
                        .all_repo_paths
                        .first()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default()
                });
            let last_active_age_ms = now
                .signed_duration_since(info.last_active)
                .num_milliseconds()
                .max(0) as u64;
            Some(serde_json::json!({
                "name": name,
                "session_id": info.session_id,
                "provider": provider.as_str(),
                "platform": platform.as_str(),
                "cwd": cwd,
                "running": running,
                "attached": attached_now,
                "last_active": info.last_active.to_rfc3339(),
                "last_active_age_ms": last_active_age_ms,
            }))
        })
        .collect();

    if candidates.is_empty() {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!(
                    "Target '{}' matched no persisted attachable sessions",
                    target
                ),
            ),
        );
    }

    candidates.sort_by(|a, b| {
        let ar = a.get("running").and_then(|v| v.as_bool()).unwrap_or(false);
        let br = b.get("running").and_then(|v| v.as_bool()).unwrap_or(false);
        match br.cmp(&ar) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }
        let an = a.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        let bn = b.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        an.cmp(bn)
    });

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "resolved_at_unix_ms": resolved_at_unix_ms,
            "resolved_at_monotonic_ms": resolved_at_monotonic_ms,
            "candidates": candidates,
        }),
    )
}

/// Handle session.attach RPC method.
///
/// Attaches to a persisted session and returns session info so the CLI can
/// open an interactive pane with the matching provider CLI.
///
/// If the coworker is currently running headless, the daemon pauses it first.
/// If the coworker is not running, the persisted session is attached directly.
pub(super) async fn handle_session_attach(
    id: RequestId,
    target: &str,
    state: &DaemonState,
) -> Response {
    let name = match resolve_attach_target(target, state, "attach").await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // Guard against double-attach
    {
        let attached = state.attached_coworkers.lock().unwrap();
        if attached.contains_key(&name.to_lowercase()) {
            return Response::error(
                id,
                RpcError::new(-32602, format!("Coworker '{}' is already attached", name)),
            );
        }
    }

    // Get session details from persistent state (eagerly persisted at spawn time).
    // If session_id is empty (race between spawn and init event), backfill from
    // session_manager which may have received the init event by now.
    let info = {
        let persistent = state.persistent_state.lock().await;
        match persistent.headless_sessions.get(&name).cloned() {
            Some(mut info) => {
                if info.session_id.is_empty()
                    && let Some(sid) = state.session_manager.get_session_id(&name).await
                {
                    info.session_id = sid;
                }
                info
            }
            None => {
                return Response::error(
                    id,
                    RpcError::new(
                        -32603,
                        format!(
                            "No session found for coworker '{}'. \
                             They may not be running in headless mode.",
                            name
                        ),
                    ),
                );
            }
        }
    };

    // If session_id is still empty after backfill, the session hasn't initialized yet.
    // Return a retryable error so callers (e.g. `midtown view`) can wait and retry.
    if info.session_id.is_empty() {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!(
                    "No session ID found for '{}' yet — session still initializing",
                    name
                ),
            ),
        );
    }

    // Check if session is currently running headless
    let running = state.session_manager.is_alive(&name).await;
    let provider = info.provider.unwrap_or(crate::auth::AuthProvider::Claude);
    let session_id = info.session_id.clone();
    let cwd = if name == "lead" {
        // Lead attaches in the lead worktree (where the headless session runs),
        // so `claude --resume` finds the session data (stored per-CWD).
        let lead_wt = crate::paths::lead_worktree_path(&state.repo_name);
        if lead_wt.exists() {
            lead_wt.to_string_lossy().to_string()
        } else {
            state
                .all_repo_paths
                .first()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default()
        }
    } else {
        state
            .coworkers
            .get(&name)
            .map(|cw| cw.working_dir.clone())
            .or(info.working_dir.clone())
            .unwrap_or_else(|| {
                state
                    .all_repo_paths
                    .first()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default()
            })
    };

    if running {
        // Gracefully pause the running headless session, giving Claude time to
        // persist its session state so `--resume` works in the interactive pane.
        state.broadcast_coworker_update(&name, "attaching", None);
        if let Err(e) = state
            .session_manager
            .graceful_shutdown(&name, std::time::Duration::from_secs(10))
            .await
        {
            return Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("Failed to pause headless session '{}': {}", name, e),
                ),
            );
        }
        // Deregister from CoworkerManager so prepare_spawn() won't reject the
        // re-spawn on detach with "already running". The headless session is gone;
        // spawn_coworker() will re-register when the new session starts.
        state.coworkers.deregister(&name);
        // Record stop time to prevent false orphan recovery during the grace period
        // (see #874). The attached_coworkers set provides the long-term exemption.
        state.record_coworker_stop_time(&name);
        info!(
            "Paused headless session '{}' for attach (session={})",
            name, session_id
        );
    } else {
        info!(
            "Attaching to persisted non-running session for '{}' (session={})",
            name, session_id
        );
    }

    // Mark as attached so stuck detection and orphan recovery skip this coworker.
    // Store attach timestamp for stale-session auto-detach.
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.insert(name.to_lowercase(), chrono::Utc::now());
    }

    // Post to channel
    let status_text = if running {
        "running headless paused, interactive session active"
    } else {
        "resuming historical session interactively"
    };
    let mut attach_msg = Message::system(format!("Attached to {} — {}", name, status_text));
    attach_msg.channel = Some(OPS_CHANNEL.to_string());
    let _ = state.send_and_broadcast_async(&attach_msg).await;

    Response::success(
        id,
        serde_json::json!({
            "session_id": session_id,
            "cwd": cwd,
            "name": name,
            "provider": provider.as_str(),
            "coworker_type": info.coworker_type,
            "channel": info.channel,
        }),
    )
}

/// Handle session.detach RPC method.
///
/// Maps a `CoworkerRole` to the equivalent `ExecutionRole` for provider lookups.
fn coworker_role_to_execution_role(
    role: &crate::launch::CoworkerRole,
) -> crate::config::ExecutionRole {
    match role {
        crate::launch::CoworkerRole::Lead => crate::config::ExecutionRole::Lead,
        crate::launch::CoworkerRole::Reviewer => crate::config::ExecutionRole::Reviewer,
        crate::launch::CoworkerRole::ChannelLead { .. } => {
            crate::config::ExecutionRole::ChannelLead
        }
        crate::launch::CoworkerRole::Coworker => crate::config::ExecutionRole::Coworker,
    }
}

/// Resumes headless execution for a coworker that was previously attached.
/// Idempotent: if the coworker is already running (e.g., a previous detach
/// succeeded), returns success without spawning a duplicate.
pub(super) async fn handle_session_detach(
    id: RequestId,
    name: &str,
    state: &DaemonState,
) -> Response {
    let name = name.to_lowercase();

    // Clear attached state first (idempotent — safe to call even if not attached)
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.remove(&name);
    }

    // Idempotency guard: if the headless session is already alive, skip re-spawn.
    // This prevents the race between manual detach and background auto-detach
    // from spawning duplicate processes. Uses session_manager (headless liveness)
    // rather than coworkers.get() (registration), because the coworker stays
    // registered during interactive attach — only the headless session is paused.
    if state.session_manager.is_alive(&name).await {
        info!("Coworker '{}' already running — detach is a no-op", name);
        return Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": format!("Coworker {} is already running", name),
            }),
        );
    }

    // Get session details from persistent state
    let session_info = {
        let persistent = state.persistent_state.lock().await;
        persistent.headless_sessions.get(&name).cloned()
    };

    let session_info = match session_info {
        Some(info) => info,
        None => {
            return Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("No session ID found for coworker '{}' to resume", name),
                ),
            );
        }
    };
    let session_id = session_info.session_id.clone();

    // Lead uses --continue (Resume) because the interactive attach also uses
    // --continue, which may create a new session. --continue picks up whatever
    // the most recent session is in the CWD.
    // Coworkers use --resume <id> since their interactive sessions use explicit IDs.
    let session_mode = if name == "lead" {
        crate::launch::SessionMode::Resume
    } else if session_id.is_empty() {
        crate::launch::SessionMode::Fresh
    } else {
        crate::launch::SessionMode::ResumeSession(session_id.clone())
    };

    let mut config = if name == "lead" {
        let mut c = crate::launch::LaunchConfig::lead(&state.repo_name, None);
        c.session_mode = session_mode;
        c
    } else {
        crate::launch::LaunchConfig::coworker(
            &name,
            &state.repo_name,
            session_mode,
            Some("You were previously running headless. The Lead attached to your session interactively and has now detached. Continue where you left off — read the channel for any updates.".to_string()),
        )
    };
    // For the lead, always use the canonical lead worktree path.
    // For coworkers, restore from persisted working_dir.
    if name == "lead" {
        let lead_wt = crate::paths::lead_worktree_path(&state.repo_name);
        if lead_wt.exists() {
            config.working_dir = Some(lead_wt);
        }
    } else if let Some(ref working_dir) = session_info.working_dir {
        config.working_dir = Some(std::path::PathBuf::from(working_dir));
    }
    {
        let execution_role = coworker_role_to_execution_role(&config.role);
        let provider = session_info.provider.unwrap_or_else(|| {
            crate::config::get_execution_provider_for_role(&state.repo_name, execution_role)
        });
        config.auth_provider = provider;
        config.model =
            super::helpers::default_model_for_provider_role(provider, &config.role).to_string();
    }
    // Don't restore auth_profile_dir from persisted profile name — let
    // spawn_coworker() re-resolve from project config (authoritative source).

    match state.spawn_coworker(&config).await {
        Ok(()) => {
            info!(
                "Resumed headless coworker '{}' after detach (session={})",
                name, session_id
            );

            let mut detach_msg =
                Message::system(format!("Detached from {} — headless session resumed", name));
            detach_msg.channel = Some(OPS_CHANNEL.to_string());
            let _ = state.send_and_broadcast_async(&detach_msg).await;

            state.broadcast_coworker_update(&name, "running", None);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Resumed headless session for {}", name),
                }),
            )
        }
        Err(e) => {
            warn!("Failed to resume coworker '{}' after detach: {}", name, e);
            Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("Failed to resume headless session for '{}': {}", name, e),
                ),
            )
        }
    }
}

/// Handle session.list RPC method.
///
/// Returns a list of headless sessions with their status.
/// All sessions are eagerly persisted at spawn time, so persistent state
/// is the single source of truth.
pub(super) async fn handle_session_list(id: RequestId, state: &DaemonState) -> Response {
    let persistent = state.persistent_state.lock().await;
    let running_coworkers: std::collections::HashSet<String> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();
    let attached = state.attached_coworkers.lock().unwrap().clone();

    let mut sessions: Vec<serde_json::Value> = persistent
        .headless_sessions
        .iter()
        .map(|(name, info)| {
            let status = if attached.contains_key(&name.to_lowercase()) {
                "attached"
            } else if running_coworkers.contains(&name.to_lowercase()) {
                "running"
            } else {
                "paused"
            };

            // Look up task assignment
            let task = {
                let assignments = state.coworker_task_assignments.lock().unwrap();
                assignments.get(name).map(|a| a.task_id.clone())
            };

            serde_json::json!({
                "name": name,
                "session_id": info.session_id,
                "status": status,
                "purpose": info.purpose,
                "last_active": info.last_active.to_rfc3339(),
                "task": task,
            })
        })
        .collect();

    sessions.sort_by(|a, b| {
        let status_rank = |status: &str| match status {
            "running" => 0,
            "attached" => 1,
            _ => 2, // paused / historical
        };
        let as_status = a.get("status").and_then(|v| v.as_str()).unwrap_or("paused");
        let bs_status = b.get("status").and_then(|v| v.as_str()).unwrap_or("paused");
        match status_rank(as_status).cmp(&status_rank(bs_status)) {
            std::cmp::Ordering::Equal => {}
            ord => return ord,
        }
        let an = a.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        let bn = b.get("name").and_then(|v| v.as_str()).unwrap_or_default();
        an.cmp(bn)
    });

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "sessions": sessions,
        }),
    )
}

/// Handle session.view RPC method.
///
/// Returns recent output for a session. For headed sessions (attached
/// interactively with a wrapper), captures the live PTY screen on demand.
/// For headless sessions, returns the tail of the JSONL event log.
pub(super) async fn handle_session_view(
    id: RequestId,
    target: &str,
    state: &DaemonState,
) -> Response {
    let name = match resolve_attach_target(target, state, "view").await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // Check if this session has an active headed wrapper lease.
    // If so, request a PTY capture on demand.
    let headed_key = DaemonState::session_key(&name);
    let has_headed_lease = {
        let sessions = state.headed_sessions.lock().await;
        sessions.get(&headed_key).is_some_and(|s| s.lease.is_some())
    };

    if has_headed_lease {
        // Request capture and wait for the wrapper to deliver it
        match state.headed_request_capture(&name).await {
            Ok(rx) => {
                match tokio::time::timeout(std::time::Duration::from_secs(3), rx).await {
                    Ok(Ok(output)) => {
                        return Response::success(
                            id,
                            serde_json::json!({
                                "success": true,
                                "output": output,
                                "source": "pty",
                            }),
                        );
                    }
                    _ => {
                        // Timeout or channel closed — fall through to JSONL
                        info!(
                            "PTY capture timed out for '{}', falling back to JSONL",
                            name
                        );
                    }
                }
            }
            Err(_) => {
                // No headed session / no lease — fall through
            }
        }
    }

    // Headless path: read JSONL event log
    match state.session_manager.get_output(&name).await {
        Some(output) => Response::success(
            id,
            serde_json::json!({
                "success": true,
                "output": output,
                "source": "jsonl",
            }),
        ),
        None => Response::error(
            id,
            RpcError::new(-32602, format!("No session output found for '{}'", name)),
        ),
    }
}

/// Handle session.clear RPC method.
///
/// Stops a running headless session and relaunches it as a fresh session
/// with the same initial prompt (plus a note that it's a fresh/cleared restart)
/// in the same worktree. Does not use --continue or --resume.
pub(super) async fn handle_session_clear(
    id: RequestId,
    target: &str,
    state: &DaemonState,
) -> Response {
    let name = match resolve_attach_target(target, state, "clear").await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // Get session info from persistent state
    let session_info = {
        let persistent = state.persistent_state.lock().await;
        persistent.headless_sessions.get(&name).cloned()
    };

    let session_info = match session_info {
        Some(info) => info,
        None => {
            return Response::error(
                id,
                RpcError::new(-32603, format!("No session found for coworker '{}'", name)),
            );
        }
    };

    // Guard: refuse to clear a session that is currently interactively attached.
    // When attached, the headless session is paused (is_alive() returns false), but
    // spawning a fresh headless process would conflict with the interactive session.
    {
        let attached = state.attached_coworkers.lock().unwrap();
        if attached.contains_key(&name.to_lowercase()) {
            return Response::error(
                id,
                RpcError::new(
                    -32602,
                    format!(
                        "Coworker '{}' is currently attached interactively. \
                         Detach first with `midtown session detach {}`.",
                        name, name
                    ),
                ),
            );
        }
    }

    // Stop the running session if alive
    if state.session_manager.is_alive(&name).await {
        info!("Stopping headless session '{}' for clear", name);
        if let Err(e) = state
            .session_manager
            .graceful_shutdown(&name, std::time::Duration::from_secs(10))
            .await
        {
            warn!("Failed to gracefully stop session '{}': {}", name, e);
            // Try force shutdown
            let _ = state.session_manager.shutdown(&name).await;
        }
    }

    // Unconditionally clean up all coworker state (cooldowns, nudges, tasks,
    // tool items, etc.) regardless of whether the session was alive. A dead
    // session still has stale registrations that would block spawn_coworker.
    state.cleanup_coworker_state(&name).await;

    // Build fresh launch config (no --resume, no --continue).
    // The decorated "fresh restart" message is sent to Claude as the initial prompt,
    // but we store the *original* prompt via persisted_initial_prompt so that
    // repeated clears don't accumulate the "This is a fresh session restart" prefix.
    let original_prompt = session_info.initial_prompt.as_deref().unwrap_or("");
    let fresh_prompt = if original_prompt.is_empty() {
        "This is a fresh session restart. Please read the channel to catch up on context."
            .to_string()
    } else {
        format!(
            "This is a fresh session restart. Your previous session was cleared.\n\n{}",
            original_prompt
        )
    };

    let mut config = if name == "lead" {
        let mut c = crate::launch::LaunchConfig::lead(&state.repo_name, None);
        c.session_mode = crate::launch::SessionMode::Fresh;
        c.initial_prompt = Some(fresh_prompt);
        // Persist the original prompt, not the decorated "fresh restart" wrapper.
        c.persisted_initial_prompt = session_info.initial_prompt.clone();
        c
    } else {
        let mut c = crate::launch::LaunchConfig::coworker(
            &name,
            &state.repo_name,
            crate::launch::SessionMode::Fresh,
            Some(fresh_prompt),
        );
        // Persist the original prompt, not the decorated "fresh restart" wrapper.
        c.persisted_initial_prompt = session_info.initial_prompt.clone();
        // Restore role-specific metadata so reviewer/channel-lead context survives the clear.
        if let Some(ref coworker_type) = session_info.coworker_type {
            match coworker_type.as_str() {
                "reviewer" => c.role = crate::launch::CoworkerRole::Reviewer,
                "channel-lead" => {
                    c.role = crate::launch::CoworkerRole::ChannelLead {
                        channel_name: session_info.channel.clone().unwrap_or_default(),
                        domain_context: String::new(),
                    }
                }
                _ => {}
            }
        }
        c.pr_number = session_info.pr_number;
        c.channel = session_info.channel.clone();
        c
    };

    // Restore working directory: lead uses canonical worktree, coworkers use persisted path
    if name == "lead" {
        let lead_wt = crate::paths::lead_worktree_path(&state.repo_name);
        if lead_wt.exists() {
            config.working_dir = Some(lead_wt);
        }
    } else if let Some(ref working_dir) = session_info.working_dir {
        config.working_dir = Some(std::path::PathBuf::from(working_dir));
    }
    {
        let execution_role = coworker_role_to_execution_role(&config.role);
        let provider = session_info.provider.unwrap_or_else(|| {
            crate::config::get_execution_provider_for_role(&state.repo_name, execution_role)
        });
        config.auth_provider = provider;
        config.model =
            super::helpers::default_model_for_provider_role(provider, &config.role).to_string();
    }

    match state.spawn_coworker(&config).await {
        Ok(()) => {
            info!("Relaunched fresh session for '{}' after clear", name);

            let mut clear_msg = crate::message::Message::system(format!(
                "Cleared session for {} — fresh session started",
                name
            ));
            clear_msg.channel = Some(OPS_CHANNEL.to_string());
            let _ = state.send_and_broadcast_async(&clear_msg).await;

            state.broadcast_coworker_update(&name, "running", None);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Cleared and restarted fresh session for {}", name),
                }),
            )
        }
        Err(e) => {
            warn!(
                "Failed to relaunch session for '{}' after clear: {}",
                name, e
            );
            Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("Failed to relaunch session for '{}': {}", name, e),
                ),
            )
        }
    }
}

/// Handle session.fork RPC method.
///
/// Forks the calling session into a new independent session bound to a thread.
/// The fork inherits the parent session's full context (conversation history,
/// tool access, etc.) but creates a new session ID. Future channel posts from
/// the forked session are automatically tagged with `thread_parent_id`.
///
/// Thread replies in the channel are routed to the forked session (by
/// `handle_channel_post`) rather than the root channel lead.
///
/// Parameters:
/// - `thread_parent_id`: The message ID of the thread root. Required.
/// - `calling_session_id`: The session ID of the calling session. Required.
///   The caller must pass its own session ID (from the `MIDTOWN_SESSION_ID`
///   env var or the system init event).
pub(super) async fn handle_session_fork(
    id: RequestId,
    thread_parent_id: &str,
    calling_session_id: &str,
    state: &DaemonState,
) -> crate::rpc::Response {
    // Guard: don't fork if a topic session already exists for this thread.
    {
        let topic = state.topic_sessions.lock().unwrap();
        if let Some(existing_sid) = topic.get(thread_parent_id) {
            return crate::rpc::Response::success(
                id,
                serde_json::json!({
                    "session_id": existing_sid,
                    "already_exists": true,
                }),
            );
        }
    }

    // Resolve the calling session's name from the reverse map.
    let caller_name = {
        let s2n = state.session_to_name.lock().unwrap();
        s2n.get(calling_session_id).cloned()
    };

    // Look up the channel lead session info to get working_dir and channel.
    let (working_dir, channel, auth_provider) = {
        let ps = state.persistent_state.lock().await;
        let info = ps.headless_sessions.iter().find_map(|(name, info)| {
            if info.session_id == calling_session_id
                || caller_name.as_deref() == Some(name.as_str())
            {
                Some(info.clone())
            } else {
                None
            }
        });
        match info {
            Some(i) => (
                i.working_dir,
                i.channel.clone(),
                i.provider.unwrap_or(crate::auth::AuthProvider::Claude),
            ),
            None => {
                warn!(
                    "session.fork: could not find session info for calling_session_id={}",
                    calling_session_id
                );
                // Fall back to repo root and no channel
                (None, None, crate::auth::AuthProvider::Claude)
            }
        }
    };

    // Determine channel for the fork — inherit from parent session or derive from
    // the caller name (channel leads are named after their channel).
    let fork_channel = channel.or_else(|| caller_name.clone());

    // Build the HeadlessConfig for the fork.
    // Forks use --resume <parent-id> --fork-session which creates an independent
    // session with the same conversation history as the parent.
    let config_dir = crate::auth::current_profile_dir_for(auth_provider);
    let repo_name = &state.repo_name;
    let team = crate::mailbox::team_name_for_repo(repo_name);

    // Name the fork after its thread (truncated) for human readability.
    let fork_name = format!(
        "fork-{}",
        thread_parent_id.get(..8).unwrap_or(thread_parent_id)
    );

    let mut env = crate::launch::build_agent_env_vars(
        &fork_name,
        &crate::launch::CoworkerRole::ChannelLead {
            channel_name: fork_channel.clone().unwrap_or_default(),
            domain_context: String::new(),
        },
        &Some(team.clone()),
        &fork_channel,
        auth_provider,
        &config_dir,
    );
    // Tell the fork its bound thread so it can pass --thread in channel posts
    env.insert(
        "MIDTOWN_BOUND_THREAD_ID".to_string(),
        thread_parent_id.to_string(),
    );

    let headless_config = crate::headless::HeadlessConfig {
        model: "sonnet".to_string(),
        system_prompt: String::new(),
        json_schema: None,
        cwd: working_dir.clone(),
        project_name: Some(repo_name.clone()),
        max_budget_usd: None,
        allow_tools: true,
        persist_session: true,
        resume_session_id: Some(calling_session_id.to_string()),
        inactivity_timeout: None,
        team_name: Some(team.clone()),
        agent_id: Some(crate::mailbox::agent_id(&fork_name, &team)),
        agent_name: Some(fork_name.clone()),
        settings_path: None,
        setting_sources: None,
        auth_provider,
        env,
        fork_session: true,
    };

    // Spawn the forked session.
    let fork_session_id = match state
        .session_manager
        .spawn_fork(&fork_name, headless_config)
        .await
    {
        Ok(sid) => sid,
        Err(e) => {
            warn!("session.fork: failed to spawn fork session: {}", e);
            return crate::rpc::Response::error(
                id,
                crate::rpc::RpcError::new(-32603, format!("Failed to fork session: {}", e)),
            );
        }
    };

    // Record the topic session mapping.
    {
        let mut topic = state.topic_sessions.lock().unwrap();
        topic.insert(thread_parent_id.to_string(), fork_session_id.clone());
    }

    // Record bound_thread_id on the session record (for output auto-tagging).
    {
        let mut ps = state.persistent_state.lock().await;
        if let Some(record) = ps.sessions.get_mut(&fork_session_id) {
            record.bound_thread_id = Some(thread_parent_id.to_string());
            if let Err(e) = ps.save_for_repo(repo_name) {
                warn!("session.fork: failed to persist session record: {}", e);
            }
        }
    }

    info!(
        "session.fork: forked {} (parent={}) → thread={}, new_session={}",
        calling_session_id,
        caller_name.as_deref().unwrap_or("?"),
        thread_parent_id,
        fork_session_id
    );

    crate::rpc::Response::success(
        id,
        serde_json::json!({
            "session_id": fork_session_id,
            "thread_parent_id": thread_parent_id,
        }),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[path = "rpc_session_tests.rs"]
#[cfg(test)]
mod tests;
