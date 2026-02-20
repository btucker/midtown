//! Agent teams mailbox writer for delivering messages to Claude Code coworkers.
//!
//! Implements the Claude Code agent teams inbox protocol: messages are written
//! as JSON arrays to `~/.claude/teams/{team-name}/inboxes/{agent-name}.json`.
//!
//! Concurrency is handled via mkdir-based locking (`.lock` directory), matching
//! the `lockfile` npm package used by Claude Code's reader. This ensures the
//! daemon and Claude Code never corrupt the inbox file with concurrent access.
//!
//! # Delivery model
//!
//! Messages written to the inbox are polled by Claude Code between turns
//! (during "attachment collection"). This means:
//! - Delivery latency is bounded by the agent's turn duration
//! - Messages are safe to write during tool execution (no terminal corruption)
//! - No retry logic is needed — the file is durable until read

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

/// A message in the agent teams inbox format.
///
/// Matches the schema expected by Claude Code's `readUnreadMessages()` function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxMessage {
    /// The message content (plain text or JSON-encoded protocol message).
    pub text: String,
    /// Sender's agent name.
    pub from: String,
    /// Sender's display color (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// Whether the message has been consumed by the recipient.
    pub read: bool,
    /// Brief summary for UI preview (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl MailboxMessage {
    /// Create a new unread message with the current timestamp.
    pub fn new(text: impl Into<String>, from: impl Into<String>) -> Self {
        MailboxMessage {
            text: text.into(),
            from: from.into(),
            color: None,
            timestamp: Utc::now().to_rfc3339(),
            read: false,
            summary: None,
        }
    }

    /// Set the display color.
    pub fn with_color(mut self, color: impl Into<String>) -> Self {
        self.color = Some(color.into());
        self
    }

    /// Set the UI summary.
    pub fn with_summary(mut self, summary: impl Into<String>) -> Self {
        self.summary = Some(summary.into());
        self
    }
}

/// Resolve the teams directory for a given team name.
///
/// Returns `~/.claude/teams/{team-name}/`.
fn teams_dir(team_name: &str) -> PathBuf {
    dirs::home_dir()
        .expect("home directory must exist")
        .join(".claude")
        .join("teams")
        .join(team_name)
}

/// Resolve the inbox file path for an agent in a team.
///
/// Returns `~/.claude/teams/{team-name}/inboxes/{agent-name}.json`.
fn inbox_path(team_name: &str, agent_name: &str) -> PathBuf {
    teams_dir(team_name)
        .join("inboxes")
        .join(format!("{}.json", agent_name))
}

/// Resolve the lock path for an agent's inbox.
///
/// Returns `~/.claude/teams/{team-name}/inboxes/{agent-name}.json.lock`.
/// This is a *directory* lock matching the `lockfile` npm package protocol.
fn lock_path(team_name: &str, agent_name: &str) -> PathBuf {
    teams_dir(team_name)
        .join("inboxes")
        .join(format!("{}.json.lock", agent_name))
}

/// RAII guard for a mkdir-based lock.
///
/// The `lockfile` npm package creates a directory as a lock indicator.
/// `mkdir` is atomic on POSIX, so only one process succeeds. The lock is
/// released by removing the directory.
struct MkdirLock {
    path: PathBuf,
}

impl MkdirLock {
    /// Attempt to acquire the lock with retries.
    ///
    /// Tries `mkdir` up to `max_retries` times with a short sleep between
    /// attempts. Returns an error if the lock cannot be acquired.
    fn acquire(path: &Path, max_retries: u32) -> std::io::Result<Self> {
        for attempt in 0..max_retries {
            match fs::create_dir(path) {
                Ok(()) => {
                    return Ok(MkdirLock {
                        path: path.to_path_buf(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Check if the lock is stale (older than 30 seconds).
                    // The lockfile npm package has a default stale timeout.
                    if let Ok(metadata) = fs::metadata(path)
                        && let Ok(modified) = metadata.modified()
                        && modified.elapsed().unwrap_or_default()
                            > std::time::Duration::from_secs(30)
                    {
                        warn!("Removing stale lock at {} (age > 30s)", path.display());
                        let _ = fs::remove_dir_all(path);
                        // Try again immediately after removing stale lock
                        if fs::create_dir(path).is_ok() {
                            return Ok(MkdirLock {
                                path: path.to_path_buf(),
                            });
                        }
                    }

                    if attempt < max_retries - 1 {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            format!(
                "Failed to acquire lock at {} after {} retries",
                path.display(),
                max_retries
            ),
        ))
    }
}

impl Drop for MkdirLock {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.path) {
            warn!("Failed to release lock at {}: {}", self.path.display(), e);
        }
    }
}

/// Write a message to a coworker's inbox.
///
/// Acquires a mkdir-based lock, reads the existing inbox (or creates an empty
/// one), appends the new message, and writes back the full JSON array.
pub fn write_to_inbox(
    team_name: &str,
    agent_name: &str,
    message: MailboxMessage,
) -> std::io::Result<()> {
    let inbox = inbox_path(team_name, agent_name);
    let lock = lock_path(team_name, agent_name);

    // Ensure the inboxes directory exists
    if let Some(parent) = inbox.parent() {
        fs::create_dir_all(parent)?;
    }

    // Acquire mkdir lock (matching lockfile npm protocol)
    let _lock = MkdirLock::acquire(&lock, 20)?;

    // Read existing messages or start with empty array
    let mut messages: Vec<MailboxMessage> = if inbox.exists() {
        let content = fs::read_to_string(&inbox)?;
        serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!(
                "Failed to parse inbox at {}: {} — starting fresh",
                inbox.display(),
                e
            );
            vec![]
        })
    } else {
        vec![]
    };

    debug!(
        "Writing message to inbox {}: from={}, existing_count={}",
        inbox.display(),
        message.from,
        messages.len()
    );

    messages.push(message);

    // Write atomically via temp file + rename
    let tmp_path = inbox.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_string_pretty(&messages)?)?;
    crate::paths::atomic_rename(&tmp_path, &inbox)?;

    Ok(())
}

/// Build the team name from a repository name.
///
/// Format: `midtown-{repo_name}` (matches `task_list_id_for_repo`).
pub fn team_name_for_repo(repo_name: &str) -> String {
    format!("midtown-{}", repo_name)
}

/// Build the agent ID from a coworker name and team name.
///
/// Format: `{name}@{team_name}` (matches Claude Code's `QU()` function).
pub fn agent_id(name: &str, team_name: &str) -> String {
    format!("{}@{}", name, team_name)
}

/// A member entry in the team config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    pub name: String,
    pub agent_id: String,
    pub agent_type: String,
}

/// Team configuration written to `~/.claude/teams/{team-name}/config.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamConfig {
    pub members: Vec<TeamMember>,
}

/// Ensure the team directory structure exists and write/update the team config.
///
/// Creates:
/// - `~/.claude/teams/{team-name}/config.json`
/// - `~/.claude/teams/{team-name}/inboxes/`
///
/// If the config already exists, it is overwritten with the new members list.
/// Uses a mkdir-based lock on `config.json.lock` to prevent concurrent writes
/// from corrupting the file, and atomic write (temp file + rename) to ensure
/// readers never see partial content.
pub fn ensure_team_config(team_name: &str, members: &[TeamMember]) -> std::io::Result<()> {
    let team_dir = teams_dir(team_name);
    ensure_team_config_at(&team_dir, members)
}

/// Internal implementation that accepts a team directory path (testable).
fn ensure_team_config_at(team_dir: &Path, members: &[TeamMember]) -> std::io::Result<()> {
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir)?;

    let config = TeamConfig {
        members: members.to_vec(),
    };
    let config_path = team_dir.join("config.json");
    let lock_path = team_dir.join("config.json.lock");

    // Acquire mkdir lock to prevent concurrent writes from corrupting the file
    let _lock = MkdirLock::acquire(&lock_path, 20)?;

    // Write atomically via temp file + rename
    let tmp_path = config_path.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_string_pretty(&config)?)?;
    fs::rename(&tmp_path, &config_path)?;

    debug!(
        "Wrote team config to {} with {} members",
        config_path.display(),
        members.len()
    );

    Ok(())
}

/// Add or update a single member in the team config.
///
/// Reads the existing config (if any), upserts the member by name, and writes
/// back. Creates the team directory and inboxes/ if they don't exist.
///
/// Uses a mkdir-based lock on `config.json.lock` to prevent concurrent spawns
/// from clobbering each other's member entries.
pub fn upsert_team_member(team_name: &str, member: TeamMember) -> std::io::Result<()> {
    let team_dir = teams_dir(team_name);
    upsert_team_member_at(&team_dir, member)
}

/// Internal implementation that accepts a team directory path (testable).
fn upsert_team_member_at(team_dir: &Path, member: TeamMember) -> std::io::Result<()> {
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir)?;

    let config_path = team_dir.join("config.json");
    let lock_path = team_dir.join("config.json.lock");

    // Acquire mkdir lock to protect the read-modify-write cycle
    let _lock = MkdirLock::acquire(&lock_path, 20)?;

    // Read existing config or start fresh
    let mut config: TeamConfig = if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        serde_json::from_str(&content).unwrap_or(TeamConfig { members: vec![] })
    } else {
        TeamConfig { members: vec![] }
    };

    // Upsert: replace existing member with same name, or append
    if let Some(existing) = config.members.iter_mut().find(|m| m.name == member.name) {
        *existing = member;
    } else {
        config.members.push(member);
    }

    // Write atomically via temp file + rename
    let tmp_path = config_path.with_extension("json.tmp");
    fs::write(&tmp_path, serde_json::to_string_pretty(&config)?)?;
    crate::paths::atomic_rename(&tmp_path, &config_path)?;

    debug!(
        "Updated team config at {} (now {} members)",
        config_path.display(),
        config.members.len()
    );

    Ok(())
}

/// Clear the inbox file for a named agent.
///
/// Called when a name is allocated to a new session (before spawn) and when a
/// session releases its name (on shutdown). This prevents a newly-allocated
/// session from inheriting the previous session's unread inbox messages.
///
/// If no inbox file exists this is a no-op (first allocation of the name).
pub fn clear_inbox(team_name: &str, agent_name: &str) -> std::io::Result<()> {
    let inboxes_dir = teams_dir(team_name).join("inboxes");
    clear_inbox_at(&inboxes_dir, agent_name)
}

/// Internal implementation that accepts an inboxes directory path (testable).
fn clear_inbox_at(inboxes_dir: &Path, agent_name: &str) -> std::io::Result<()> {
    let inbox = inboxes_dir.join(format!("{}.json", agent_name));
    let lock = inboxes_dir.join(format!("{}.json.lock", agent_name));

    // Acquire the same mkdir lock used by write_to_inbox to prevent races
    // where a concurrent write could recreate the file after we delete it.
    let _lock = MkdirLock::acquire(&lock, 20)?;

    match fs::remove_file(&inbox) {
        Ok(()) => {
            debug!("Cleared inbox for {} at {}", agent_name, inbox.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Clean up a team directory (remove config and inboxes).
///
/// Called when the daemon shuts down to avoid stale team state.
pub fn cleanup_team(team_name: &str) -> std::io::Result<()> {
    let team_dir = teams_dir(team_name);
    if team_dir.exists() {
        fs::remove_dir_all(&team_dir)?;
        debug!("Cleaned up team directory at {}", team_dir.display());
    }
    Ok(())
}

#[path = "mailbox_tests.rs"]
#[cfg(test)]
mod tests;
