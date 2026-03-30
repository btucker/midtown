use std::collections::HashSet;

use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::{AgentKind, TaskStatus};
use crate::daemon_v2::projections::Projections;

#[path = "chat_tests.rs"]
#[cfg(test)]
mod tests;

#[path = "chat_spec_tests.rs"]
#[cfg(test)]
mod spec_tests;

/// Route a channel message to all agents that should be nudged.
///
/// Three binding-based rules, applied in order with deduplication:
/// 1. **Thread-bound agent**: if the message is a thread reply and an agent is bound
///    to that thread, nudge it. Otherwise fall through to the channel lead.
/// 2. **Channel lead**: nudged on every message (top-level or thread fallback).
/// 3. **Explicit references**: @mentions and !N task references nudge the named/assigned agent.
///
/// No agent-type checks — routing is determined by bindings (thread, channel, name, task).
/// No running-state checks — the executor is responsible for resuming stopped agents.
/// Self-nudges (sender == agent name) are suppressed.
pub fn route_message(
    proj: &Projections,
    channel: &str,
    sender: &str,
    content: &str,
    thread_id: Option<&str>,
) -> Vec<Command> {
    let mut commands = Vec::new();
    let mut nudged = HashSet::new();

    // 1. Thread-bound agent or channel lead
    if let Some(tid) = thread_id {
        if let Some(agent) = proj
            .agents
            .by_thread
            .get(tid)
            .and_then(|id| proj.agents.by_id.get(id))
        {
            nudge(
                agent,
                sender,
                &format!("Thread reply from {sender}: {content}"),
                &mut nudged,
                &mut commands,
            );
        } else {
            nudge_channel_lead(
                proj,
                channel,
                sender,
                &format!("Thread reply from {sender} in #{channel}: {content}"),
                &mut nudged,
                &mut commands,
            );
        }
    } else {
        nudge_channel_lead(
            proj,
            channel,
            sender,
            &format!("Message from {sender} in #{channel}: {content}"),
            &mut nudged,
            &mut commands,
        );
    }

    // 2. @mentions and !N task references
    route_refs(proj, channel, sender, content, &mut nudged, &mut commands);

    commands
}

/// Parse @mentions and !N task references, emitting NudgeAgent commands.
fn route_refs(
    proj: &Projections,
    channel: &str,
    sender: &str,
    content: &str,
    nudged: &mut HashSet<String>,
    commands: &mut Vec<Command>,
) {
    // !N task references → nudge the assigned agent
    for task_id in extract_task_refs(content) {
        if let Some(agent_id) = proj.agents.by_task.get(&task_id)
            && let Some(agent) = proj.agents.by_id.get(agent_id)
        {
            nudge(
                agent,
                sender,
                &format!("!{task_id} reference from {sender}: {content}"),
                nudged,
                commands,
            );
        }
    }

    // @mentions
    for word in content.split_whitespace() {
        if !word.starts_with('@') {
            continue;
        }
        let target = word
            .trim_start_matches('@')
            .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        if target.is_empty() || target == sender {
            continue;
        }

        if target == "all" {
            // Determine if this is the main channel (has a project-lead)
            let is_main = proj
                .agents
                .by_channel
                .get(channel)
                .map(|ids| {
                    ids.iter().any(|id| {
                        proj.agents
                            .by_id
                            .get(id)
                            .is_some_and(|a| a.agent_type == "midtown-project-lead")
                    })
                })
                .unwrap_or(false);

            if is_main {
                // Main channel @all: nudge ALL leads + ALL in-progress task agents
                // across ALL channels
                for agent in proj.agents.by_id.values() {
                    let is_lead = agent.kind == AgentKind::Lead;
                    let has_in_progress_task = agent.task_id.as_ref().is_some_and(|tid| {
                        proj.work
                            .tasks
                            .get(tid)
                            .is_some_and(|t| t.status == TaskStatus::InProgress)
                    });
                    if is_lead || has_in_progress_task {
                        nudge(
                            agent,
                            sender,
                            &format!("@all from {sender}: {content}"),
                            nudged,
                            commands,
                        );
                    }
                }
            } else {
                // Topic channel @all: nudge channel lead + in-progress task agents
                // in THIS channel only
                if let Some(agents) = proj.agents.by_channel.get(channel) {
                    for agent_id in agents {
                        if let Some(agent) = proj.agents.by_id.get(agent_id) {
                            let is_lead = agent.kind == AgentKind::Lead;
                            let has_in_progress_task = agent.task_id.as_ref().is_some_and(|tid| {
                                proj.work
                                    .tasks
                                    .get(tid)
                                    .is_some_and(|t| t.status == TaskStatus::InProgress)
                            });
                            if is_lead || has_in_progress_task {
                                nudge(
                                    agent,
                                    sender,
                                    &format!("@all from {sender}: {content}"),
                                    nudged,
                                    commands,
                                );
                            }
                        }
                    }
                }
            }
        } else if target == "lead" || target == channel {
            nudge_channel_lead(
                proj,
                channel,
                sender,
                &format!("@{target} mention from {sender}: {content}"),
                nudged,
                commands,
            );
        } else if let Some(agent_id) = proj.agents.by_name.get(target)
            && let Some(agent) = proj.agents.by_id.get(agent_id)
        {
            nudge(
                agent,
                sender,
                &format!("@{target} mention from {sender}: {content}"),
                nudged,
                commands,
            );
        }
    }
}

/// Nudge a single agent, suppressing self-nudges and duplicates.
fn nudge(
    agent: &crate::daemon_v2::projections::agents::Agent,
    sender: &str,
    message: &str,
    nudged: &mut HashSet<String>,
    commands: &mut Vec<Command>,
) {
    if agent.name == sender {
        return;
    }
    if !nudged.insert(agent.id.clone()) {
        return;
    }
    commands.push(Command::NudgeAgent {
        id: agent.id.clone(),
        message: message.to_string(),
    });
}

/// Find the channel lead and nudge it.
fn nudge_channel_lead(
    proj: &Projections,
    channel: &str,
    sender: &str,
    message: &str,
    nudged: &mut HashSet<String>,
    commands: &mut Vec<Command>,
) {
    if let Some(agent) = proj.agents.channel_lead(channel) {
        nudge(agent, sender, message, nudged, commands);
    }
}

/// Extract task references like `!42` or `task !7` from content.
fn extract_task_refs(content: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let content_lower = content.to_lowercase();

    // "task !N" pattern
    if let Some(idx) = content_lower.find("task !") {
        let after = &content[idx + 6..];
        let num: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            refs.push(num);
            return refs;
        }
    }

    // Standalone "!N" — after whitespace or at start
    for word in content.split_whitespace() {
        if let Some(after) = word.strip_prefix('!') {
            let num: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if !num.is_empty() {
                refs.push(num);
            }
        }
    }
    refs
}
