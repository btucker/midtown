use std::path::Path;

use serde_json::{Value, json};

use crate::daemon_v2::Projections;
use crate::daemon_v2::decisions::{Command, SpawnConfig, chat};
use crate::daemon_v2::events::{AgentKind, DomainEvent, Provider};
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
                "session_id": agent.session_id,
                "pid": agent.pid,
                "running": proj.agents.running.contains(&agent.id),
                "icon": agent.icon,
                "color": agent.color,
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

    let agent_type = params
        .get("agent_type")
        .and_then(|v| v.as_str())
        .map(String::from);

    let icon = params
        .get("icon")
        .and_then(|v| v.as_str())
        .map(String::from);

    Ok(vec![DomainEvent::TaskCreated {
        id,
        subject,
        channel,
        blocked_by,
        agent_type,
        icon,
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

    pub fn invalid_params(msg: &str) -> Self {
        Self {
            code: -32602,
            message: msg.into(),
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

// ── Session RPC handlers ─────────────────────────────────────────────────

/// Handle `session.fork` — spawn a fork session bound to a thread.
///
/// Required fields: `thread_parent_id`, `channel`.
/// Optional fields: `name`, `message` (initial prompt).
///
/// If a running fork already exists for the given thread, returns its ID
/// without spawning a new one.
pub fn handle_session_fork(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<(Value, Vec<Command>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let thread_parent_id = params
        .get("thread_parent_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing thread_parent_id"))?;
    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing channel"))?;

    // Check if a running fork already exists for this thread
    if let Some(existing) = proj.agents.fork_for_thread(thread_parent_id)
        && proj.agents.running.contains(&existing.id)
    {
        return Ok((
            json!({"ok": true, "fork_id": existing.id, "existing": true}),
            vec![],
        ));
    }

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            format!(
                "fork-{}",
                &thread_parent_id[..8.min(thread_parent_id.len())]
            )
        });

    let initial_message = params
        .get("message")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let agent_type = params
        .get("agent_type")
        .and_then(|v| v.as_str())
        .unwrap_or("midtown-channel-lead")
        .to_string();

    let icon = params
        .get("icon")
        .and_then(|v| v.as_str())
        .map(String::from);

    // Find the running lead for this channel to fork its session context.
    let fork_from_session = proj
        .agents
        .by_channel
        .get(channel)
        .and_then(|ids| {
            ids.iter().find(|id| {
                proj.agents.running.contains(*id)
                    && proj
                        .agents
                        .by_id
                        .get(*id)
                        .is_some_and(|a| a.kind == AgentKind::Lead)
            })
        })
        .and_then(|id| proj.agents.by_id.get(id))
        .and_then(|agent| agent.session_id.clone());

    let has_parent_context = fork_from_session.is_some();

    let command = Command::SpawnAgent(SpawnConfig {
        name,
        kind: AgentKind::Fork,
        agent_type,
        provider: Provider::ClaudeCode,
        channel: Some(channel.to_string()),
        task_id: None,
        initial_prompt: initial_message,
        working_dir: None,
        model: None,
        bound_thread_id: Some(thread_parent_id.to_string()),
        fork_from_session,
        icon,
        color: None,
    });

    Ok((
        json!({
            "ok": true,
            "forking": true,
            "fork_from_session": has_parent_context,
        }),
        vec![command],
    ))
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

/// Post a message to a channel. Returns events and mention-routing commands.
pub fn handle_channel_post(
    params: Option<&Value>,
    channels_dir: &Path,
    proj: &Projections,
) -> Result<(Value, Vec<DomainEvent>, Vec<Command>), RpcError> {
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

    let mention_commands = chat::route_mentions(proj, channel, sender, content);

    Ok((
        json!({ "ok": true, "id": msg_id }),
        events,
        mention_commands,
    ))
}

/// Update channel settings. Returns events for the daemon to apply.
pub fn handle_channel_update(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing channel"))?;

    let mut events = Vec::new();

    if let Some(lead_driven) = params.get("lead_driven").and_then(|v| v.as_bool()) {
        events.push(DomainEvent::ChannelLeadDrivenSet {
            channel: channel.to_string(),
            lead_driven,
        });
    }

    // Handle directory setting — subdirectory for AGENTS.md/CLAUDE.md loading
    if params.get("directory").is_some() {
        let directory = params
            .get("directory")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty());
        events.push(DomainEvent::ChannelDirectorySet {
            channel: channel.to_string(),
            directory,
        });
    }

    Ok(events)
}

// ── v1 compatibility handlers ───────────────────────────────────────────

/// Handle `task.done` (v1 alias) — marks a task as completed.
///
/// Required fields: `id` (task ID string).
pub fn handle_task_done(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            // Also accept numeric id (v1 sends {"id": 42})
            params.get("id").and_then(|v| v.as_u64()).and(None)
        })
        .map(String::from)
        .or_else(|| {
            params
                .get("id")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
        })
        .ok_or_else(|| RpcError::invalid_params("missing required field: id"))?;

    Ok(vec![DomainEvent::TaskCompleted { task_id: id }])
}

/// Handle `prs.status` (v1 alias) — returns open PR info from WorkIndex.
pub fn handle_prs_status(proj: &Projections) -> Result<Value, RpcError> {
    let prs: Vec<Value> = proj
        .work
        .prs
        .values()
        .map(|pr| {
            json!({
                "number": pr.number,
                "branch": pr.branch,
                "author": pr.author,
                "needs_review": proj.work.needing_review.contains(&pr.number),
            })
        })
        .collect();
    Ok(json!({ "prs": prs }))
}

/// Handle `coworker.spawn` (v1 alias) — spawns a worker agent.
///
/// Accepts v1 params: `prompt`, `agent` (agent_type), `channel`, `task_id`.
pub fn handle_coworker_spawn(
    params: Option<&Value>,
    _proj: &Projections,
) -> Result<(Value, Vec<Command>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("worker-{}", &uuid::Uuid::new_v4().to_string()[..8]));

    let agent_type = params
        .get("agent")
        .or_else(|| params.get("agent_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("midtown-code-author")
        .to_string();

    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .map(String::from);
    let task_id = params
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let prompt = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .map(String::from);
    let icon = params
        .get("icon")
        .and_then(|v| v.as_str())
        .map(String::from);

    let command = Command::SpawnAgent(SpawnConfig {
        name: name.clone(),
        kind: AgentKind::Worker,
        agent_type,
        provider: Provider::ClaudeCode,
        channel,
        task_id,
        initial_prompt: prompt,
        working_dir: None,
        model: None,
        bound_thread_id: None,
        fork_from_session: None,
        icon,
        color: None,
    });

    Ok((json!({"ok": true, "name": name}), vec![command]))
}

/// Handle `coworker.break` (v1 alias) — stop an agent by name.
pub fn handle_agent_stop(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<Vec<Command>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: name"))?;

    let agent_id = proj
        .agents
        .by_name
        .get(name)
        .ok_or_else(|| RpcError {
            code: -32001,
            message: format!("Agent '{}' not found", name),
        })?
        .clone();

    Ok(vec![Command::StopAgent {
        id: agent_id,
        reason: "stopped via coworker.break RPC".into(),
    }])
}

/// Handle `coworker.nudge` (v1 alias) — nudge an agent by name.
pub fn handle_agent_nudge(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<Vec<Command>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: name"))?;

    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("nudge")
        .to_string();

    let agent_id = proj
        .agents
        .by_name
        .get(name)
        .ok_or_else(|| RpcError {
            code: -32001,
            message: format!("Agent '{}' not found", name),
        })?
        .clone();

    Ok(vec![Command::NudgeAgent {
        id: agent_id,
        message,
    }])
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
