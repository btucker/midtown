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
        "task.create" => {
            let result = handlers::handle_task_create(params);
            match result {
                Ok(events) => {
                    let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                    (response, events, vec![])
                }
                Err(err) => (err.to_json(&id), vec![], vec![]),
            }
        }
        // v1 alias: task.done → TaskCompleted event
        "task.done" => {
            let result = handlers::handle_task_done(params);
            match result {
                Ok(events) => {
                    let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                    (response, events, vec![])
                }
                Err(err) => (err.to_json(&id), vec![], vec![]),
            }
        }
        "channel.post" => match handlers::handle_channel_post(params, channels_dir, proj) {
            Ok((value, events, commands)) => {
                let response = json!({ "jsonrpc": "2.0", "result": value, "id": id });
                (response, events, commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        "task.update" => {
            let result = handlers::handle_task_update(params, proj);
            match result {
                Ok(events) => {
                    let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                    (response, events, vec![])
                }
                Err(err) => (err.to_json(&id), vec![], vec![]),
            }
        }
        "pr.action" => match handlers::handle_pr_action(params, proj) {
            Ok(commands) => {
                let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                (response, vec![], commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        "channel.update" => {
            let result = handlers::handle_channel_update(params);
            match result {
                Ok(events) => {
                    let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                    (response, events, vec![])
                }
                Err(err) => (err.to_json(&id), vec![], vec![]),
            }
        }
        "session.fork" => match handlers::handle_session_fork(params, proj) {
            Ok((value, commands)) => {
                let response = json!({ "jsonrpc": "2.0", "result": value, "id": id });
                (response, vec![], commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        // v1 alias: lead.spawn — lead is auto-spawned by scheduler, return success
        "lead.spawn" => {
            let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
            (response, vec![], vec![])
        }
        // v1 alias: coworker.spawn — spawn a worker agent
        "coworker.spawn" => match handlers::handle_coworker_spawn(params, proj) {
            Ok((value, commands)) => {
                let response = json!({ "jsonrpc": "2.0", "result": value, "id": id });
                (response, vec![], commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        // v1 alias: coworker.break — stop an agent by name
        "coworker.break" => match handlers::handle_agent_stop(params, proj) {
            Ok(commands) => {
                let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                (response, vec![], commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        // coworker.report-state — record the agent's self-reported state
        "coworker.report-state" => {
            let result = handlers::handle_report_state(params, proj);
            match result {
                Ok(events) => {
                    let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                    (response, events, vec![])
                }
                Err(err) => (err.to_json(&id), vec![], vec![]),
            }
        }
        // v1 alias: coworker.nudge — nudge an agent by name
        "coworker.nudge" => match handlers::handle_agent_nudge(params, proj) {
            Ok(commands) => {
                let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                (response, vec![], commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        "task.prompt" => match handlers::handle_task_prompt(params, proj) {
            Ok(commands) => {
                let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                (response, vec![], commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
        "task.handoff" => match handlers::handle_task_handoff(params, proj) {
            Ok((events, commands)) => {
                let response = json!({ "jsonrpc": "2.0", "result": { "ok": true }, "id": id });
                (response, events, commands)
            }
            Err(err) => (err.to_json(&id), vec![], vec![]),
        },
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
                "channel.create" => handlers::handle_channel_create(params, channels_dir),
                "channel.archive" => handlers::handle_channel_archive(params, channels_dir),
                "channel.unarchive" => handlers::handle_channel_unarchive(params, channels_dir),
                // Stubs for CLI methods that don't have full v2 implementations yet
                "reminder.list"
                | "reminder.create"
                | "reminder.cancel"
                | "workflow.set_state"
                | "workflow.list"
                | "session.detach"
                | "task.update"
                | "pr.review"
                | "pr.merge"
                | "pr.list-external"
                | "pr.allow"
                | "daemon.check-pending" => Ok(json!({"ok": true, "stub": true})),
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
