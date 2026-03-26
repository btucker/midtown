//! Session management RPC handlers.
//!
//! Handles `session.resolve`, `session.attach`, `session.detach`,
//! `session.list`, `session.view`, `session.clear`, and `session.cancel` methods,
//! allowing interactive terminal sessions to be connected to headless coworker
//! processes.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

use crate::message::{Message, MessageType};
use crate::rpc::{RequestId, Response, RpcError};
use crate::web;

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
            // Look up coworker names from session records (single source of truth)
            let persistent = state.persistent_state.lock().await;
            let matches: Vec<String> = persistent
                .sessions
                .values()
                .filter_map(|record| {
                    if record.task_id.as_deref() == Some(id_str.as_str()) {
                        if record.name.is_empty() {
                            None
                        } else {
                            Some(record.name.to_lowercase())
                        }
                    } else {
                        None
                    }
                })
                .collect();
            drop(persistent);

            if matches.is_empty() {
                return Err(format!("No coworker is assigned to task !{}", id));
            }
            matches
        }
        AttachTarget::Pr(pr_num) => {
            let mut matches: Vec<String> = Vec::new();
            // Check reviewer assignments
            let persistent = state.persistent_state.lock().await;
            if let Some(span) = persistent.active_reviewer_for_pr(pr_num) {
                matches.push(span.name.to_lowercase());
            }
            matches.extend(persistent.sessions.values().filter_map(|record| {
                if record.pr_number == Some(pr_num) {
                    if record.name.is_empty() {
                        None
                    } else {
                        Some(record.name.to_lowercase())
                    }
                } else {
                    None
                }
            }));
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
                .sessions
                .values()
                .filter_map(|record| {
                    if platform_for_provider(record.provider) == platform {
                        if record.name.is_empty() {
                            None
                        } else {
                            Some(record.name.to_lowercase())
                        }
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
                .sessions
                .values()
                .filter_map(|record| {
                    if record.session_id != session_id {
                        return None;
                    }

                    if platform_for_provider(record.provider) == platform {
                        if record.name.is_empty() {
                            None
                        } else {
                            Some(record.name.to_lowercase())
                        }
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
            "Multiple sessions match '{}': {}. Choose one via `midtown agent {} name/<coworker>`.",
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
            let record = persistent.session_by_name(&name)?;
            let coworker = running_coworkers.get(&name);
            let provider = record.provider.unwrap_or(crate::auth::AuthProvider::Claude);
            let platform = platform_for_provider(record.provider);
            let attached_now = attached.contains_key(&name);
            let running = coworker.is_some();
            let cwd = coworker
                .map(|cw| cw.working_dir.clone())
                .or_else(|| {
                    let wd = &record.working_dir;
                    if wd.is_empty() {
                        None
                    } else {
                        Some(wd.clone())
                    }
                })
                .unwrap_or_else(|| {
                    state
                        .all_repo_paths
                        .first()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default()
                });
            let last_active_age_ms = now
                .signed_duration_since(record.last_active)
                .num_milliseconds()
                .max(0) as u64;
            Some(serde_json::json!({
                "name": name,
                "session_id": record.session_id,
                "provider": provider.as_str(),
                "platform": platform.as_str(),
                "cwd": cwd,
                "running": running,
                "attached": attached_now,
                "last_active": record.last_active.to_rfc3339(),
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
    let record = {
        let persistent = state.persistent_state.lock().await;
        match persistent.session_by_name(&name).cloned() {
            Some(mut record) => {
                if record.session_id.is_empty()
                    && let Some(sid) = state.session_manager.get_session_id(&name).await
                {
                    record.session_id = sid;
                }
                record
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
    if record.session_id.is_empty() {
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
    let provider = record.provider.unwrap_or(crate::auth::AuthProvider::Claude);
    let session_id = record.session_id.clone();
    let cwd = if name == "lead" {
        // Lead attaches in the lead worktree (where the headless session runs),
        // so `claude --resume` finds the session data (stored per-CWD).
        let lead_wt = state.paths.lead_worktree();
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
            .or_else(|| {
                let wd = &record.working_dir;
                if wd.is_empty() {
                    None
                } else {
                    Some(wd.clone())
                }
            })
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
        state.broadcast_coworker_update(&name, "attaching", None, None, None, None);
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
            "profile": record.profile,
            "agent_type": record.agent_type,
            "channel": record.channel,
        }),
    )
}

/// Handle session.detach RPC method.
///
fn fork_channel_lead_model(
    repo_name: &str,
    auth_provider: crate::auth::AuthProvider,
    _fork_channel: Option<&str>,
) -> String {
    super::helpers::resolve_model_for_role(repo_name, auth_provider, "midtown-channel-lead")
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
        persistent.session_by_name(&name).cloned()
    };

    let session_info = match session_info {
        Some(record) => record,
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
        let mut c = crate::launch::LaunchConfig::lead(state.paths.dir_key(), None);
        c.session_mode = session_mode;
        c
    } else {
        crate::launch::LaunchConfig::coworker(
            &name,
            state.paths.dir_key(),
            session_mode,
            Some("You were previously running headless. The Lead attached to your session interactively and has now detached. Continue where you left off — read the channel for any updates.".to_string()),
            session_info.task_id.clone(),
        )
    };
    // For the lead, always use the canonical lead worktree path.
    // For coworkers, restore from persisted working_dir.
    if name == "lead" {
        let lead_wt = state.paths.lead_worktree();
        if lead_wt.exists() {
            config.working_dir = Some(lead_wt);
        }
    } else if !session_info.working_dir.is_empty() {
        config.working_dir = Some(std::path::PathBuf::from(&session_info.working_dir));
    }
    {
        let execution_role = crate::config::execution_role_for_agent_type(&config.agent_type);
        let provider = session_info.provider.unwrap_or_else(|| {
            crate::config::get_execution_provider_for_role(state.paths.dir_key(), execution_role)
        });
        config.auth_provider = provider;
        config.model = super::helpers::resolve_model_for_role(
            state.paths.dir_key(),
            provider,
            &config.agent_type,
        );
    }
    // Don't restore auth_profile_dir from persisted profile name — let
    // spawn_coworker() re-resolve from project config (authoritative source).

    match state.spawn_coworker(&config).await {
        Ok(_) => {
            info!(
                "Resumed headless coworker '{}' after detach (session={})",
                name, session_id
            );

            let mut detach_msg =
                Message::system(format!("Detached from {} — headless session resumed", name));
            detach_msg.channel = Some(OPS_CHANNEL.to_string());
            let _ = state.send_and_broadcast_async(&detach_msg).await;

            state.broadcast_coworker_update(&name, "running", None, None, None, None);

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
        .sessions
        .values()
        .filter_map(|record| {
            if record.name.is_empty() {
                return None;
            }
            let name = &record.name;
            let status = if attached.contains_key(&name.to_lowercase()) {
                "attached"
            } else if running_coworkers.contains(&name.to_lowercase()) {
                "running"
            } else {
                "paused"
            };

            // Task assignment is on the session record itself
            let task = record.task_id.clone();

            Some(serde_json::json!({
                "name": name,
                "session_id": record.session_id,
                "status": status,
                "purpose": record.purpose,
                "last_active": record.last_active.to_rfc3339(),
                "task": task,
            }))
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
/// Returns recent output for a session by reading the tail of the JSONL event log.
pub(super) async fn handle_session_view(
    id: RequestId,
    target: &str,
    state: &DaemonState,
) -> Response {
    let name = match resolve_attach_target(target, state, "view").await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // Read JSONL event log
    match state.session_manager.get_output_with_path(&name).await {
        Some((output, log_path, log_offset)) => Response::success(
            id,
            serde_json::json!({
                "success": true,
                "output": output,
                "log_path": log_path.to_string_lossy(),
                "log_offset": log_offset,
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
        persistent.session_by_name(&name).cloned()
    };

    let session_info = match session_info {
        Some(record) => record,
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
                         Detach first with `midtown agent detach {}`.",
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
        let mut c = crate::launch::LaunchConfig::lead(state.paths.dir_key(), None);
        c.session_mode = crate::launch::SessionMode::Fresh;
        c.initial_prompt = Some(fresh_prompt);
        // Persist the original prompt, not the decorated "fresh restart" wrapper.
        c.persisted_initial_prompt = session_info.initial_prompt.clone();
        c
    } else {
        let mut c = crate::launch::LaunchConfig::coworker(
            &name,
            state.paths.dir_key(),
            crate::launch::SessionMode::Fresh,
            Some(fresh_prompt),
            session_info.task_id.clone(),
        );
        // Persist the original prompt, not the decorated "fresh restart" wrapper.
        c.persisted_initial_prompt = session_info.initial_prompt.clone();
        // Restore agent_type so reviewer/channel-lead context survives the clear.
        c.agent_type = session_info.agent_type.clone();
        c.pr_number = session_info.pr_number;
        c.channel = session_info.channel.clone();
        c
    };

    // Restore working directory: lead uses canonical worktree, coworkers use persisted path
    if name == "lead" {
        let lead_wt = state.paths.lead_worktree();
        if lead_wt.exists() {
            config.working_dir = Some(lead_wt);
        }
    } else if !session_info.working_dir.is_empty() {
        config.working_dir = Some(std::path::PathBuf::from(&session_info.working_dir));
    }
    {
        let execution_role = crate::config::execution_role_for_agent_type(&config.agent_type);
        let provider = session_info.provider.unwrap_or_else(|| {
            crate::config::get_execution_provider_for_role(state.paths.dir_key(), execution_role)
        });
        config.auth_provider = provider;
        config.model = super::helpers::resolve_model_for_role(
            state.paths.dir_key(),
            provider,
            &config.agent_type,
        );
    }

    match state.spawn_coworker(&config).await {
        Ok(_) => {
            info!("Relaunched fresh session for '{}' after clear", name);

            let mut clear_msg = crate::message::Message::system(format!(
                "Cleared session for {} — fresh session started",
                name
            ));
            clear_msg.channel = Some(OPS_CHANNEL.to_string());
            let _ = state.send_and_broadcast_async(&clear_msg).await;

            state.broadcast_coworker_update(&name, "running", None, None, None, None);

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

/// Handle session.cancel RPC method.
///
/// Gracefully stops a running session and immediately resumes it with `--resume`.
/// This preserves the session's full context while aborting whatever the session
/// was doing. Used by the web UI's Esc-to-cancel feature.
pub(super) async fn handle_session_cancel(
    id: RequestId,
    target: &str,
    state: &DaemonState,
) -> Response {
    let name = match resolve_attach_target(target, state, "cancel").await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // Get session info from persistent state
    let session_info = {
        let persistent = state.persistent_state.lock().await;
        persistent.session_by_name(&name).cloned()
    };

    let session_info = match session_info {
        Some(record) => record,
        None => {
            return Response::error(
                id,
                RpcError::new(-32603, format!("No session found for '{}'", name)),
            );
        }
    };

    // Guard: refuse to cancel a session that is currently interactively attached.
    {
        let attached = state.attached_coworkers.lock().unwrap();
        if attached.contains_key(&name.to_lowercase()) {
            return Response::error(
                id,
                RpcError::new(
                    -32602,
                    format!(
                        "Session '{}' is currently attached interactively. \
                         Detach first with `midtown agent detach {}`.",
                        name, name
                    ),
                ),
            );
        }
    }

    // Gracefully stop the running session (SIGTERM → wait → SIGKILL fallback).
    // This lets Claude Code persist session state for --resume.
    let session_id: Option<String> = if state.session_manager.is_alive(&name).await {
        info!("Stopping session '{}' for cancel+resume", name);
        match state
            .session_manager
            .graceful_shutdown(&name, std::time::Duration::from_secs(10))
            .await
        {
            Ok(sid) => sid,
            Err(e) => {
                warn!("Failed to gracefully stop session '{}': {}", name, e);
                let _ = state.session_manager.shutdown(&name).await;
                Some(session_info.session_id.clone()).filter(|s| !s.is_empty())
            }
        }
    } else {
        // Session not alive — use persisted session_id for resume
        Some(session_info.session_id.clone()).filter(|s| !s.is_empty())
    };

    // Clean up coworker state so spawn_coworker doesn't see stale registrations.
    state.cleanup_coworker_state(&name).await;

    // Build a resume launch config using the saved session_id.
    let is_lead =
        name == "lead" || crate::daemon::helpers::is_project_lead(&name, &state.project_name);

    let mut config = if is_lead {
        let mut c = crate::launch::LaunchConfig::lead(state.paths.dir_key(), None);
        c.persisted_initial_prompt = session_info.initial_prompt.clone();
        c
    } else {
        let mut c = crate::launch::LaunchConfig::coworker(
            &name,
            state.paths.dir_key(),
            crate::launch::SessionMode::Fresh, // overridden below
            None,
            session_info.task_id.clone(),
        );
        c.persisted_initial_prompt = session_info.initial_prompt.clone();
        c.agent_type = session_info.agent_type.clone();
        c.pr_number = session_info.pr_number;
        c.channel = session_info.channel.clone();
        c
    };

    // Set resume mode with the saved session_id
    if let Some(ref sid) = session_id {
        config.session_mode = crate::launch::SessionMode::ResumeSession(sid.clone());
    }

    // Restore working directory
    if is_lead {
        let lead_wt = state.paths.lead_worktree();
        if lead_wt.exists() {
            config.working_dir = Some(lead_wt);
        }
    } else if !session_info.working_dir.is_empty() {
        config.working_dir = Some(std::path::PathBuf::from(&session_info.working_dir));
    }

    // Restore auth provider and model
    {
        let execution_role = crate::config::execution_role_for_agent_type(&config.agent_type);
        let provider = session_info.provider.unwrap_or_else(|| {
            crate::config::get_execution_provider_for_role(state.paths.dir_key(), execution_role)
        });
        config.auth_provider = provider;
        config.model = super::helpers::resolve_model_for_role(
            state.paths.dir_key(),
            provider,
            &config.agent_type,
        );
    }

    match state.spawn_coworker(&config).await {
        Ok(_) => {
            info!("Resumed session '{}' after cancel", name);
            state.broadcast_coworker_update(&name, "running", None, None, None, None);

            Response::success(
                id,
                serde_json::json!({
                    "success": true,
                    "message": format!("Cancelled and resumed session for {}", name),
                }),
            )
        }
        Err(e) => {
            warn!("Failed to resume session '{}' after cancel: {}", name, e);
            Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!("Failed to resume session '{}': {}", name, e),
                ),
            )
        }
    }
}

/// Derive a short, human-readable slug from message content for fork session names.
///
/// Extracts the first 1-3 meaningful words from a message, lowercased and joined with
/// hyphens. Strips @mentions, replaces all non-alphanumeric characters with hyphens
/// (collapsing consecutive hyphens), and filters common filler words. Falls back to
/// the first 8 characters of `thread_parent_id` if no meaningful words are found.
fn slugify_fork_hint(message: &str, thread_parent_id: &str) -> String {
    // Words to skip when building the slug
    static STOP_WORDS: &[&str] = &[
        "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
        "do", "does", "did", "will", "would", "could", "should", "may", "might", "can", "shall",
        "to", "of", "in", "for", "on", "with", "at", "by", "from", "as", "into", "it", "its",
        "this", "that", "these", "those", "i", "we", "you", "he", "she", "they", "me", "us", "him",
        "her", "them", "my", "our", "your", "his", "their", "and", "but", "or", "not", "no", "so",
        "if", "then", "than", "just", "also", "about", "up", "out", "how", "what", "when", "where",
        "why", "which", "who", "all", "each", "some", "any", "here", "there",
    ];

    // Split on whitespace, skip @mentions, then replace ALL non-alphanumeric chars
    // with hyphens (not just trim edges). This handles interior punctuation like
    // "fix/auth" → "fix-auth" and "fix::auth" → "fix-auth".
    let words: Vec<String> = message
        .split_whitespace()
        // Skip @mentions
        .filter(|w| !w.starts_with('@'))
        // Replace all non-alphanumeric chars with hyphens, collapse consecutive,
        // and strip leading/trailing hyphens.
        .map(|w| {
            let replaced: String = w
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect();
            // Collapse consecutive hyphens and trim
            let mut result = String::new();
            let mut prev_hyphen = true; // treat start as hyphen to skip leading
            for c in replaced.chars() {
                if c == '-' {
                    if !prev_hyphen {
                        prev_hyphen = true;
                    }
                } else {
                    if prev_hyphen && !result.is_empty() {
                        result.push('-');
                    }
                    result.push(c);
                    prev_hyphen = false;
                }
            }
            result.to_lowercase()
        })
        // Filter empty and stop words
        .filter(|w| !w.is_empty() && !STOP_WORDS.contains(&w.as_str()))
        .take(3)
        .collect();

    if words.is_empty() {
        thread_parent_id
            .get(..8)
            .unwrap_or(thread_parent_id)
            .to_string()
    } else {
        words.join("-")
    }
}

/// Build the `HeadlessConfig` and fork name for a fork session.
///
/// Uses `LaunchConfig` internally then converts to `HeadlessConfig` with
/// fork-specific adjustments (bound thread ID env, disallowed tools, settings).
///
/// **Architecture note:** Fork sessions launch as *fresh* sessions (not
/// `--resume --fork-session`) because headless sessions don't persist JSONL
/// files to disk, so `--fork-session` has nothing to fork from.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_fork_config(
    thread_parent_id: &str,
    _calling_session_id: &str,
    caller_name: Option<&str>,
    fork_name_hint: Option<&str>,
    fork_channel: Option<&str>,
    working_dir: Option<&str>,
    auth_provider: crate::auth::AuthProvider,
    is_channel_lead: bool,
    repo_name: &str,
    name_override: Option<&str>,
) -> (String, crate::headless::HeadlessConfig) {
    // Derive the fork name
    let fork_name = if let Some(name) = name_override {
        name.to_string()
    } else {
        let tid_suffix = thread_parent_id.get(..4).unwrap_or(thread_parent_id);
        if let Some(hint) = fork_name_hint.filter(|h| !h.is_empty()) {
            let slug = slugify_fork_hint(hint, thread_parent_id);
            match caller_name {
                Some(name) => format!("{}-{}-{}", name, slug, tid_suffix),
                None => format!("fork-{}-{}", slug, tid_suffix),
            }
        } else {
            format!(
                "fork-{}",
                thread_parent_id.get(..8).unwrap_or(thread_parent_id),
            )
        }
    };

    // Build LaunchConfig and convert to HeadlessConfig
    let agent_type = if is_channel_lead {
        "midtown-channel-lead"
    } else {
        "midtown-project-lead"
    };
    let launch_config =
        crate::launch::LaunchConfig::new(&fork_name, agent_type, repo_name, None, None)
            .with_channel(fork_channel.map(String::from))
            .with_auth_provider(auth_provider)
            .with_auth_profile_dir(Some(
                crate::auth::active_profile_dir_for_project_with_provider(repo_name, auth_provider),
            ));

    let paths = crate::paths::ProjectPaths::new(repo_name);
    let mut headless_config = launch_config.to_headless_config(&paths);

    // Fork-specific adjustments
    headless_config.cwd = working_dir.map(String::from);
    headless_config.model = fork_channel_lead_model(repo_name, auth_provider, fork_channel);
    headless_config.env.insert(
        "MIDTOWN_BOUND_THREAD_ID".to_string(),
        thread_parent_id.to_string(),
    );

    // Fork sessions use the full system prompt (no --agent) for Codex compatibility.
    // For Claude/z.ai, we override agent_name to None and use the full prompt.
    headless_config.agent_name = None;
    headless_config.system_prompt = crate::agents::main_lead_system_prompt(repo_name);

    // Fork channel leads get stricter tool restrictions (Edit re-added)
    if is_channel_lead && !matches!(auth_provider, crate::auth::AuthProvider::Codex) {
        headless_config.disallowed_tools = crate::launch::channel_lead_fork_disallowed_tools();
    }

    // Lead settings for fork sessions
    headless_config.settings_path = if matches!(auth_provider, crate::auth::AuthProvider::Codex) {
        None
    } else {
        match crate::settings::write_lead_settings_file() {
            Ok(path) => Some(path.to_string_lossy().to_string()),
            Err(e) => {
                warn!("Failed to write lead settings file for fork session: {e}");
                None
            }
        }
    };

    // Pre-assign session ID for non-Codex providers
    headless_config.session_id = match auth_provider {
        crate::auth::AuthProvider::Codex => None,
        _ => Some(uuid::Uuid::new_v4().to_string()),
    };

    // Ensure autoCompact settings exist in fork working directory
    if let Some(wd) = working_dir {
        crate::settings::ensure_auto_compact_settings(std::path::Path::new(wd));
    }

    (fork_name, headless_config)
}

/// Create a fork session bound to a thread, or return an existing one.
///
/// This is the shared implementation used by `handle_session_fork` (explicit fork via
/// `midtown agent fork`), `handle_session_fork_thread` (web-UI-triggered fork), and
/// potentially other fork paths.
///
/// Uses `pending_forks` HashSet for concurrent fork creation guard and
/// `session_by_thread()` on SessionRecord for existing fork detection.
///
/// `channel_hint` lets callers provide a known channel name that takes priority over the
/// session record's channel field.
///
/// `fork_name_hint` provides a human-readable description for the fork session name.
/// When provided, the hint is slugified (non-alphanumeric chars replaced with hyphens,
/// stop words removed, limited to 3 words) and a short thread ID suffix is appended
/// for uniqueness: `{caller_name}-{slug}-{tid_prefix}` (e.g. `web-push-notifications-a1b2`).
/// When `caller_name` is unknown: `fork-{slug}-{tid_prefix}`.
/// When `None` or empty, falls back to `fork-{first-8-chars-of-thread-id}`.
///
/// Returns `Ok((session_id, already_existed, fork_channel))` where `already_existed` is
/// true when a live fork was found via SessionRecord (dead entries are treated as absent).
/// The `fork_channel` is the resolved channel for the fork session (`None` only for
/// pre-existing forks).
/// Returns `Err` if a concurrent fork is in progress or spawn fails.
#[allow(clippy::too_many_arguments)]
pub(super) async fn create_fork_session(
    thread_parent_id: &str,
    calling_session_id: &str,
    channel_hint: Option<&str>,
    fork_name_hint: Option<&str>,
    color: Option<&str>,
    icon: Option<&str>,
    caller: &str,
    state: &DaemonState,
) -> Result<(String, bool, Option<String>), String> {
    // Check for concurrent fork creation.
    {
        let pending = state.pending_forks.lock().unwrap();
        if pending.contains(thread_parent_id) {
            return Err("fork in progress for this thread".to_string());
        }
    }

    // Check for an existing running fork via SessionRecord.
    let existing = {
        let ps = state.persistent_state.lock().await;
        ps.session_by_thread(thread_parent_id)
            .filter(|s| s.is_running)
            .map(|s| (s.session_id.clone(), s.name.clone()))
    };
    if let Some((existing_sid, existing_name)) = existing {
        if state.session_manager.is_alive(&existing_name).await {
            return Ok((existing_sid, true, None));
        }
        // Stale entry — session record says running but process is dead.
        // Mark it stopped and proceed to create a fresh fork.
        warn!(
            "{}: clearing stale session for thread {} (session_id={}, name={})",
            caller, thread_parent_id, existing_sid, existing_name
        );
        let mut ps = state.persistent_state.lock().await;
        if let Some(record) = ps.sessions.get_mut(&existing_sid) {
            record.is_running = false;
        }
        let _ = ps.save_for_repo(state.paths.dir_key());
    }

    // Reserve the slot to prevent concurrent fork creation.
    {
        let mut pending = state.pending_forks.lock().unwrap();
        if !pending.insert(thread_parent_id.to_string()) {
            return Err("fork in progress for this thread".to_string());
        }
    }

    // Resolve the calling session's name from persistent state.
    let caller_name = {
        let ps = state.persistent_state.lock().await;
        ps.sessions.get(calling_session_id).map(|s| s.name.clone())
    };

    // Look up the calling session info to get working_dir, channel, and role.
    let (working_dir, channel, auth_provider, is_channel_lead) = {
        let ps = state.persistent_state.lock().await;
        // Try direct session_id lookup first, then fall back to name-based lookup.
        let record = ps
            .sessions
            .get(calling_session_id)
            .or_else(|| caller_name.as_ref().and_then(|n| ps.session_by_name(n)));
        match record {
            Some(r) => {
                let wd = if r.working_dir.is_empty() {
                    None
                } else {
                    Some(r.working_dir.clone())
                };
                (
                    wd,
                    r.channel.clone(),
                    r.provider.unwrap_or(crate::auth::AuthProvider::Claude),
                    r.agent_type == "midtown-channel-lead",
                )
            }
            None => {
                warn!(
                    "{}: could not find session info for calling_session_id={}",
                    caller, calling_session_id
                );
                // Fall back to repo root and no channel
                (None, None, crate::auth::AuthProvider::Claude, false)
            }
        }
    };

    // Determine channel for the fork — explicit hint takes priority (the web UI
    // fork path supplies the channel directly), then session record, then
    // default (main) channel.
    let fork_channel = channel_hint
        .map(String::from)
        .or(channel)
        .or_else(|| Some(state.project_name.clone()));

    let (fork_name, headless_config) = build_fork_config(
        thread_parent_id,
        calling_session_id,
        caller_name.as_deref(),
        fork_name_hint,
        fork_channel.as_deref(),
        working_dir.as_deref(),
        auth_provider,
        is_channel_lead,
        state.paths.dir_key(),
        None, // normal fork — derive name from hint
    );

    // Spawn the forked session.
    let fork_session_id = match state
        .session_manager
        .spawn_fork(&fork_name, headless_config)
        .await
    {
        Ok(sid) => sid,
        Err(e) => {
            // Release the pending guard — spawn failed, slot available for retry.
            state.pending_forks.lock().unwrap().remove(thread_parent_id);
            warn!("{}: failed to spawn fork session: {}", caller, e);
            return Err(format!("Failed to fork session: {}", e));
        }
    };

    // Persist the SessionRecord for the new fork.
    {
        let mut ps = state.persistent_state.lock().await;
        let parent_record = ps
            .sessions
            .get(calling_session_id)
            .or_else(|| caller_name.as_ref().and_then(|n| ps.session_by_name(n)))
            .cloned();
        ps.sessions.insert(
            fork_session_id.clone(),
            crate::daemon::state::SessionRecord {
                session_id: fork_session_id.clone(),
                task_id: parent_record.as_ref().and_then(|r| r.task_id.clone()),
                name: fork_name.clone(),
                working_dir: working_dir.clone().unwrap_or_default(),
                branch: None,
                pr_number: None,
                initial_prompt: parent_record
                    .as_ref()
                    .and_then(|r| r.initial_prompt.clone()),
                agent_type: "midtown-channel-lead".to_string(),
                is_running: true,
                created_at: chrono::Utc::now(),
                resume_on_startup: false,
                bound_thread_id: Some(thread_parent_id.to_string()),
                last_active: chrono::Utc::now(),
                purpose: format!(
                    "fork of {} in thread {}",
                    fork_channel.as_deref().unwrap_or("unknown"),
                    thread_parent_id
                ),
                pid: None,
                channel: fork_channel.clone(),
                provider: parent_record.as_ref().and_then(|r| r.provider),
                platform: parent_record
                    .as_ref()
                    .and_then(|r| r.provider)
                    .map(crate::platform::Platform::from_provider),
                profile: parent_record.as_ref().and_then(|r| r.profile.clone()),
                restart_count: 0,
                color: color.map(|c| c.to_string()),
                icon: icon.map(|i| i.to_string()),
                avatar_badge: None,
            },
        );
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!("{}: failed to persist session record: {}", caller, e);
        }
    }

    // Release the pending guard — SessionRecord is now the source of truth.
    state.pending_forks.lock().unwrap().remove(thread_parent_id);

    info!(
        "{}: forked {} (parent={}) → thread={}, new_session={}",
        caller,
        calling_session_id,
        caller_name.as_deref().unwrap_or("?"),
        thread_parent_id,
        fork_session_id
    );

    Ok((fork_session_id, false, fork_channel))
}

/// Format text as a markdown blockquote, prefixing every line with `> `.
fn format_blockquote(content: &str) -> String {
    content
        .lines()
        .map(|l| format!("> {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Build a concise summary of recent channel activity for fork context.
///
/// Returns a formatted summary of the last ~30 top-level messages (excluding
/// thread replies, auto-output, and nudges) to give the fork situational
/// awareness of what's happening in the channel.
async fn build_channel_summary_for_fork(channel: &crate::channel::Channel) -> Option<String> {
    let messages = match channel.read_last_n_messages_async(30).await {
        Ok((msgs, _)) => msgs,
        Err(e) => {
            debug!("Failed to read channel messages for fork summary: {}", e);
            return None;
        }
    };

    let lines: Vec<String> = messages
        .iter()
        .filter(|m| m.thread_parent_id.is_none())
        .filter(|m| !m.auto_output)
        .filter(|m| m.message_type != MessageType::Nudge)
        .map(|m| {
            let time = m.timestamp.format("%H:%M");
            let content = if m.content.chars().count() > 150 {
                let truncated: String = m.content.chars().take(150).collect();
                format!("{truncated}...")
            } else {
                m.content.clone()
            };
            // Collapse multiline content to single line for summary brevity.
            let content = content.replace('\n', " ");
            format!("[{time}] {}: {content}", m.from)
        })
        .collect();

    if lines.is_empty() {
        return None;
    }

    Some(format!(
        "## Recent channel activity\n\n{}",
        lines.join("\n")
    ))
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
/// **Side effects for fresh forks:**
/// - Sends a `NudgeSession` so the fork has an initial message to act on.
///   The nudge follows a 3-priority fallback chain:
///   1. Explicit `initial_message` from the caller (always preferred).
///   2. Parent message content looked up by `thread_parent_id` from the
///      channel history. For channel leads, this is combined with
///      `fork_initial_framing` and a channel summary; for non-channel-lead
///      callers, the parent message is wrapped as "The following message
///      needs investigation" with a channel summary appended.
///   3. Bare `fork_initial_framing` (with channel summary) for channel
///      leads when no parent message is found. Non-channel-lead callers
///      get no nudge in this case (the framing text assumes a
///      channel-lead role).
/// - Broadcasts `ThreadOwnership` to web clients so the "Dedicated session"
///   indicator appears in the UI regardless of whether the fork was created
///   via CLI or web UI.
///
/// Parameters:
///
/// - `thread_parent_id`: The message ID of the thread root. Required.
/// - `calling_session_id`: The session ID of the calling session. Required.
///   The caller must pass its own session ID (from the `MIDTOWN_SESSION_ID`
///   env var or the system init event).
/// - `name_hint`: Optional descriptive name for the fork (e.g. "investigate auth bug").
/// - `initial_message`: Optional initial message for the fork. When provided, this is
///   sent as the nudge instead of any fallback. This lets callers combine fork + nudge
///   into a single command with precise instructions.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_session_fork(
    id: RequestId,
    thread_parent_id: &str,
    calling_session_id: &str,
    name_hint: Option<&str>,
    initial_message: Option<&str>,
    color: Option<&str>,
    icon: Option<&str>,
    state: &DaemonState,
) -> crate::rpc::Response {
    // Validate thread_parent_id is a UUID — reject Claude API message IDs
    // (e.g., "msg_01...") which would silently create a fork bound to a
    // non-existent thread.
    if uuid::Uuid::parse_str(thread_parent_id).is_err() {
        return crate::rpc::Response::error(
            id,
            crate::rpc::RpcError::new(
                -32602,
                format!(
                    "Invalid thread_parent_id '{}': expected a channel message UUID \
                     (e.g., 'a1b2c3d4-e5f6-7890-abcd-ef1234567890'). \
                     This looks like a Claude API message ID — use the channel message UUID \
                     from the nudge parentheses instead.",
                    thread_parent_id
                ),
            ),
        );
    }
    match create_fork_session(
        thread_parent_id,
        calling_session_id,
        None,
        name_hint,
        color,
        icon,
        "session.fork",
        state,
    )
    .await
    {
        Ok((sid, true, _)) => crate::rpc::Response::success(
            id,
            serde_json::json!({
                "session_id": sid,
                "already_exists": true,
            }),
        ),
        Ok((sid, false, fork_channel)) => {
            // Send nudge to the fresh fork. Priority:
            // 1. Explicit --initial-message from the caller
            // 2. Look up the parent message content by thread_parent_id and
            //    include it (with fork_initial_framing for channel leads)
            // 3. Bare fork_initial_framing for channel leads (no parent found)
            //
            // Without a nudge the fork session sits idle forever with no
            // initial message to act on — this was the root cause of "forks
            // not working".
            //
            // Check channel-lead status up front so we don't need try_lock()
            // inside a sync closure — .lock().await is correct in async context
            // and avoids non-deterministic framing on lock contention.
            let is_channel_lead = {
                let ps_guard = state.persistent_state.lock().await;
                ps_guard
                    .sessions
                    .get(calling_session_id)
                    .map(|r| r.agent_type == "midtown-channel-lead")
                    .unwrap_or(false)
            };
            // (persistent_prompt, nudge_message): persistent is crash-recovery-safe
            // (no volatile channel summary), nudge is the full message sent to the fork.
            let (persistent_prompt, nudge_message) = if let Some(msg) = initial_message {
                let s = msg.to_string();
                (Some(s.clone()), Some(s))
            } else {
                // Open channel once for both parent message lookup and summary.
                let channel_obj = fork_channel.as_ref().and_then(|ch| {
                    let base_dir = state.paths.base_dir().to_path_buf();
                    match crate::channel::Channel::new(&base_dir, ch) {
                        Ok(c) => Some(c),
                        Err(e) => {
                            warn!("Failed to open channel {:?} for fork summary: {}", ch, e);
                            None
                        }
                    }
                });

                let parent_content = if let Some(ref channel) = channel_obj {
                    match channel.find_message_by_id_async(thread_parent_id).await {
                        Ok(Some(msg)) => Some((msg.from, msg.content)),
                        Ok(None) => {
                            debug!(
                                "Parent message {} not found in channel {}",
                                thread_parent_id,
                                fork_channel.as_deref().unwrap_or("?")
                            );
                            None
                        }
                        Err(e) => {
                            warn!(
                                "Failed to look up parent message {}: {}",
                                thread_parent_id, e
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                let channel_summary = if let Some(ref channel) = channel_obj {
                    build_channel_summary_for_fork(channel).await
                } else {
                    None
                };

                // Build the persistent (crash-recovery-safe) prompt and the
                // full nudge (which includes the volatile channel summary).
                // Only the persistent portion is saved to initial_prompt.
                let (persistent_prompt, nudge_prompt) = match (is_channel_lead, parent_content) {
                    // Channel lead + parent message: framing + quoted message
                    (true, Some((from, content))) => {
                        let framing = fork_channel
                            .as_ref()
                            .map(|ch| super::rpc_channel::fork_initial_framing(ch))
                            .unwrap_or_default();
                        let quoted = format!("{from} wrote:\n{}", format_blockquote(&content));
                        let persistent = format!("{framing}\n\n{quoted}");
                        let full = if let Some(ref summary) = channel_summary {
                            format!("{framing}\n\n{summary}\n\n{quoted}")
                        } else {
                            persistent.clone()
                        };
                        (Some(persistent), Some(full))
                    }
                    // Non-channel-lead + parent message: investigation context
                    (false, Some((from, content))) => {
                        let header = "The following message needs investigation:";
                        let quoted = format!("{from} wrote:\n{}", format_blockquote(&content));
                        let persistent = format!("{header}\n\n{quoted}");
                        let full = if let Some(ref summary) = channel_summary {
                            format!("{header}\n\n{summary}\n\n{quoted}")
                        } else {
                            persistent.clone()
                        };
                        (Some(persistent), Some(full))
                    }
                    // Channel lead, no parent found: framing + optional summary
                    (true, None) => {
                        let framing = fork_channel
                            .as_ref()
                            .map(|ch| super::rpc_channel::fork_initial_framing(ch));
                        let full = match (&framing, &channel_summary) {
                            (Some(f), Some(s)) => Some(format!("{f}\n\n{s}")),
                            _ => framing.clone(),
                        };
                        (framing, full)
                    }
                    // Non-channel-lead, no parent: no nudge
                    (false, None) => (None, None),
                };
                // Return (persistent, full) — caller persists `persistent` and
                // sends `full` as the nudge.
                (persistent_prompt, nudge_prompt)
            };
            if let Some(message) = nudge_message {
                // Persist only the static portion (no channel summary) so crash
                // recovery doesn't replay stale time-stamped activity data.
                if let Some(ref persistent) = persistent_prompt {
                    let mut ps = state.persistent_state.lock().await;
                    if let Some(record) = ps.sessions.get_mut(&sid) {
                        record.initial_prompt = Some(persistent.clone());
                    }
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!("session.fork: failed to persist initial_prompt: {}", e);
                    }
                }
                let nudge = crate::daemon::effects::Effect::NudgeSession {
                    session_id: sid.clone(),
                    reason: crate::daemon::wake_reason::WakeReason::Nudge { message },
                };
                crate::daemon::effects::execute_effects(vec![nudge], state).await;
            }

            // Broadcast thread ownership to web clients — matches the web-UI
            // fork path so the "Dedicated session" indicator appears regardless
            // of how the fork was created.
            if let Some(ref ch) = fork_channel {
                let (owner, parent_lead) = {
                    let ps = state.persistent_state.lock().await;
                    let owner = ps.sessions.get(&sid).map(|s| s.name.clone());
                    // Resolve parent lead via channel_lead_sessions (not the caller)
                    // so non-lead callers don't get misattributed as the parent.
                    let parent_lead = ps
                        .channel_lead_sessions
                        .get(ch.as_str())
                        .and_then(|lead_sid| ps.sessions.get(lead_sid).map(|s| s.name.clone()));
                    (owner, parent_lead)
                };
                state.broadcast_web_update(web::WebUpdate::ThreadOwnership(
                    web::ThreadOwnershipData {
                        thread_parent_id: thread_parent_id.to_string(),
                        channel: ch.clone(),
                        has_dedicated_session: true,
                        owner,
                        parent_lead,
                    },
                ));
            }

            crate::rpc::Response::success(
                id,
                serde_json::json!({
                    "session_id": sid,
                    "thread_parent_id": thread_parent_id,
                }),
            )
        }
        Err(ref e) if e.starts_with("fork in progress") => {
            // A concurrent fork request already reserved this slot.
            // Return a distinct pending response so the caller can
            // distinguish "retry shortly" from a hard spawn failure.
            crate::rpc::Response::success(
                id,
                serde_json::json!({
                    "pending": true,
                    "thread_parent_id": thread_parent_id,
                    "message": "fork in progress — retry shortly",
                }),
            )
        }
        Err(e) => crate::rpc::Response::error(id, crate::rpc::RpcError::new(-32603, e)),
    }
}

// ============================================================================
// Web-UI thread forking
// ============================================================================

/// Handle `session.fork_thread` RPC — web-UI-friendly fork.
///
/// Takes `thread_parent_id` + `channel` (no session ID needed). Resolves the
/// channel lead session ID server-side and delegates to `create_fork_session()`.
pub(super) async fn handle_session_fork_thread(
    id: RequestId,
    thread_parent_id: &str,
    channel: &str,
    state: &DaemonState,
) -> Response {
    // Validate thread_parent_id is a UUID
    if uuid::Uuid::parse_str(thread_parent_id).is_err() {
        return Response::error(
            id,
            crate::rpc::RpcError::new(
                -32602,
                format!(
                    "Invalid thread_parent_id '{}': expected a channel message UUID.",
                    thread_parent_id
                ),
            ),
        );
    }
    // Resolve channel lead session ID from persistent state
    let lead_session_id = {
        let ps = state.persistent_state.lock().await;
        ps.channel_lead_sessions
            .get(channel)
            .filter(|s| !s.is_empty())
            .cloned()
    };
    let Some(lead_session_id) = lead_session_id else {
        return Response::error(
            id,
            RpcError::new(
                -32602,
                format!("No channel lead session for channel '{}'", channel),
            ),
        );
    };
    // Verify the session record still exists. If the lead session has died,
    // clean up the stale mapping so the next attempt returns "No channel lead"
    // (self-healing) rather than repeating the "stale" error forever.
    {
        let mut ps = state.persistent_state.lock().await;
        if !ps.sessions.contains_key(&lead_session_id) {
            ps.channel_lead_sessions.remove(channel);
            return Response::error(
                id,
                RpcError::new(
                    -32602,
                    format!("Channel lead session for '{}' is stale", channel),
                ),
            );
        }
    }

    match create_fork_session(
        thread_parent_id,
        &lead_session_id,
        Some(channel),
        None, // no name hint from web UI
        None, // no color from web UI
        None, // no icon from web UI
        "session.fork_thread",
        state,
    )
    .await
    {
        Ok((sid, is_existing, _)) => {
            // Send framing + channel summary nudge to fresh forks
            if !is_existing {
                let framing = super::rpc_channel::fork_initial_framing(channel);

                // Build channel summary for situational awareness
                let channel_summary = {
                    let base_dir = state.paths.base_dir().to_path_buf();
                    match crate::channel::Channel::new(&base_dir, channel) {
                        Ok(ch) => build_channel_summary_for_fork(&ch).await,
                        Err(_) => None,
                    }
                };

                // Full nudge includes volatile summary; persist only static framing
                let full_nudge = if let Some(ref summary) = channel_summary {
                    format!("{framing}\n\n{summary}")
                } else {
                    framing.clone()
                };

                // Persist only the static framing for crash recovery
                {
                    let mut ps = state.persistent_state.lock().await;
                    if let Some(record) = ps.sessions.get_mut(&sid) {
                        record.initial_prompt = Some(framing);
                    }
                    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
                        warn!(
                            "session.fork_thread: failed to persist initial_prompt: {}",
                            e
                        );
                    }
                }
                let framing_effect = crate::daemon::effects::Effect::NudgeSession {
                    session_id: sid.clone(),
                    reason: crate::daemon::wake_reason::WakeReason::Nudge {
                        message: full_nudge,
                    },
                };
                crate::daemon::effects::execute_effects(vec![framing_effect], state).await;
            }

            // Broadcast ownership change to web clients
            let (owner, parent_lead) = {
                let ps = state.persistent_state.lock().await;
                let owner = ps.sessions.get(&sid).map(|s| s.name.clone());
                let parent_lead = ps.sessions.get(&lead_session_id).map(|s| s.name.clone());
                (owner, parent_lead)
            };
            state.broadcast_web_update(web::WebUpdate::ThreadOwnership(web::ThreadOwnershipData {
                thread_parent_id: thread_parent_id.to_string(),
                channel: channel.to_string(),
                has_dedicated_session: true,
                owner,
                parent_lead,
            }));

            debug!(
                "session.fork_thread: {} → fork {} (existing={})",
                thread_parent_id, sid, is_existing
            );
            Response::success(
                id,
                serde_json::json!({
                    "session_id": sid,
                    "already_exists": is_existing,
                }),
            )
        }
        Err(ref e) if e.starts_with("fork in progress") => Response::success(
            id,
            serde_json::json!({
                "pending": true,
                "thread_parent_id": thread_parent_id,
            }),
        ),
        Err(e) => Response::error(id, RpcError::new(-32603, e)),
    }
}

/// Handle `session.unfork_thread` RPC — kill the dedicated session for a thread.
///
/// Looks up the fork session from SessionRecord, triggers `ShutdownSession`
/// (cleanup is automatic via `cleanup_coworker_state`), and broadcasts
/// `ThreadOwnership(false)`.
pub(super) async fn handle_session_unfork_thread(
    id: RequestId,
    thread_parent_id: &str,
    channel: &str,
    state: &DaemonState,
) -> Response {
    let fork_session = {
        let ps = state.persistent_state.lock().await;
        ps.session_by_thread(thread_parent_id)
            .map(|s| (s.session_id.clone(), s.name.clone()))
    };

    let Some((fork_session_id, fork_name)) = fork_session else {
        return Response::error(
            id,
            RpcError::new(-32602, "No dedicated session for this thread"),
        );
    };

    // Verify the fork session has a name mapping before attempting shutdown.
    if fork_name.is_empty() {
        warn!(
            "session.unfork_thread: fork session {} has no name mapping (stale)",
            fork_session_id
        );
        state.broadcast_web_update(web::WebUpdate::ThreadOwnership(web::ThreadOwnershipData {
            thread_parent_id: thread_parent_id.to_string(),
            channel: channel.to_string(),
            has_dedicated_session: false,
            owner: None,
            parent_lead: None,
        }));
        return Response::error(
            id,
            RpcError::new(-32603, "Fork session is stale (missing name mapping)"),
        );
    }

    // ShutdownSession → shutdown_coworker_impl → cleanup_coworker_state
    // which marks the SessionRecord as is_running=false.
    let effect = crate::daemon::effects::Effect::ShutdownSession {
        session_id: fork_session_id.clone(),
        reason: "User returned thread to channel lead".to_string(),
    };
    crate::daemon::effects::execute_effects(vec![effect], state).await;

    // Ownership broadcast is handled by cleanup_coworker_state (called from
    // shutdown_coworker_impl inside ShutdownSession).

    debug!(
        "session.unfork_thread: {} (session {})",
        thread_parent_id, fork_session_id
    );
    Response::success(
        id,
        serde_json::json!({
            "success": true,
        }),
    )
}

/// Handle `session.thread_ownership` RPC — query whether a thread has a dedicated session.
///
/// Broadcasts `ThreadOwnership` to all web clients so the requesting client
/// (and any others) learn the current state.
pub(super) async fn handle_session_thread_ownership(
    id: RequestId,
    thread_parent_id: &str,
    channel: &str,
    state: &DaemonState,
) -> Response {
    let (fork_session_id, has_dedicated, owner, parent_lead) = {
        let ps = state.persistent_state.lock().await;
        let fork = ps
            .session_by_thread(thread_parent_id)
            .filter(|s| s.is_running);
        let fork_sid = fork.map(|s| s.session_id.clone());
        let has_dedicated = fork_sid.is_some();
        let owner = fork.map(|s| s.name.clone());
        let parent_lead = if has_dedicated {
            ps.channel_lead_sessions
                .get(channel)
                .and_then(|sid| ps.sessions.get(sid).map(|s| s.name.clone()))
        } else {
            None
        };
        (fork_sid, has_dedicated, owner, parent_lead)
    };

    let _ = fork_session_id; // used for has_dedicated derivation

    state.broadcast_web_update(web::WebUpdate::ThreadOwnership(web::ThreadOwnershipData {
        thread_parent_id: thread_parent_id.to_string(),
        channel: channel.to_string(),
        has_dedicated_session: has_dedicated,
        owner,
        parent_lead,
    }));

    Response::success(
        id,
        serde_json::json!({
            "has_dedicated_session": has_dedicated,
        }),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[path = "rpc_session_tests.rs"]
#[cfg(test)]
mod tests;
