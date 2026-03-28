use std::path::Path;

use serde_json::{Value, json};

use crate::daemon_v2::Projections;
use crate::daemon_v2::events::{AgentKind, DomainEvent};
use crate::daemon_v2::executor::channel_io;

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

/// Handle `task.create` — creates a new task from RPC params.
///
/// Required fields: `id`, `subject`, `channel`.
/// Optional fields: `blocked_by` (array of task IDs).
pub fn handle_task_create(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError {
        code: -32602,
        message: "Missing params".into(),
    })?;

    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError {
            code: -32602,
            message: "Missing required field: id".into(),
        })?
        .to_string();

    let subject = params
        .get("subject")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError {
            code: -32602,
            message: "Missing required field: subject".into(),
        })?
        .to_string();

    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError {
            code: -32602,
            message: "Missing required field: channel".into(),
        })?
        .to_string();

    let blocked_by = params
        .get("blocked_by")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    Ok(vec![DomainEvent::TaskCreated {
        id,
        subject,
        channel,
        blocked_by,
    }])
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

// ── Channel RPC handlers ─────────────────────────────────────────────────

/// List all channels.
pub fn handle_channel_list(channels_dir: &Path) -> Result<Value, RpcError> {
    let channels = channel_io::list_channels(channels_dir).map_err(|e| RpcError {
        code: -32000,
        message: e,
    })?;
    let list: Vec<Value> = channels
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "is_archived": c.is_archived,
                "is_dm": c.is_dm,
            })
        })
        .collect();
    Ok(json!(list))
}

/// Post a message to a channel. Returns events for the daemon to apply.
pub fn handle_channel_post(
    params: Option<&Value>,
    channels_dir: &Path,
) -> Result<(Value, Vec<DomainEvent>), RpcError> {
    let params = params.ok_or_else(|| RpcError {
        code: -32602,
        message: "Missing params".into(),
    })?;

    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError {
            code: -32602,
            message: "Missing required field: channel".into(),
        })?;

    let sender = params
        .get("sender")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError {
            code: -32602,
            message: "Missing required field: sender".into(),
        })?;

    let content = params
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError {
            code: -32602,
            message: "Missing required field: content".into(),
        })?;

    let thread_id = params.get("thread_id").and_then(|v| v.as_str());

    channel_io::post_message(channels_dir, channel, sender, content, thread_id).map_err(|e| {
        RpcError {
            code: -32000,
            message: e,
        }
    })?;

    let msg_id = uuid::Uuid::new_v4().to_string();
    let events = vec![DomainEvent::MessagePosted {
        id: msg_id.clone(),
        channel: channel.to_string(),
        sender: sender.to_string(),
        content: content.to_string(),
        thread_id: thread_id.map(String::from),
    }];

    Ok((json!({ "ok": true, "id": msg_id }), events))
}

/// Read messages from a channel.
pub fn handle_channel_read(params: Option<&Value>, channels_dir: &Path) -> Result<Value, RpcError> {
    let params = params.ok_or_else(|| RpcError {
        code: -32602,
        message: "Missing params".into(),
    })?;

    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError {
            code: -32602,
            message: "Missing required field: channel".into(),
        })?;

    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let messages =
        channel_io::read_messages(channels_dir, channel, limit).map_err(|e| RpcError {
            code: -32000,
            message: e,
        })?;

    Ok(json!(messages))
}
