pub mod handlers;

use std::path::Path;

use handlers::{AgentFilter, RpcError};
use serde_json::{Value, json};

use crate::daemon_v2::Projections;
use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::DomainEvent;

#[path = "rpc_tests.rs"]
#[cfg(test)]
mod tests;

type RpcReturn = (Value, Vec<DomainEvent>, Vec<Command>);

/// Wrap a handler that returns events into a JSON-RPC success response.
fn events_response(id: &Value, result: Result<Vec<DomainEvent>, RpcError>) -> RpcReturn {
    match result {
        Ok(events) => (
            json!({"jsonrpc":"2.0","result":{"ok":true},"id":id}),
            events,
            vec![],
        ),
        Err(err) => (err.to_json(id), vec![], vec![]),
    }
}

/// Wrap a handler that returns commands into a JSON-RPC success response.
fn commands_response(id: &Value, result: Result<Vec<Command>, RpcError>) -> RpcReturn {
    match result {
        Ok(commands) => (
            json!({"jsonrpc":"2.0","result":{"ok":true},"id":id}),
            vec![],
            commands,
        ),
        Err(err) => (err.to_json(id), vec![], vec![]),
    }
}

/// Wrap a handler that returns a value + commands into a JSON-RPC success response.
fn value_commands_response(
    id: &Value,
    result: Result<(Value, Vec<Command>), RpcError>,
) -> RpcReturn {
    match result {
        Ok((value, commands)) => (
            json!({"jsonrpc":"2.0","result":value,"id":id}),
            vec![],
            commands,
        ),
        Err(err) => (err.to_json(id), vec![], vec![]),
    }
}

/// Dispatch a JSON-RPC request, returning the response JSON, any domain
/// events produced by mutating methods (e.g., `task.create`), and any
/// commands to execute (e.g., `session.fork` spawning a new agent).
pub fn dispatch_request(
    request: Value,
    proj: &Projections,
    channels_dir: &Path,
) -> (Value, Vec<DomainEvent>, Vec<Command>) {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = match request.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            let err = RpcError {
                code: -32600,
                message: "Missing method".into(),
            };
            return (err.to_json(&id), vec![], vec![]);
        }
    };
    let params = request.get("params");

    // Mutating methods return events alongside the result.
    match method {
        "task.create" => events_response(&id, handlers::handle_task_create(params, proj)),
        "task.done" => events_response(&id, handlers::handle_task_done(params)),
        "task.update" => match handlers::handle_task_update(params, proj) {
            Ok((events, commands)) => (
                json!({"jsonrpc":"2.0","result":{"ok":true},"id":id}),
                events,
                commands,
            ),
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        "channel.update" => events_response(&id, handlers::handle_channel_update(params)),
        "channel.rename" => {
            events_response(&id, handlers::handle_channel_rename(params, channels_dir))
        }
        "channel.archive" => {
            events_response(&id, handlers::handle_channel_archive(params, channels_dir))
        }
        "channel.unarchive" => events_response(
            &id,
            handlers::handle_channel_unarchive(params, channels_dir),
        ),
        "coworker.report-state" => {
            events_response(&id, handlers::handle_report_state(params, proj))
        }
        "workflow.set_state" => events_response(&id, handlers::handle_workflow_set_state(params)),
        "workflow.set_lead_driven" => {
            // Transform into channel.update with lead_driven param
            let transformed = params.map(|p| {
                json!({
                    "channel": p.get("channel").cloned().unwrap_or(Value::Null),
                    "lead_driven": p.get("enabled").cloned().unwrap_or(Value::Bool(false)),
                })
            });
            events_response(&id, handlers::handle_channel_update(transformed.as_ref()))
        }
        "reminder.cancel" => events_response(&id, handlers::handle_reminder_cancel(params)),

        "pr.action" => commands_response(&id, handlers::handle_pr_action(params, proj)),
        "coworker.break" | "session.detach" => {
            commands_response(&id, handlers::handle_agent_stop(params, proj))
        }
        "coworker.nudge" => commands_response(&id, handlers::handle_agent_nudge(params, proj)),
        "task.prompt" => commands_response(&id, handlers::handle_task_prompt(params, proj)),
        "task.request" => match handlers::handle_task_request(params, proj, channels_dir) {
            Ok((result, events, commands)) => (
                json!({"jsonrpc":"2.0","result":result,"id":id}),
                events,
                commands,
            ),
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },

        "session.fork" => value_commands_response(&id, handlers::handle_session_fork(params, proj)),
        "coworker.spawn" => {
            value_commands_response(&id, handlers::handle_coworker_spawn(params, proj))
        }
        "oneshot.execute" => {
            value_commands_response(&id, handlers::handle_oneshot_execute(params, proj))
        }

        "channel.post" => match handlers::handle_channel_post(params, channels_dir, proj) {
            Ok((value, events, commands)) => (
                json!({"jsonrpc":"2.0","result":value,"id":id}),
                events,
                commands,
            ),
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        "task.handoff" => match handlers::handle_task_handoff(params, proj) {
            Ok((events, commands)) => (
                json!({"jsonrpc":"2.0","result":{"ok":true},"id":id}),
                events,
                commands,
            ),
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        // v1 alias: lead.spawn — lead is auto-spawned by scheduler, return success
        "lead.spawn" => (
            json!({"jsonrpc":"2.0","result":{"ok":true},"id":id}),
            vec![],
            vec![],
        ),
        // reminder.create returns the new reminder ID in the response
        "reminder.create" => match handlers::handle_reminder_create(params) {
            Ok(events) => {
                let reminder_id = events
                    .first()
                    .and_then(|e| {
                        if let DomainEvent::ReminderCreated { id: rid, .. } = e {
                            Some(rid.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default();
                (
                    json!({"jsonrpc":"2.0","result":{"ok":true,"id":reminder_id},"id":id}),
                    events,
                    vec![],
                )
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        // auth.switch — switch auth profile for a provider
        "auth.switch" => {
            let profile = params
                .and_then(|p| p.get("profile").and_then(|v| v.as_str()))
                .unwrap_or("default");
            let provider_str = params
                .and_then(|p| p.get("provider").and_then(|v| v.as_str()))
                .unwrap_or("claude");
            let provider = match provider_str {
                "codex" => crate::auth::AuthProvider::Codex,
                _ => crate::auth::AuthProvider::Claude,
            };
            match crate::auth::set_current_profile_for(provider, profile) {
                Ok(()) => {
                    tracing::info!(%profile, %provider_str, "auth profile switched");
                    (
                        json!({"jsonrpc":"2.0","result":{"ok":true,"profile":profile},"id":id}),
                        vec![],
                        vec![],
                    )
                }
                Err(e) => {
                    let err = handlers::RpcError {
                        code: -32000,
                        message: format!("Failed to switch profile: {e}"),
                    };
                    (err.to_json(&id), vec![], vec![])
                }
            }
        }
        _ => {
            // Read-only methods — no events produced.
            let result = match method {
                // v1 compatibility aliases (read-only)
                "ping" => Ok(json!("pong")),
                "version" => Ok(json!({
                    "name": "midtown",
                    "version": env!("CARGO_PKG_VERSION"),
                    "daemon": "v2",
                })),
                "status" | "snapshot" => handlers::handle_status(proj),
                "agent.list" | "coworker.list" | "coworkers.status" | "session.list" => {
                    let filter = AgentFilter::from_params(params);
                    handlers::handle_agent_list(proj, filter)
                }
                "task.list" => handlers::handle_task_list(proj),
                "pr.list" => handlers::handle_pr_list(proj),
                "prs.status" => handlers::handle_prs_status(proj),
                "channel.list" => handlers::handle_channel_list(channels_dir),
                "channel.read" => handlers::handle_channel_read(params, channels_dir),
                "reminder.list" => handlers::handle_reminder_list(proj),
                "channel.create" => handlers::handle_channel_create(params, channels_dir),
<<<<<<< HEAD
                "channel.archive" => handlers::handle_channel_archive(params, channels_dir),
                "channel.unarchive" => handlers::handle_channel_unarchive(params, channels_dir),
=======
                "channel.rename" => handlers::handle_channel_rename(params, channels_dir),
>>>>>>> 567b60ff (fix: emit events on channel.archive/unarchive for WebSocket notification)
                // Stubs for CLI methods that don't have full v2 implementations yet
                "workflow.list" => handlers::handle_workflow_list(proj),
                "pr.list-external" | "pr.allow" => Ok(json!({"ok": true, "stub": true})),
                // daemon.set-draining and daemon.check-pending return info
                // about draining state (actual flag managed by the daemon loop)
                "daemon.set-draining" | "daemon.check-pending" => {
                    Ok(json!({"ok": true, "draining": false}))
                }
                // pr.merge and pr.review are shortcuts for pr.action
                "pr.merge" => {
                    // Transform into pr.action merge
                    let number = params
                        .and_then(|p| p.get("number").or_else(|| p.get("pr")))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let merged = json!({"action": "merge", "number": number});
                    match handlers::handle_pr_action(Some(&merged), proj) {
                        Ok(commands) => {
                            return (
                                json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id }),
                                vec![],
                                commands,
                            );
                        }
                        Err(err) => return (err.to_json(&id), vec![], vec![]),
                    }
                }
                "pr.review" | "pr.review-post" => match handlers::handle_pr_review_post(params) {
                    Ok(result) => Ok(result),
                    Err(err) => Err(err),
                },
                // shutdown is handled at the daemon level (RpcOutcome::Shutdown)
                // but we also handle it here for web API dispatch
                "shutdown" => Ok(json!({"ok": true})),
                _ => Err(RpcError::method_not_found()),
            };
            let response = match result {
                Ok(value) => json!({ "jsonrpc": "2.0", "result": value, "id": id }),
                Err(err) => err.to_json(&id),
            };
            (response, vec![], vec![])
        }
    }
}
