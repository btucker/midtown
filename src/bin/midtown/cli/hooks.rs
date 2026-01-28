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

/// Handle the Lead stop hook - read channel, check for orphaned tasks, idle coworkers, and mergeable PRs.
fn handle_lead_stop_hook() -> Result<Response, String> {
    // First, read channel messages to sync
    let new_messages = read_channel_messages().unwrap_or_default();

    let repo = detect_git_repo().ok_or("Not in a git repository")?;
    let channel =
        midtown::Channel::for_repo(&repo).map_err(|e| format!("Failed to open channel: {}", e))?;

    // Collect all status items for the response
    let mut status_items: Vec<String> = Vec::new();

    // Include new channel messages in the response
    if !new_messages.is_empty() {
        let formatted = format_channel_messages(&new_messages);
        status_items.push(format!(
            "{} new channel message{}:\n- {}",
            new_messages.len(),
            if new_messages.len() == 1 { "" } else { "s" },
            formatted
        ));
    }

    // Check for orphaned tasks (in_progress but owned by dead coworkers)
    let orphaned = find_orphaned_tasks();

    if !orphaned.is_empty() {
        for (task_id, owner) in &orphaned {
            let message = midtown::Message::text(
                "lead",
                format!(
                    "⚠️ Task {} is in_progress but coworker '{}' is not running",
                    task_id, owner
                ),
            );
            let _ = channel.send(&message);
        }

        status_items.push(format!(
            "Warning: {} orphaned task(s) found",
            orphaned.len()
        ));
    }

    // Note: Idle coworkers are now automatically shut down by the daemon after 5 minutes.
    // No need to notify the Lead about idle coworkers here.

    // Check for mergeable PRs with passing CI
    let mergeable_prs = find_mergeable_prs();

    if !mergeable_prs.is_empty() {
        let pr_messages: Vec<String> = mergeable_prs
            .iter()
            .map(|pr| {
                format!(
                    "PR #{} \"{}\" has passing CI and is ready to merge.\nPlease review it and ask the human if you should merge.",
                    pr.number, pr.title
                )
            })
            .collect();

        status_items.extend(pr_messages);
    }

    // Check for stuck PRs (CI passed + approved + no activity for 10+ minutes)
    let stuck_prs = find_stuck_prs(&repo);

    for pr in stuck_prs {
        let message = midtown::Message::text(
            "lead",
            format!(
                "🔔 PR #{} \"{}\" appears ready to merge - CI passed, has approval, no blockers. Consider merging or noting why it's blocked.",
                pr.number, pr.title
            ),
        );
        let _ = channel.send(&message);
        status_items.push(format!("Flagged stuck PR #{}", pr.number));
    }

    // Build the response message
    let base_message = if status_items.is_empty() {
        "Channel synced, no orphaned tasks, no mergeable PRs".to_string()
    } else {
        status_items.join("\n\n")
    };

    // Always append test coverage reminder
    let message = format!(
        "{}\n\n📊 Test coverage: Keep an eye on test coverage. If new code lacks tests or coverage gaps are emerging, create tasks to close them.",
        base_message
    );

    Ok(Response::Message { message })
}

/// Information about a mergeable PR.
#[derive(Debug, PartialEq)]
struct MergeablePr {
    number: u64,
    title: String,
}

/// Information about a PR that appears stuck and ready to merge.
#[derive(Debug, PartialEq)]
struct StuckPr {
    number: u64,
    title: String,
}

/// Find PRs that are mergeable with all CI checks passing.
fn find_mergeable_prs() -> Vec<MergeablePr> {
    // Query GitHub for PRs with mergeable status and check results
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,mergeable,statusCheckRollup",
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_mergeable_prs(&stdout)
}

/// Parse PR JSON output and filter for mergeable PRs with passing checks.
fn parse_mergeable_prs(json_str: &str) -> Vec<MergeablePr> {
    let prs: Vec<serde_json::Value> = match serde_json::from_str(json_str) {
        Ok(prs) => prs,
        Err(_) => return Vec::new(),
    };

    prs.iter()
        .filter_map(|pr| {
            let number = pr.get("number")?.as_u64()?;
            let title = pr.get("title")?.as_str()?.to_string();
            let mergeable = pr.get("mergeable")?.as_str()?;

            // Check if PR is mergeable
            if mergeable != "MERGEABLE" {
                return None;
            }

            // Check if all status checks passed
            let checks = pr.get("statusCheckRollup")?.as_array()?;

            // If there are no checks, consider it as not ready (require at least one check)
            if checks.is_empty() {
                return None;
            }

            // All checks must be successful
            let all_passed = checks.iter().all(|check| {
                // Check for conclusion field (used by check runs)
                if let Some(conclusion) = check.get("conclusion").and_then(|c| c.as_str()) {
                    return conclusion == "SUCCESS";
                }
                // Check for state field (used by status contexts)
                if let Some(state) = check.get("state").and_then(|s| s.as_str()) {
                    return state == "SUCCESS";
                }
                false
            });

            if all_passed {
                Some(MergeablePr { number, title })
            } else {
                None
            }
        })
        .collect()
}

/// Find PRs that appear stuck: CI passing, approved, no changes requested, and inactive.
fn find_stuck_prs(repo_name: &str) -> Vec<StuckPr> {
    // Query GitHub for PRs with review info and timestamps
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--json",
            "number,title,reviews,statusCheckRollup,updatedAt",
        ])
        .output();

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let candidates = parse_stuck_prs(&stdout);

    // Filter out PRs we've already flagged recently (within last hour)
    let flagged = get_flagged_prs(repo_name);
    candidates
        .into_iter()
        .filter(|pr| !flagged.contains(&pr.number))
        .inspect(|pr| {
            // Mark as flagged for next time
            mark_pr_flagged(repo_name, pr.number);
        })
        .collect()
}

/// Parse PR JSON output and filter for stuck PRs.
/// A PR is "stuck" if:
/// - All CI checks pass
/// - Has at least one approving review
/// - No reviews with changes_requested
/// - Last activity was more than 10 minutes ago
fn parse_stuck_prs(json_str: &str) -> Vec<StuckPr> {
    let prs: Vec<serde_json::Value> = match serde_json::from_str(json_str) {
        Ok(prs) => prs,
        Err(_) => return Vec::new(),
    };

    let ten_minutes_ago = chrono::Utc::now() - chrono::Duration::minutes(10);

    prs.iter()
        .filter_map(|pr| {
            let number = pr.get("number")?.as_u64()?;
            let title = pr.get("title")?.as_str()?.to_string();

            // Check if all status checks passed
            let checks = pr.get("statusCheckRollup")?.as_array()?;
            if checks.is_empty() {
                return None;
            }
            let all_checks_passed = checks.iter().all(|check| {
                if let Some(conclusion) = check.get("conclusion").and_then(|c| c.as_str()) {
                    return conclusion == "SUCCESS";
                }
                if let Some(state) = check.get("state").and_then(|s| s.as_str()) {
                    return state == "SUCCESS";
                }
                false
            });
            if !all_checks_passed {
                return None;
            }

            // Check reviews
            let reviews = pr.get("reviews").and_then(|r| r.as_array())?;

            // Must have at least one APPROVED review
            let has_approval = reviews.iter().any(|review| {
                review
                    .get("state")
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| s == "APPROVED")
            });
            if !has_approval {
                return None;
            }

            // Must not have any CHANGES_REQUESTED reviews
            let has_changes_requested = reviews.iter().any(|review| {
                review
                    .get("state")
                    .and_then(|s| s.as_str())
                    .is_some_and(|s| s == "CHANGES_REQUESTED")
            });
            if has_changes_requested {
                return None;
            }

            // Check if last activity was more than 10 minutes ago
            let updated_at = pr.get("updatedAt")?.as_str()?;
            let updated_time = chrono::DateTime::parse_from_rfc3339(updated_at).ok()?;
            if updated_time > ten_minutes_ago {
                return None; // Still active, not stuck
            }

            Some(StuckPr { number, title })
        })
        .collect()
}

/// Get the path to the flagged PRs directory for tracking.
fn flagged_prs_dir_path(repo_name: &str) -> PathBuf {
    midtown::paths::projects_dir_for_repo(repo_name).join("flagged_prs")
}

/// Get set of recently flagged PR numbers (within the last hour).
fn get_flagged_prs(repo_name: &str) -> HashSet<u64> {
    let dir_path = flagged_prs_dir_path(repo_name);
    let one_hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);

    if let Ok(entries) = std::fs::read_dir(&dir_path) {
        entries
            .filter_map(|e| e.ok())
            .filter_map(|entry| {
                // Check if file is recent (within last hour)
                let metadata = entry.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                if modified < one_hour_ago {
                    // Clean up old flagged entries
                    let _ = std::fs::remove_file(entry.path());
                    return None;
                }
                // Parse PR number from filename
                entry.file_name().into_string().ok()?.parse().ok()
            })
            .collect()
    } else {
        HashSet::new()
    }
}

/// Mark a PR as flagged to avoid spam.
fn mark_pr_flagged(repo_name: &str, pr_number: u64) {
    let dir_path = flagged_prs_dir_path(repo_name);
    let _ = std::fs::create_dir_all(&dir_path);
    let flag_path = dir_path.join(pr_number.to_string());
    // Create empty file - modification time is used for staleness check
    let _ = std::fs::write(&flag_path, "");
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
    midtown::tasks::get_in_progress_tasks()
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

    #[test]
    fn test_parse_mergeable_prs_with_passing_checks() {
        let json = r#"[
            {
                "number": 42,
                "title": "feat: Add widget",
                "mergeable": "MERGEABLE",
                "statusCheckRollup": [
                    {"conclusion": "SUCCESS"},
                    {"conclusion": "SUCCESS"}
                ]
            }
        ]"#;

        let prs = parse_mergeable_prs(json);
        assert_eq!(prs.len(), 1);
        assert_eq!(
            prs[0],
            MergeablePr {
                number: 42,
                title: "feat: Add widget".to_string()
            }
        );
    }

    #[test]
    fn test_parse_mergeable_prs_with_failing_checks() {
        let json = r#"[
            {
                "number": 42,
                "title": "feat: Add widget",
                "mergeable": "MERGEABLE",
                "statusCheckRollup": [
                    {"conclusion": "SUCCESS"},
                    {"conclusion": "FAILURE"}
                ]
            }
        ]"#;

        let prs = parse_mergeable_prs(json);
        assert!(prs.is_empty());
    }

    #[test]
    fn test_parse_mergeable_prs_not_mergeable() {
        let json = r#"[
            {
                "number": 42,
                "title": "feat: Add widget",
                "mergeable": "CONFLICTING",
                "statusCheckRollup": [
                    {"conclusion": "SUCCESS"}
                ]
            }
        ]"#;

        let prs = parse_mergeable_prs(json);
        assert!(prs.is_empty());
    }

    #[test]
    fn test_parse_mergeable_prs_no_checks() {
        let json = r#"[
            {
                "number": 42,
                "title": "feat: Add widget",
                "mergeable": "MERGEABLE",
                "statusCheckRollup": []
            }
        ]"#;

        let prs = parse_mergeable_prs(json);
        assert!(prs.is_empty());
    }

    #[test]
    fn test_parse_mergeable_prs_with_state_field() {
        // Some GitHub status contexts use "state" instead of "conclusion"
        let json = r#"[
            {
                "number": 42,
                "title": "feat: Add widget",
                "mergeable": "MERGEABLE",
                "statusCheckRollup": [
                    {"state": "SUCCESS"}
                ]
            }
        ]"#;

        let prs = parse_mergeable_prs(json);
        assert_eq!(prs.len(), 1);
    }

    #[test]
    fn test_parse_mergeable_prs_multiple() {
        let json = r#"[
            {
                "number": 42,
                "title": "feat: Add widget",
                "mergeable": "MERGEABLE",
                "statusCheckRollup": [{"conclusion": "SUCCESS"}]
            },
            {
                "number": 43,
                "title": "fix: Bug fix",
                "mergeable": "CONFLICTING",
                "statusCheckRollup": [{"conclusion": "SUCCESS"}]
            },
            {
                "number": 44,
                "title": "docs: Update readme",
                "mergeable": "MERGEABLE",
                "statusCheckRollup": [{"conclusion": "SUCCESS"}]
            }
        ]"#;

        let prs = parse_mergeable_prs(json);
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 42);
        assert_eq!(prs[1].number, 44);
    }

    #[test]
    fn test_parse_stuck_prs_approved_and_passing() {
        // PR is old (20 minutes ago), has approval, CI passes - should be stuck
        let old_time = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        let json = format!(
            r#"[
            {{
                "number": 42,
                "title": "feat: Add widget",
                "updatedAt": "{}",
                "statusCheckRollup": [{{"conclusion": "SUCCESS"}}],
                "reviews": [{{"state": "APPROVED"}}]
            }}
        ]"#,
            old_time
        );

        let prs = parse_stuck_prs(&json);
        assert_eq!(prs.len(), 1);
        assert_eq!(
            prs[0],
            StuckPr {
                number: 42,
                title: "feat: Add widget".to_string()
            }
        );
    }

    #[test]
    fn test_parse_stuck_prs_recent_activity_not_stuck() {
        // PR was updated 5 minutes ago - not stuck yet
        let recent_time = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        let json = format!(
            r#"[
            {{
                "number": 42,
                "title": "feat: Add widget",
                "updatedAt": "{}",
                "statusCheckRollup": [{{"conclusion": "SUCCESS"}}],
                "reviews": [{{"state": "APPROVED"}}]
            }}
        ]"#,
            recent_time
        );

        let prs = parse_stuck_prs(&json);
        assert!(prs.is_empty(), "Recent PRs should not be considered stuck");
    }

    #[test]
    fn test_parse_stuck_prs_no_approval() {
        // PR is old but has no approval
        let old_time = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        let json = format!(
            r#"[
            {{
                "number": 42,
                "title": "feat: Add widget",
                "updatedAt": "{}",
                "statusCheckRollup": [{{"conclusion": "SUCCESS"}}],
                "reviews": [{{"state": "COMMENTED"}}]
            }}
        ]"#,
            old_time
        );

        let prs = parse_stuck_prs(&json);
        assert!(prs.is_empty(), "PRs without approval should not be stuck");
    }

    #[test]
    fn test_parse_stuck_prs_changes_requested() {
        // PR is old, has approval, but also has changes requested
        let old_time = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        let json = format!(
            r#"[
            {{
                "number": 42,
                "title": "feat: Add widget",
                "updatedAt": "{}",
                "statusCheckRollup": [{{"conclusion": "SUCCESS"}}],
                "reviews": [
                    {{"state": "APPROVED"}},
                    {{"state": "CHANGES_REQUESTED"}}
                ]
            }}
        ]"#,
            old_time
        );

        let prs = parse_stuck_prs(&json);
        assert!(
            prs.is_empty(),
            "PRs with changes_requested should not be stuck"
        );
    }

    #[test]
    fn test_parse_stuck_prs_failing_ci() {
        // PR is old with approval but CI is failing
        let old_time = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        let json = format!(
            r#"[
            {{
                "number": 42,
                "title": "feat: Add widget",
                "updatedAt": "{}",
                "statusCheckRollup": [{{"conclusion": "FAILURE"}}],
                "reviews": [{{"state": "APPROVED"}}]
            }}
        ]"#,
            old_time
        );

        let prs = parse_stuck_prs(&json);
        assert!(prs.is_empty(), "PRs with failing CI should not be stuck");
    }

    #[test]
    fn test_parse_stuck_prs_no_checks() {
        // PR is old with approval but no CI checks
        let old_time = (chrono::Utc::now() - chrono::Duration::minutes(20)).to_rfc3339();
        let json = format!(
            r#"[
            {{
                "number": 42,
                "title": "feat: Add widget",
                "updatedAt": "{}",
                "statusCheckRollup": [],
                "reviews": [{{"state": "APPROVED"}}]
            }}
        ]"#,
            old_time
        );

        let prs = parse_stuck_prs(&json);
        assert!(prs.is_empty(), "PRs with no CI checks should not be stuck");
    }
}
