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
    /// Shutdown a coworker
    Shutdown {
        /// Name of the coworker to shutdown
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
    /// Handle Claude Code stop hook (checks for unclaimed tasks)
    StopHook,
    /// Handle Claude Code PostToolUse hook for task operations
    ///
    /// Reads tool use context from stdin and posts task activity to channel.
    /// Called automatically by Claude Code when TaskUpdate or TaskCreate tools are used.
    TaskHook,
    /// Link this session's tasks to the Lead's task directory (SessionStart hook)
    LinkTasks,
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
        CoworkerCommand::Shutdown { name } => client.coworker_shutdown(name),
        CoworkerCommand::List => client.coworker_list(),
        CoworkerCommand::Nudge { name, message } => client.coworker_nudge(name, message.as_deref()),
        CoworkerCommand::NudgeConfig { command } => handle_nudge_config(command, client),
        CoworkerCommand::StopHook => handle_stop_hook_standalone(),
        CoworkerCommand::TaskHook => handle_task_hook_standalone(),
        CoworkerCommand::LinkTasks => handle_link_tasks_standalone(),
        CoworkerCommand::AskHook => handle_ask_hook_standalone(),
    }
}

/// Handle the stop hook for Claude Code (standalone, no daemon required).
///
/// This command is designed to be used as a Claude Code stop hook. It:
/// 1. Reads channel messages (syncs any pending messages)
/// 2. Checks for unclaimed tasks from Claude Code task storage
/// 3. Checks for PRs needing review (that this coworker didn't create)
/// 4. Checks if this coworker's PRs have been approved and can be merged
/// 5. Returns JSON to indicate whether Claude should continue or stop
///
/// If work is available, returns `{"decision": "block", "reason": "..."}` to
/// prevent stopping and allow the coworker to continue working.
pub fn handle_stop_hook_standalone() -> Result<Response, String> {
    // First, read channel messages to sync any pending updates
    let new_messages = read_channel_messages().unwrap_or_default();

    // Get coworker name from environment
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "coworker".to_string());

    // Collect all available work
    let mut work_items: Vec<String> = Vec::new();

    // Check for new channel messages (nudges, requests, etc.)
    if !new_messages.is_empty() {
        let formatted = format_channel_messages(&new_messages);
        work_items.push(format!(
            "{} new channel message{}:\n- {}",
            new_messages.len(),
            if new_messages.len() == 1 { "" } else { "s" },
            formatted
        ));
    }

    // Check for unclaimed tasks
    let unclaimed_count = count_unclaimed_tasks();
    if unclaimed_count > 0 {
        work_items.push(format!(
            "{} unclaimed task{}",
            unclaimed_count,
            if unclaimed_count == 1 { "" } else { "s" }
        ));
    }

    // Check for PRs needing review (not created by this coworker)
    let prs_needing_review = get_prs_needing_review(&agent);
    if !prs_needing_review.is_empty() {
        work_items.push(format!(
            "{} PR{} needing review (use /code-review): {}",
            prs_needing_review.len(),
            if prs_needing_review.len() == 1 {
                ""
            } else {
                "s"
            },
            prs_needing_review.join(", ")
        ));
    }

    // Check for approved PRs that can be merged
    let approved_prs = get_approved_prs_by_coworker(&agent);
    if !approved_prs.is_empty() {
        work_items.push(format!(
            "{} approved PR{} ready to merge: {}",
            approved_prs.len(),
            if approved_prs.len() == 1 { "" } else { "s" },
            approved_prs.join(", ")
        ));
    }

    if !work_items.is_empty() {
        Ok(Response::StopHookDecision {
            decision: "block".to_string(),
            reason: work_items.join("; "),
        })
    } else {
        Ok(Response::StopHookDecision {
            decision: "approve".to_string(),
            reason: "No pending work".to_string(),
        })
    }
}

/// Get PRs that need review and weren't created by this coworker.
fn get_prs_needing_review(agent: &str) -> Vec<String> {
    let output = std::process::Command::new("gh")
        .args(["pr", "list", "--json", "number,author,reviewRequests,title"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                return prs
                    .iter()
                    .filter(|pr| {
                        // Skip PRs created by this coworker (check branch prefix)
                        let author = pr
                            .get("author")
                            .and_then(|a| a.get("login"))
                            .and_then(|l| l.as_str())
                            .unwrap_or("");

                        // Check if PR has review requests or is waiting for review
                        let has_review_requests = pr
                            .get("reviewRequests")
                            .and_then(|r| r.as_array())
                            .map(|a| !a.is_empty())
                            .unwrap_or(false);

                        // Don't review own PRs (check by branch name convention)
                        // Coworkers create branches like "lexington/feature-name"
                        !author.eq_ignore_ascii_case(agent) && has_review_requests
                    })
                    .map(|pr| {
                        let number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                        format!("#{}", number)
                    })
                    .collect();
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Get PRs created by this coworker that have been approved.
fn get_approved_prs_by_coworker(agent: &str) -> Vec<String> {
    // Get PRs from branches matching coworker's naming convention
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,headRefName,reviewDecision,title",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                return prs
                    .iter()
                    .filter(|pr| {
                        // Check if branch starts with coworker name (e.g., "lexington/...")
                        let branch = pr.get("headRefName").and_then(|b| b.as_str()).unwrap_or("");
                        let is_coworker_pr = branch
                            .split('/')
                            .next()
                            .map(|prefix| prefix.eq_ignore_ascii_case(agent))
                            .unwrap_or(false);

                        // Check if approved
                        let review_decision = pr
                            .get("reviewDecision")
                            .and_then(|r| r.as_str())
                            .unwrap_or("");

                        is_coworker_pr && review_decision == "APPROVED"
                    })
                    .map(|pr| {
                        let number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                        format!("#{}", number)
                    })
                    .collect();
            }
            Vec::new()
        }
        _ => Vec::new(),
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

/// Read channel messages and return them.
fn read_channel_messages() -> Result<Vec<midtown::Message>, String> {
    // Try to detect repo and read channel
    if let Some(repo) = detect_git_repo() {
        let channel = midtown::Channel::for_repo(&repo)
            .map_err(|e| format!("Failed to open channel: {}", e))?;

        // Get agent name from environment or use default
        let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "coworker".to_string());

        // Read new messages since cursor (advances cursor position)
        let messages = channel
            .read_since_cursor(&agent)
            .map_err(|e| format!("Failed to read channel: {}", e))?;
        return Ok(messages);
    }
    Ok(Vec::new())
}

/// Format channel messages for display in stop hook reason.
fn format_channel_messages(messages: &[midtown::Message]) -> String {
    messages
        .iter()
        .map(|msg| match msg.message_type {
            midtown::MessageType::Action => format!("* {} {}", msg.from, msg.content),
            _ => format!("{}: {}", msg.from, msg.content),
        })
        .collect::<Vec<_>>()
        .join("\n- ")
}

/// Count unclaimed tasks from Claude Code task storage.
fn count_unclaimed_tasks() -> usize {
    midtown::tasks::count_unclaimed_tasks()
}

/// Try to detect the current git repository name.
fn detect_git_repo() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout);
                std::path::Path::new(path.trim())
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
            } else {
                None
            }
        })
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

/// Link this coworker's task directory to the Lead's.
///
/// Called by SessionStart hook to share tasks across sessions.
pub fn handle_link_tasks_standalone() -> Result<Response, String> {
    use std::fs;
    use std::os::unix::fs::symlink;

    // Get repo name to find Lead's session file
    let repo = detect_git_repo().ok_or("Not in a git repository")?;

    // Read Lead's session UUID from ~/.midtown/lead/<repo>/session-id
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let lead_session_file = midtown::paths::lead_session_file_for_repo(&repo);

    let lead_uuid = fs::read_to_string(&lead_session_file)
        .map_err(|_| {
            format!(
                "Lead session not found at {:?}. Is midtown running?",
                lead_session_file
            )
        })?
        .trim()
        .to_string();

    // Find this session's task directory (newest in ~/.claude/tasks/)
    let tasks_dir = home.join(".claude").join("tasks");
    let my_uuid = find_newest_task_dir(&tasks_dir)?;

    // Don't link to ourselves
    if my_uuid == lead_uuid {
        return Ok(Response::Message {
            message: "Already using Lead's task directory".to_string(),
        });
    }

    let my_task_dir = tasks_dir.join(&my_uuid);
    let lead_task_dir = tasks_dir.join(&lead_uuid);

    // Verify Lead's task dir exists
    if !lead_task_dir.exists() {
        return Err(format!(
            "Lead's task directory not found: {:?}",
            lead_task_dir
        ));
    }

    // Remove our task dir and replace with symlink
    if my_task_dir.exists() {
        fs::remove_dir_all(&my_task_dir)
            .map_err(|e| format!("Failed to remove task directory: {}", e))?;
    }

    symlink(&lead_task_dir, &my_task_dir)
        .map_err(|e| format!("Failed to create symlink: {}", e))?;

    Ok(Response::Message {
        message: format!("Linked tasks: {} -> {}", my_uuid, lead_uuid),
    })
}

/// Find the most recently created directory in the given path.
fn find_newest_task_dir(tasks_dir: &std::path::Path) -> Result<String, String> {
    use std::fs;

    let entries: Vec<_> = fs::read_dir(tasks_dir)
        .map_err(|e| format!("Cannot read tasks directory: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();

    entries
        .iter()
        .max_by_key(|e| e.metadata().and_then(|m| m.created()).ok())
        .and_then(|e| e.file_name().to_str().map(|s| s.to_string()))
        .ok_or_else(|| "No task directories found".to_string())
}
