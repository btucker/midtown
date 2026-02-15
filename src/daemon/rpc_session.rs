//! Session attach/detach RPC handlers.
//!
//! Handles `session.resolve`, `session.attach`, `session.detach`, and
//! `session.list` methods,
//! allowing interactive terminal sessions to be connected to headless coworker
//! processes.

use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tracing::{info, warn};

use crate::message::Message;
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Target parsing
// ============================================================================

/// Parsed attach target from a "type:value" string.
#[derive(Debug, PartialEq)]
enum AttachTarget {
    Name(String),
    Task(u32),
    Pr(u64),
    PlatformSession {
        platform: crate::auth::AuthProvider,
        session_id: String,
    },
}

/// Parse an attach target string into a typed enum.
///
/// Pure function — no state access. Validates format and types.
fn parse_attach_target(target: &str) -> Result<AttachTarget, String> {
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
            let assignments = state.coworker_task_assignments.lock().unwrap();
            let matches: Vec<String> = assignments
                .iter()
                .filter_map(|(coworker, assignment)| {
                    if assignment.task_id == id_str {
                        Some(coworker.to_lowercase())
                    } else {
                        None
                    }
                })
                .collect();
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
                    "No running headless session found for {}/{}",
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
async fn resolve_attach_target(target: &str, state: &DaemonState) -> Result<String, String> {
    let mut names = resolve_attach_target_candidates(target, state).await?;
    match names.len() {
        0 => Err(format!("No attachable sessions for target '{}'", target)),
        1 => Ok(names.remove(0)),
        _ => Err(format!(
            "Multiple sessions match '{}': {}. Choose one via `midtown session attach name/<coworker>`.",
            target,
            names.join(", ")
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
    let mut candidates: Vec<serde_json::Value> = names
        .into_iter()
        .filter_map(|name| {
            let coworker = state.coworkers.get(&name)?;
            let info = persistent.headless_sessions.get(&name)?;
            let provider = info.provider.unwrap_or(crate::auth::AuthProvider::Claude);
            let platform = platform_for_provider(info.provider);
            let attached_now = attached.contains(&name);
            let last_active_age_ms = now
                .signed_duration_since(info.last_active)
                .num_milliseconds()
                .max(0) as u64;
            Some(serde_json::json!({
                "name": name,
                "session_id": info.session_id,
                "provider": provider.as_str(),
                "platform": platform.as_str(),
                "cwd": coworker.working_dir,
                "running": true,
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
                    "Target '{}' matched no currently running attachable sessions",
                    target
                ),
            ),
        );
    }

    candidates.sort_by(|a, b| {
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
/// Pauses the headless coworker process and returns session info so the CLI
/// can create an interactive pane with the matching provider CLI.
pub(super) async fn handle_session_attach(
    id: RequestId,
    target: &str,
    state: &DaemonState,
) -> Response {
    let name = match resolve_attach_target(target, state).await {
        Ok(n) => n,
        Err(e) => return Response::error(id, RpcError::new(-32602, e)),
    };

    // Verify the coworker is running
    if state.coworkers.get(&name).is_none() {
        return Response::error(
            id,
            RpcError::new(-32602, format!("Coworker '{}' is not running", name)),
        );
    }

    // Guard against double-attach
    {
        let attached = state.attached_coworkers.lock().unwrap();
        if attached.contains(&name.to_lowercase()) {
            return Response::error(
                id,
                RpcError::new(-32602, format!("Coworker '{}' is already attached", name)),
            );
        }
    }

    // Get session details from persistent state
    let session_info = {
        let persistent = state.persistent_state.lock().await;
        persistent.headless_sessions.get(&name).cloned()
    };

    let (session_id, provider) = match session_info {
        Some(info) => (
            info.session_id,
            info.provider.unwrap_or(crate::auth::AuthProvider::Claude),
        ),
        None => {
            return Response::error(
                id,
                RpcError::new(
                    -32603,
                    format!(
                        "No session ID found for coworker '{}'. \
                         They may not be running in headless mode.",
                        name
                    ),
                ),
            );
        }
    };

    // Get working directory before shutting down
    let cwd = state
        .coworkers
        .get(&name)
        .map(|cw| cw.working_dir.clone())
        .unwrap_or_default();

    // Shut down the headless coworker (kills the process but session persists on disk)
    state.broadcast_coworker_update(&name, "attaching", None);
    if let Err(e) = state.coworkers.shutdown(&name) {
        return Response::error(
            id,
            RpcError::new(
                -32603,
                format!("Failed to pause coworker '{}': {}", name, e),
            ),
        );
    }
    // Record stop time to prevent false orphan recovery during the grace period
    // (see #874). The attached_coworkers set provides the long-term exemption.
    state.record_coworker_stop_time(&name);

    // Mark as attached so stuck detection and orphan recovery skip this coworker
    {
        let mut attached = state.attached_coworkers.lock().unwrap();
        attached.insert(name.to_lowercase());
    }

    info!(
        "Paused headless coworker '{}' for attach (session={})",
        name, session_id
    );

    // Post to channel
    let _ = state
        .send_and_broadcast_async(&Message::system(format!(
            "Attached to {} — headless paused, interactive tmux session active",
            name
        )))
        .await;

    Response::success(
        id,
        serde_json::json!({
            "session_id": session_id,
            "cwd": cwd,
            "name": name,
            "provider": provider.as_str(),
        }),
    )
}

/// Handle session.detach RPC method.
///
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

    // Idempotency guard: if the coworker is already running, skip re-spawn.
    // This prevents the race between manual detach and background auto-detach
    // from spawning duplicate processes.
    if state.coworkers.get(&name).is_some() {
        info!("Coworker '{}' already running — detach is a no-op", name);
        return Response::success(
            id,
            serde_json::json!({
                "success": true,
                "message": format!("Coworker {} is already running", name),
            }),
        );
    }

    // Get session ID from persistent state
    let session_id = {
        let persistent = state.persistent_state.lock().await;
        persistent
            .headless_sessions
            .get(&name)
            .map(|info| info.session_id.clone())
    };

    let session_id = match session_id {
        Some(sid) => sid,
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

    // Re-spawn the coworker with the resumed session
    let config = crate::launch::LaunchConfig::coworker(
        &name,
        &state.repo_name,
        crate::launch::SessionMode::ResumeSession(session_id.clone()),
        Some("You were previously running headless. The Lead attached to your session interactively and has now detached. Continue where you left off — read the channel for any updates.".to_string()),
    );

    match state.spawn_coworker(&config).await {
        Ok(()) => {
            info!(
                "Resumed headless coworker '{}' after detach (session={})",
                name, session_id
            );

            let _ = state
                .send_and_broadcast_async(&Message::system(format!(
                    "Detached from {} — headless session resumed",
                    name
                )))
                .await;

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
pub(super) async fn handle_session_list(id: RequestId, state: &DaemonState) -> Response {
    let persistent = state.persistent_state.lock().await;
    let running_coworkers: std::collections::HashSet<String> = state
        .coworkers
        .list()
        .iter()
        .map(|cw| cw.name.to_lowercase())
        .collect();
    let attached = state.attached_coworkers.lock().unwrap().clone();

    let sessions: Vec<serde_json::Value> = persistent
        .headless_sessions
        .iter()
        .map(|(name, info)| {
            let status = if attached.contains(&name.to_lowercase()) {
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

    Response::success(
        id,
        serde_json::json!({
            "success": true,
            "sessions": sessions,
        }),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_attach_target_name() {
        assert_eq!(
            parse_attach_target("name:park").unwrap(),
            AttachTarget::Name("park".to_string())
        );
        // Names are lowercased
        assert_eq!(
            parse_attach_target("name:Park").unwrap(),
            AttachTarget::Name("park".to_string())
        );
    }

    #[test]
    fn test_parse_attach_target_name_empty() {
        assert!(parse_attach_target("name:").is_err());
    }

    #[test]
    fn test_parse_attach_target_name_slash() {
        assert_eq!(
            parse_attach_target("name/park").unwrap(),
            AttachTarget::Name("park".to_string())
        );
    }

    #[test]
    fn test_parse_attach_target_task() {
        assert_eq!(
            parse_attach_target("task:42").unwrap(),
            AttachTarget::Task(42)
        );
    }

    #[test]
    fn test_parse_attach_target_task_slash() {
        assert_eq!(
            parse_attach_target("task/42").unwrap(),
            AttachTarget::Task(42)
        );
    }

    #[test]
    fn test_parse_attach_target_task_invalid() {
        assert!(parse_attach_target("task:abc").is_err());
        assert!(parse_attach_target("task:-1").is_err());
    }

    #[test]
    fn test_parse_attach_target_pr() {
        assert_eq!(
            parse_attach_target("pr:123").unwrap(),
            AttachTarget::Pr(123)
        );
    }

    #[test]
    fn test_parse_attach_target_provider_session() {
        assert_eq!(
            parse_attach_target("claude/abc-123").unwrap(),
            AttachTarget::PlatformSession {
                platform: crate::auth::AuthProvider::Claude,
                session_id: "abc-123".to_string()
            }
        );
        assert_eq!(
            parse_attach_target("codex/thread-1").unwrap(),
            AttachTarget::PlatformSession {
                platform: crate::auth::AuthProvider::Codex,
                session_id: "thread-1".to_string()
            }
        );
    }

    #[test]
    fn test_parse_attach_target_rejects_zai_platform() {
        assert!(parse_attach_target("zai/abc-123").is_err());
        assert!(parse_attach_target("z.ai/abc-123").is_err());
    }

    #[test]
    fn test_parse_attach_target_pr_invalid() {
        assert!(parse_attach_target("pr:abc").is_err());
    }

    #[test]
    fn test_parse_attach_target_invalid_format() {
        assert!(parse_attach_target("invalid").is_err());
        assert!(parse_attach_target("unknown:value").is_err());
        assert!(parse_attach_target("").is_err());
    }
}
