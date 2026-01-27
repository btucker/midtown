//! Hook handlers for insight posting and idle notifications.
//!
//! These hooks are used by both Lead and coworkers to share insights
//! and notify when idle.

use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;

use clap::Subcommand;

use super::Response;

#[derive(Subcommand, Debug, Clone)]
pub enum HookCommand {
    /// Handle PostToolUse hook - parse transcript for new insights and post them
    Insight,
    /// Handle Notification hook for idle_prompt - post idle status to channel
    Idle,
    /// Handle Lead stop hook - check channel and warn about orphaned tasks
    LeadStop,
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
    }
}

/// Handle the insight hook - parse transcript for ★ Insight blocks and post new ones.
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

    // Parse transcript for insights
    let insights = parse_insights_from_transcript(&transcript_path)?;

    if insights.is_empty() {
        return Ok(Response::Message {
            message: "No new insights".to_string(),
        });
    }

    // Detect repo first - needed for both channel and insight tracking
    let repo = detect_git_repo().ok_or("Not in a git repository")?;

    // Get previously posted insights (tracked per-repo, not per-transcript)
    let mut posted = get_posted_insights(&repo);

    // Post new insights to channel
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "Lead".to_string());

    let mut posted_count = 0;
    for insight in &insights {
        // Use hash to check if already posted
        let hash = hash_insight(insight);
        if posted.contains(&hash) {
            continue;
        }

        // Atomically try to claim this insight - prevents race conditions
        // between concurrent hook invocations
        if !try_claim_insight(&repo, &hash) {
            // Another process beat us to it
            posted.insert(hash);
            continue;
        }

        // Post insight to channel
        let message = midtown::Message::text(&agent, format!("💡 {}", insight));
        if channel.send(&message).is_ok() {
            posted_count += 1;
            // Track in-memory for subsequent iterations
            posted.insert(hash);
        }
    }

    Ok(Response::Message {
        message: format!("Posted {} new insight(s)", posted_count),
    })
}

/// Handle the Lead stop hook - read channel and check for orphaned tasks and idle coworkers.
fn handle_lead_stop_hook() -> Result<Response, String> {
    // First, read channel messages to sync
    let _ = read_channel_messages();

    let repo = detect_git_repo().ok_or("Not in a git repository")?;
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    // Check for orphaned tasks (in_progress but owned by dead coworkers)
    let orphaned = find_orphaned_tasks();

    if !orphaned.is_empty() {
        for (task_id, owner) in &orphaned {
            let message = midtown::Message::text(
                "Lead",
                format!(
                    "⚠️ Task {} is in_progress but coworker '{}' is not running",
                    task_id, owner
                ),
            );
            let _ = channel.send(&message);
        }

        return Ok(Response::Message {
            message: format!("Warning: {} orphaned task(s) found", orphaned.len()),
        });
    }

    // Check for idle coworkers with no remaining work
    let idle_coworkers = find_idle_coworkers();
    let pending_tasks = get_pending_tasks();

    if !idle_coworkers.is_empty() && pending_tasks.is_empty() {
        let coworker_list = idle_coworkers.join(", ");
        let message = midtown::Message::text(
            "Lead",
            format!(
                "💤 Coworkers [{}] are idle with no remaining tasks. Consider shutting them down.",
                coworker_list
            ),
        );
        let _ = channel.send(&message);

        return Ok(Response::Message {
            message: format!(
                "Coworkers {} are idle with no remaining tasks. Consider: midtown stop",
                coworker_list
            ),
        });
    }

    Ok(Response::Message {
        message: "Channel synced, no orphaned tasks".to_string(),
    })
}

/// Find tasks that are in_progress but owned by coworkers that aren't running.
fn find_orphaned_tasks() -> Vec<(String, String)> {
    // Get list of active coworkers from daemon
    let active_coworkers = get_active_coworkers();

    // Get in_progress tasks
    let in_progress = get_in_progress_tasks();

    // Find tasks owned by dead coworkers
    let mut orphaned = Vec::new();
    for (task_id, owner) in in_progress {
        // Skip if owner is Lead or empty
        if owner.is_empty() || owner.eq_ignore_ascii_case("lead") {
            continue;
        }
        // Check if owner is in active coworkers
        if !active_coworkers
            .iter()
            .any(|cw| cw.eq_ignore_ascii_case(&owner))
        {
            orphaned.push((task_id, owner));
        }
    }

    orphaned
}

/// Find coworkers that are running but not actively working on any task.
fn find_idle_coworkers() -> Vec<String> {
    let active_coworkers = get_active_coworkers();
    let in_progress = get_in_progress_tasks();

    // Build set of coworkers who own in_progress tasks
    let busy_coworkers: HashSet<String> = in_progress
        .iter()
        .map(|(_, owner)| owner.to_lowercase())
        .collect();

    // Return coworkers who are active but not busy
    active_coworkers
        .into_iter()
        .filter(|cw| !busy_coworkers.contains(&cw.to_lowercase()))
        .collect()
}

/// Get list of pending tasks (tasks that can still be claimed).
fn get_pending_tasks() -> Vec<String> {
    let output = std::process::Command::new("bd")
        .args(["list", "--json"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(tasks) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                return tasks
                    .iter()
                    .filter(|task| {
                        task.get("status")
                            .and_then(|s| s.as_str())
                            .map(|s| s == "pending")
                            .unwrap_or(false)
                    })
                    .filter_map(|task| {
                        task.get("id").and_then(|i| {
                            i.as_str()
                                .map(|s| s.to_string())
                                .or_else(|| i.as_u64().map(|n| n.to_string()))
                        })
                    })
                    .collect();
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Get list of active coworker names from daemon.
fn get_active_coworkers() -> Vec<String> {
    // Try to connect to daemon and get coworker list
    let socket_path = get_daemon_socket_path();
    if !socket_path.exists() {
        return Vec::new();
    }

    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = match UnixStream::connect(&socket_path) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    let request = r#"{"jsonrpc":"2.0","method":"coworker.list","id":1}"#;
    if writeln!(stream, "{}", request).is_err() {
        return Vec::new();
    }
    let _ = stream.flush();

    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    if reader.read_line(&mut response).is_err() {
        return Vec::new();
    }

    // Parse response
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&response)
        && let Some(result) = json.get("result")
        && let Some(coworkers) = result.get("coworkers").and_then(|c| c.as_array())
    {
        return coworkers
            .iter()
            .filter_map(|cw| cw.get("name").and_then(|n| n.as_str()))
            .map(|s| s.to_string())
            .collect();
    }

    Vec::new()
}

/// Get daemon socket path for the current repository.
fn get_daemon_socket_path() -> PathBuf {
    midtown::paths::daemon_socket()
}

/// Get list of in_progress tasks with their owners.
fn get_in_progress_tasks() -> Vec<(String, String)> {
    // Use bd (beads) to get task list
    let output = std::process::Command::new("bd")
        .args(["list", "--json"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(tasks) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                return tasks
                    .iter()
                    .filter(|task| {
                        task.get("status")
                            .and_then(|s| s.as_str())
                            .map(|s| s == "in_progress")
                            .unwrap_or(false)
                    })
                    .map(|task| {
                        let id = task
                            .get("id")
                            .and_then(|i| {
                                i.as_str()
                                    .map(|s| s.to_string())
                                    .or_else(|| i.as_u64().map(|n| n.to_string()))
                            })
                            .unwrap_or_else(|| "?".to_string());
                        let owner = task
                            .get("owner")
                            .and_then(|o| o.as_str())
                            .unwrap_or("")
                            .to_string();
                        (id, owner)
                    })
                    .collect();
            }
            Vec::new()
        }
        _ => Vec::new(),
    }
}

/// Read channel messages silently (for stop hook sync).
fn read_channel_messages() -> Result<(), String> {
    if let Some(repo) = detect_git_repo() {
        let channel = midtown::Channel::for_repo(&repo)
            .map_err(|e| format!("Failed to open channel: {}", e))?;
        let _ = channel.read_since_cursor("Lead");
    }
    Ok(())
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

    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "coworker".to_string());

    let message = midtown::Message::action(&agent, "waiting for input");
    channel
        .send(&message)
        .map_err(|e| format!("Failed to post to channel: {}", e))?;

    Ok(Response::Message {
        message: format!("{} posted idle status", agent),
    })
}

/// Parse insights from transcript JSONL file.
fn parse_insights_from_transcript(transcript_path: &str) -> Result<Vec<String>, String> {
    let content = std::fs::read_to_string(transcript_path)
        .map_err(|e| format!("Failed to read transcript: {}", e))?;

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

/// Get the path to the insights directory for the current repository.
/// Each posted insight hash becomes a file in this directory.
/// Uses repo name (not transcript) to prevent duplicates across sessions.
fn insights_dir_path(repo_name: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".midtown")
        .join("insights")
        .join(repo_name)
}

/// Get set of already-posted insight hashes for the given repository.
fn get_posted_insights(repo_name: &str) -> HashSet<String> {
    let dir_path = insights_dir_path(repo_name);
    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    } else {
        HashSet::new()
    }
}

/// Atomically try to claim an insight for posting.
/// Returns true if we successfully claimed it (file didn't exist and we created it).
/// Returns false if another process already claimed it (file exists).
fn try_claim_insight(repo_name: &str, hash: &str) -> bool {
    let dir_path = insights_dir_path(repo_name);
    let _ = std::fs::create_dir_all(&dir_path);

    let hash_path = dir_path.join(hash);

    // Use create_new(true) for atomic "create only if not exists"
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&hash_path)
    {
        Ok(_) => true, // We created it - we own this insight
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(_) => false, // Other errors - fail safe by not posting
    }
}

/// Simple hash of insight content.
/// Normalizes text before hashing to prevent duplicates from whitespace variations:
/// - Trims leading/trailing whitespace
/// - Collapses multiple whitespace/newlines to single space
/// - Lowercases for consistency
fn hash_insight(insight: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    // Normalize: trim, collapse whitespace, lowercase
    let normalized: String = insight
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
    fn test_hash_insight_deterministic() {
        let insight = "Test insight content";
        let hash1 = hash_insight(insight);
        let hash2 = hash_insight(insight);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_insight_different() {
        let hash1 = hash_insight("Insight one");
        let hash2 = hash_insight("Insight two");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_insight_normalizes_whitespace() {
        // Same insight with different whitespace should produce same hash
        let hash1 = hash_insight("This is an insight");
        let hash2 = hash_insight("  This  is   an   insight  ");
        let hash3 = hash_insight("This\n  is\nan\ninsight");
        let hash4 = hash_insight("THIS IS AN INSIGHT");

        assert_eq!(hash1, hash2, "extra whitespace should be normalized");
        assert_eq!(hash1, hash3, "newlines should be normalized");
        assert_eq!(hash1, hash4, "case should be normalized");
    }
}
