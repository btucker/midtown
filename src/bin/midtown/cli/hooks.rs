//! Hook handlers for insight posting and idle notifications.
//!
//! These hooks are used by both Lead and coworkers to share insights
//! and notify when idle.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use clap::Subcommand;

use super::Response;

#[derive(Subcommand, Debug, Clone)]
pub enum HookCommand {
    /// Handle PostToolUse hook - parse transcript for new insights and post them
    Insight,
    /// Handle Notification hook for idle_prompt - post idle status to channel
    Idle,
    /// Handle Lead stop hook - read channel messages for the Lead
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

    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "lead".to_string());

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

/// Handle the Lead stop hook - read channel messages for the Lead.
/// Orphan recovery, mergeable PR detection, and stuck PR detection are handled by the daemon.
fn handle_lead_stop_hook() -> Result<Response, String> {
    // Read channel messages to sync
    let new_messages = read_channel_messages().unwrap_or_default();

    let message = if new_messages.is_empty() {
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

    Ok(Response::Message { message })
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

    let agent = std::env::var("MIDTOWN_AGENT").unwrap_or_else(|_| "coworker".to_string());
    let personality = midtown::config::get_personality();

    let idle_text = midtown::daemon_messages::idle_waiting(personality);
    let message = midtown::Message::action(&agent, &idle_text);
    channel
        .send(&message)
        .map_err(|e| format!("Failed to post to channel: {}", e))?;

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

/// Get the path to the insights directory for the current repository.
/// Each posted insight hash becomes a file in this directory.
/// Uses repo name (not transcript) to prevent duplicates across sessions.
fn insights_dir_path(repo_name: &str) -> PathBuf {
    midtown::paths::projects_dir_for_repo(repo_name).join("insights")
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
