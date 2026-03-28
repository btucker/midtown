use serde_json::{Value, json};

use crate::daemon_v2::Projections;
use crate::daemon_v2::events::AgentKind;

#[derive(Debug, Clone)]
pub struct AgentFilter {
    pub kind: Option<AgentKind>,
    pub running_only: bool,
}

impl AgentFilter {
    pub fn from_params(params: Option<&Value>) -> Option<Self> {
        let params = params?;
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .and_then(|s| match s {
                "lead" => Some(AgentKind::Lead),
                "fork" => Some(AgentKind::Fork),
                "worker" => Some(AgentKind::Worker),
                _ => None,
            });
        let running_only = params
            .get("running_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Some(Self { kind, running_only })
    }
}

pub fn handle_status(proj: &Projections) -> Result<Value, RpcError> {
    let total = proj.agents.by_id.len();
    let running = proj.agents.running.len();
    let pending_tasks = proj.work.pending_tasks.len();
    let in_progress_tasks = proj.work.in_progress_tasks.len();
    let open_prs = proj.work.open_prs.len();

    Ok(json!({
        "agents": {
            "total": total,
            "running": running,
        },
        "tasks": {
            "pending": pending_tasks,
            "in_progress": in_progress_tasks,
        },
        "prs": {
            "open": open_prs,
        },
    }))
}

pub fn handle_agent_list(
    proj: &Projections,
    filter: Option<AgentFilter>,
) -> Result<Value, RpcError> {
    let agents: Vec<Value> = proj
        .agents
        .by_id
        .values()
        .filter(|agent| {
            if let Some(ref f) = filter {
                if let Some(ref kind) = f.kind
                    && &agent.kind != kind
                {
                    return false;
                }
                if f.running_only && !proj.agents.running.contains(&agent.id) {
                    return false;
                }
            }
            true
        })
        .map(|agent| {
            json!({
                "id": agent.id,
                "name": agent.name,
                "kind": agent.kind,
                "agent_type": agent.agent_type,
                "provider": agent.provider,
                "channel": agent.channel,
                "task_id": agent.task_id,
                "pid": agent.pid,
                "running": proj.agents.running.contains(&agent.id),
            })
        })
        .collect();

    Ok(json!(agents))
}

#[derive(Debug)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn method_not_found() -> Self {
        Self {
            code: -32601,
            message: "Method not found".into(),
        }
    }

    pub fn to_json(&self, id: &Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "error": { "code": self.code, "message": self.message },
            "id": id,
        })
    }
}
