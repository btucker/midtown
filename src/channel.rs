//! Channel management for append-only message logs

use crate::Result;
use crate::cursor::Cursor;
use crate::message::Message;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    /// Whether this is a direct-message channel (name starts with "dm-")
    pub is_dm: bool,
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
        cleanup_archived_channel_conflicts(&base_dir);

        // New layout: channels/<name>/history/current.jsonl
        let channels_dir = base_dir.join("channels");
        let channel_dir = channels_dir.join(&channel_name);
        let archived_dir = channels_dir.join(format!("{}.archived", channel_name));
        if archived_dir.exists() {
            return Err(crate::Error::ChannelArchived(channel_name.clone()));
        }
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

    /// Open the default channel for a specific repository
    ///
    /// The default channel shares the repository name (e.g., "offload" for the offload project).
    /// Uses ~/.midtown/projects/<repo>/ as the base directory.
    pub fn for_repo(repo: &str) -> Result<Self> {
        let base_dir = crate::paths::projects_dir_for_repo(repo);
        Self::new(base_dir, repo)
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

    /// Get the path to the history directory for this channel.
    ///
    /// Derived from `channel_file` (which is always `…/history/current.jsonl`)
    /// so it works for both active and archived channels.
    fn history_dir(&self) -> PathBuf {
        self.channel_file
            .parent()
            .unwrap_or(Path::new("."))
            .to_path_buf()
    }

    /// List all `.jsonl` history files in chronological order.
    ///
    /// Returns date-named archives (`YYYY-MM-DD.jsonl`) sorted ascending,
    /// followed by `current.jsonl` last. Excludes temp files (`.rotating`).
    fn list_all_history_files(&self) -> Vec<PathBuf> {
        let history_dir = self.history_dir();
        let mut dated_files: Vec<PathBuf> = Vec::new();
        let mut has_current = false;

        if let Ok(entries) = fs::read_dir(&history_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "jsonl")
                    && let Some(name) = path.file_name().and_then(|n| n.to_str())
                {
                    if name == "current.jsonl" {
                        has_current = true;
                    } else if !name.contains(".rotating") {
                        dated_files.push(path);
                    }
                }
            }
        }

        // Sort date-named files ascending (oldest first)
        dated_files.sort();

        // Append current.jsonl last (it always has the newest messages)
        if has_current {
            dated_files.push(self.channel_file.clone());
        }

        dated_files
    }

    /// Read all messages from a single JSONL file, skipping malformed lines.
    ///
    /// Blocks the calling thread during lock retries (`std::thread::sleep`).
    /// In async contexts, prefer [`read_messages_from_file_async`] instead.
    fn read_messages_from_file(path: &Path) -> Result<Vec<Message>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(path)?;

        // Try to acquire shared lock with bounded retries
        for attempt in 0..10 {
            match file.try_lock_shared() {
                Ok(()) => break,
                Err(e) if attempt == 9 => {
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
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }

        Self::parse_messages_from_file(file, path)
    }

    /// Async variant of [`read_messages_from_file`] that uses `tokio::time::sleep`
    /// instead of `std::thread::sleep` to avoid blocking the tokio runtime during
    /// lock retries.
    async fn read_messages_from_file_async(path: &Path) -> Result<Vec<Message>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(path)?;

        // Try to acquire shared lock with bounded retries (async-friendly)
        for attempt in 0..10 {
            match file.try_lock_shared() {
                Ok(()) => break,
                Err(e) if attempt == 9 => {
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
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }

        Self::parse_messages_from_file(file, path)
    }

    /// Parse messages from an already-opened and locked file.
    fn parse_messages_from_file(file: File, path: &Path) -> Result<Vec<Message>> {
        let reader = BufReader::new(file);
        let mut messages = Vec::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            if !line.trim().is_empty() {
                match serde_json::from_str::<Message>(&line) {
                    Ok(message) => messages.push(message),
                    Err(e) => {
                        tracing::warn!(
                            "Skipping malformed line {} in {}: {}",
                            line_num + 1,
                            path.display(),
                            e
                        );
                    }
                }
            }
        }

        Ok(messages)
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
        cleanup_archived_channel_conflicts(&base_dir);
        let mut channels: Vec<ChannelInfo> = Vec::new();
        let mut channel_map: HashMap<String, ChannelInfo> = HashMap::new();

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

                // If an archived sibling exists (with real history), prefer it over the active dir.
                if !is_archived {
                    let archived_dir = channels_dir.join(format!("{}.archived", channel_name));
                    if archived_dir.join("history").join("current.jsonl").exists() {
                        // Skip this active entry — the archived directory is the source of truth.
                        continue;
                    }
                }

                // A channel exists only if it has a history/current.jsonl file
                if !path.join("history").join("current.jsonl").exists() {
                    continue;
                }

                let entry = channel_map
                    .entry(channel_name.clone())
                    .or_insert(ChannelInfo {
                        name: channel_name.clone(),
                        is_archived,
                        is_dm: channel_name.starts_with("dm-"),
                    });
                if is_archived {
                    entry.is_archived = true;
                }
            }
        }

        channels.extend(channel_map.into_values());

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
        cleanup_archived_channel_conflicts(&base_dir);
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
    /// - `old` is `main_channel_name` (the main channel cannot be renamed)
    /// - `new` is an invalid channel name
    /// - `old` directory does not exist
    /// - `new` directory already exists
    pub fn rename_channel(
        base_dir: impl Into<PathBuf>,
        old: &str,
        new: &str,
        main_channel_name: &str,
    ) -> Result<()> {
        if old == main_channel_name {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Cannot rename the '{}' channel", main_channel_name),
            )
            .into());
        }

        // Validate the old name to prevent path traversal (e.g., "../worktrees").
        // The new name is validated below, but the old name must also be safe since
        // it's used in Path::join("channels", old).
        if !Self::is_valid_channel_name(old) {
            return Err(crate::Error::InvalidMessage(format!(
                "Invalid channel name '{}': must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
                old
            )));
        }

        if !Self::is_valid_channel_name(new) {
            return Err(crate::Error::InvalidMessage(format!(
                "Invalid channel name '{}': must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
                new
            )));
        }

        // Reject names reserved for coworker sessions to prevent naming collisions.
        if crate::coworker::AVENUE_NAMES.contains(&new) {
            return Err(crate::Error::InvalidMessage(format!(
                "Channel name '{}' is reserved for coworker sessions and cannot be used as a channel name",
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

    /// Restore an archived channel by renaming `<name>.archived/` back to `<name>/`.
    pub fn unarchive_channel(base_dir: impl Into<PathBuf>, name: &str) -> Result<()> {
        if !Self::is_valid_channel_name(name) {
            return Err(crate::Error::InvalidMessage(format!(
                "Invalid channel name '{}': must be non-empty and contain only alphanumeric characters, hyphens, and underscores",
                name
            )));
        }

        if crate::coworker::AVENUE_NAMES.contains(&name) {
            return Err(crate::Error::InvalidMessage(format!(
                "Channel name '{}' is reserved for coworker sessions and cannot be used as a channel name",
                name
            )));
        }

        let base_dir = base_dir.into();
        let channels_dir = base_dir.join("channels");
        let archived_dir = channels_dir.join(format!("{}.archived", name));
        if !archived_dir.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("Channel '{}' is not archived", name),
            )
            .into());
        }

        let active_dir = channels_dir.join(name);
        if active_dir.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("Channel '{}' already exists", name),
            )
            .into());
        }

        fs::rename(&archived_dir, &active_dir)?;
        Ok(())
    }

    /// Mark a channel as archived
    ///
    /// Renames the channel directory from `channels/<name>/` to `channels/<name>.archived/`.
    /// Archived channels are excluded from the list() results.
    ///
    /// Returns an error if trying to archive the main channel (not allowed).
    /// The `main_channel_name` parameter identifies which channel is the main channel
    /// for this project (e.g., "offload" for the offload project).
    pub fn archive(&self, main_channel_name: &str) -> Result<()> {
        if self.channel_name == main_channel_name {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Cannot archive the '{}' channel", main_channel_name),
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

    /// Read all messages from the channel, including rotated archives.
    ///
    /// Reads every `.jsonl` file in the history directory (date-named archives
    /// in ascending order, then `current.jsonl`). Messages are sorted by
    /// timestamp to ensure chronological order. Each file is locked
    /// individually via `read_messages_from_file`.
    ///
    /// Blocks the calling thread during lock retries. In async contexts,
    /// use [`read_all_async`] instead to avoid stalling the tokio runtime.
    pub fn read_all(&self) -> Result<Vec<Message>> {
        let history_files = self.list_all_history_files();

        if history_files.is_empty() {
            return Ok(Vec::new());
        }

        let mut messages = Vec::new();
        for path in &history_files {
            messages.extend(Self::read_messages_from_file(path)?);
        }

        // Sort by timestamp to ensure chronological order
        messages.sort_by_key(|m| m.timestamp);

        Ok(messages)
    }

    /// Async variant of [`read_all`] that yields the tokio runtime during lock retries.
    pub async fn read_all_async(&self) -> Result<Vec<Message>> {
        let history_files = self.list_all_history_files();

        if history_files.is_empty() {
            return Ok(Vec::new());
        }

        let mut messages = Vec::new();
        for path in &history_files {
            messages.extend(Self::read_messages_from_file_async(path).await?);
        }

        // Sort by timestamp to ensure chronological order
        messages.sort_by_key(|m| m.timestamp);

        Ok(messages)
    }

    /// Read new messages starting from `position` without acquiring a file lock.
    ///
    /// Returns `(messages, new_position, last_message_id)`. Safe to call
    /// concurrently with writers because `O_APPEND` writes are atomic and we
    /// stop at the last complete newline, so partial lines are never returned.
    ///
    /// This is the hot path called on every `tailf` event. No locking means no
    /// 50 ms blocking sleep when the writer hasn't released its exclusive lock yet.
    pub fn read_messages_from_position(
        &self,
        position: u64,
    ) -> Result<(Vec<Message>, u64, Option<String>)> {
        if !self.channel_file.exists() {
            return Ok((Vec::new(), position, None));
        }

        let file = File::open(&self.channel_file)?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(position))?;

        let mut messages = Vec::new();
        let mut last_id: Option<String> = None;
        let mut current_position = position;
        let mut line_buf = String::new();

        loop {
            line_buf.clear();
            let bytes_read = reader.read_line(&mut line_buf)?;
            if bytes_read == 0 {
                break; // EOF
            }

            // Only process complete lines. A line without a trailing '\n' is a
            // partial write in progress — leave the cursor before it so the next
            // call re-reads it once the write completes.
            if !line_buf.ends_with('\n') {
                break;
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
                        tracing::warn!(
                            "Skipping malformed line at position {} in channel file: {}",
                            current_position - bytes_read as u64,
                            e
                        );
                    }
                }
            }
        }

        Ok((messages, current_position, last_id))
    }

    /// Read messages since the agent session's cursor position.
    ///
    /// Returns new messages and updates the cursor on disk. Cursors are scoped
    /// to `(agent, session_id)` so concurrent or successive sessions with the
    /// same agent name don't share state.
    ///
    /// Uses [`read_messages_from_position`] internally — no file lock is
    /// acquired. `O_APPEND` writes are atomic and we stop at the last complete
    /// newline, so reads are safe without locking.
    pub fn read_since_cursor(&self, agent: &str, session_id: &str) -> Result<Vec<Message>> {
        if !self.channel_file.exists() {
            return Ok(Vec::new());
        }

        let mut cursor =
            Cursor::load_or_create(&self.base_dir, &self.channel_name, agent, session_id)?;

        let (messages, new_position, last_id) =
            self.read_messages_from_position(cursor.position)?;

        if new_position != cursor.position {
            cursor.update(new_position, last_id);
            cursor.save(&self.base_dir, &self.channel_name)?;
        }

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

    /// Set an agent session's cursor to the end of the file.
    ///
    /// This is useful after initial load to ensure subsequent reads only
    /// pick up new messages. For new sessions that should not replay
    /// historical messages, call this before the first `read_since_cursor`.
    ///
    /// Returns `(position, last_message_id)` so callers can initialise an
    /// in-memory cursor cache without a separate disk read.
    pub fn set_cursor_to_end(
        &self,
        agent: &str,
        session_id: &str,
    ) -> Result<(u64, Option<String>)> {
        // `position` is a byte offset in current.jsonl — used by
        // `read_messages_from_position` to stream new messages as they
        // arrive. `last_message_id` comes from all history files (via
        // `read_last_n_messages`) so unread-count calculations correctly
        // treat the channel as fully read even after rotation.
        let last_message_id = self
            .read_last_n_messages(1)
            .ok()
            .and_then(|(msgs, _)| msgs.into_iter().next().map(|m| m.id));
        let position = self.file_size();
        let mut cursor =
            Cursor::load_or_create(&self.base_dir, &self.channel_name, agent, session_id)?;
        cursor.update(position, last_message_id.clone());
        cursor.save(&self.base_dir, &self.channel_name)?;
        Ok((position, last_message_id))
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
    /// the number of messages loaded from the tail. This count is passed to
    /// `read_messages_before_position` to load the next page of history.
    ///
    /// If the channel has fewer than N messages, returns all messages with
    /// start_position = 0 (all history loaded).
    pub fn read_last_n_messages(&self, n: usize) -> Result<(Vec<Message>, u64)> {
        let history_files = self.list_all_history_files();

        if history_files.is_empty() {
            return Ok((Vec::new(), 0));
        }

        // Read files in reverse order (current.jsonl first, then newest archive)
        // and stop as soon as we have enough messages.
        let mut all_messages = Vec::new();
        let total_files = history_files.len();
        let mut files_read = 0;

        for path in history_files.iter().rev() {
            let file_messages = Self::read_messages_from_file(path)?;
            all_messages.extend(file_messages);
            files_read += 1;

            if all_messages.len() >= n {
                break;
            }
        }

        // Sort all collected messages by timestamp
        all_messages.sort_by_key(|m| m.timestamp);

        let total = all_messages.len();
        let read_all = files_read == total_files;

        if total <= n && read_all {
            // All history loaded
            Ok((all_messages, 0))
        } else if total <= n {
            // Read fewer files than exist but still have <= n messages
            // (e.g., unread files might have more). Signal count loaded so far.
            Ok((all_messages, total as u64))
        } else {
            // More messages than requested — keep only last N
            all_messages = all_messages.split_off(total - n);
            Ok((all_messages, n as u64))
        }
    }

    /// Async variant of [`read_last_n_messages`] that yields the tokio runtime
    /// during lock retries.
    pub async fn read_last_n_messages_async(&self, n: usize) -> Result<(Vec<Message>, u64)> {
        let history_files = self.list_all_history_files();

        if history_files.is_empty() {
            return Ok((Vec::new(), 0));
        }

        let mut all_messages = Vec::new();
        let total_files = history_files.len();
        let mut files_read = 0;

        for path in history_files.iter().rev() {
            let file_messages = Self::read_messages_from_file_async(path).await?;
            all_messages.extend(file_messages);
            files_read += 1;

            if all_messages.len() >= n {
                break;
            }
        }

        all_messages.sort_by_key(|m| m.timestamp);

        let total = all_messages.len();
        let read_all = files_read == total_files;

        if total <= n && read_all {
            Ok((all_messages, 0))
        } else if total <= n {
            Ok((all_messages, total as u64))
        } else {
            all_messages = all_messages.split_off(total - n);
            Ok((all_messages, n as u64))
        }
    }

    /// Read messages before the already-loaded tail (for loading history)
    ///
    /// `position` is the number of messages already loaded from the tail
    /// (as returned by `read_last_n_messages` or a previous call to this method).
    /// Returns up to N messages just before those, plus a new position for the
    /// next page. If new_start_position is 0, all history has been loaded.
    pub fn read_messages_before_position(
        &self,
        position: u64,
        n: usize,
    ) -> Result<(Vec<Message>, u64)> {
        if position == 0 {
            return Ok((Vec::new(), 0));
        }

        let skip = position as usize;

        // Read all messages from all history files
        let history_files = self.list_all_history_files();
        let mut all_messages = Vec::new();
        for path in &history_files {
            all_messages.extend(Self::read_messages_from_file(path)?);
        }
        all_messages.sort_by_key(|m| m.timestamp);

        let total = all_messages.len();
        if total <= skip {
            // Caller already has all messages
            return Ok((Vec::new(), 0));
        }

        // Messages before the already-loaded tail
        let available = total - skip;
        let take = available.min(n);
        let start_idx = available - take;

        let page = all_messages[start_idx..start_idx + take].to_vec();

        let new_position = if start_idx == 0 {
            0 // All history loaded
        } else {
            (skip + take) as u64
        };

        Ok((page, new_position))
    }

    /// Async variant of [`read_messages_before_position`] that yields the tokio
    /// runtime during lock retries.
    pub async fn read_messages_before_position_async(
        &self,
        position: u64,
        n: usize,
    ) -> Result<(Vec<Message>, u64)> {
        if position == 0 {
            return Ok((Vec::new(), 0));
        }

        let skip = position as usize;

        let history_files = self.list_all_history_files();
        let mut all_messages = Vec::new();
        for path in &history_files {
            all_messages.extend(Self::read_messages_from_file_async(path).await?);
        }
        all_messages.sort_by_key(|m| m.timestamp);

        let total = all_messages.len();
        if total <= skip {
            return Ok((Vec::new(), 0));
        }

        let available = total - skip;
        let take = available.min(n);
        let start_idx = available - take;

        let page = all_messages[start_idx..start_idx + take].to_vec();

        let new_position = if start_idx == 0 {
            0 // All history loaded
        } else {
            (skip + take) as u64
        };

        Ok((page, new_position))
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

/// Maximum total size (in bytes) for concatenated channel notes.
///
/// Notes are injected into the system prompt, so we cap total size to avoid
/// bloating context windows. 100 KB is ~25k tokens — generous for domain
/// knowledge while staying well within context limits.
const MAX_NOTES_BYTES: usize = 100 * 1024;

/// Load all notes for a channel as a single string for domain context injection.
///
/// Reads all `.md` files from the channel's notes directory, concatenating
/// their contents with filename-derived headers. Returns an empty string if
/// the directory doesn't exist or contains no notes.
///
/// Files are read in alphabetical order and truncated once the total exceeds
/// [`MAX_NOTES_BYTES`]. Individual I/O errors are logged and skipped.
///
/// This is a standalone function (not a `Channel` method) because callers
/// in the daemon often have a `base_dir` without a `Channel` instance.
pub fn load_channel_notes(base_dir: &Path, channel_name: &str) -> String {
    // Defense-in-depth: reject channel names that could escape the notes directory.
    if !Channel::is_valid_channel_name(channel_name) {
        return String::new();
    }

    let notes_dir = base_dir.join("channels").join(channel_name).join("notes");

    if !notes_dir.is_dir() {
        return String::new();
    }

    let mut entries: Vec<_> = match fs::read_dir(&notes_dir) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .collect(),
        Err(e) => {
            tracing::warn!(
                "Failed to read notes directory for channel '{}': {}",
                channel_name,
                e
            );
            return String::new();
        }
    };

    entries.sort_by_key(|e| e.file_name());

    let mut sections = Vec::new();
    let mut total_bytes = 0usize;
    for entry in entries {
        let path = entry.path();
        let filename = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                let section = format!("## {}\n\n{}", filename, content.trim());
                total_bytes += section.len();
                if total_bytes > MAX_NOTES_BYTES {
                    tracing::info!(
                        "Channel '{}' notes truncated at {} bytes (limit: {})",
                        channel_name,
                        total_bytes,
                        MAX_NOTES_BYTES
                    );
                    // Include partial last section up to limit
                    let overshoot = total_bytes - MAX_NOTES_BYTES;
                    if section.len() > overshoot {
                        sections.push(section[..section.len() - overshoot].to_string());
                    }
                    break;
                }
                sections.push(section);
            }
            Ok(_) => {} // empty file, skip silently
            Err(e) => {
                tracing::warn!(
                    "Failed to read note file '{}' for channel '{}': {}",
                    path.display(),
                    channel_name,
                    e
                );
            }
        }
    }

    if sections.is_empty() {
        String::new()
    } else {
        format!("# Channel Notes\n\n{}", sections.join("\n\n"))
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

    /// Check if a channel is archived.
    ///
    /// Returns `true` when a `channels/<name>.archived/` directory exists,
    /// meaning the channel can no longer receive messages.
    pub fn is_channel_archived(&self, channel_name: &str) -> bool {
        self.base_dir
            .join("channels")
            .join(format!("{}.archived", channel_name))
            .exists()
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
    /// Used after renaming a channel to evict stale cache entries.
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

/// Remove any active channel directories that have an archived counterpart.
///
/// Archived channels should only exist under `<name>.archived/`. A prior bug
/// recreated `<name>/` when accessing archived channels, causing both directories
/// to exist. This helper deletes the zombie active directory so the archived
/// data remains authoritative.
fn cleanup_archived_channel_conflicts(base_dir: &Path) {
    let channels_dir = base_dir.join("channels");
    if !channels_dir.exists() {
        return;
    }

    let entries = match fs::read_dir(&channels_dir) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::warn!(
                "Failed to scan channels directory '{:?}' for archived conflicts: {}",
                channels_dir,
                err
            );
            return;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };

        if !dir_name.ends_with(".archived") {
            continue;
        }

        let channel_name = dir_name.trim_end_matches(".archived");
        if !Channel::is_valid_channel_name(channel_name) {
            continue;
        }

        let archived_history = path.join("history").join("current.jsonl");
        if !archived_history.exists() {
            continue;
        }

        let active_dir = channels_dir.join(channel_name);
        if !active_dir.exists() {
            continue;
        }

        match fs::remove_dir_all(&active_dir) {
            Ok(_) => tracing::info!(
                "Removed zombie active channel directory '{}' because an archived copy exists",
                channel_name
            ),
            Err(err) => tracing::warn!(
                "Failed to remove zombie active channel directory '{}': {}",
                channel_name,
                err
            ),
        }
    }
}

#[path = "channel_tests.rs"]
#[cfg(test)]
mod tests;
