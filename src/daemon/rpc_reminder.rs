//! Reminder RPC handlers.
//!
//! Handles `reminder.create`, `reminder.list`, and `reminder.cancel` methods.
//! Reminders are triggers that fire on conditions (all work merged) or cron
//! schedules, notifying the lead with a stored message.

use tracing::{error, info};

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

// ============================================================================
// Handlers
// ============================================================================

/// Handle reminder.create RPC method.
pub(super) async fn handle_reminder_create(
    id: RequestId,
    trigger: &str,
    message: &str,
    cron_expr: Option<&str>,
    repeat: i32,
    state: &DaemonState,
) -> Response {
    // Validate cron expression if provided
    if let Some(expr) = cron_expr
        && let Err(e) = crate::reminders::validate_cron_expression(expr)
    {
        return Response::error(id, RpcError::new(-32602, e));
    }

    let repeat_policy = match repeat {
        -1 => crate::reminders::RepeatPolicy::Indefinite,
        0 => crate::reminders::RepeatPolicy::Once,
        n if n > 0 => crate::reminders::RepeatPolicy::Times(n as u32),
        _ => {
            return Response::error(
                id,
                RpcError::new(
                    -32602,
                    "repeat must be -1, 0, or a positive number".to_string(),
                ),
            );
        }
    };

    let reminder_trigger = match trigger {
        "all-work-merged" => crate::reminders::ReminderTrigger::AllWorkMerged,
        "cron-utc" => crate::reminders::ReminderTrigger::CronUtc {
            cron_expr: cron_expr.unwrap().to_string(),
        },
        _ => return Response::error(id, RpcError::invalid_params()),
    };

    let mut ps = state.persistent_state.lock().await;
    let reminder_id =
        ps.reminders
            .add(reminder_trigger, message.to_string(), repeat_policy.clone());

    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
        error!("Failed to save daemon-state.json: {}", e);
    }

    let confirmation = match trigger {
        "cron-utc" => format!(
            "Reminder set (id: {}): I'll notify you on cron schedule '{}' (UTC). Repeat: {}. Message: \"{}\"",
            reminder_id,
            cron_expr.unwrap(),
            repeat_policy,
            message
        ),
        _ => format!(
            "Reminder set (id: {}): I'll notify you when all tasks are completed and all PRs are merged. Repeat: {}. Message: \"{}\"",
            reminder_id, repeat_policy, message
        ),
    };
    info!("{}", confirmation);
    Response::success(id, serde_json::json!({ "message": confirmation }))
}

/// Handle reminder.list RPC method.
pub(super) async fn handle_reminder_list(id: RequestId, state: &DaemonState) -> Response {
    let ps = state.persistent_state.lock().await;
    let active = ps.reminders.active();

    if active.is_empty() {
        return Response::success(id, serde_json::json!({ "message": "No active reminders." }));
    }

    let lines: Vec<String> = active
        .iter()
        .map(|r| {
            format!(
                "  {} [{}] \"{}\" ({}, created {})",
                r.id,
                r.trigger,
                r.message,
                r.fires_remaining(),
                r.created_at.format("%Y-%m-%d %H:%M UTC")
            )
        })
        .collect();

    let output = format!("Active reminders:\n{}", lines.join("\n"));
    Response::success(id, serde_json::json!({ "message": output }))
}

/// Handle reminder.cancel RPC method.
pub(super) async fn handle_reminder_cancel(
    id: RequestId,
    reminder_id: &str,
    state: &DaemonState,
) -> Response {
    let mut ps = state.persistent_state.lock().await;
    if ps.reminders.cancel(reminder_id) {
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            error!("Failed to save daemon-state.json: {}", e);
        }
        let msg = format!("Reminder {} cancelled.", reminder_id);
        info!("{}", msg);
        Response::success(id, serde_json::json!({ "message": msg }))
    } else {
        Response::error(
            id,
            RpcError::new(-32602, format!("Reminder '{}' not found", reminder_id)),
        )
    }
}
