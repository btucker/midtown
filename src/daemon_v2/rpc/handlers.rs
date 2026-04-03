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
    // Build coworkers array (running agents) for web UI compatibility
    let coworkers: Vec<Value> = proj
        .agents
        .by_id
        .values()
        .filter(|a| proj.agents.running.contains(&a.id))
        .map(|a| {
            let task_name = a
                .task_id
                .as_deref()
                .and_then(|tid| proj.work.tasks.get(tid))
                .map(|t| format!("!{} {}", t.id, t.subject));
            let channel = a.channel.as_deref().and_then(|ch| {
                // Map DM channels back to the task's channel for display
                if ch.starts_with("dm-") {
                    a.task_id
                        .as_deref()
                        .and_then(|tid| proj.work.tasks.get(tid))
                        .map(|t| t.channel.as_str())
                } else {
                    Some(ch)
                }
            });
            json!({
                "name": a.name,
                "status": "running",
                "coworker_type": format!("{:?}", a.kind).to_lowercase(),
                "current_task": task_name,
                "task_id": a.task_id,
                "channel": channel,
                "color": a.color,
                "icon": a.icon,
            })
        })
        .collect();

    // Build tasks array for kanban board
    // Build agent_id → agent_name map for task owner resolution
    let task_owner: std::collections::HashMap<&str, &str> = proj
        .agents
        .by_id
        .values()
        .filter_map(|a| a.task_id.as_deref().map(|tid| (tid, a.name.as_str())))
        .collect();

    let tasks: Vec<Value> = proj
        .work
        .tasks
        .values()
        .map(|t| {
            let owner = task_owner.get(t.id.as_str()).copied();
            let updated_at = t.completed_at.unwrap_or(t.created_at).to_rfc3339();
            json!({
                "id": t.id,
                "subject": t.subject,
                "status": match t.status {
                    crate::daemon_v2::events::TaskStatus::Pending => "pending",
                    crate::daemon_v2::events::TaskStatus::InProgress => "in_progress",
                    crate::daemon_v2::events::TaskStatus::Completed => "completed",
                },
                "channel": t.channel,
                "owner": owner,
                "thread_id": t.thread_id,
                "message_id": t.message_id,
                "pr_number": t.pr_number,
                "agent_type": t.agent_type,
                "color": t.color,
                "icon": t.icon,
                "updated_at": updated_at,
            })
        })
        .collect();

    // Build pull_requests array
    let pull_requests: Vec<Value> = proj
        .work
        .prs
        .values()
        .filter(|pr| !pr.is_merged && !pr.is_closed)
        .map(|pr| {
            json!({
                "number": pr.number,
                "title": pr.branch,
                "author": pr.github_author,
                "status": if pr.review_state == crate::daemon_v2::events::ReviewState::Approved { "approved" } else { "open" },
                "ci_status": format!("{:?}", pr.ci_status).to_lowercase(),
                "needs_review": pr.needs_review,
            })
        })
        .collect();

    Ok(json!({
        "agents": {
            "total": proj.agents.by_id.values().filter(|a| !a.gc).count(),
            "running": proj.agents.running.len(),
        },
        "coworkers": coworkers,
        "tasks": tasks,
        "pull_requests": pull_requests,
        "max_in_progress_tasks": crate::config::max_in_progress_from_config(),
        "prs": {
            "open": proj.work.open_prs.len(),
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
        // Spec 4.4: GC'd agents excluded from active queries
        .filter(|agent| !agent.gc)
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
pub fn handle_task_create(
    params: Option<&Value>,
    proj: &crate::daemon_v2::Projections,
    channels_dir: &Path,
) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError {
        code: -32602,
        message: "Missing params".into(),
    })?;

    // Accept client-provided ID (string or numeric) or generate server-side.
    // V1 CLI doesn't send IDs — they were generated server-side via auto-increment.
    let id = params
        .get("id")
        .and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .unwrap_or_else(|| {
            let max_id = proj
                .work
                .tasks
                .keys()
                .filter_map(|k| k.parse::<u64>().ok())
                .max()
                .unwrap_or(0);
            (max_id + 1).to_string()
        });

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

    let agent_name = params
        .get("agent_name")
        .and_then(|v| v.as_str())
        .map(String::from);

    let icon = params
        .get("icon")
        .and_then(|v| v.as_str())
        .map(String::from);

    let color = params
        .get("color")
        .and_then(|v| v.as_str())
        .map(String::from);

    let parent = params
        .get("parent")
        .and_then(|v| v.as_str())
        .map(String::from);

    let explicit_thread_id = params
        .get("thread_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let explicit_message_id = params
        .get("message_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    // If no thread_id provided, post an announcement and use it as thread anchor
    let (thread_id, message_id) = if let Some(tid) = explicit_thread_id {
        (Some(tid), explicit_message_id)
    } else {
        let announcement = format!("📋 Task created: **{}**", subject);
        match channel_io::post_message(channels_dir, &channel, "midtown", &announcement, None) {
            Ok(msg_id) => (Some(msg_id.clone()), Some(msg_id)),
            Err(_) => (None, None),
        }
    };

    Ok(vec![DomainEvent::TaskCreated {
        id,
        subject,
        channel,
        blocked_by,
        agent_type,
        agent_name,
        icon,
        color,
        parent,
        thread_id,
        message_id,
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
    // Channel can be explicit or resolved from the calling session's agent
    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            let session_id = params.get("calling_session_id")?.as_str()?;
            proj.agents
                .by_id
                .values()
                .find(|a| a.session_id.as_deref() == Some(session_id))
                .and_then(|a| a.channel.clone())
        })
        .ok_or_else(|| {
            RpcError::invalid_params("missing channel (and no calling_session_id to resolve it)")
        })?;
    let channel = channel.as_str();

    // Check if a fork already exists for this thread (running or stopped).
    // Running forks are returned as-is; stopped forks are resumed.
    if let Some(existing) = proj.agents.fork_for_thread(thread_parent_id) {
        if proj.agents.running.contains(&existing.id) {
            return Ok((
                json!({"ok": true, "fork_id": existing.id, "existing": true}),
                vec![],
            ));
        }
        // Stopped fork — resume it
        return Ok((
            json!({"ok": true, "fork_id": existing.id, "existing": true}),
            vec![Command::ResumeAgent {
                id: existing.id.clone(),
            }],
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

    let raw_message = params
        .get("message")
        .or_else(|| params.get("initial_message"))
        .and_then(|v| v.as_str());
    // Wrap the initial message with fork context so the agent knows its role
    let initial_message = Some(format!(
        "You are a thread fork — your output is automatically posted to thread {thread_parent_id} \
         in #{channel}. Focus on the task below and post results in this thread. \
         Do not post to the main channel.\n\n{}",
        raw_message.unwrap_or("Investigate this thread and report your findings.")
    ));

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
        .unwrap_or("midtown");

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

    let msg_id = channel_io::post_message(channels_dir, channel, sender, content, thread_id)
        .map_err(|e| RpcError {
            code: -32000,
            message: e,
        })?;
    let events = vec![DomainEvent::MessagePosted {
        id: msg_id.clone(),
        channel: channel.to_string(),
        sender: sender.to_string(),
        content: content.to_string(),
        thread_id: thread_id.map(String::from),
        tool_data: None,
        auto_output: false,
    }];

    let mention_commands =
        chat::route_message(proj, channel, sender, content, thread_id, Some(&msg_id));

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

/// Handle `coworker.report-state` — records an agent's self-reported state.
/// Spec 2.2: agents call this via `midtown state` to report idle/working status.
pub fn handle_report_state(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: name"))?;

    let state = params
        .get("state")
        .or_else(|| params.get("phase"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: state"))?;

    let agent_id = proj
        .agents
        .by_name
        .get(name)
        .ok_or_else(|| RpcError {
            code: -32001,
            message: format!("agent not found: {name}"),
        })?
        .clone();

    Ok(vec![DomainEvent::AgentStateReported {
        id: agent_id,
        state: state.to_string(),
    }])
}

/// Handle `task.done` — marks a task as completed.
///
/// Required fields: `id` (task ID string).
pub fn handle_task_done(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            // Accept numeric id (e.g. {"id": 42} → "42")
            params
                .get("id")
                .and_then(|v| v.as_u64())
                .map(|n| n.to_string())
        })
        .ok_or_else(|| RpcError::invalid_params("missing required field: id"))?;

    Ok(vec![DomainEvent::TaskCompleted { task_id: id }])
}

/// Handle `task.list` — returns all tasks from WorkIndex.
pub fn handle_task_list(proj: &Projections) -> Result<Value, RpcError> {
    let tasks: Vec<Value> = proj
        .work
        .tasks
        .values()
        .map(|t| {
            json!({
                "id": t.id,
                "subject": t.subject,
                "channel": t.channel,
                "status": t.status,
                "pr_number": t.pr_number,
                "agent_type": t.agent_type,
                "icon": t.icon,
                "created_at": t.created_at.to_rfc3339(),
                "completed_at": t.completed_at.map(|d| d.to_rfc3339()),
            })
        })
        .collect();
    Ok(json!(tasks))
}

/// Handle `task.update` — verify the task exists, nudge the assigned agent
/// if one exists, and return events + commands.
///
/// Required fields: `id`.
pub fn handle_task_update(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<(Vec<DomainEvent>, Vec<Command>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let id = params
        .get("id")
        .and_then(|v| {
            v.as_str()
                .map(String::from)
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .ok_or_else(|| RpcError::invalid_params("missing id"))?;

    if !proj.work.tasks.contains_key(&id) {
        return Err(RpcError {
            code: -32000,
            message: format!("task {id} not found"),
        });
    }

    // Auto-nudge the assigned agent so they see the update.
    let commands: Vec<Command> = proj
        .agents
        .by_task
        .get(&id)
        .map(|agent_id| Command::NudgeAgent {
            id: agent_id.clone(),
            message: format!(
                "Your task !{id} was updated — run `midtown task view {id}` to see the changes"
            ),
        })
        .into_iter()
        .collect();

    let thread_id = params
        .get("thread_id")
        .and_then(|v| v.as_str())
        .map(String::from);
    let message_id = params
        .get("message_id")
        .and_then(|v| v.as_str())
        .map(String::from);

    let events = if thread_id.is_some() || message_id.is_some() {
        vec![DomainEvent::TaskUpdated {
            task_id: id,
            thread_id,
            message_id,
        }]
    } else {
        vec![]
    };

    Ok((events, commands))
}

/// Handle `pr.list` — returns all PR data from WorkIndex.
pub fn handle_pr_list(proj: &Projections) -> Result<Value, RpcError> {
    let mut open = Vec::new();
    let mut merged = Vec::new();
    for pr in proj.work.prs.values() {
        let entry = json!({
            "number": pr.number,
            "branch": pr.branch,
            "author": pr.github_author,
            "ci_status": pr.ci_status,
            "review_state": pr.review_state,
            "is_merged": pr.is_merged,
            "is_closed": pr.is_closed,
            "needs_review": pr.needs_review,
        });
        if pr.is_merged {
            merged.push(entry);
        } else {
            open.push(entry);
        }
    }
    Ok(json!({ "prs": open, "merged_prs": merged }))
}

/// Handle `pr.action` — merge, comment, or rerun CI for a PR.
///
/// Required fields: `action`, `number`.
/// Optional fields: `body` (for comment action), `run_id` (for rerun action).
pub fn handle_pr_action(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<Vec<crate::daemon_v2::decisions::Command>, RpcError> {
    use crate::daemon_v2::decisions::Command;

    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let action = params
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing action"))?;
    let number = params
        .get("number")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::invalid_params("missing number"))?;

    if !proj.work.prs.contains_key(&number) {
        return Err(RpcError {
            code: -32000,
            message: format!("PR {number} not found"),
        });
    }

    match action {
        "merge" => Ok(vec![Command::MergePr { number }]),
        "comment" => {
            let body = params
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(vec![Command::PostPrComment { number, body }])
        }
        "rerun" => {
            let run_id = params
                .get("run_id")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| RpcError::invalid_params("missing run_id for rerun action"))?;
            Ok(vec![Command::RerunCi { run_id }])
        }
        other => Err(RpcError {
            code: -32602,
            message: format!("unknown action: {other}"),
        }),
    }
}

/// Handle `coworker.spawn` — spawns a worker agent.
pub fn handle_coworker_spawn(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<(Value, Vec<Command>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    // Spec 13: use adjective-noun naming when no name is provided
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            let existing: std::collections::HashSet<String> =
                proj.agents.by_name.keys().cloned().collect();
            crate::daemon_v2::naming::generate_name(&existing)
        });

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

    let provider = match params
        .get("provider")
        .and_then(|v| v.as_str())
        .unwrap_or("claude")
    {
        "codex" => Provider::Codex,
        _ => Provider::ClaudeCode,
    };

    let bound_thread_id = params
        .get("thread")
        .or_else(|| params.get("bound_thread_id"))
        .and_then(|v| v.as_str())
        .map(String::from);

    let command = Command::SpawnAgent(SpawnConfig {
        name: name.clone(),
        kind: AgentKind::Worker,
        agent_type,
        provider,
        channel,
        task_id,
        initial_prompt: prompt,
        working_dir: None,
        model: None,
        bound_thread_id,
        fork_from_session: None,
        icon,
        color: None,
    });

    Ok((json!({"ok": true, "name": name}), vec![command]))
}

/// Handle `coworker.break` / `session.detach` — stop an agent by name.
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

/// Handle `coworker.nudge` — nudge an agent by name.
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
        .or_else(|| params.get("last"))
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    let thread_parent_id = params
        .get("thread_parent_id")
        .or_else(|| params.get("thread"))
        .and_then(|v| v.as_str());

    let before = params.get("before").and_then(|v| v.as_str());

    let since = params
        .get("since")
        .and_then(|v| v.as_str())
        .and_then(parse_duration_secs);

    let mut messages = if let Some(tid) = thread_parent_id {
        channel_io::read_thread_messages(channels_dir, channel, tid, limit)
    } else {
        channel_io::read_messages(channels_dir, channel, limit, before)
    }
    .map_err(|e| RpcError {
        code: -32000,
        message: e,
    })?;

    // Filter by --since duration if provided
    if let Some(secs) = since {
        let cutoff = chrono::Utc::now() - chrono::Duration::seconds(secs as i64);
        let cutoff_str = cutoff.to_rfc3339();
        messages.retain(|msg| {
            msg.get("timestamp")
                .and_then(|v| v.as_str())
                .is_some_and(|ts| ts >= cutoff_str.as_str())
        });
    }

    // Handle --message <id> with optional --context N
    let message_id = params.get("message").and_then(|v| v.as_str());
    let context_n = params
        .get("context")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    if let Some(msg_id) = message_id {
        // Read ALL messages (not just limited) to find the target and context
        let all = channel_io::read_messages(channels_dir, channel, None, None).unwrap_or_default();
        let n = context_n.unwrap_or(0);
        if let Some(pos) = all
            .iter()
            .position(|m| m.get("id").and_then(|v| v.as_str()) == Some(msg_id))
        {
            let start = pos.saturating_sub(n);
            let end = (pos + 1 + n).min(all.len());
            return Ok(json!(all[start..end]));
        }
        // Message not found — return empty array
        return Ok(json!([]));
    }

    Ok(json!(messages))
}

/// Handle `channel.create` — create a new channel directory.
pub fn handle_channel_create(
    params: Option<&Value>,
    channels_dir: &Path,
) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: name"))?;

    // Creating a channel = writing a system message to create the directory structure
    channel_io::post_system_message(channels_dir, name, &format!("Channel {name} created"))
        .map_err(|e| RpcError {
            code: -32000,
            message: e,
        })?;

    Ok(vec![DomainEvent::ChannelCreated {
        channel: name.to_string(),
    }])
}

/// Handle `channel.archive` — rename channel directory with .archived suffix.
pub fn handle_channel_archive(
    params: Option<&Value>,
    channels_dir: &Path,
) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let name = params
        .get("channel")
        .or_else(|| params.get("name"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: channel"))?;

    let ch_dir = channels_dir.join("channels").join(name);
    let archived_dir = channels_dir
        .join("channels")
        .join(format!("{name}.archived"));

    if ch_dir.exists() {
        std::fs::rename(&ch_dir, &archived_dir).map_err(|e| RpcError {
            code: -32000,
            message: format!("failed to archive: {e}"),
        })?;
    }

    Ok(vec![DomainEvent::ChannelArchived {
        channel: name.to_string(),
    }])
}

/// Handle `channel.unarchive` — remove .archived suffix from channel directory.
pub fn handle_channel_unarchive(
    params: Option<&Value>,
    channels_dir: &Path,
) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let name = params
        .get("channel")
        .or_else(|| params.get("name"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: channel"))?;

    let archived_dir = channels_dir
        .join("channels")
        .join(format!("{name}.archived"));
    let ch_dir = channels_dir.join("channels").join(name);

    if !archived_dir.exists() {
        return Err(RpcError {
            code: -32000,
            message: format!("channel '{name}' is not archived"),
        });
    }

    std::fs::rename(&archived_dir, &ch_dir).map_err(|e| RpcError {
        code: -32000,
        message: format!("failed to unarchive: {e}"),
    })?;

    Ok(vec![DomainEvent::ChannelUnarchived {
        channel: name.to_string(),
    }])
}

/// Handle `oneshot.execute` — spawn a one-off worker with a prompt.
/// Returns the agent name so the caller can track it.
pub fn handle_oneshot_execute(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<(Value, Vec<Command>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let prompt = params
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: prompt"))?;

    let agent_type = params
        .get("agent_type")
        .or_else(|| params.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or("midtown-code-author")
        .to_string();

    let model = params
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from);

    let existing_names: std::collections::HashSet<String> =
        proj.agents.by_name.keys().cloned().collect();
    let name = crate::daemon_v2::naming::generate_name(&existing_names);

    let command = Command::SpawnAgent(crate::daemon_v2::decisions::SpawnConfig {
        name: name.clone(),
        kind: crate::daemon_v2::events::AgentKind::Worker,
        agent_type,
        provider: crate::daemon_v2::events::Provider::ClaudeCode,
        channel: None, // oneshot agents get a DM channel
        task_id: None,
        initial_prompt: Some(prompt.to_string()),
        working_dir: None,
        model,
        bound_thread_id: None,
        fork_from_session: None,
        icon: None,
        color: None,
    });

    Ok((json!({"ok": true, "agent": name}), vec![command]))
}

/// Handle `channel.rename` — rename a channel directory and emit event
/// so projections (agent bindings, channel metadata) update in-memory.
pub fn handle_channel_rename(
    params: Option<&Value>,
    channels_dir: &Path,
) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let old_name = params
        .get("old")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: old"))?;
    let new_name = params
        .get("new")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: new"))?;

    let old_dir = channels_dir.join("channels").join(old_name);
    let new_dir = channels_dir.join("channels").join(new_name);

    if !old_dir.exists() {
        return Err(RpcError {
            code: -32000,
            message: format!("channel not found: {old_name}"),
        });
    }
    if new_dir.exists() {
        return Err(RpcError {
            code: -32000,
            message: format!("channel already exists: {new_name}"),
        });
    }

    std::fs::rename(&old_dir, &new_dir).map_err(|e| RpcError {
        code: -32000,
        message: format!("failed to rename: {e}"),
    })?;

    Ok(vec![DomainEvent::ChannelRenamed {
        old_name: old_name.to_string(),
        new_name: new_name.to_string(),
    }])
}

/// Handle `task.prompt` — send a message to the agent working on a task.
pub fn handle_task_prompt(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<Vec<Command>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let task_id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: id"))?;

    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: message"))?;

    let agent_id = proj.agents.by_task.get(task_id).ok_or_else(|| RpcError {
        code: -32000,
        message: format!("no agent assigned to task {task_id}"),
    })?;

    let from = params
        .get("from")
        .and_then(|v| v.as_str())
        .unwrap_or("user");

    Ok(vec![Command::NudgeAgent {
        id: agent_id.clone(),
        message: format!("[from {from}] {message}"),
    }])
}

/// Handle `task.request` — an agent requests new work by posting to its channel.
/// The message is formatted as a task request so the lead can review it.
pub fn handle_task_request(
    params: Option<&Value>,
    proj: &Projections,
    channels_dir: &Path,
) -> Result<(Value, Vec<DomainEvent>, Vec<Command>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: message"))?;

    let from = params
        .get("from")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    // Find the agent's channel — look up by name, fall back to main channel
    let channel = proj
        .agents
        .by_name
        .get(from)
        .and_then(|id| proj.agents.by_id.get(id))
        .and_then(|agent| agent.channel.as_deref())
        .unwrap_or("midtown");

    // Post the request as a formatted channel message
    let formatted = format!("[Task Request from {from}] {message}");

    let msg_id = crate::daemon_v2::executor::channel_io::post_message(
        channels_dir,
        channel,
        from,
        &formatted,
        None,
    )
    .map_err(|e| RpcError {
        code: -32000,
        message: format!("failed to post task request: {e}"),
    })?;

    // Route to the channel lead so it gets nudged
    let commands = crate::daemon_v2::decisions::chat::route_message(
        proj, channel, from, &formatted, None, None,
    );

    Ok((json!({"ok": true, "id": msg_id}), vec![], commands))
}

/// Handle `task.handoff` — reassign a task from one agent to another.
pub fn handle_task_handoff(
    params: Option<&Value>,
    proj: &Projections,
) -> Result<(Vec<DomainEvent>, Vec<Command>), RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let task_id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: id"))?;

    let task = proj.work.tasks.get(task_id).ok_or_else(|| RpcError {
        code: -32000,
        message: format!("task not found: {task_id}"),
    })?;

    let mut commands = Vec::new();

    if let Some(agent_id) = proj.agents.by_task.get(task_id) {
        commands.push(Command::StopAgent {
            id: agent_id.clone(),
            reason: "task handoff".into(),
        });
    }

    let agent_type = params
        .get("agent_type")
        .or_else(|| params.get("agent"))
        .and_then(|v| v.as_str())
        .unwrap_or(task.agent_type.as_deref().unwrap_or("midtown-code-author"))
        .to_string();

    let prompt = params
        .get("message")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| task.subject.clone());

    let existing_names: std::collections::HashSet<String> =
        proj.agents.by_name.keys().cloned().collect();
    let name = crate::daemon_v2::naming::generate_name(&existing_names);

    commands.push(Command::SpawnAgent(
        crate::daemon_v2::decisions::SpawnConfig {
            name,
            kind: crate::daemon_v2::events::AgentKind::Worker,
            agent_type,
            provider: crate::daemon_v2::events::Provider::ClaudeCode,
            channel: Some(task.channel.clone()),
            task_id: Some(task_id.to_string()),
            initial_prompt: Some(prompt),
            working_dir: None,
            model: params
                .get("model")
                .and_then(|v| v.as_str())
                .map(String::from),
            bound_thread_id: None,
            fork_from_session: None,
            icon: task.icon.clone(),
            color: task.color.clone(),
        },
    ));

    let events = vec![DomainEvent::TaskReset {
        task_id: task_id.to_string(),
        reason: "task handoff".into(),
    }];

    Ok((events, commands))
}

// ── Reminder handlers ───────────────────────────────────────────────────

/// Handle `reminder.create` — create a new reminder.
pub fn handle_reminder_create(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let trigger = params
        .get("trigger")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: trigger"))?;

    let message = params
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: message"))?;

    let cron_expr = params
        .get("cron_expr")
        .and_then(|v| v.as_str())
        .map(String::from);

    let id = format!("{:08x}", fastrand::u32(..));

    Ok(vec![DomainEvent::ReminderCreated {
        id,
        trigger: trigger.to_string(),
        message: message.to_string(),
        cron_expr,
    }])
}

/// Handle `reminder.list` — list all active reminders.
pub fn handle_reminder_list(proj: &Projections) -> Result<Value, RpcError> {
    let reminders: Vec<Value> = proj
        .reminders
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "trigger": r.trigger,
                "message": r.message,
                "cron_expr": r.cron_expr,
            })
        })
        .collect();
    Ok(json!(reminders))
}

/// Handle `reminder.cancel` — cancel a reminder by ID.
pub fn handle_reminder_cancel(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: id"))?;

    Ok(vec![DomainEvent::ReminderCancelled { id: id.to_string() }])
}

// ── Workflow handlers ───────────────────────────────────────────────────

/// Handle `workflow.set_state` — set a workflow state key on a channel.
pub fn handle_workflow_set_state(params: Option<&Value>) -> Result<Vec<DomainEvent>, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;
    let channel = params
        .get("channel")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: channel"))?;
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: key"))?;
    let state = params
        .get("state")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: state"))?;

    Ok(vec![DomainEvent::WorkflowStateSet {
        channel: channel.to_string(),
        key: key.to_string(),
        state: state.to_string(),
    }])
}

/// Handle `workflow.list` — list all workflow states.
pub fn handle_workflow_list(proj: &Projections) -> Result<Value, RpcError> {
    let states: Vec<Value> = proj
        .workflow_states
        .iter()
        .map(|w| json!({"channel": w.channel, "key": w.key, "state": w.state}))
        .collect();
    Ok(json!(states))
}

#[path = "handlers_tests.rs"]
#[cfg(test)]
mod tests;

/// Handle `pr.review-post` — post a review to a GitHub PR.
/// The reviewer writes the review body, and this command posts it via `gh pr review`.
pub fn handle_pr_review_post(params: Option<&Value>) -> Result<Value, RpcError> {
    let params = params.ok_or_else(|| RpcError::invalid_params("missing params"))?;

    let pr_number = params
        .get("pr")
        .or_else(|| params.get("number"))
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::invalid_params("missing required field: pr"))?;

    let body = params
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::invalid_params("missing required field: body"))?;

    // Add midtown frontmatter if not already present
    let review_body = if body.contains("<!-- midtown") {
        body.to_string()
    } else {
        let from = params
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("reviewer");
        format!("<!-- midtown from:{from} type:review -->\n\n{body}")
    };

    // Post the review via gh CLI
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "review",
            &pr_number.to_string(),
            "--comment",
            "--body",
            &review_body,
        ])
        .output()
        .map_err(|e| RpcError {
            code: -32000,
            message: format!("failed to run gh pr review: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(RpcError {
            code: -32000,
            message: format!("gh pr review failed: {stderr}"),
        });
    }

    Ok(json!({"ok": true, "pr": pr_number}))
}

/// Parse a human-friendly duration string (e.g., "5m", "1h", "30s", "2w") to seconds.
///
/// Returns `None` if the input is empty, has an unknown suffix, a non-numeric
/// prefix, or if the resulting value would overflow `u64`.
fn parse_duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, suffix) = s.split_at(s.len().saturating_sub(1));
    let n: u64 = num.parse().ok()?;
    match suffix {
        "s" => Some(n),
        "m" => Some(n * 60),
        "h" => Some(n * 3600),
        "d" => Some(n * 86400),
        "w" => Some(n * 604800),
        _ => None,
    }
}
