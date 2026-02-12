//! Session attach/detach RPC handlers.
//!
//! Handles `session.attach`, `session.detach`, and `session.list` for
//! interactive debugging of headless coworker sessions.

use tracing::{info, warn};

use crate::message::Message;
use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Attach target parsing
// ============================================================================

/// Parsed attach target from a "type:value" string.
#[derive(Debug, PartialEq)]
pub(super) enum AttachTarget {
    Name(String),
    Task(u32),
    Pr(u64),
}

/// Parse an attach target string into a typed enum.
///
/// Pure function — no state access. Validates format and types.
pub(super) fn parse_attach_target(target: &str) -> Result<AttachTarget, String> {
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

/// Resolve an attach target to a coworker name using daemon state.
async fn resolve_attach_target(target: &str, state: &DaemonState) -> Result<String, String> {
    let parsed = parse_attach_target(target)?;

    match parsed {
        AttachTarget::Name(name) => Ok(name),
        AttachTarget::Task(id) => {
            let id_str = id.to_string();
            let assignments = state.coworker_task_assignments.lock().unwrap();
            for (coworker, assignment) in assignments.iter() {
                if assignment.task_id == id_str {
                    return Ok(coworker.clone());
                }
            }
            Err(format!("No coworker is assigned to task !{}", id))
        }
        AttachTarget::Pr(pr_num) => {
            // Check reviewer assignments
            let persistent = state.persistent_state.lock().await;
            if let Some(reviewer) = persistent.github.get_reviewer(pr_num) {
                return Ok(reviewer.to_lowercase());
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
                    return Ok(cw.name.to_lowercase());
                }
            }
            Err(format!("No coworker is working on PR #{}", pr_num))
        }
    }
}

// ============================================================================
// Session RPC handlers
// ============================================================================

/// Handle session.attach RPC method.
///
/// Pauses the headless coworker process and returns session info so the CLI
/// can create a tmux window with `claude --resume <session-id>`.
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

    // Get the session ID from persistent state
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
    fn test_parse_attach_target_task() {
        assert_eq!(
            parse_attach_target("task:42").unwrap(),
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
