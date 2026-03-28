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
        "channel.post" => match handlers::handle_channel_post(params, channels_dir) {
            Ok((value, events)) => {
                let response = json!({ "jsonrpc": "2.0", "result": value, "id": id });
                (response, events, vec![])
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
        _ => {
            // Read-only methods — no events produced.
            let result = match method {
                "status" => handlers::handle_status(proj),
                "agent.list" => {
                    let filter = AgentFilter::from_params(params);
                    handlers::handle_agent_list(proj, filter)
                }
                "channel.list" => handlers::handle_channel_list(channels_dir),
                "channel.read" => handlers::handle_channel_read(params, channels_dir),
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
