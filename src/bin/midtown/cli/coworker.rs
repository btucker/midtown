use clap::Subcommand;

use super::Response;
use crate::client::DaemonClient;

#[derive(Subcommand, Debug, Clone)]
pub enum CoworkerCommand {
    /// Spawn a new coworker
    Spawn {
        /// Resume the previous Claude session (passes --continue to claude)
        #[arg(long)]
        resume: bool,
        /// Initial prompt to send after spawn (avoids separate nudge step)
        #[arg(long, short)]
        prompt: Option<String>,
    },
    /// Send a coworker on a break
    Break {
        /// Name of the coworker to send on a break
        name: String,
    },
    /// List all coworkers
    List,
    /// Nudge a coworker to check in
    Nudge {
        /// Name of the coworker to nudge
        name: String,
        /// Custom message (optional)
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Configure nudging settings
    NudgeConfig {
        #[command(subcommand)]
        command: NudgeConfigCommand,
    },
    /// Handle Claude Code PostToolUse hook for task operations
    ///
    /// Reads tool use context from stdin and posts task activity to channel.
    /// Called automatically by Claude Code when TaskUpdate or TaskCreate tools are used.
    TaskHook,
    /// Handle Claude Code PostToolUse hook for AskUserQuestion
    ///
    /// Reads tool use context from stdin and notifies daemon to nudge Lead.
    /// Called automatically by Claude Code when AskUserQuestion tool is used.
    AskHook,
}

#[derive(Subcommand, Debug, Clone)]
pub enum NudgeConfigCommand {
    /// Show current nudge configuration
    Show,
    /// Set nudge interval (in seconds)
    Interval {
        /// Interval in seconds (0 to disable periodic nudging)
        seconds: u64,
    },
    /// Set nudge message template
    Template {
        /// Message template with {task} placeholder
        template: String,
    },
    /// Enable nudging
    Enable,
    /// Disable nudging
    Disable,
}

pub fn handle(cmd: &CoworkerCommand, client: &DaemonClient) -> Result<Response, String> {
    match cmd {
        CoworkerCommand::Spawn { resume, prompt } => {
            client.coworker_spawn(*resume, prompt.as_deref())
        }
        CoworkerCommand::Break { name } => client.coworker_break(name),
        CoworkerCommand::List => client.coworker_list(),
        CoworkerCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
        CoworkerCommand::NudgeConfig { command } => handle_nudge_config(command, client),
        CoworkerCommand::TaskHook => handle_task_hook_standalone(),
        CoworkerCommand::AskHook => handle_ask_hook_standalone(),
    }
}

/// Handle the PostToolUse hook for task operations.
///
/// This command is designed to be used as a Claude Code PostToolUse hook
/// for TaskUpdate and TaskCreate tools. It:
/// 1. Reads tool use context from stdin (JSON)
/// 2. Parses the task operation (claim, complete, create)
/// 3. Posts appropriate action message to channel
///
/// Example outputs:
/// - `* lexington claimed task 5: Fix auth middleware`
/// - `* lexington completed task 5`
/// - `* Lead created task 7: Add unit tests`
pub fn handle_task_hook_standalone() -> Result<Response, String> {
    use std::io::Read;

    // Read stdin for tool context
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("Failed to read stdin: {}", e))?;

    // Parse the JSON input
    let context: serde_json::Value =
        serde_json::from_str(&input).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    // Get agent name from environment or use "coworker"
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "coworker".to_string());

    // Detect repo and open channel
    let repo = detect_git_repo().ok_or("Not in a git repository")?;
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    // Parse tool input to determine the operation
    let tool_name = context["tool_name"]
        .as_str()
        .or_else(|| context["toolName"].as_str())
        .unwrap_or("");
    let tool_input = &context["tool_input"];

    let action_message = match tool_name {
        "TaskCreate" => {
            // Extract subject from tool input
            let subject = tool_input["subject"].as_str().unwrap_or("new task");
            format!("created task: {}", subject)
        }
        "TaskUpdate" => {
            // Parse task update - check for status changes
            let task_id = tool_input["taskId"]
                .as_str()
                .unwrap_or(tool_input["task_id"].as_str().unwrap_or("?"));

            if let Some(new_status) = tool_input["status"].as_str() {
                match new_status {
                    "in_progress" => {
                        // Check if owner was also set (claiming)
                        if tool_input.get("owner").is_some() {
                            format!("claimed task {}", task_id)
                        } else {
                            format!("started task {}", task_id)
                        }
                    }
                    "completed" => format!("completed task {}", task_id),
                    _ => format!("updated task {} to {}", task_id, new_status),
                }
            } else if tool_input.get("owner").is_some() {
                format!("claimed task {}", task_id)
            } else {
                // Generic update
                format!("updated task {}", task_id)
            }
        }
        _ => {
            // Unknown tool - silently succeed
            return Ok(Response::Message {
                message: "OK".to_string(),
            });
        }
    };

    // Post action message to channel
    let message = midtown::Message::action(&agent, &action_message);
    channel
        .send(&message)
        .map_err(|e| format!("Failed to post to channel: {}", e))?;

    Ok(Response::Message {
        message: format!("Posted: * {} {}", agent, action_message),
    })
}

/// Handle the PostToolUse hook for AskUserQuestion.
///
/// This command is designed to be used as a Claude Code PostToolUse hook
/// for the AskUserQuestion tool. It:
/// 1. Reads tool use context from stdin (JSON)
/// 2. Extracts the question(s) being asked
/// 3. Notifies daemon via RPC to nudge the Lead with the question
///
/// Example: When a coworker uses AskUserQuestion to ask "Should I use REST or GraphQL?",
/// this hook triggers and the Lead gets nudged with that question.
pub fn handle_ask_hook_standalone() -> Result<Response, String> {
    use std::io::Read;

    // Read stdin for tool context
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("Failed to read stdin: {}", e))?;

    // Parse the JSON input
    let context: serde_json::Value =
        serde_json::from_str(&input).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    // Get agent name from environment
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "coworker".to_string());

    // Extract questions from tool input
    let tool_input = &context["tool_input"];
    let questions = extract_questions(tool_input);

    if questions.is_empty() {
        return Ok(Response::Message {
            message: "No questions found in tool input".to_string(),
        });
    }

    // Format the question(s) for the nudge
    let question_text = if questions.len() == 1 {
        questions[0].clone()
    } else {
        questions.join("; ")
    };

    // Try to notify daemon via RPC
    let client_result = crate::client::DaemonClient::connect();
    match client_result {
        Ok(client) => {
            // Call daemon RPC to notify about the question
            match client.coworker_asking(&agent, &question_text) {
                Ok(_) => Ok(Response::Message {
                    message: format!("Notified Lead: {} is asking: {}", agent, question_text),
                }),
                Err(e) => {
                    // Fallback: post to channel directly if daemon call fails
                    post_question_to_channel(&agent, &question_text)?;
                    Ok(Response::Message {
                        message: format!("Posted question to channel (daemon error: {})", e),
                    })
                }
            }
        }
        Err(_) => {
            // Fallback: post to channel directly if daemon not running
            post_question_to_channel(&agent, &question_text)?;
            Ok(Response::Message {
                message: "Posted question to channel (daemon not running)".to_string(),
            })
        }
    }
}

/// Extract questions from AskUserQuestion tool input.
fn extract_questions(tool_input: &serde_json::Value) -> Vec<String> {
    let mut questions = Vec::new();

    // AskUserQuestion has a "questions" array with "question" fields
    if let Some(qs) = tool_input.get("questions").and_then(|q| q.as_array()) {
        for q in qs {
            if let Some(text) = q.get("question").and_then(|t| t.as_str()) {
                questions.push(text.to_string());
            }
        }
    }

    questions
}

/// Post a question to the channel as a fallback when daemon is unavailable.
fn post_question_to_channel(agent: &str, question: &str) -> Result<(), String> {
    let repo = detect_git_repo().ok_or("Not in a git repository")?;
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    let message = midtown::Message::text(agent, format!("Question for Lead: {}", question));
    channel
        .send(&message)
        .map_err(|e| format!("Failed to post to channel: {}", e))?;

    Ok(())
}

/// Try to detect the current git repository name.
/// Uses the worktree-aware detect_repo_name() to avoid returning coworker
/// worktree names instead of the actual repository name.
fn detect_git_repo() -> Option<String> {
    midtown::paths::detect_repo_name()
}

fn handle_nudge_config(
    cmd: &NudgeConfigCommand,
    client: &DaemonClient,
) -> Result<Response, String> {
    match cmd {
        NudgeConfigCommand::Show => client.nudge_config_show(),
        NudgeConfigCommand::Interval { seconds } => client.nudge_config_interval(*seconds),
        NudgeConfigCommand::Template { template } => client.nudge_config_template(template),
        NudgeConfigCommand::Enable => client.nudge_config_enable(true),
        NudgeConfigCommand::Disable => client.nudge_config_enable(false),
    }
}
