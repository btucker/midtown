pub mod handlers;

use handlers::{AgentFilter, RpcError};
use serde_json::{Value, json};

use crate::daemon_v2::Projections;
use crate::daemon_v2::events::DomainEvent;

#[path = "rpc_tests.rs"]
#[cfg(test)]
mod tests;

/// Dispatch a JSON-RPC request, returning the response JSON and any domain
/// events produced by mutating methods.
pub fn dispatch_request(request: Value, proj: &Projections) -> (Value, Vec<DomainEvent>) {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = match request.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            let err = RpcError {
                code: -32600,
                message: "Missing method".into(),
            };
            return (err.to_json(&id), vec![]);
        }
    };
    let params = request.get("params");

    let result = match method {
        "status" => handlers::handle_status(proj),
        "agent.list" => {
            let filter = AgentFilter::from_params(params);
            handlers::handle_agent_list(proj, filter)
        }
        _ => Err(RpcError::method_not_found()),
    };

    let response = match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "result": value, "id": id }),
        Err(err) => err.to_json(&id),
    };
    (response, vec![])
}
