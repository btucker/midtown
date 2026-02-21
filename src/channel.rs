//! Channel management for append-only message logs

use crate::Result;
use crate::cursor::Cursor;
use crate::message::Message;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Metadata about a channel returned by [`Channel::list()`].
///
/// Contains the channel name and whether it is archived, allowing callers
/// to choose the correct open method ([`Channel::new()`] vs
/// [`Channel::open_archived()`]) without creating ghost files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelInfo {
    /// Channel name (without `.jsonl` or `.archived.jsonl` extension)
    pub name: String,
    /// Whether this channel is archived
    pub is_archived: bool,
}

/// A channel for agent communication.
///
/// Channels use an append-only JSONL file for messages and per-agent cursor
/// tracking for read positions. File locking ensures thread-safe concurrent access.
///
/// # Channel Directory Layout
///
/// Each channel is stored as a directory under `channels/`:
/// - `channels/<name>/history/current.jsonl` — active message file
/// - `channels/<name>/history/YYYY-MM-DD.jsonl` — rotated daily archives
/// - `channels/<name>/notes/` — channel lead domain knowledge (markdown files)
/// - `channels/<name>/cursors/<session_id>.json` — per-session read positions
///
/// Archived channels use a `.archived` suffix on the directory name:
/// - `channels/<name>.archived/history/current.jsonl`
///
/// # Examples
///
/// Basic channel operations:
///
/// ```
/// # use tempfile::TempDir;
/// use midtown::{Channel, Message};
///
/// # let temp_dir = TempDir::new().unwrap();
/// # let channel = Channel::new(temp_dir.path(), "midtown").unwrap();
/// // Send messages to the channel
/// channel.send(&Message::text("alice", "Hello!")).unwrap();
/// channel.send(&Message::text("bob", "Hi there!")).unwrap();
///
/// // Read all messages
/// let messages = channel.read_all().unwrap();
/// assert_eq!(messages.len(), 2);
/// assert_eq!(messages[0].content, "Hello!");
/// ```
///
/// Cursor-based reading for incremental updates:
///
/// ```
/// # use tempfile::TempDir;
/// use midtown::{Channel, Message};
///
/// # let temp_dir = TempDir::new().unwrap();
/// # let channel = Channel::new(temp_dir.path(), "midtown").unwrap();
/// // Send initial messages
/// channel.send(&Message::text("alice", "First")).unwrap();
/// channel.send(&Message::text("bob", "Second")).unwrap();
///
/// // Agent reads all messages (moves cursor)
/// let msgs = channel.read_since_cursor("agent1", "session-abc").unwrap();
/// assert_eq!(msgs.len(), 2);
///
/// // New message arrives
/// channel.send(&Message::text("alice", "Third")).unwrap();
///
/// // Agent only sees new message
/// let new_msgs = channel.read_since_cursor("agent1", "session-abc").unwrap();
/// assert_eq!(new_msgs.len(), 1);
/// assert_eq!(new_msgs[0].content, "Third");
/// ```
/// Migrate all channels under `base_dir` from the flat JSONL layout to per-channel directories.
///
/// This is called once per `base_dir` per process (via `OnceLock`) from `Channel::new()`.
/// It is idempotent — safe to interrupt and resume.
///
/// Migrates:
/// - `channel.jsonl` → `channels/midtown/history/current.jsonl`
/// - `channel-YYYY-MM-DD.jsonl` → `channels/midtown/history/YYYY-MM-DD.jsonl`
/// - `channels/<name>.jsonl` → `channels/<name>/history/current.jsonl`
/// - `channels/<name>.archived.jsonl` → `channels/<name>.archived/history/current.jsonl`
/// - `cursors/<agent>.json` → deleted (cursors are now session-scoped)
/// - `cursors/<channel>/<agent>.json` → deleted (cursors are now session-scoped)
fn auto_migrate_channels(base_dir: &Path) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    static MIGRATED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let migrated = MIGRATED.get_or_init(|| Mutex::new(HashSet::new()));

    let key = base_dir.to_string_lossy().to_string();
    {
        let mut guard = migrated.lock().unwrap();
        if guard.contains(&key) {
            return;
        }
        guard.insert(key);
    }

    let _ = do_migrate_channels(base_dir);
}

fn do_migrate_channels(base_dir: &Path) -> std::io::Result<()> {
    let channels_dir = base_dir.join("channels");

    // 1. Migrate legacy channel.jsonl → channels/midtown/history/current.jsonl
    let legacy_midtown = base_dir.join("channel.jsonl");
    if legacy_midtown.exists() {
        let midtown_dir = channels_dir.join("midtown");
        let history_dir = midtown_dir.join("history");
        let new_file = history_dir.join("current.jsonl");
        if !new_file.exists() {
            fs::create_dir_all(&history_dir)?;
            fs::create_dir_all(midtown_dir.join("notes"))?;
            fs::create_dir_all(midtown_dir.join("cursors"))?;
            fs::rename(&legacy_midtown, &new_file)?;

            // Also migrate rotated archives: channel-YYYY-MM-DD.jsonl → history/YYYY-MM-DD.jsonl
            if let Ok(entries) = fs::read_dir(base_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if let Some(fname) = path.file_name().and_then(|s| s.to_str())
                        && fname.starts_with("channel-")
                        && fname.ends_with(".jsonl")
                    {
                        // e.g., "channel-2026-02-19.jsonl" → "2026-02-19.jsonl"
                        let archive_name = fname.trim_start_matches("channel-");
                        let dest = history_dir.join(archive_name);
                        if !dest.exists() {
                            let _ = fs::rename(&path, &dest);
                        }
                    }
                }
            }
        }
    }

    // 2. Migrate flat channel files in channels/ directory
    if channels_dir.exists() {
        let entries: Vec<_> = fs::read_dir(&channels_dir)?.flatten().collect();
        for entry in entries {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let fname = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };

            if fname.ends_with(".archived.jsonl") {
                // channels/<name>.archived.jsonl → channels/<name>.archived/history/current.jsonl
                let stem = fname.trim_end_matches(".archived.jsonl");
                let archived_dir = channels_dir.join(format!("{}.archived", stem));
                let history_dir = archived_dir.join("history");
                let new_file = history_dir.join("current.jsonl");
                if !new_file.exists() {
                    fs::create_dir_all(&history_dir)?;
                    fs::create_dir_all(archived_dir.join("notes"))?;
                    fs::create_dir_all(archived_dir.join("cursors"))?;
                    let _ = fs::rename(&path, &new_file);
                }
            } else if fname.ends_with(".jsonl") {
                // channels/<name>.jsonl → channels/<name>/history/current.jsonl
                let channel_name = fname.trim_end_matches(".jsonl");
                let channel_dir = channels_dir.join(channel_name);
                let history_dir = channel_dir.join("history");
                let new_file = history_dir.join("current.jsonl");
                fs::create_dir_all(&history_dir)?;
                fs::create_dir_all(channel_dir.join("notes"))?;
                fs::create_dir_all(channel_dir.join("cursors"))?;
                if !new_file.exists() {
                    let _ = fs::rename(&path, &new_file);
                } else {
                    // Destination already exists (e.g., step 1 migrated channel.jsonl
                    // to channels/midtown/ and now we have channels/midtown.jsonl too).
                    // Append the orphaned content and remove the source file.
                    if let Ok(content) = fs::read(&path) {
                        let mut dest = OpenOptions::new().append(true).open(&new_file)?;
                        dest.write_all(&content)?;
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
    }

    // 3. Migrate old cursors directory
    let old_cursors_base = base_dir.join("cursors");
    if old_cursors_base.exists() {
        if let Ok(entries) = fs::read_dir(&old_cursors_base) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|e| e == "json") {
                    // Legacy midtown cursor: cursors/<agent>.json — delete (cursors are ephemeral)
                    let _ = fs::remove_file(&path);
                } else if path.is_dir() {
                    // Per-channel cursor dir: cursors/<channel>/ — delete all files (ephemeral)
                    if let Ok(cursor_entries) = fs::read_dir(&path) {
                        for cursor_entry in cursor_entries.flatten() {
                            let cursor_path = cursor_entry.path();
                            if cursor_path.extension().is_some_and(|e| e == "json") {
                                let _ = fs::remove_file(&cursor_path);
                            }
                        }
                    }
                    let _ = fs::remove_dir(&path);
                }
            }
        }
        // Try to remove old cursors dir if now empty
        let _ = fs::remove_dir(&old_cursors_base);
    }

    // 4. Delete old flat cursor files channels/<channel>/cursors/<agent>.json.
    // Cursors are now session-scoped: channels/<channel>/cursors/<session_id>.json.
    // Old flat files are ephemeral (no data loss), so we delete them.
    if channels_dir.exists()
        && let Ok(channel_dirs) = fs::read_dir(&channels_dir)
    {
        for channel_entry in channel_dirs.flatten() {
            let channel_path = channel_entry.path();
            if !channel_path.is_dir() {
                continue;
            }
            let cursors_dir = channel_path.join("cursors");
            if !cursors_dir.exists() {
                continue;
            }
            if let Ok(cursor_entries) = fs::read_dir(&cursors_dir) {
                for cursor_entry in cursor_entries.flatten() {
                    let cursor_path = cursor_entry.path();
                    // Old flat format: cursors/<agent>.json (no session_id field).
                    // New session-scoped format: cursors/<session_id>.json (has session_id field).
                    // Only delete old-format cursors to avoid destroying new ones on restart.
                    if cursor_path.is_file()
                        && cursor_path.extension().is_some_and(|e| e == "json")
                        && !cursor_path
                            .to_str()
                            .is_some_and(|s| s.ends_with(".json.tmp"))
                        && let Ok(content) = fs::read_to_string(&cursor_path)
                        && !content.contains("\"session_id\"")
                    {
                        let _ = fs::remove_file(&cursor_path);
                    }
                }
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
pub struct Channel {
    /// Base directory for this channel (~/.midtown/projects/<repo>/)
    base_dir: PathBuf,
    /// Channel name (e.g., "midtown", "pr-discussion")
    channel_name: String,
    /// Path to the active history/current.jsonl file
    channel_file: PathBuf,
}

impl Channel {
    /// Create a new channel with the specified name at the base directory
    ///
    /// Creates the directory structure and channel file if they don't exist.
    /// The channel file is created eagerly so that file watchers (tailf) can
    /// immediately start monitoring it.
    ///
    /// # Channel File Layout
    ///
    /// - "midtown" channel: Uses `channel.jsonl` (legacy) or `channels/midtown.jsonl`
    /// - Other channels: Use `channels/<name>.jsonl`
    ///
    /// For backward compatibility, if `channel.jsonl` exists, it's treated as the
    /// "midtown" channel. New channels always use the `channels/` directory.
    /// Validates a channel name.
    ///
    /// Channel names must be non-empty and contain only alphanumeric characters,
    /// hyphens, and underscores. This prevents path traversal attacks and ensures
    /// filesystem compatibility.
    fn is_valid_channel_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }

    pub fn new(base_dir: impl Into<PathBuf>, channel_name: impl Into<String>) -> Result<Self> {
        let base_dir = base_dir.into();
        let channel_name = channel_name.into();

        // Validate channel name: must be non-empty and contain only
        // alphanumeric characters, hyphens, and underscores.
        if !Self::is_valid_channel_name(&channel_name) {
            return Err(crate::Error::InvalidMessage(format!(
                "Invalid channel name '{}': must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
                channel_name
            )));
        }

        // Reject names reserved for coworker sessions to prevent naming collisions.
        if crate::coworker::AVENUE_NAMES.contains(&channel_name.as_str()) {
            return Err(crate::Error::InvalidMessage(format!(
                "Channel name '{}' is reserved for coworker sessions and cannot be used as a channel name",
                channel_name
            )));
        }

        // Run one-time migration from flat JSONL layout to per-channel directories.
        // This is idempotent and only runs once per base_dir per process.
        auto_migrate_channels(&base_dir);

        // New layout: channels/<name>/history/current.jsonl
        let channel_dir = base_dir.join("channels").join(&channel_name);
        let history_dir = channel_dir.join("history");
        fs::create_dir_all(&history_dir)?;
        fs::create_dir_all(channel_dir.join("notes"))?;
        fs::create_dir_all(channel_dir.join("cursors"))?;

        let channel_file = history_dir.join("current.jsonl");

        // Create the channel file if it doesn't exist.
        // This enables file watchers like tailf to start monitoring immediately.
        // (tailf wraps `tail -f` which requires the file to exist)
        //
        // Use OpenOptions with create(true) + append(true) to avoid TOCTOU race.
        // File::create() would truncate if another process created the file between
        // the exists() check and the create() call.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&channel_file)?;

        Ok(Self {
            base_dir,
            channel_name,
            channel_file,
        })
    }

    /// Open the default "midtown" channel for a specific repository
    ///
    /// Uses ~/.midtown/projects/<repo>/ as the base directory.
    pub fn for_repo(repo: &str) -> Result<Self> {
        let base_dir = crate::paths::projects_dir_for_repo(repo);
        Self::new(base_dir, "midtown")
    }

    /// Open a named channel for a specific repository
    ///
    /// Uses ~/.midtown/projects/<repo>/ as the base directory.
    pub fn for_repo_named(repo: &str, channel_name: impl Into<String>) -> Result<Self> {
        let base_dir = crate::paths::projects_dir_for_repo(repo);
        Self::new(base_dir, channel_name)
    }

    /// Get the base directory path
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Get the channel name
    pub fn channel_name(&self) -> &str {
        &self.channel_name
    }

    /// Get the path to the active channel file (history/current.jsonl)
    pub fn channel_file_path(&self) -> &Path {
        &self.channel_file
    }

    /// Get the path to the notes directory for this channel
    ///
    /// Channel leads store domain knowledge as markdown files in this directory.
    pub fn notes_dir(&self) -> PathBuf {
        self.base_dir
            .join("channels")
            .join(&self.channel_name)
            .join("notes")
    }

    /// List all available channels in the base directory
    ///
    /// Returns channel metadata including name and archived status.
    /// Scans `channels/` for subdirectories containing `history/current.jsonl`.
    /// Active channels are plain directories; archived channels use a `.archived` suffix.
    ///
    /// # Arguments
    ///
    /// * `base_dir` - Base directory to search for channels
    /// * `include_archived` - If true, includes archived channels in the result
    /// * `project_name` - Optional project name. If provided, the channel matching this name is pinned first in the list.
    pub fn list(
        base_dir: impl Into<PathBuf>,
        include_archived: bool,
        project_name: Option<&str>,
    ) -> Result<Vec<ChannelInfo>> {
        let base_dir = base_dir.into();
        let mut channels: Vec<ChannelInfo> = Vec::new();

        let channels_dir = base_dir.join("channels");
        if channels_dir.exists() {
            for entry in fs::read_dir(&channels_dir)? {
                let entry = entry?;
                let path = entry.path();

                // Only process directories
                if !path.is_dir() {
                    continue;
                }

                let dir_name = match path.file_name().and_then(|s| s.to_str()) {
                    Some(n) => n.to_string(),
                    None => continue,
                };

                let is_archived = dir_name.ends_with(".archived");

                // Skip archived channels unless include_archived is true
                if is_archived && !include_archived {
                    continue;
                }

                // Extract the channel name (remove .archived suffix if present)
                let channel_name = if is_archived {
                    dir_name.trim_end_matches(".archived").to_string()
                } else {
                    dir_name.clone()
                };

                // Validate channel name - skip directories with invalid names
                if !Self::is_valid_channel_name(&channel_name) {
                    continue;
                }

                // A channel exists only if it has a history/current.jsonl file
                if !path.join("history").join("current.jsonl").exists() {
                    continue;
                }

                // If both active and archived directories exist for the same channel,
                // the active one wins — the channel is not considered archived.
                if let Some(existing) = channels.iter_mut().find(|c| c.name == channel_name) {
                    if !is_archived {
                        // Active directory found, override any previous archived entry
                        existing.is_archived = false;
                    }
                    // Skip duplicate (archived entry when active already exists,
                    // or second active entry)
                    continue;
                }

                channels.push(ChannelInfo {
                    name: channel_name,
                    is_archived,
                });
            }
        }

        // Sort with main project channel pinned first, then alphabetically
        channels.sort_by(|a, b| {
            if let Some(main) = project_name {
                match (a.name == main, b.name == main) {
                    (true, false) => std::cmp::Ordering::Less, // a is main → a first
                    (false, true) => std::cmp::Ordering::Greater, // b is main → b first
                    _ => a.name.cmp(&b.name),                  // neither or both → alphabetical
                }
            } else {
                a.name.cmp(&b.name) // no project name → alphabetical
            }
        });

        Ok(channels)
    }

    /// List all archived channels in the base directory
    ///
    /// Returns channel names (without `.archived` directory suffix).
    /// Scans `channels/` for directories matching `*.archived` that contain
    /// `history/current.jsonl`.
    pub fn list_archived(base_dir: impl Into<PathBuf>) -> Result<Vec<String>> {
        let base_dir = base_dir.into();
        let mut archived = Vec::new();

        let channels_dir = base_dir.join("channels");
        if channels_dir.exists() {
            for entry in fs::read_dir(&channels_dir)? {
                let entry = entry?;
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let dir_name = match path.file_name().and_then(|s| s.to_str()) {
                    Some(n) => n,
                    None => continue,
                };
                if dir_name.ends_with(".archived")
                    && path.join("history").join("current.jsonl").exists()
                {
                    let name = dir_name.trim_end_matches(".archived");
                    if Self::is_valid_channel_name(name) {
                        archived.push(name.to_string());
                    }
                }
            }
        }

        archived.sort();
        Ok(archived)
    }

    /// Create a new channel
    ///
    /// This is a convenience method that creates the channel and returns it.
    /// If the channel already exists, this just opens it.
    pub fn create(base_dir: impl Into<PathBuf>, channel_name: impl Into<String>) -> Result<Self> {
        Self::new(base_dir, channel_name)
    }

    /// Open an archived channel for reading
    ///
    /// Opens a channel that has been archived (directory renamed to `<name>.archived`).
    /// The returned Channel instance can read messages but should not be used for sending
    /// (though technically possible, it would write to the archived file).
    pub fn open_archived(
        base_dir: impl Into<PathBuf>,
        channel_name: impl Into<String>,
    ) -> Result<Self> {
        let base_dir = base_dir.into();
        let channel_name = channel_name.into();

        // Archived channels use a `.archived` suffix on the directory name
        let archived_dir = base_dir
            .join("channels")
            .join(format!("{}.archived", channel_name));
        let channel_file = archived_dir.join("history").join("current.jsonl");

        if !channel_file.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Archived channel '{}' not found", channel_name),
            )
            .into());
        }

        Ok(Self {
            base_dir,
            channel_name,
            channel_file,
        })
    }

    /// Rename a channel
    ///
    /// Renames the channel directory from `channels/<old>/` to `channels/<new>/`.
    /// This is a static method because the caller may not hold an open Channel instance.
    ///
    /// Returns an error if:
    /// - `old` is "midtown" (the main channel cannot be renamed)
    /// - `new` is an invalid channel name
    /// - `old` directory does not exist
    /// - `new` directory already exists
    pub fn rename_channel(base_dir: impl Into<PathBuf>, old: &str, new: &str) -> Result<()> {
        if old == "midtown" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Cannot rename the 'midtown' channel",
            )
            .into());
        }

        if !Self::is_valid_channel_name(new) {
            return Err(crate::Error::InvalidMessage(format!(
                "Invalid channel name '{}': must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
                new
            )));
        }

        let base_dir = base_dir.into();
        let old_dir = base_dir.join("channels").join(old);
        let new_dir = base_dir.join("channels").join(new);

        if !old_dir.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Channel '{}' does not exist", old),
            )
            .into());
        }

        if new_dir.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Channel '{}' already exists", new),
            )
            .into());
        }

        fs::rename(&old_dir, &new_dir)?;
        Ok(())
    }

    /// Mark a channel as archived
    ///
    /// Renames the channel directory from `channels/<name>/` to `channels/<name>.archived/`.
    /// Archived channels are excluded from the list() results.
    ///
    /// Returns an error if trying to archive the "midtown" channel (not allowed).
    pub fn archive(&self) -> Result<()> {
        if self.channel_name == "midtown" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Cannot archive the 'midtown' channel",
            )
            .into());
        }

        let channel_dir = self.base_dir.join("channels").join(&self.channel_name);
        let archived_dir = self
            .base_dir
            .join("channels")
            .join(format!("{}.archived", &self.channel_name));

        // Clean up any orphaned .bak dir from a previous crash between renames.
        let backup_dir = self
            .base_dir
            .join("channels")
            .join(format!("{}.archived.bak", &self.channel_name));
        if backup_dir.exists() {
            let _ = fs::remove_dir_all(&backup_dir);
        }

        if archived_dir.exists() {
            // Move old archive to a temp name so we can restore it if rename fails
            fs::rename(&archived_dir, &backup_dir)?;

            match fs::rename(&channel_dir, &archived_dir) {
                Ok(()) => {
                    // Success — remove the backup
                    let _ = fs::remove_dir_all(&backup_dir);
                }
                Err(e) => {
                    // Restore the backup
                    let _ = fs::rename(&backup_dir, &archived_dir);
                    return Err(e.into());
                }
            }
            return Ok(());
        }

        fs::rename(&channel_dir, &archived_dir)?;
        Ok(())
    }

    /// Append a message to the channel
    ///
    /// Uses file locking to ensure atomic append operations.
    /// Retries with backoff for up to 2 seconds if the lock is contended,
    /// preventing indefinite blocking when many processes write concurrently
    /// (e.g., multiple coworker hooks firing simultaneously).
    pub fn send(&self, message: &Message) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.channel_file)?;

        // Try to acquire exclusive lock with bounded retries instead of blocking
        // indefinitely. This prevents PostToolUse hooks from stalling Claude Code
        // when many coworkers contend on the channel file simultaneously.
        let mut acquired = false;
        for attempt in 0..20 {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    acquired = true;
                    break;
                }
                Err(_) if attempt < 19 => {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }

        if !acquired {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Failed to acquire channel lock after 2s",
            )
            .into());
        }

        // Set channel field if it's None (allows Message::new() to be channel-agnostic)
        let message_to_write = if message.channel.is_none() {
            let mut msg = message.clone();
            msg.channel = Some(self.channel_name.clone());
            msg
        } else {
            message.clone()
        };

        // Serialize and append
        let mut json = serde_json::to_string(&message_to_write)?;
        json.push('\n');
        file.write_all(json.as_bytes())?;
        file.sync_all()?;

        // Lock is automatically released when file is dropped
        Ok(())
    }

    /// Read all messages from the channel
    ///
    /// Messages are sorted by timestamp to ensure chronological order,
    /// regardless of the order they were written to the file.
    ///
    /// Uses bounded retries to acquire a shared lock when there's lock contention.
    /// Retries up to 10 times with 50ms delays (500ms total) to handle transient
    /// lock contention from writers. Returns an error if the lock can't be acquired
    /// after 500ms.
    pub fn read_all(&self) -> Result<Vec<Message>> {
        if !self.channel_file.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.channel_file)?;

        // Try to acquire shared lock with bounded retries. In high-concurrency scenarios
        // (e.g., E2E tests), a write lock may be held briefly after a write completes due
        // to OS-level file handle cleanup. Retry with short delays to avoid spurious failures.
        for attempt in 0..10 {
            match file.try_lock_shared() {
                Ok(()) => break,
                Err(_) if attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!(
                            "Failed to acquire shared lock after {} attempts: {}",
                            attempt + 1,
                            e
                        ),
                    )
                    .into());
                }
            }
        }

        let reader = BufReader::new(file);
        let mut messages = Vec::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            if !line.trim().is_empty() {
                match serde_json::from_str::<Message>(&line) {
                    Ok(message) => messages.push(message),
                    Err(e) => {
                        // Skip malformed lines rather than failing completely.
                        // This allows reading the channel even if some lines are corrupted.
                        tracing::warn!(
                            "Skipping malformed line {} in channel file: {}",
                            line_num + 1,
                            e
                        );
                    }
                }
            }
        }

        // Sort by timestamp to ensure chronological order
        messages.sort_by_key(|m| m.timestamp);

        Ok(messages)
    }

    /// Read messages since the agent session's cursor position
    ///
    /// Returns new messages and updates the cursor. Cursors are scoped to
    /// `(agent, session_id)` so concurrent or successive sessions with the
    /// same agent name don't share state.
    ///
    /// Uses a non-blocking lock to avoid blocking when there's lock contention.
    /// If the lock can't be acquired immediately, returns an error.
    pub fn read_since_cursor(&self, agent: &str, session_id: &str) -> Result<Vec<Message>> {
        if !self.channel_file.exists() {
            return Ok(Vec::new());
        }

        let mut cursor =
            Cursor::load_or_create(&self.base_dir, &self.channel_name, agent, session_id)?;

        let file = File::open(&self.channel_file)?;

        // Try to acquire shared lock with bounded retries to handle lock contention.
        let mut acquired = false;
        for attempt in 0..10 {
            match file.try_lock_shared() {
                Ok(()) => {
                    acquired = true;
                    break;
                }
                Err(_) if attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!("Failed to acquire shared lock after 500ms: {}", e),
                    )
                    .into());
                }
            }
        }

        if !acquired {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Failed to acquire shared lock after 500ms",
            )
            .into());
        }

        let mut reader = BufReader::new(file);

        // Seek to cursor position
        reader.seek(SeekFrom::Start(cursor.position))?;

        let mut messages = Vec::new();
        let mut last_id = cursor.last_message_id.clone();
        let mut current_position = cursor.position;
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            let bytes_read = reader.read_line(&mut line_buf)?;
            if bytes_read == 0 {
                break; // EOF
            }
            current_position += bytes_read as u64;

            let line = line_buf.trim();
            if !line.is_empty() {
                match serde_json::from_str::<Message>(line) {
                    Ok(message) => {
                        last_id = Some(message.id.clone());
                        messages.push(message);
                    }
                    Err(e) => {
                        // Skip malformed lines rather than failing completely.
                        // This allows reading the channel even if some lines are corrupted.
                        tracing::warn!(
                            "Skipping malformed line at position {} in channel file: {}",
                            current_position - bytes_read as u64,
                            e
                        );
                    }
                }
            }
        }

        // Update cursor position
        cursor.update(current_position, last_id);
        cursor.save(&self.base_dir, &self.channel_name)?;

        Ok(messages)
    }

    /// Get the current cursor for an agent session
    pub fn get_cursor(&self, agent: &str, session_id: &str) -> Result<Cursor> {
        Cursor::load_or_create(&self.base_dir, &self.channel_name, agent, session_id)
    }

    /// Reset an agent session's cursor to the beginning
    pub fn reset_cursor(&self, agent: &str, session_id: &str) -> Result<()> {
        let mut cursor =
            Cursor::load_or_create(&self.base_dir, &self.channel_name, agent, session_id)?;
        cursor.reset();
        cursor.save(&self.base_dir, &self.channel_name)?;
        Ok(())
    }

    /// Set an agent session's cursor to the end of the file
    ///
    /// This is useful after initial load to ensure subsequent reads only
    /// pick up new messages. For new sessions that should not replay
    /// historical messages, call this before the first `read_since_cursor`.
    pub fn set_cursor_to_end(&self, agent: &str, session_id: &str) -> Result<()> {
        // Read the last message ID so that unread-count calculations
        // (which key off last_message_id) correctly treat the channel as fully read.
        let last_message_id = self
            .read_last_n_messages(1)
            .ok()
            .and_then(|(msgs, _)| msgs.into_iter().next().map(|m| m.id));
        let mut cursor =
            Cursor::load_or_create(&self.base_dir, &self.channel_name, agent, session_id)?;
        cursor.update(self.file_size(), last_message_id);
        cursor.save(&self.base_dir, &self.channel_name)?;
        Ok(())
    }

    /// Get the total number of messages in the channel
    pub fn message_count(&self) -> Result<usize> {
        Ok(self.read_all()?.len())
    }

    /// Check if the channel file exists
    pub fn exists(&self) -> bool {
        self.channel_file.exists()
    }

    /// Get the current file size in bytes
    ///
    /// This is a cheap O(1) operation that can be used to detect if new
    /// messages have been added without reading the entire file.
    /// Returns 0 if the file doesn't exist.
    pub fn file_size(&self) -> u64 {
        fs::metadata(&self.channel_file)
            .map(|m| m.len())
            .unwrap_or(0)
    }

    /// Read the last N messages from the channel
    ///
    /// Returns a tuple of (messages, start_position) where start_position is
    /// the byte offset where these messages begin. This can be used for
    /// subsequent calls to load more history.
    ///
    /// If the channel has fewer than N messages, returns all messages with
    /// start_position = 0.
    pub fn read_last_n_messages(&self, n: usize) -> Result<(Vec<Message>, u64)> {
        if !self.channel_file.exists() {
            return Ok((Vec::new(), 0));
        }

        let file = File::open(&self.channel_file)?;

        // Try to acquire shared lock with bounded retries to handle lock contention.
        // This prevents failures when a writer is still releasing its exclusive lock.
        let mut acquired = false;
        for attempt in 0..10 {
            match file.try_lock_shared() {
                Ok(()) => {
                    acquired = true;
                    break;
                }
                Err(_) if attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!("Failed to acquire shared lock after 500ms: {}", e),
                    )
                    .into());
                }
            }
        }

        if !acquired {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Failed to acquire shared lock after 500ms",
            )
            .into());
        }

        let file_size = file.metadata()?.len();
        if file_size == 0 {
            return Ok((Vec::new(), 0));
        }

        // Strategy: estimate where to start reading to get ~N messages.
        // Average JSONL line is ~200 bytes, so estimate N*300 bytes from end
        // to have some buffer.
        let estimated_bytes = (n as u64) * 300;
        let start_estimate = file_size.saturating_sub(estimated_bytes);

        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(start_estimate))?;

        // If we're not at the start, skip to next newline to avoid partial line
        let mut actual_start = start_estimate;
        if start_estimate > 0 {
            let mut skip_buf = String::new();
            let bytes_skipped = reader.read_line(&mut skip_buf)?;
            actual_start = start_estimate + bytes_skipped as u64;
        }

        // Read all remaining lines
        let mut all_messages = Vec::new();
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let bytes_read = reader.read_line(&mut line_buf)?;
            if bytes_read == 0 {
                break;
            }
            let line = line_buf.trim();
            if !line.is_empty()
                && let Ok(message) = serde_json::from_str::<Message>(line)
            {
                all_messages.push(message);
            }
        }

        // If we got more than N messages, keep only the last N
        // and recalculate the actual start position
        if all_messages.len() > n {
            let to_skip = all_messages.len() - n;
            all_messages = all_messages.split_off(to_skip);
            // Signal that there's more history available by using non-zero position
            // (0 means "all history loaded", non-zero means "more history exists")
            actual_start = actual_start.max(1);
        } else if start_estimate == 0 {
            actual_start = 0;
        }

        // Sort by timestamp
        all_messages.sort_by_key(|m| m.timestamp);

        Ok((all_messages, actual_start))
    }

    /// Read messages before a given byte position (for loading history)
    ///
    /// Reads up to N messages that appear before the specified position.
    /// Returns (messages, new_start_position). If new_start_position is 0,
    /// all history has been loaded.
    pub fn read_messages_before_position(
        &self,
        position: u64,
        n: usize,
    ) -> Result<(Vec<Message>, u64)> {
        if !self.channel_file.exists() || position == 0 {
            return Ok((Vec::new(), 0));
        }

        let file = File::open(&self.channel_file)?;

        // Try to acquire shared lock with bounded retries to handle lock contention.
        let mut acquired = false;
        for attempt in 0..10 {
            match file.try_lock_shared() {
                Ok(()) => {
                    acquired = true;
                    break;
                }
                Err(_) if attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!("Failed to acquire shared lock after 500ms: {}", e),
                    )
                    .into());
                }
            }
        }

        if !acquired {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Failed to acquire shared lock after 500ms",
            )
            .into());
        }

        // Estimate where to start reading
        let estimated_bytes = (n as u64) * 300;
        let start_estimate = position.saturating_sub(estimated_bytes);

        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(start_estimate))?;

        // Skip partial line if not at start
        let mut actual_start = start_estimate;
        if start_estimate > 0 {
            let mut skip_buf = String::new();
            let bytes_skipped = reader.read_line(&mut skip_buf)?;
            actual_start = start_estimate + bytes_skipped as u64;
        }

        // Read lines until we reach the target position
        let mut messages = Vec::new();
        let mut line_buf = String::new();
        let mut current_pos = actual_start;

        loop {
            if current_pos >= position {
                break;
            }
            line_buf.clear();
            let bytes_read = reader.read_line(&mut line_buf)?;
            if bytes_read == 0 {
                break;
            }
            current_pos += bytes_read as u64;

            let line = line_buf.trim();
            if !line.is_empty()
                && let Ok(message) = serde_json::from_str::<Message>(line)
            {
                messages.push(message);
            }
        }

        // Keep only last N messages if we got more
        let final_start = if messages.len() > n {
            let to_skip = messages.len() - n;
            messages = messages.split_off(to_skip);
            // There's still more history
            actual_start.max(1) // Non-zero means more history available
        } else if start_estimate == 0 {
            0 // All history loaded
        } else {
            actual_start
        };

        // Sort by timestamp
        messages.sort_by_key(|m| m.timestamp);

        Ok((messages, final_start))
    }

    /// Rotate the channel log file.
    ///
    /// Keeps messages from the last `retain_minutes` in channel.jsonl and
    /// archives everything older to `channel-YYYY-MM-DD.jsonl`. If the archive
    /// file already exists, older messages are appended to it.
    ///
    /// After rotation, all agent cursors are reset to 0 because byte positions
    /// in the channel file have changed.
    ///
    /// Returns the number of messages archived, or 0 if no rotation was needed.
    pub fn rotate(&self, retain_minutes: i64) -> Result<usize> {
        use chrono::Utc;

        if !self.channel_file.exists() {
            return Ok(0);
        }

        // Read all messages under exclusive lock
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.channel_file)?;
        file.lock_exclusive()?;

        let reader = BufReader::new(&file);
        let mut all_messages = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty()
                && let Ok(msg) = serde_json::from_str::<Message>(&line)
            {
                all_messages.push(msg);
            }
        }

        if all_messages.is_empty() {
            return Ok(0);
        }

        // Sort by timestamp for correct partitioning
        all_messages.sort_by_key(|m| m.timestamp);

        let cutoff = Utc::now() - chrono::Duration::minutes(retain_minutes);

        let (archive, retain): (Vec<_>, Vec<_>) =
            all_messages.into_iter().partition(|m| m.timestamp < cutoff);

        if archive.is_empty() {
            return Ok(0);
        }

        let archived_count = archive.len();

        // Write archived messages to channels/<name>/history/YYYY-MM-DD.jsonl
        let date_str = Utc::now().format("%Y-%m-%d").to_string();
        let history_dir = self
            .base_dir
            .join("channels")
            .join(&self.channel_name)
            .join("history");
        let archive_file_path = history_dir.join(format!("{}.jsonl", date_str));

        {
            let mut archive_file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&archive_file_path)?;

            for msg in &archive {
                let mut json = serde_json::to_string(msg)?;
                json.push('\n');
                archive_file.write_all(json.as_bytes())?;
            }
            archive_file.sync_all()?;
        }

        // Write retained messages to a temp file, then rename over channel.jsonl
        let temp_path = self.channel_file.with_extension("jsonl.rotating");
        {
            let mut temp_file = File::create(&temp_path)?;
            for msg in &retain {
                let mut json = serde_json::to_string(msg)?;
                json.push('\n');
                temp_file.write_all(json.as_bytes())?;
            }
            temp_file.sync_all()?;
        }

        // Atomic replace: rename temp over the channel file
        // Note: on Unix, rename is atomic within the same filesystem.
        crate::paths::atomic_rename(&temp_path, &self.channel_file)?;

        // Reset all cursor files for this channel since byte positions have changed.
        // Cursors live in channels/<name>/cursors/<session_id>.json.
        let channel_cursors_dir = self
            .base_dir
            .join("channels")
            .join(&self.channel_name)
            .join("cursors");
        if channel_cursors_dir.exists()
            && let Ok(cursor_files) = fs::read_dir(&channel_cursors_dir)
        {
            for entry in cursor_files.flatten() {
                let cursor_path = entry.path();
                if cursor_path.is_file()
                    && cursor_path.extension().is_some_and(|e| e == "json")
                    && !cursor_path
                        .to_str()
                        .is_some_and(|s| s.ends_with(".json.tmp"))
                    && let Ok(mut cursor) = crate::cursor::Cursor::load(&cursor_path)
                {
                    cursor.reset();
                    let _ = cursor.save(&self.base_dir, &self.channel_name);
                }
            }
        }

        // Lock released when `file` is dropped
        Ok(archived_count)
    }

    /// Check if the channel needs rotation based on the age of the oldest message.
    ///
    /// Returns true if the oldest message is older than `max_age_hours`.
    pub fn needs_rotation(&self, max_age_hours: u64) -> bool {
        use chrono::Utc;

        if !self.channel_file.exists() {
            return false;
        }

        // Quick check: read just the first non-empty line
        let file = match File::open(&self.channel_file) {
            Ok(f) => f,
            Err(_) => return false,
        };

        if file.try_lock_shared().is_err() {
            return false;
        }

        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => return false,
            };
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(msg) = serde_json::from_str::<Message>(&line) {
                let age = Utc::now() - msg.timestamp;
                return age.num_hours() >= max_age_hours as i64;
            }
            // If first line can't be parsed, skip it
        }

        false
    }
}

/// Router for managing multiple channels with lazy initialization.
///
/// ChannelRouter maintains a cache of Channel instances, opening them on-demand
/// when a message is routed to a specific channel. This enables multi-channel
/// message routing without pre-opening all possible channels at startup.
///
/// # Examples
///
/// ```
/// use midtown::{ChannelRouter, Message};
///
/// let unique = std::time::SystemTime::now()
///     .duration_since(std::time::UNIX_EPOCH)
///     .unwrap()
///     .as_nanos();
/// let base_dir = std::env::temp_dir().join(format!("midtown-channel-router-doc-{unique}"));
/// let router = ChannelRouter::new(base_dir, "midtown");
///
/// // Send to main channel (uses default repo name)
/// let msg1 = Message::text("agent1", "Hello");
/// router.send(&msg1).unwrap();
///
/// // Send to a topic channel (lazy-opens "pr-42" channel)
/// let msg2 = Message::for_channel("pr-42", "agent1", "Review feedback", midtown::MessageType::Text);
/// router.send(&msg2).unwrap();
/// ```
pub struct ChannelRouter {
    /// Base directory for all channels
    base_dir: PathBuf,
    /// Default channel name (repo name)
    default_channel_name: String,
    /// Cache of opened channels (Arc-wrapped for shared cache across clones)
    channels: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Channel>>>,
}

impl ChannelRouter {
    /// Create a new ChannelRouter with the given base directory and default channel name.
    ///
    /// The default channel name is typically the repository name (e.g., "midtown").
    pub fn new(base_dir: impl Into<PathBuf>, default_channel_name: impl Into<String>) -> Self {
        Self {
            base_dir: base_dir.into(),
            default_channel_name: default_channel_name.into(),
            channels: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Get the base directory path for channels managed by this router.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Send a message to the appropriate channel based on Message.channel field.
    ///
    /// If the message's channel is None or empty, uses the default channel name.
    /// Channels are opened lazily on first use and cached for subsequent sends.
    pub fn send(&self, message: &Message) -> Result<()> {
        // Use the message's channel if set, otherwise use the router's default channel
        let channel_name = message
            .channel
            .as_deref()
            .unwrap_or(&self.default_channel_name);

        // Fast path: check if channel is already open
        {
            let channels = self.channels.lock().unwrap();
            if let Some(channel) = channels.get(channel_name) {
                return channel.send(message);
            }
        }

        // Slow path: open channel and cache it
        let mut channels = self.channels.lock().unwrap();
        // Double-check after acquiring exclusive lock (another thread may have opened it)
        if let Some(channel) = channels.get(channel_name) {
            return channel.send(message);
        }

        // Open new channel
        let channel = Channel::new(&self.base_dir, channel_name)?;
        // Cache the channel before attempting send - the Channel itself is valid
        // even if the subsequent write fails (filesystem error, permissions, etc).
        // The Channel holds no mutable state, so caching a channel that failed
        // a write is safe - the next send() will retry the write.
        channels.insert(channel_name.to_string(), channel.clone());
        channel.send(message)
    }

    /// Get or create a channel by name.
    ///
    /// Returns a clone of the cached Channel. Channels are opened lazily on first access.
    pub fn get_channel(&self, channel_name: &str) -> Result<Channel> {
        // Fast path: check if channel is already open
        {
            let channels = self.channels.lock().unwrap();
            if let Some(channel) = channels.get(channel_name) {
                return Ok(channel.clone());
            }
        }

        // Slow path: open channel and cache it
        let mut channels = self.channels.lock().unwrap();
        // Double-check after acquiring exclusive lock
        if let Some(channel) = channels.get(channel_name) {
            return Ok(channel.clone());
        }

        let channel = Channel::new(&self.base_dir, channel_name)?;
        channels.insert(channel_name.to_string(), channel.clone());
        Ok(channel)
    }

    /// Get the default channel name.
    pub fn default_channel_name(&self) -> &str {
        &self.default_channel_name
    }

    /// Get the default channel (uses default_channel_name).
    pub fn default_channel(&self) -> Result<Channel> {
        self.get_channel(&self.default_channel_name)
    }

    /// List all currently open (cached) channel names.
    ///
    /// Does not scan the filesystem - only returns channels that have been
    /// opened during this router's lifetime.
    pub fn open_channels(&self) -> Vec<String> {
        let channels = self.channels.lock().unwrap();
        channels.keys().cloned().collect()
    }

    /// Remove a channel from the cache by name.
    ///
    /// Used after renaming or archiving a channel to evict stale cache entries.
    /// Subsequent sends to the removed channel name will re-open the channel from disk.
    pub fn remove_channel(&self, channel_name: &str) {
        let mut channels = self.channels.lock().unwrap();
        channels.remove(channel_name);
    }
}

impl Clone for ChannelRouter {
    fn clone(&self) -> Self {
        Self {
            base_dir: self.base_dir.clone(),
            default_channel_name: self.default_channel_name.clone(),
            // Arc::clone shares the same cache across clones
            channels: std::sync::Arc::clone(&self.channels),
        }
    }
}

#[path = "channel_tests.rs"]
#[cfg(test)]
mod tests;
