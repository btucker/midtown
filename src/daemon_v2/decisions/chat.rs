use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::AgentKind;
use crate::daemon_v2::projections::Projections;

#[path = "chat_tests.rs"]
#[cfg(test)]
mod tests;

/// Extract @mentions and !N task references from message content and return NudgeAgent commands.
pub fn route_mentions(
    proj: &Projections,
    channel: &str,
    sender: &str,
    content: &str,
) -> Vec<Command> {
    let mut commands = Vec::new();
    let mut nudged_ids = std::collections::HashSet::new();

    // Extract !N task references and route to assigned agent
    for task_id in extract_task_refs(content) {
        if let Some(agent_id) = proj.agents.by_task.get(&task_id)
            && proj.agents.running.contains(agent_id)
            && nudged_ids.insert(agent_id.clone())
        {
            commands.push(Command::NudgeAgent {
                id: agent_id.clone(),
                message: format!("!{task_id} reference from {sender}: {content}"),
            });
        }
    }

    // Find @mentions (words starting with @)
    for word in content.split_whitespace() {
        if !word.starts_with('@') {
            continue;
        }
        let target = word
            .trim_start_matches('@')
            .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        if target.is_empty() {
            continue;
        }

        // @all → nudge every running agent in this channel (except sender)
        if target == "all" {
            if let Some(channel_agents) = proj.agents.by_channel.get(channel) {
                for agent_id in channel_agents {
                    if !proj.agents.running.contains(agent_id) {
                        continue;
                    }
                    // Skip if agent's name matches sender
                    if let Some(agent) = proj.agents.by_id.get(agent_id)
                        && agent.name == sender
                    {
                        continue;
                    }
                    commands.push(Command::NudgeAgent {
                        id: agent_id.clone(),
                        message: format!("@all from {sender}: {content}"),
                    });
                }
            }
            continue;
        }

        if target == sender {
            continue; // Don't self-mention
        }

        // @lead or @<channel-name> → nudge the channel lead
        if target == "lead" || target == channel {
            if let Some(lead_id) = find_running_lead(proj, channel) {
                commands.push(Command::NudgeAgent {
                    id: lead_id,
                    message: format!("@{target} mention from {sender}: {content}"),
                });
            }
            continue;
        }

        // @<agent-name> → nudge by name
        if let Some(agent_id) = proj.agents.by_name.get(target)
            && proj.agents.running.contains(agent_id)
        {
            commands.push(Command::NudgeAgent {
                id: agent_id.clone(),
                message: format!("@{target} mention from {sender}: {content}"),
            });
        }
    }

    commands
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

fn find_running_lead(proj: &Projections, channel: &str) -> Option<String> {
    proj.agents
        .by_channel
        .get(channel)?
        .iter()
        .find(|id| {
            proj.agents.running.contains(*id)
                && proj
                    .agents
                    .by_id
                    .get(*id)
                    .is_some_and(|a| a.kind == AgentKind::Lead)
        })
        .cloned()
}
