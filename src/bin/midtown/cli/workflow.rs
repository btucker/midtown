use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum WorkflowCommand {
    /// List available workflows and current channel assignments
    List,
    /// Assign a workflow to a channel
    Assign {
        /// Channel name
        channel: String,
        /// Workflow name to assign
        workflow: String,
    },
    /// Remove workflow assignment from a channel
    Unassign {
        /// Channel name
        channel: String,
    },
    /// Exclude a task from its channel's workflow
    Exclude {
        /// Task ID to exclude
        task_id: String,
    },
    /// Re-include a previously excluded task in the workflow
    Include {
        /// Task ID to include
        task_id: String,
    },
    /// Enable or disable lead-driven mode for a channel
    ///
    /// In lead-driven mode, the daemon relays workflow events as @mentions
    /// to the channel lead instead of executing its built-in state machine.
    LeadDriven {
        /// Channel name
        channel: String,
        /// Disable lead-driven mode (default: enable)
        #[arg(long)]
        disable: bool,
    },
}

pub fn handle(cmd: &WorkflowCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        WorkflowCommand::List => handle_list(client),
        WorkflowCommand::Assign { channel, workflow } => client.workflow_assign(channel, workflow),
        WorkflowCommand::Unassign { channel } => client.workflow_unassign(channel),
        WorkflowCommand::Exclude { task_id } => handle_exclude(task_id, client),
        WorkflowCommand::Include { task_id } => handle_include(task_id, client),
        WorkflowCommand::LeadDriven { channel, disable } => {
            let enabled = !disable;
            client.workflow_set_lead_driven(channel, enabled)
        }
    }
}

fn handle_list(client: &DaemonClient) -> Result<Response, String> {
    let result = client.workflow_list_raw()?;

    let workflows = result
        .get("workflows")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let assignments = result
        .get("assignments")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();

    if workflows.is_empty() && assignments.is_empty() {
        return Ok(Response::message("No workflows found"));
    }

    let mut out = String::new();

    if !workflows.is_empty() {
        out.push_str("Available Workflows\n─────────────────────────────\n");
        for wf in &workflows {
            let name = wf.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let has_agents = wf
                .get("has_agents_md")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let agents_indicator = if has_agents { " (has AGENTS.md)" } else { "" };
            out.push_str(&format!("  {}{}\n", name, agents_indicator));
        }
    }

    if !assignments.is_empty() {
        if !workflows.is_empty() {
            out.push('\n');
        }
        out.push_str("Channel Assignments\n─────────────────────────────\n");
        for (channel, workflow) in &assignments {
            let wf_name = workflow.as_str().unwrap_or("?");
            out.push_str(&format!("  {} → {}\n", channel, wf_name));
        }
    }

    Ok(Response::message(out.trim_end()))
}

fn handle_exclude(task_id: &str, client: &DaemonClient) -> Result<Response, String> {
    let task_id = task_id
        .strip_prefix('#')
        .or_else(|| task_id.strip_prefix('!'))
        .unwrap_or(task_id);

    // Look up which channel this task belongs to
    let channel = resolve_task_channel(task_id, client)?;

    client.workflow_set_state(
        &channel,
        Some(&format!("tasks.{}.excluded", task_id)),
        serde_json::json!(true),
    )
}

fn handle_include(task_id: &str, client: &DaemonClient) -> Result<Response, String> {
    let task_id = task_id
        .strip_prefix('#')
        .or_else(|| task_id.strip_prefix('!'))
        .unwrap_or(task_id);

    // Look up which channel this task belongs to
    let channel = resolve_task_channel(task_id, client)?;

    client.workflow_set_state(
        &channel,
        Some(&format!("tasks.{}.excluded", task_id)),
        serde_json::Value::Null,
    )
}

/// Resolve which channel a task belongs to by querying task metadata.
fn resolve_task_channel(task_id: &str, client: &DaemonClient) -> Result<String, String> {
    let metadata = client.task_metadata(task_id)?;
    metadata
        .get("channel")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            format!(
                "Task !{} has no channel assignment. Use `midtown workflow exclude` only for tasks assigned to channels with workflows.",
                task_id
            )
        })
}

#[path = "workflow_tests.rs"]
#[cfg(test)]
mod tests;
