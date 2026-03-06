//! Hook handlers for idle notifications, task activity, and lead stop sync.

use std::io::Read;
use std::os::unix::net::UnixStream;

use clap::Subcommand;

use super::Response;

/// Append a timestamped log line to `~/.midtown/projects/<repo>/logs/hooks.log`.
///
/// Lightweight alternative to tracing for hooks — hooks are short-lived processes
/// where a full tracing subscriber would add unnecessary overhead.
fn hook_log(repo: &str, message: &str) {
    let log_path = midtown::paths::hooks_log_file_for_repo(repo);
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&log_path)
    {
        use std::io::Write;
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "unknown".to_string());
        let _ = writeln!(file, "{} [{}] {}", now, agent, message);
    }
}

#[derive(Subcommand, Debug, Clone)]
pub enum HookCommand {
    /// Handle Notification hook for idle_prompt - post idle status to channel
    Idle,
    /// Handle Lead stop hook - read channel messages for the Lead
    LeadStop,
    /// Handle PostToolUse hook for TaskUpdate/TaskCreate - posts task activity to channel
    Task,
    /// Handle PostToolUse hook for AskUserQuestion - notifies daemon to nudge Lead
    Ask,
}

/// Input structure for hooks (from Claude Code via stdin)
#[derive(Debug, serde::Deserialize)]
struct HookInput {
    session_id: Option<String>,
    #[allow(dead_code)]
    hook_event_name: Option<String>,
    // For Notification hooks
    notification_type: Option<String>,
    #[allow(dead_code)]
    message: Option<String>,
}

pub fn handle(cmd: &HookCommand) -> Result<Response, String> {
    match cmd {
        HookCommand::Idle => handle_idle_hook(),
        HookCommand::LeadStop => handle_lead_stop_hook(),
        HookCommand::Task => handle_task_hook(),
        HookCommand::Ask => handle_ask_hook(),
    }
}

/// Handle the Lead stop hook - read channel messages for the Lead.
/// Orphan recovery, mergeable PR detection, and stuck PR detection are handled by the daemon.
fn handle_lead_stop_hook() -> Result<Response, String> {
    // Read hook input from stdin to get the session_id for cursor scoping.
    // Claude Code passes JSON with session_id on every Stop hook invocation.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let session_id = serde_json::from_str::<HookInput>(&input)
        .ok()
        .and_then(|h| h.session_id)
        .unwrap_or_else(|| "lead-default".to_string());

    // Read channel messages to sync
    let new_messages = read_channel_messages(&session_id).unwrap_or_default();

    if let Some(ref repo) = detect_git_repo() {
        hook_log(
            repo,
            &format!("lead-stop: {} new message(s)", new_messages.len()),
        );
    }

    // Lightweight daemon health check: try connecting to the socket.
    // If the daemon crashed, the socket file may still exist but connect() will fail.
    let daemon_restarted = check_daemon_health();

    let mut message = if new_messages.is_empty() {
        "Channel synced, no new messages".to_string()
    } else {
        let formatted = format_channel_messages(&new_messages);
        format!(
            "{} new channel message{}:\n- {}",
            new_messages.len(),
            if new_messages.len() == 1 { "" } else { "s" },
            formatted
        )
    };

    if daemon_restarted {
        message.push_str("\n⚠️ Daemon was unresponsive and has been restarted.");
    }

    Ok(Response::Message { message })
}

/// Check daemon health by attempting a socket connection.
///
/// Returns `true` if the daemon was dead and a restart was triggered.
/// Returns `false` if the daemon is healthy or if we're not in a git repo.
fn check_daemon_health() -> bool {
    let socket_path = midtown::paths::daemon_socket();

    // If the socket file doesn't exist, the daemon was never started or was
    // cleanly stopped — not our job to start it.
    if !socket_path.exists() {
        return false;
    }

    // Try an actual TCP-level connect to verify the daemon is listening.
    // This is lightweight — no RPC payload, just a socket handshake.
    match UnixStream::connect(&socket_path) {
        Ok(_stream) => {
            // Daemon is alive — nothing to do. The stream drops immediately.
            false
        }
        Err(e) => {
            // Socket file exists but connect failed — daemon is dead.
            let repo = detect_git_repo();
            if let Some(ref repo) = repo {
                hook_log(
                    repo,
                    &format!("lead-stop: daemon health check failed ({}), restarting", e),
                );
            }

            // Trigger restart — this stops the dead daemon and starts a fresh one.
            // Use force=true since the daemon is dead anyway
            match super::handle_restart(true) {
                Ok(_) => {
                    if let Some(ref repo) = repo {
                        hook_log(repo, "lead-stop: daemon restarted successfully");
                    }
                    true
                }
                Err(err) => {
                    if let Some(ref repo) = repo {
                        hook_log(repo, &format!("lead-stop: daemon restart failed: {}", err));
                    }
                    false
                }
            }
        }
    }
}

/// Read channel messages and return them (for stop hook sync).
fn read_channel_messages(session_id: &str) -> Result<Vec<midtown::Message>, String> {
    if let Some(repo) = detect_git_repo() {
        return read_channel_messages_for_repo(&repo, session_id);
    }
    Ok(Vec::new())
}

/// Read channel messages for a given repo, respecting `MIDTOWN_CHANNEL`.
///
/// Uses session-scoped cursors so each lead session independently tracks its
/// read position. On the first Stop event for a new session (no cursor file
/// exists), initializes the cursor at EOF so only messages from this session
/// are reported — not the entire channel history.
fn read_channel_messages_for_repo(
    repo: &str,
    session_id: &str,
) -> Result<Vec<midtown::Message>, String> {
    let channel = open_channel_for_hook(repo)?;

    // For new sessions (no cursor file yet), start at EOF so the first stop
    // event only reports messages that arrived during this session.
    let cursor_exists =
        midtown::Cursor::file_path(channel.base_dir(), channel.channel_name(), session_id).exists();
    if !cursor_exists {
        let _ = channel.set_cursor_to_end("lead", session_id);
        return Ok(Vec::new());
    }

    channel
        .read_since_cursor("lead", session_id)
        .map_err(|e| format!("Failed to read channel: {}", e))
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

/// Handle the idle hook - post to channel that agent is idle.
fn handle_idle_hook() -> Result<Response, String> {
    // Read hook input from stdin
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("Failed to read stdin: {}", e))?;

    let hook_input: HookInput =
        serde_json::from_str(&input).map_err(|e| format!("Failed to parse hook input: {}", e))?;

    // Verify this is an idle_prompt notification
    if hook_input.notification_type.as_deref() != Some("idle_prompt") {
        return Ok(Response::Message {
            message: "Not an idle_prompt notification".to_string(),
        });
    }

    // Post idle status to channel
    let repo = detect_git_repo().ok_or("Not in a git repository")?;
    let channel = open_channel_for_hook(&repo)?;

    // Coworkers have MIDTOWN_AGENT set to their name at spawn time (see launch.rs).
    // Lead sessions don't have this set, so default to the repo name.
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| repo.clone());

    hook_log(&repo, &format!("idle: {} posting idle status", agent));

    let idle_text = midtown::daemon_messages::idle_waiting();
    let message = midtown::Message::action(&agent, &idle_text);
    if let Err(e) = channel.send(&message) {
        // Don't fail the hook on lock contention — Claude waits for hooks synchronously
        hook_log(
            &repo,
            &format!("idle: channel send failed ({}), skipping", e),
        );
    }

    Ok(Response::Message {
        message: format!("{} posted idle status", agent),
    })
}

/// Handle the PostToolUse hook for task operations (TaskUpdate/TaskCreate).
///
/// Reads tool use context from stdin (JSON), parses the task operation,
/// and posts appropriate action message to channel.
fn handle_task_hook() -> Result<Response, String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("Failed to read stdin: {}", e))?;

    let context: serde_json::Value =
        serde_json::from_str(&input).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    let repo = detect_git_repo().ok_or("Not in a git repository")?;

    // Coworkers have MIDTOWN_AGENT set to their name at spawn time (see launch.rs).
    // Lead sessions don't have this set, so default to the repo name.
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| repo.clone());
    let channel = open_channel_for_hook(&repo)?;

    let tool_name = context["tool_name"]
        .as_str()
        .or_else(|| context["toolName"].as_str())
        .unwrap_or("");
    let tool_input = &context["tool_input"];

    let action_message = match tool_name {
        "TaskCreate" => {
            let subject = tool_input["subject"].as_str().unwrap_or("new task");
            format!("created task: {}", subject)
        }
        "TaskUpdate" => {
            let task_id = tool_input["taskId"]
                .as_str()
                .unwrap_or(tool_input["task_id"].as_str().unwrap_or("?"));

            if let Some(new_status) = tool_input["status"].as_str() {
                match new_status {
                    "in_progress" => {
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
                format!("updated task {}", task_id)
            }
        }
        _ => {
            return Ok(Response::Message {
                message: "OK".to_string(),
            });
        }
    };

    hook_log(&repo, &format!("task: {} {}", agent, action_message));

    let message = midtown::Message::action(&agent, &action_message);
    if let Err(e) = channel.send(&message) {
        // Channel lock contention — log but don't fail the hook (which would stall Claude)
        hook_log(
            &repo,
            &format!("task: channel send failed ({}), skipping", e),
        );
    }

    // For the Lead agent: ensure tasks are persisted to the shared directory.
    // This is a write-through safety net for the /resume case where Claude Code
    // stores tasks only in-memory. The hook mirrors task data to the shared dir
    // so the daemon can see it.
    let _remapped_task_id = if agent.to_lowercase() == repo.to_lowercase() {
        ensure_lead_task_persistence(&repo, tool_name, tool_input, &context)
    } else {
        None
    };

    // Notify daemon for follow-up actions. Uses hook timeout (5s) to avoid blocking.
    if let Ok(client) = crate::client::DaemonClient::connect_for_hook() {
        // Safety-net: report structured state for TaskUpdate operations via RPC.
        // The primary mechanism is `midtown state`, but this catches transitions
        // if the coworker forgets to call it explicitly.
        if tool_name == "TaskUpdate" && agent.to_lowercase() != repo.to_lowercase() {
            let task_id_num = tool_input["taskId"]
                .as_str()
                .or_else(|| tool_input["task_id"].as_str())
                .and_then(|s| s.parse::<u32>().ok());

            let phase_str = tool_input["status"]
                .as_str()
                .and_then(|status| match status {
                    "in_progress" => Some("developing"),
                    "completed" => Some("completed"),
                    _ => None,
                })
                .or_else(|| {
                    // Owner-only update (no status change) → Claiming
                    if tool_input.get("owner").is_some() {
                        Some("claiming")
                    } else {
                        None
                    }
                });

            if let Some(phase_str) = phase_str {
                let _ = client.coworker_report_state(&agent, phase_str, task_id_num, None, None);
            }
        }

        if tool_name == "TaskCreate" {
            let _ = client.check_pending();
        }
    }

    Ok(Response::Message {
        message: format!("Posted: * {} {}", agent, action_message),
    })
}

/// Ensure the Lead's task is persisted to the shared directory.
///
/// Claude Code may fail to persist tasks after `/resume`, storing them only in-memory
/// with IDs starting from 1. This function acts as a write-through layer: it checks
/// whether the task exists in the shared directory and creates it if missing.
///
/// For TaskCreate: creates the task with the next sequential ID and stores an
/// internal→shared ID mapping for future TaskUpdate remapping.
///
/// For TaskUpdate: remaps the internal ID to the shared ID and applies the update.
///
/// Returns the remapped shared task ID if remapping occurred (for TaskUpdate),
/// or None if no remapping was needed.
fn ensure_lead_task_persistence(
    repo: &str,
    tool_name: &str,
    tool_input: &serde_json::Value,
    context: &serde_json::Value,
) -> Option<String> {
    let tasks_dir = midtown::tasks::shared_tasks_dir_for_repo(repo);

    match tool_name {
        "TaskCreate" => {
            let subject = tool_input["subject"].as_str().unwrap_or("");
            let description = tool_input["description"].as_str().unwrap_or("");

            if subject.is_empty() {
                return None;
            }

            match midtown::tasks::ensure_task_in_shared_dir(&tasks_dir, subject, description) {
                Ok((shared_id, was_created)) => {
                    if was_created {
                        // Task wasn't persisted by Claude Code — we created it.
                        // Try to extract the internal ID from tool_result for mapping.
                        let internal_id = extract_internal_task_id(context);
                        if let Some(ref iid) = internal_id {
                            midtown::tasks::store_lead_task_id_mapping(repo, iid, &shared_id);
                        }
                        hook_log(
                            repo,
                            &format!(
                                "task: mirrored '{}' to shared dir as #{} (internal: {:?})",
                                subject, shared_id, internal_id
                            ),
                        );
                    }
                    None // TaskCreate doesn't need to return a remapped ID
                }
                Err(e) => {
                    hook_log(repo, &format!("task: mirror failed: {}", e));
                    None
                }
            }
        }
        "TaskUpdate" => {
            let task_id = tool_input["taskId"]
                .as_str()
                .or_else(|| tool_input["task_id"].as_str())
                .unwrap_or("");

            if task_id.is_empty() {
                return None;
            }

            // Check if the task exists directly in the shared directory
            let task_file = tasks_dir.join(format!("{}.json", task_id));
            if task_file.exists() {
                // Task exists with this ID — no remapping needed.
                // Still apply updates ourselves as a safety net.
                if let Err(e) =
                    midtown::tasks::update_task_fields_in_dir(&tasks_dir, task_id, tool_input)
                {
                    hook_log(repo, &format!("task: update {} failed: {}", task_id, e));
                }
                return None;
            }

            // Task doesn't exist at this ID — check the mapping for a remap
            if let Some(shared_id) = midtown::tasks::lookup_lead_task_id(repo, task_id) {
                hook_log(
                    repo,
                    &format!(
                        "task: remapping internal {} → shared {}",
                        task_id, shared_id
                    ),
                );
                if let Err(e) =
                    midtown::tasks::update_task_fields_in_dir(&tasks_dir, &shared_id, tool_input)
                {
                    hook_log(
                        repo,
                        &format!("task: remapped update {} failed: {}", shared_id, e),
                    );
                }
                Some(shared_id)
            } else {
                hook_log(
                    repo,
                    &format!(
                        "task: no mapping found for internal ID {}, cannot remap",
                        task_id
                    ),
                );
                None
            }
        }
        _ => None,
    }
}

/// Extract the internal task ID from the PostToolUse tool_result.
///
/// Claude Code's tool_result for TaskCreate may be:
/// - A JSON object with an "id" field
/// - A string containing "task N" or "task !N"
/// - A string containing "id: N" or "id N"
fn extract_internal_task_id(context: &serde_json::Value) -> Option<String> {
    let result = &context["tool_result"];

    // Try as JSON object with "id" field
    if let Some(id) = result.get("id") {
        return id
            .as_str()
            .map(String::from)
            .or_else(|| id.as_u64().map(|n| n.to_string()));
    }

    // Try as string containing a task ID pattern
    if let Some(s) = result.as_str() {
        // Look for patterns like "task 1", "task !1", "Task !1:", "id: 1"
        let lower = s.to_lowercase();
        if let Some(pos) = lower.find("task") {
            let rest = &s[pos + 4..];
            return extract_first_number(rest);
        }
        if let Some(pos) = lower.find("id") {
            let rest = &s[pos + 2..];
            return extract_first_number(rest);
        }
        // Try if the whole string is a number
        let trimmed = s.trim();
        if trimmed.parse::<u64>().is_ok() {
            return Some(trimmed.to_string());
        }
    }

    None
}

/// Extract the first number from a string, skipping leading punctuation/whitespace.
fn extract_first_number(s: &str) -> Option<String> {
    let mut chars = s.chars().peekable();
    // Skip non-digit characters (spaces, #, :, etc.)
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            break;
        }
        chars.next();
    }
    // Collect digits
    let num: String = chars.take_while(|c| c.is_ascii_digit()).collect();
    if num.is_empty() { None } else { Some(num) }
}

/// Handle the PostToolUse hook for AskUserQuestion.
///
/// Reads tool use context from stdin (JSON), extracts the question(s),
/// and notifies the daemon to nudge the Lead with the question.
fn handle_ask_hook() -> Result<Response, String> {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("Failed to read stdin: {}", e))?;

    let context: serde_json::Value =
        serde_json::from_str(&input).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    // Coworkers have MIDTOWN_AGENT set to their name at spawn time (see launch.rs).
    // Lead sessions don't have this set, so default to the repo name.
    let agent =
        std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| detect_git_repo().unwrap_or_default());

    let tool_input = &context["tool_input"];
    let questions = extract_ask_questions(tool_input);

    if questions.is_empty() {
        return Ok(Response::Message {
            message: "No questions found in tool input".to_string(),
        });
    }

    let question_text = if questions.len() == 1 {
        questions[0].clone()
    } else {
        questions.join("; ")
    };

    // Log before attempting RPC — useful for debugging hook contention
    if let Some(ref repo) = detect_git_repo() {
        hook_log(repo, &format!("ask: {} asking: {}", agent, question_text));
    }

    // Try to notify daemon via RPC (use hook timeout)
    match crate::client::DaemonClient::connect_for_hook() {
        Ok(client) => match client.coworker_asking(&agent, &question_text) {
            Ok(_) => Ok(Response::Message {
                message: format!("Notified Lead: {} is asking: {}", agent, question_text),
            }),
            Err(e) => {
                post_question_to_channel(&agent, &question_text)?;
                Ok(Response::Message {
                    message: format!("Posted question to channel (daemon error: {})", e),
                })
            }
        },
        Err(_) => {
            post_question_to_channel(&agent, &question_text)?;
            Ok(Response::Message {
                message: "Posted question to channel (daemon not running)".to_string(),
            })
        }
    }
}

/// Extract questions from AskUserQuestion tool input.
fn extract_ask_questions(tool_input: &serde_json::Value) -> Vec<String> {
    let mut questions = Vec::new();
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
    let channel = open_channel_for_hook(&repo)?;

    let message = midtown::Message::text(agent, format!("Question for Lead: {}", question));
    channel
        .send(&message)
        .map_err(|e| format!("Failed to post to channel: {}", e))
}

/// Try to detect the current git repository name.
/// Uses --git-common-dir to handle worktrees correctly (they share the main repo's .git).
fn detect_git_repo() -> Option<String> {
    // First try git-common-dir which works correctly for worktrees
    let common_dir = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        });

    if let Some(git_dir) = common_dir {
        let git_path = std::path::Path::new(&git_dir);
        // The git-common-dir is the .git folder - get its parent's name
        if let Some(parent) = git_path.parent() {
            // Handle relative ".git" by getting the actual toplevel
            if git_dir == ".git" {
                return std::process::Command::new("git")
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
                    });
            }
            // For worktrees, parent is the main repo directory
            return parent.file_name().map(|s| s.to_string_lossy().to_string());
        }
    }

    None
}

/// Open the appropriate channel for a hook, respecting `MIDTOWN_CHANNEL`.
///
/// Coworkers assigned to a topic channel have `MIDTOWN_CHANNEL` set to the
/// channel name (e.g. "tui", "web"). When this env var is present, hooks post
/// to that channel instead of the default "midtown" main channel — the same
/// routing the RPC client applies.
fn open_channel_for_hook(repo: &str) -> Result<midtown::Channel, String> {
    if let Ok(channel_name) = std::env::var("MIDTOWN_CHANNEL") {
        midtown::Channel::for_repo_named(repo, channel_name)
            .map_err(|e| format!("Failed to open channel: {}", e))
    } else {
        midtown::Channel::for_repo(repo).map_err(|e| format!("Failed to open channel: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Guard for tests that mutate the `MIDTOWN_CHANNEL` env var.
    /// Rust runs tests in parallel; without serialization, concurrent
    /// set_var/remove_var calls cause flaky failures.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn test_open_channel_for_hook_respects_midtown_channel() {
        let _guard = ENV_MUTEX.lock().unwrap();

        // Without MIDTOWN_CHANNEL set, hooks should use the default "midtown" channel.
        // With MIDTOWN_CHANNEL set, hooks should route to the named topic channel.
        let repo = format!("test-hook-channel-routing-{}", std::process::id());
        let projects_dir = midtown::paths::projects_dir_for_repo(&repo);
        let _ = std::fs::remove_dir_all(&projects_dir);

        // Default: no MIDTOWN_CHANNEL → opens the channel named after the repo
        unsafe { std::env::remove_var("MIDTOWN_CHANNEL") };
        let ch = open_channel_for_hook(&repo).expect("should open default channel");
        assert_eq!(ch.channel_name(), repo.as_str());

        // With MIDTOWN_CHANNEL set → opens the named topic channel
        unsafe { std::env::set_var("MIDTOWN_CHANNEL", "tui") };
        let ch = open_channel_for_hook(&repo).expect("should open topic channel");
        assert_eq!(ch.channel_name(), "tui");

        // Clean up
        unsafe { std::env::remove_var("MIDTOWN_CHANNEL") };
        let _ = std::fs::remove_dir_all(&projects_dir);
    }

    #[test]
    fn test_post_question_to_channel_uses_topic_channel() {
        let _guard = ENV_MUTEX.lock().unwrap();

        // When MIDTOWN_CHANNEL is set, questions should go to the topic channel.
        let repo = format!("test-question-topic-channel-{}", std::process::id());
        let projects_dir = midtown::paths::projects_dir_for_repo(&repo);
        let _ = std::fs::remove_dir_all(&projects_dir);

        // Override detect_git_repo by setting MIDTOWN_CHANNEL and calling
        // open_channel_for_hook directly (post_question_to_channel uses detect_git_repo
        // which depends on actual git state). Instead, test the routing layer directly.
        unsafe { std::env::set_var("MIDTOWN_CHANNEL", "tui") };
        let ch = open_channel_for_hook(&repo).expect("should open topic channel");
        assert_eq!(ch.channel_name(), "tui");

        // Send a question-style message through the topic channel
        let question = "Test question for topic routing";
        let message =
            midtown::Message::text("channel-lead", format!("Question for Lead: {}", question));
        ch.send(&message).expect("should send to topic channel");

        // Verify message is in topic channel
        let topic_ch = midtown::Channel::for_repo_named(&repo, "tui").unwrap();
        let topic_msgs = topic_ch.read_all().expect("should read topic channel");
        let found = topic_msgs.iter().any(|m| m.content.contains(question));
        assert!(found, "question should appear in topic channel");

        // Main channel should be empty
        if let Ok(main_ch) = midtown::Channel::for_repo(&repo) {
            let main_msgs = main_ch.read_all().expect("should read main channel");
            let found_in_main = main_msgs.iter().any(|m| m.content.contains(question));
            assert!(!found_in_main, "question should NOT appear in main channel");
        }

        unsafe { std::env::remove_var("MIDTOWN_CHANNEL") };
        let _ = std::fs::remove_dir_all(&projects_dir);
    }

    #[test]
    fn test_read_channel_messages_uses_topic_channel() {
        let _guard = ENV_MUTEX.lock().unwrap();

        // When MIDTOWN_CHANNEL is set, read_channel_messages() should read from the topic
        // channel, not the main "midtown" channel — consistent with the other hook functions.
        let repo = format!("test-read-channel-msg-{}", std::process::id());
        let projects_dir = midtown::paths::projects_dir_for_repo(&repo);
        let _ = std::fs::remove_dir_all(&projects_dir);

        // With MIDTOWN_CHANNEL set to "tui":
        // - First call initializes a fresh session cursor at EOF and returns empty.
        // - We then post a message, so subsequent calls should find it.
        unsafe { std::env::set_var("MIDTOWN_CHANNEL", "tui") };
        let first_call = read_channel_messages_for_repo(&repo, "test-session").unwrap();
        assert!(
            first_call.is_empty(),
            "fresh cursor should return no messages"
        );

        // Post a message to the topic channel
        let topic_ch = midtown::Channel::for_repo_named(&repo, "tui").unwrap();
        let msg = midtown::Message::text("lead", "topic channel message");
        topic_ch.send(&msg).unwrap();

        // Second call should see the new message from the topic channel
        let messages = read_channel_messages_for_repo(&repo, "test-session").unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m.content.contains("topic channel message")),
            "should read messages from the topic channel"
        );

        // Main channel should have no messages (different channel, different cursor)
        unsafe { std::env::remove_var("MIDTOWN_CHANNEL") };
        let main_messages = read_channel_messages_for_repo(&repo, "test-session-main").unwrap();
        assert!(main_messages.is_empty(), "main channel should be empty");

        let _ = std::fs::remove_dir_all(&projects_dir);
    }

    #[test]
    fn test_extract_internal_task_id_json_object() {
        let context = serde_json::json!({
            "tool_result": {"id": 5, "subject": "Test task"}
        });
        assert_eq!(extract_internal_task_id(&context), Some("5".to_string()));
    }

    #[test]
    fn test_extract_internal_task_id_json_string_id() {
        let context = serde_json::json!({
            "tool_result": {"id": "42"}
        });
        assert_eq!(extract_internal_task_id(&context), Some("42".to_string()));
    }

    #[test]
    fn test_extract_internal_task_id_string_task_hash() {
        let context = serde_json::json!({
            "tool_result": "Task !42 created successfully"
        });
        assert_eq!(extract_internal_task_id(&context), Some("42".to_string()));
    }

    #[test]
    fn test_extract_internal_task_id_string_task_no_hash() {
        let context = serde_json::json!({
            "tool_result": "task 7 is now pending"
        });
        assert_eq!(extract_internal_task_id(&context), Some("7".to_string()));
    }

    #[test]
    fn test_extract_internal_task_id_string_id_colon() {
        let context = serde_json::json!({
            "tool_result": "Created with id: 99"
        });
        assert_eq!(extract_internal_task_id(&context), Some("99".to_string()));
    }

    #[test]
    fn test_extract_internal_task_id_plain_number() {
        let context = serde_json::json!({
            "tool_result": "3"
        });
        assert_eq!(extract_internal_task_id(&context), Some("3".to_string()));
    }

    #[test]
    fn test_extract_internal_task_id_returns_none() {
        let context = serde_json::json!({
            "tool_result": "no number here at all"
        });
        assert_eq!(extract_internal_task_id(&context), None);
    }

    #[test]
    fn test_extract_internal_task_id_missing_result() {
        let context = serde_json::json!({});
        assert_eq!(extract_internal_task_id(&context), None);
    }
}
