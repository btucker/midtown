pub mod handlers;

use handlers::{AgentFilter, RpcError};
use serde_json::{Value, json};

use crate::daemon_v2::Projections;

#[path = "rpc_tests.rs"]
#[cfg(test)]
mod tests;

pub fn dispatch_request(request: Value, proj: &Projections) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = match request.get("method").and_then(|m| m.as_str()) {
        Some(m) => m,
        None => {
            return RpcError {
                code: -32600,
                message: "Missing method".into(),
            }
            .to_json(&id);
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

    match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "result": value, "id": id }),
        Err(err) => err.to_json(&id),
    }
}
