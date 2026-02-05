//! Hook handlers for insight posting and idle notifications.
//!
//! These hooks are used by both Lead and coworkers to share insights
//! and notify when idle.

use std::io::{Read, Seek, SeekFrom};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

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
    /// Handle PostToolUse hook - parse transcript for new insights and post them
    Insight,
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
    #[allow(dead_code)]
    session_id: Option<String>,
    transcript_path: Option<String>,
    #[allow(dead_code)]
    hook_event_name: Option<String>,
    // For Notification hooks
    notification_type: Option<String>,
    #[allow(dead_code)]
    message: Option<String>,
}

pub fn handle(cmd: &HookCommand) -> Result<Response, String> {
    match cmd {
        HookCommand::Insight => handle_insight_hook(),
        HookCommand::Idle => handle_idle_hook(),
        HookCommand::LeadStop => handle_lead_stop_hook(),
        HookCommand::Task => handle_task_hook(),
        HookCommand::Ask => handle_ask_hook(),
    }
}

/// Handle the insight hook - parse transcript for ★ Insight blocks and report them.
///
/// This hook fires on EVERY PostToolUse event from every coworker and the Lead.
/// With many concurrent Claude Code instances, it must be fast and non-blocking
/// to avoid stalling the calling Claude process (hooks are synchronous).
///
/// Insights are reported to the daemon via RPC, which handles deduplication,
/// channel posting, and spawning headless architect sessions for diagram
/// generation. If the daemon is not running, falls back to direct channel posting.
fn handle_insight_hook() -> Result<Response, String> {
    // Read hook input from stdin
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| format!("Failed to read stdin: {}", e))?;

    let hook_input: HookInput =
        serde_json::from_str(&input).map_err(|e| format!("Failed to parse hook input: {}", e))?;

    let transcript_path = hook_input
        .transcript_path
        .ok_or("No transcript_path in hook input")?;

    // Parse transcript for insights — this is cheap (cursor-based, reads only new bytes)
    let insights = parse_insights_from_transcript(&transcript_path)?;

    if insights.is_empty() {
        return Ok(Response::Message {
            message: "No new insights".to_string(),
        });
    }

    let repo = detect_git_repo().ok_or("Not in a git repository")?;
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());

    hook_log(
        &repo,
        &format!("insight: found {} candidate(s)", insights.len()),
    );

    // Try to report insights via daemon RPC (handles dedup + architect pipeline)
    let client = crate::client::DaemonClient::connect().ok();

    let mut posted_count = 0;
    for insight in &insights {
        if let Some(ref client) = client {
            match client.report_insight(&agent, insight) {
                Ok(result) => {
                    if result
                        .get("posted")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                    {
                        posted_count += 1;
                    }
                }
                Err(e) => {
                    hook_log(
                        &repo,
                        &format!("insight: RPC failed ({}), falling back to channel", e),
                    );
                    // Fallback: post directly to channel
                    if post_insight_to_channel(&repo, &agent, insight) {
                        posted_count += 1;
                    }
                }
            }
        } else {
            // Daemon not running — post directly to channel (standalone mode)
            if post_insight_to_channel(&repo, &agent, insight) {
                posted_count += 1;
            }
        }
    }

    hook_log(
        &repo,
        &format!("insight: posted {} new insight(s)", posted_count),
    );

    Ok(Response::Message {
        message: format!("Posted {} new insight(s)", posted_count),
    })
}

/// Post an insight directly to the channel (fallback when daemon is unavailable).
///
/// Uses atomic file creation for deduplication to prevent concurrent hook
/// invocations from posting the same insight when the daemon is down.
fn post_insight_to_channel(repo: &str, agent: &str, insight: &str) -> bool {
    // Atomic file-based dedup: prevents TOCTOU race between concurrent hooks.
    // Uses create_new(true) so only one process can claim a given insight hash.
    let hash = hash_insight_for_fallback(insight);
    if !try_claim_insight(repo, &hash) {
        return false;
    }

    let channel = match midtown::Channel::for_repo(repo) {
        Ok(ch) => ch,
        Err(e) => {
            hook_log(repo, &format!("insight: failed to open channel ({})", e));
            return false;
        }
    };

    let message = midtown::Message::text(agent, format!("💡 {}", insight));
    match channel.send(&message) {
        Ok(()) => true,
        Err(e) => {
            hook_log(
                repo,
                &format!("insight: channel send failed ({}), skipping", e),
            );
            false
        }
    }
}

/// Atomically try to claim an insight for posting (fallback dedup).
///
/// Creates a file named by the insight hash in the per-repo insights directory.
/// Returns true if we created it (we own this insight), false if it already exists.
fn try_claim_insight(repo_name: &str, hash: &str) -> bool {
    let dir_path = midtown::paths::projects_dir_for_repo(repo_name).join("insights");
    let _ = std::fs::create_dir_all(&dir_path);

    let hash_path = dir_path.join(hash);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&hash_path)
        .is_ok()
}

/// Hash insight content for fallback deduplication.
///
/// Normalizes text (trim, collapse whitespace, lowercase) before hashing.
fn hash_insight_for_fallback(insight: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let normalized: String = insight
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// Handle the Lead stop hook - read channel messages for the Lead.
/// Orphan recovery, mergeable PR detection, and stuck PR detection are handled by the daemon.
fn handle_lead_stop_hook() -> Result<Response, String> {
    // Read channel messages to sync
    let new_messages = read_channel_messages().unwrap_or_default();

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
            match super::handle_restart() {
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
fn read_channel_messages() -> Result<Vec<midtown::Message>, String> {
    if let Some(repo) = detect_git_repo() {
        let channel = midtown::Channel::for_repo(&repo)
            .map_err(|e| format!("Failed to open channel: {}", e))?;
        let messages = channel
            .read_since_cursor("lead")
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
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    // Coworkers have MIDTOWN_AGENT set to their name at spawn time (see tmux.rs).
    // Lead sessions don't have this set, so default to "lead".
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());
    let personality = midtown::config::get_personality();

    hook_log(&repo, &format!("idle: {} posting idle status", agent));

    let idle_text = midtown::daemon_messages::idle_waiting(personality);
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

/// Parse insights from transcript JSONL file, reading only new content since last run.
///
/// Uses a cursor file to track the byte offset of the last read. On subsequent
/// calls, seeks to that offset and only parses new bytes, avoiding the cost of
/// re-reading the entire (potentially multi-MB) transcript on every tool call.
fn parse_insights_from_transcript(transcript_path: &str) -> Result<Vec<String>, String> {
    let cursor_offset = read_transcript_cursor(transcript_path);

    let mut file = std::fs::File::open(transcript_path)
        .map_err(|e| format!("Failed to open transcript: {}", e))?;

    // Seek to where we left off
    if cursor_offset > 0 {
        file.seek(SeekFrom::Start(cursor_offset))
            .map_err(|e| format!("Failed to seek in transcript: {}", e))?;
    }

    let mut content = String::new();
    file.read_to_string(&mut content)
        .map_err(|e| format!("Failed to read transcript: {}", e))?;

    // Update cursor to current end of file
    let new_offset = cursor_offset + content.len() as u64;
    write_transcript_cursor(transcript_path, new_offset);

    if content.is_empty() {
        return Ok(Vec::new());
    }

    let mut insights = Vec::new();

    for line in content.lines() {
        // Parse each JSONL line
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        // Look for assistant messages
        if entry.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }

        let Some(message) = entry.get("message") else {
            continue;
        };

        let Some(content_array) = message.get("content").and_then(|c| c.as_array()) else {
            continue;
        };

        for block in content_array {
            if block.get("type").and_then(|t| t.as_str()) == Some("text")
                && let Some(text) = block.get("text").and_then(|t| t.as_str())
            {
                insights.extend(extract_insights(text));
            }
        }
    }

    Ok(insights)
}

/// Get the cursor file path for a given transcript.
/// Stores the cursor in the same directory as the transcript for isolation.
fn transcript_cursor_path(transcript_path: &str) -> PathBuf {
    let transcript = PathBuf::from(transcript_path);
    let parent = transcript.parent().unwrap_or(std::path::Path::new("."));
    let filename = transcript
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("transcript");
    parent.join(format!(".{}.cursor", filename))
}

/// Read the byte offset cursor for a transcript file.
/// Returns 0 if no cursor exists (first run).
fn read_transcript_cursor(transcript_path: &str) -> u64 {
    let cursor_path = transcript_cursor_path(transcript_path);
    std::fs::read_to_string(cursor_path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Write the byte offset cursor for a transcript file.
fn write_transcript_cursor(transcript_path: &str, offset: u64) {
    let cursor_path = transcript_cursor_path(transcript_path);
    // Parent directory should exist since it's where the transcript lives
    let _ = std::fs::write(cursor_path, offset.to_string());
}

/// Extract insight blocks from text.
fn extract_insights(text: &str) -> Vec<String> {
    let mut insights = Vec::new();

    // Look for insight blocks: ★ Insight ... ─────
    // The markers may optionally have backticks around them
    let start_marker = "★ Insight";
    // End marker - look for a line of dashes (with optional backtick prefix)
    let end_markers = [
        "`─────────────────────────────────────────────────`",
        "─────────────────────────────────────────────────",
    ];

    let mut pos = 0;
    while let Some(start) = text[pos..].find(start_marker) {
        let start_abs = pos + start;
        // Find the content after the header line
        if let Some(header_end) = text[start_abs..].find('\n') {
            let content_start = start_abs + header_end + 1;
            // Find the closing line - try both end marker variants
            let end_pos = end_markers
                .iter()
                .filter_map(|marker| text[content_start..].find(marker))
                .min();

            if let Some(end) = end_pos {
                let insight = text[content_start..content_start + end].trim().to_string();
                if !insight.is_empty() {
                    insights.push(insight);
                }
                pos = content_start + end;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    insights
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

    // Coworkers have MIDTOWN_AGENT set to their name at spawn time (see tmux.rs).
    // Lead sessions don't have this set, so default to "lead".
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());

    let repo = detect_git_repo().ok_or("Not in a git repository")?;
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

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

    // Notify daemon for follow-up actions. Uses a 5s timeout via DaemonClient,
    // so it won't block indefinitely.
    if let Ok(client) = crate::client::DaemonClient::connect() {
        // Safety-net: report structured state for TaskUpdate operations via RPC.
        // The primary mechanism is `midtown state`, but this catches transitions
        // if the coworker forgets to call it explicitly.
        if tool_name == "TaskUpdate" && agent != "lead" {
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
                let _ = client.coworker_report_state(&agent, phase_str, task_id_num);
            }
        }

        match tool_name {
            "TaskCreate" => {
                let _ = client.check_pending();
            }
            "TaskUpdate" => {
                // Nudge the task owner if someone else updated their task
                let task_id = tool_input["taskId"]
                    .as_str()
                    .or_else(|| tool_input["task_id"].as_str())
                    .unwrap_or("");
                if !task_id.is_empty() {
                    // Include task list ID so daemon can check for cross-list collisions
                    let task_list_id = std::env::var("CLAUDE_CODE_TASK_LIST_ID").ok();
                    let _ = client.task_updated(task_id, &agent, task_list_id.as_deref());
                }
            }
            _ => {}
        }
    }

    Ok(Response::Message {
        message: format!("Posted: * {} {}", agent, action_message),
    })
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

    // Coworkers have MIDTOWN_AGENT set to their name at spawn time (see tmux.rs).
    // Lead sessions don't have this set, so default to "lead".
    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());

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

    // Try to notify daemon via RPC
    match crate::client::DaemonClient::connect() {
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
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    let message = midtown::Message::text(agent, format!("Question for Lead: {}", question));
    channel
        .send(&message)
        .map_err(|e| format!("Failed to post to channel: {}", e))?;

    Ok(())
}

/// Public accessor for `detect_git_repo` (used by `handle_state` in mod.rs).
pub fn detect_git_repo_public() -> Option<String> {
    detect_git_repo()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_insights_single() {
        let text = r#"Some text before

`★ Insight ─────────────────────────────────────`
This is an insight about something important.
It can span multiple lines.
`─────────────────────────────────────────────────`

Some text after"#;

        let insights = extract_insights(text);
        assert_eq!(insights.len(), 1);
        assert!(insights[0].contains("This is an insight"));
    }

    #[test]
    fn test_extract_insights_multiple() {
        let text = r#"
`★ Insight ─────────────────────────────────────`
First insight
`─────────────────────────────────────────────────`

Some middle text

`★ Insight ─────────────────────────────────────`
Second insight
`─────────────────────────────────────────────────`
"#;

        let insights = extract_insights(text);
        assert_eq!(insights.len(), 2);
        assert!(insights[0].contains("First"));
        assert!(insights[1].contains("Second"));
    }

    #[test]
    fn test_extract_insights_none() {
        let text = "Just some regular text without any insights.";
        let insights = extract_insights(text);
        assert!(insights.is_empty());
    }

    #[test]
    fn test_cursor_read_write() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        std::fs::write(&transcript, "").unwrap();
        let path_str = transcript.to_str().unwrap();

        // First read should return 0
        assert_eq!(read_transcript_cursor(path_str), 0);

        // Write a cursor
        write_transcript_cursor(path_str, 42);

        // Should read back the value
        assert_eq!(read_transcript_cursor(path_str), 42);

        // Update cursor
        write_transcript_cursor(path_str, 1024);
        assert_eq!(read_transcript_cursor(path_str), 1024);
    }

    #[test]
    fn test_parse_insights_incremental() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("transcript.jsonl");
        let path_str = transcript.to_str().unwrap();

        // Write a transcript with one insight
        let line1 = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "text",
                    "text": "`★ Insight ─────────────────────────────────────`\nFirst insight\n`─────────────────────────────────────────────────`"
                }]
            }
        });
        std::fs::write(&transcript, format!("{}\n", line1)).unwrap();

        // First parse should find the insight
        let insights = parse_insights_from_transcript(path_str).unwrap();
        assert_eq!(insights.len(), 1);
        assert!(insights[0].contains("First insight"));

        // Second parse (no new content) should find nothing
        let insights = parse_insights_from_transcript(path_str).unwrap();
        assert!(insights.is_empty(), "should find no new insights");

        // Append a second insight
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        let line2 = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "text",
                    "text": "`★ Insight ─────────────────────────────────────`\nSecond insight\n`─────────────────────────────────────────────────`"
                }]
            }
        });
        writeln!(file, "{}", line2).unwrap();

        // Third parse should only find the new insight
        let insights = parse_insights_from_transcript(path_str).unwrap();
        assert_eq!(insights.len(), 1);
        assert!(insights[0].contains("Second insight"));
    }

    #[test]
    fn test_hash_insight_for_fallback_deterministic() {
        let hash1 = hash_insight_for_fallback("Test insight content");
        let hash2 = hash_insight_for_fallback("Test insight content");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_insight_for_fallback_normalizes_whitespace() {
        let hash1 = hash_insight_for_fallback("This is an insight");
        let hash2 = hash_insight_for_fallback("  This  is   an   insight  ");
        let hash3 = hash_insight_for_fallback("This\n  is\nan\ninsight");
        let hash4 = hash_insight_for_fallback("THIS IS AN INSIGHT");

        assert_eq!(hash1, hash2, "extra whitespace should be normalized");
        assert_eq!(hash1, hash3, "newlines should be normalized");
        assert_eq!(hash1, hash4, "case should be normalized");
    }

    #[test]
    fn test_try_claim_insight_atomic_dedup() {
        // Use a unique repo name to avoid collisions with other tests/state
        let repo = format!("test-dedup-{}", std::process::id());
        let insights_dir = midtown::paths::projects_dir_for_repo(&repo).join("insights");

        // Ensure clean state
        let _ = std::fs::remove_dir_all(&insights_dir);

        let hash = hash_insight_for_fallback("The daemon follows an event-driven architecture");

        // First claim should succeed
        assert!(
            try_claim_insight(&repo, &hash),
            "first claim should succeed"
        );

        // Second claim with same hash should fail (file already exists)
        assert!(
            !try_claim_insight(&repo, &hash),
            "second claim should fail — duplicate"
        );

        // Different insight should succeed
        let hash2 = hash_insight_for_fallback("A completely different insight");
        assert!(
            try_claim_insight(&repo, &hash2),
            "different insight should succeed"
        );

        // Clean up test directory
        let _ = std::fs::remove_dir_all(midtown::paths::projects_dir_for_repo(&repo));
    }

    #[test]
    fn test_post_insight_to_channel_deduplicates() {
        // Simulates two "concurrent" fallback posts with the same insight.
        // Only the first should succeed; the second should be blocked by
        // the atomic file claim (the race condition the reviewer flagged).
        let repo = format!("test-channel-dedup-{}", std::process::id());
        let projects_dir = midtown::paths::projects_dir_for_repo(&repo);

        // Ensure clean state
        let _ = std::fs::remove_dir_all(&projects_dir);

        let insight = "The insight pipeline uses headless architect sessions";

        // First post should succeed (claims the insight + posts to channel)
        let first = post_insight_to_channel(&repo, "coworker-a", insight);
        assert!(first, "first fallback post should succeed");

        // Second post with same insight should fail (atomic claim blocks it)
        let second = post_insight_to_channel(&repo, "coworker-b", insight);
        assert!(!second, "second fallback post should be blocked by dedup");

        // Verify only one message was posted to the channel
        let channel = midtown::Channel::for_repo(&repo).unwrap();
        let messages = channel.read_all().unwrap();
        let insight_messages: Vec<_> = messages
            .iter()
            .filter(|m| m.content.contains(insight))
            .collect();
        assert_eq!(
            insight_messages.len(),
            1,
            "only one insight message should be in the channel"
        );

        // Clean up test directory
        let _ = std::fs::remove_dir_all(&projects_dir);
    }
}
