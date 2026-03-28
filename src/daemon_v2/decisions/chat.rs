use crate::daemon_v2::decisions::Command;
use crate::daemon_v2::events::AgentKind;
use crate::daemon_v2::projections::Projections;

#[path = "chat_tests.rs"]
#[cfg(test)]
mod tests;

/// Extract @mentions from message content and return NudgeAgent commands.
pub fn route_mentions(
    proj: &Projections,
    channel: &str,
    sender: &str,
    content: &str,
) -> Vec<Command> {
    let mut commands = Vec::new();

    // Find @mentions (words starting with @)
    for word in content.split_whitespace() {
        if !word.starts_with('@') {
            continue;
        }
        let target = word
            .trim_start_matches('@')
            .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        if target.is_empty() || target == sender {
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
