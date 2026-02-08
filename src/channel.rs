//! Channel management for append-only message logs

use crate::Result;
use crate::cursor::Cursor;
use crate::message::Message;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A channel for agent communication.
///
/// Channels use an append-only JSONL file for messages and per-agent cursor
/// tracking for read positions. File locking ensures thread-safe concurrent access.
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
/// # let channel = Channel::new(temp_dir.path()).unwrap();
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
/// # let channel = Channel::new(temp_dir.path()).unwrap();
/// // Send initial messages
/// channel.send(&Message::text("alice", "First")).unwrap();
/// channel.send(&Message::text("bob", "Second")).unwrap();
///
/// // Agent reads all messages (moves cursor)
/// let msgs = channel.read_since_cursor("agent1").unwrap();
/// assert_eq!(msgs.len(), 2);
///
/// // New message arrives
/// channel.send(&Message::text("alice", "Third")).unwrap();
///
/// // Agent only sees new message
/// let new_msgs = channel.read_since_cursor("agent1").unwrap();
/// assert_eq!(new_msgs.len(), 1);
/// assert_eq!(new_msgs[0].content, "Third");
/// ```
#[derive(Clone)]
pub struct Channel {
    /// Base directory for this channel (~/.midtown/projects/<repo>/)
    base_dir: PathBuf,
    /// Path to the channel.jsonl file
    channel_file: PathBuf,
}

impl Channel {
    /// Create a new channel at the specified base directory
    ///
    /// Creates the directory structure and channel file if they don't exist.
    /// The channel file is created eagerly so that file watchers (tailf) can
    /// immediately start monitoring it.
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self> {
        let base_dir = base_dir.into();
        fs::create_dir_all(&base_dir)?;
        fs::create_dir_all(base_dir.join("cursors"))?;

        let channel_file = base_dir.join("channel.jsonl");

        // Create the channel file if it doesn't exist.
        // This enables file watchers like tailf to start monitoring immediately.
        // (tailf wraps `tail -f` which requires the file to exist)
        if !channel_file.exists() {
            File::create(&channel_file)?;
        }

        Ok(Self {
            base_dir,
            channel_file,
        })
    }

    /// Open a channel for a specific repository
    ///
    /// Uses ~/.midtown/projects/<repo>/ as the base directory.
    pub fn for_repo(repo: &str) -> Result<Self> {
        let base_dir = crate::paths::projects_dir_for_repo(repo);
        Self::new(base_dir)
    }

    /// Get the base directory path
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Get the path to the channel.jsonl file
    pub fn channel_file_path(&self) -> &Path {
        &self.channel_file
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

        // Serialize and append
        let mut json = serde_json::to_string(message)?;
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
    /// Uses a non-blocking lock to avoid freezing the UI when there's lock
    /// contention from writers. If the lock can't be acquired immediately,
    /// returns an error and the caller can retry later.
    pub fn read_all(&self) -> Result<Vec<Message>> {
        if !self.channel_file.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.channel_file)?;
        // Try to acquire shared lock without blocking - avoids UI freeze when
        // writers hold exclusive locks
        file.try_lock_shared()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::WouldBlock, e.to_string()))?;

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

    /// Read messages since the agent's cursor position
    ///
    /// Returns new messages and updates the cursor.
    ///
    /// Uses a non-blocking lock to avoid blocking when there's lock contention.
    /// If the lock can't be acquired immediately, returns an error.
    pub fn read_since_cursor(&self, agent: &str) -> Result<Vec<Message>> {
        if !self.channel_file.exists() {
            return Ok(Vec::new());
        }

        let mut cursor = Cursor::load_or_create(&self.base_dir, agent)?;

        let file = File::open(&self.channel_file)?;
        // Try to acquire shared lock without blocking
        file.try_lock_shared()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::WouldBlock, e.to_string()))?;

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
        cursor.save(&self.base_dir)?;

        Ok(messages)
    }

    /// Get the current cursor for an agent
    pub fn get_cursor(&self, agent: &str) -> Result<Cursor> {
        Cursor::load_or_create(&self.base_dir, agent)
    }

    /// Reset an agent's cursor to the beginning
    pub fn reset_cursor(&self, agent: &str) -> Result<()> {
        let mut cursor = Cursor::load_or_create(&self.base_dir, agent)?;
        cursor.reset();
        cursor.save(&self.base_dir)?;
        Ok(())
    }

    /// Set an agent's cursor to the end of the file
    ///
    /// This is useful after initial load to ensure subsequent reads
    /// only pick up new messages.
    pub fn set_cursor_to_end(&self, agent: &str) -> Result<()> {
        let mut cursor = Cursor::load_or_create(&self.base_dir, agent)?;
        cursor.update(self.file_size(), None);
        cursor.save(&self.base_dir)?;
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
        file.try_lock_shared()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::WouldBlock, e.to_string()))?;

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
        file.try_lock_shared()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::WouldBlock, e.to_string()))?;

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

        // Write archived messages to channel-YYYY-MM-DD.jsonl
        let date_str = Utc::now().format("%Y-%m-%d").to_string();
        let archive_file_path = self.base_dir.join(format!("channel-{}.jsonl", date_str));

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

        // Reset all cursor files since byte positions have changed
        let cursors_dir = self.base_dir.join("cursors");
        if cursors_dir.exists()
            && let Ok(entries) = fs::read_dir(&cursors_dir)
        {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e == "json")
                    && let Some(agent) = path.file_stem().and_then(|s| s.to_str())
                    && let Ok(mut cursor) =
                        crate::cursor::Cursor::load_or_create(&self.base_dir, agent)
                {
                    cursor.reset();
                    let _ = cursor.save(&self.base_dir);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageType;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Retry a fallible operation with backoff to handle transient lock contention in CI.
    /// All channel read methods use try_lock_shared() which returns WouldBlock when
    /// an exclusive write lock is held by another thread.
    fn retry_with_backoff<T>(max_attempts: u32, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        for attempt in 0..max_attempts {
            match f() {
                Ok(val) => return Ok(val),
                Err(e) if attempt < max_attempts - 1 => {
                    thread::sleep(Duration::from_millis(10 * (attempt as u64 + 1)));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }

    fn read_all_with_retry(channel: &Channel, max_attempts: u32) -> Result<Vec<Message>> {
        retry_with_backoff(max_attempts, || channel.read_all())
    }

    fn message_count_with_retry(channel: &Channel, max_attempts: u32) -> Result<usize> {
        retry_with_backoff(max_attempts, || channel.message_count())
    }

    fn read_last_n_with_retry(
        channel: &Channel,
        n: usize,
        max_attempts: u32,
    ) -> Result<(Vec<Message>, u64)> {
        retry_with_backoff(max_attempts, || channel.read_last_n_messages(n))
    }

    fn read_before_pos_with_retry(
        channel: &Channel,
        pos: u64,
        n: usize,
        max_attempts: u32,
    ) -> Result<(Vec<Message>, u64)> {
        retry_with_backoff(max_attempts, || {
            channel.read_messages_before_position(pos, n)
        })
    }

    fn read_since_cursor_with_retry(
        channel: &Channel,
        agent: &str,
        max_attempts: u32,
    ) -> Result<Vec<Message>> {
        retry_with_backoff(max_attempts, || channel.read_since_cursor(agent))
    }

    #[test]
    fn test_channel_creation() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();
        assert!(temp_dir.path().join("cursors").exists());
        // Channel file should exist (for tailf) but be empty (no messages)
        assert!(channel.exists());
        assert_eq!(message_count_with_retry(&channel, 5).unwrap(), 0);
    }

    #[test]
    fn test_channel_file_exists_for_tailf() {
        // The channel.jsonl file must exist after Channel::new() for tailf to work.
        // tailf wraps `tail -f` which fails on non-existent files.
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // The channel file should exist (even if empty) so tailf can watch it
        assert!(
            channel.channel_file_path().exists(),
            "channel.jsonl must exist after Channel::new() for tailf compatibility"
        );
    }

    #[test]
    fn test_send_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        let msg1 = Message::text("agent1", "Hello");
        let msg2 = Message::text("agent2", "World");

        channel.send(&msg1).unwrap();
        channel.send(&msg2).unwrap();

        // Use retry helper to handle transient lock contention in CI
        let messages = read_all_with_retry(&channel, 5).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "Hello");
        assert_eq!(messages[1].content, "World");
    }

    #[test]
    fn test_cursor_based_reading() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Send first batch
        channel.send(&Message::text("agent1", "Message 1")).unwrap();
        channel.send(&Message::text("agent1", "Message 2")).unwrap();

        // Agent reads all messages (retry to handle transient lock contention in CI)
        let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
        assert_eq!(messages.len(), 2);

        // Send more messages
        channel.send(&Message::text("agent1", "Message 3")).unwrap();

        // Agent should only see new message
        let new_messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
        assert_eq!(new_messages.len(), 1);
        assert_eq!(new_messages[0].content, "Message 3");

        // Another agent sees all messages
        let all_messages = read_since_cursor_with_retry(&channel, "other_reader", 5).unwrap();
        assert_eq!(all_messages.len(), 3);
    }

    #[test]
    fn test_cursor_reset() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        channel.send(&Message::text("agent1", "Message")).unwrap();

        // Read once (retry to handle transient lock contention in CI)
        let _ = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();

        // Reset cursor
        channel.reset_cursor("reader").unwrap();

        // Should see message again
        let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_message_count() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Use retry helper to handle transient lock contention in CI
        assert_eq!(message_count_with_retry(&channel, 5).unwrap(), 0);

        channel.send(&Message::text("agent1", "1")).unwrap();
        channel.send(&Message::text("agent1", "2")).unwrap();
        channel.send(&Message::text("agent1", "3")).unwrap();

        assert_eq!(message_count_with_retry(&channel, 5).unwrap(), 3);
    }

    #[test]
    fn test_file_size_increases_with_messages() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Empty channel has size 0
        assert_eq!(channel.file_size(), 0);

        // Sending a message increases file size
        channel.send(&Message::text("agent1", "Hello")).unwrap();
        let size1 = channel.file_size();
        assert!(size1 > 0);

        // Sending another message increases it further
        channel.send(&Message::text("agent1", "World")).unwrap();
        let size2 = channel.file_size();
        assert!(size2 > size1);

        // Size stays the same when no messages are added
        assert_eq!(channel.file_size(), size2);
    }

    #[test]
    fn test_different_message_types() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        channel.send(&Message::text("agent1", "text")).unwrap();
        channel
            .send(&Message::system("system notification"))
            .unwrap();
        channel
            .send(&Message::command("agent1", "run test"))
            .unwrap();
        channel.send(&Message::status("agent1", "working")).unwrap();
        channel.send(&Message::error("agent1", "failed")).unwrap();

        // Use retry helper to handle transient lock contention in CI
        let messages = read_all_with_retry(&channel, 5).unwrap();
        assert_eq!(messages.len(), 5);
        assert_eq!(messages[0].message_type, MessageType::Text);
        assert_eq!(messages[1].message_type, MessageType::System);
        assert_eq!(messages[2].message_type, MessageType::Command);
        assert_eq!(messages[3].message_type, MessageType::Status);
        assert_eq!(messages[4].message_type, MessageType::Error);
    }

    #[test]
    fn test_empty_channel_read() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Reading from empty channel should return empty vec
        let messages = read_all_with_retry(&channel, 5).unwrap();
        assert!(messages.is_empty());

        let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
        assert!(messages.is_empty());
    }

    #[test]
    fn test_messages_sorted_by_timestamp() {
        use chrono::{Duration, Utc};
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Create messages with out-of-order timestamps
        // Simulate: msg written at T+40min has timestamp T (old message arrived late)
        let now = Utc::now();
        let old_time = now - Duration::minutes(40);

        // Write messages directly to file in wrong order (simulating delayed write)
        let channel_file = temp_dir.path().join("channel.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&channel_file)
            .unwrap();

        // First: write a "new" message (timestamp = now)
        let new_msg = Message {
            id: "new".to_string(),
            timestamp: now,
            from: "agent1".to_string(),
            content: "New message".to_string(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
        };
        writeln!(file, "{}", serde_json::to_string(&new_msg).unwrap()).unwrap();

        // Second: write an "old" message that arrived late (timestamp = 40 min ago)
        let old_msg = Message {
            id: "old".to_string(),
            timestamp: old_time,
            from: "agent2".to_string(),
            content: "Old message (delayed)".to_string(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
        };
        writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();

        drop(file);

        // Read messages - they should be sorted by timestamp (oldest first)
        let messages = read_all_with_retry(&channel, 5).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].content, "Old message (delayed)",
            "Older message should appear first (sorted by timestamp)"
        );
        assert_eq!(
            messages[1].content, "New message",
            "Newer message should appear second (sorted by timestamp)"
        );
    }

    #[test]
    fn test_read_last_n_messages_small_channel() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Send 5 messages
        for i in 1..=5 {
            channel
                .send(&Message::text("agent1", format!("Message {}", i)))
                .unwrap();
        }

        // Request last 10 messages, but only 5 exist
        let (messages, start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
        assert_eq!(messages.len(), 5);
        assert_eq!(start_pos, 0, "start_pos should be 0 when all messages fit");
        assert_eq!(messages[0].content, "Message 1");
        assert_eq!(messages[4].content, "Message 5");
    }

    #[test]
    fn test_read_last_n_messages_large_channel() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Send 50 messages
        for i in 1..=50 {
            channel
                .send(&Message::text("agent1", format!("Message {}", i)))
                .unwrap();
        }

        // Request last 10 messages
        let (messages, _start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
        assert_eq!(messages.len(), 10);
        // Should have messages 41-50 (last 10)
        assert_eq!(messages[0].content, "Message 41");
        assert_eq!(messages[9].content, "Message 50");
    }

    #[test]
    fn test_read_last_n_messages_empty_channel() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        let (messages, start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
        assert!(messages.is_empty());
        assert_eq!(start_pos, 0);
    }

    #[test]
    fn test_read_messages_before_position() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Send 30 messages
        for i in 1..=30 {
            channel
                .send(&Message::text("agent1", format!("Message {}", i)))
                .unwrap();
        }

        // First, get the last 10 messages to establish a position
        let (recent_msgs, start_pos) = read_last_n_with_retry(&channel, 10, 5).unwrap();
        assert_eq!(recent_msgs.len(), 10);
        assert_eq!(recent_msgs[0].content, "Message 21");

        // Now load 10 more messages before that position
        if start_pos > 0 {
            let (older_msgs, _) = read_before_pos_with_retry(&channel, start_pos, 10, 5).unwrap();
            assert!(!older_msgs.is_empty());
            // Should have some messages with lower numbers
            for msg in &older_msgs {
                // Extract number from "Message N" and verify it's < 21
                let num: i32 = msg
                    .content
                    .strip_prefix("Message ")
                    .unwrap()
                    .parse()
                    .unwrap();
                assert!(num < 21, "Older messages should have numbers < 21");
            }
        }
    }

    #[test]
    fn test_read_messages_before_position_zero() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        channel.send(&Message::text("agent1", "Message 1")).unwrap();

        // Position 0 means no more history
        let (messages, start_pos) = read_before_pos_with_retry(&channel, 0, 10, 5).unwrap();
        assert!(messages.is_empty());
        assert_eq!(start_pos, 0);
    }

    #[test]
    fn test_rotate_empty_channel() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Rotating an empty channel should be a no-op
        let archived = channel.rotate(60).unwrap();
        assert_eq!(archived, 0);
    }

    #[test]
    fn test_rotate_all_recent_messages() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Send messages that are all very recent (within last 60 min)
        channel.send(&Message::text("agent1", "Recent 1")).unwrap();
        channel.send(&Message::text("agent1", "Recent 2")).unwrap();

        let archived = channel.rotate(60).unwrap();
        assert_eq!(archived, 0, "No messages should be archived");

        // All messages still present
        let messages = read_all_with_retry(&channel, 5).unwrap();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_rotate_archives_old_messages() {
        use chrono::{Duration, Utc};
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Write old messages directly with timestamps > 60 min ago
        let now = Utc::now();
        let old_time = now - Duration::hours(3);
        let channel_file = temp_dir.path().join("channel.jsonl");

        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&channel_file)
                .unwrap();

            // Write 3 old messages
            for i in 1..=3 {
                let msg = Message {
                    id: format!("old-{}", i),
                    timestamp: old_time + Duration::minutes(i as i64),
                    from: "agent1".to_string(),
                    content: format!("Old message {}", i),
                    message_type: MessageType::Text,
                    channel: None,
                    source_channel: None,
                };
                writeln!(file, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
            }

            // Write 2 recent messages
            for i in 1..=2 {
                let msg = Message {
                    id: format!("new-{}", i),
                    timestamp: now - Duration::minutes(i as i64),
                    from: "agent1".to_string(),
                    content: format!("Recent message {}", i),
                    message_type: MessageType::Text,
                    channel: None,
                    source_channel: None,
                };
                writeln!(file, "{}", serde_json::to_string(&msg).unwrap()).unwrap();
            }
        }

        // Rotate with 60 min retention
        let archived = channel.rotate(60).unwrap();
        assert_eq!(archived, 3, "3 old messages should be archived");

        // Only recent messages remain in channel
        let remaining = read_all_with_retry(&channel, 5).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining[0].content.starts_with("Recent"));
        assert!(remaining[1].content.starts_with("Recent"));

        // Archive file should exist with old messages
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let archive_path = temp_dir.path().join(format!("channel-{}.jsonl", today));
        assert!(archive_path.exists(), "Archive file should exist");

        // Read archive and verify
        let archive_content = std::fs::read_to_string(&archive_path).unwrap();
        let archive_msgs: Vec<Message> = archive_content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(archive_msgs.len(), 3);
        assert!(archive_msgs[0].content.starts_with("Old"));
    }

    #[test]
    fn test_rotate_resets_cursors() {
        use chrono::{Duration, Utc};
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Write old + recent messages
        let now = Utc::now();
        let old_time = now - Duration::hours(3);
        let channel_file = temp_dir.path().join("channel.jsonl");

        {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&channel_file)
                .unwrap();

            let old_msg = Message {
                id: "old-1".to_string(),
                timestamp: old_time,
                from: "agent1".to_string(),
                content: "Old".to_string(),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
            };
            writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();
        }

        // Send a recent one normally
        channel.send(&Message::text("agent1", "Recent")).unwrap();

        // Agent reads to establish a cursor at some byte offset
        let _ = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
        let cursor_before = channel.get_cursor("reader").unwrap();
        assert!(cursor_before.position > 0, "Cursor should be past 0");

        // Rotate
        let archived = channel.rotate(60).unwrap();
        assert_eq!(archived, 1);

        // Cursor should be reset to 0
        let cursor_after = channel.get_cursor("reader").unwrap();
        assert_eq!(
            cursor_after.position, 0,
            "Cursor should be reset after rotation"
        );
    }

    #[test]
    fn test_needs_rotation() {
        use chrono::{Duration, Utc};
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Empty channel doesn't need rotation
        assert!(!channel.needs_rotation(24));

        // Channel with only recent messages doesn't need rotation
        channel.send(&Message::text("agent1", "Recent")).unwrap();
        assert!(!channel.needs_rotation(24));

        // Channel with old messages needs rotation
        let channel_file = temp_dir.path().join("channel.jsonl");
        let old_time = Utc::now() - Duration::hours(25);
        {
            // Prepend an old message by rewriting the file
            let existing = std::fs::read_to_string(&channel_file).unwrap();
            let mut file = std::fs::File::create(&channel_file).unwrap();
            let old_msg = Message {
                id: "old-1".to_string(),
                timestamp: old_time,
                from: "agent1".to_string(),
                content: "Very old".to_string(),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
            };
            writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();
            file.write_all(existing.as_bytes()).unwrap();
        }

        // Retry: needs_rotation uses try_lock_shared internally and returns false on WouldBlock
        let mut rotation_needed = false;
        for _ in 0..5 {
            if channel.needs_rotation(24) {
                rotation_needed = true;
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            rotation_needed,
            "Channel with old messages should need rotation"
        );
    }

    #[test]
    fn test_read_all_skips_malformed_lines() {
        // Regression test: A raw text line in channel.jsonl (not valid JSON) was causing
        // read_all() to fail completely. This happened in production when a raw message
        // was somehow written directly to the file without JSON wrapping.
        //
        // The fix should skip invalid lines and continue reading valid messages,
        // allowing the channel to be read even with some corruption.
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Write some valid messages
        channel
            .send(&Message::text("agent1", "First valid message"))
            .unwrap();
        channel
            .send(&Message::text("agent2", "Second valid message"))
            .unwrap();

        // Manually inject a malformed line (raw text, not JSON)
        // This simulates the corruption observed in production
        let channel_file = temp_dir.path().join("channel.jsonl");
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&channel_file)
                .unwrap();
            writeln!(file, "@lead Some raw text that is not JSON").unwrap();
        }

        // Write another valid message after the corruption
        channel
            .send(&Message::text("agent3", "Third valid message"))
            .unwrap();

        // read_all() should skip the invalid line and return the 3 valid messages
        let messages = read_all_with_retry(&channel, 5).unwrap();
        assert_eq!(
            messages.len(),
            3,
            "Should skip malformed line and read 3 valid messages"
        );
        assert_eq!(messages[0].content, "First valid message");
        assert_eq!(messages[1].content, "Second valid message");
        assert_eq!(messages[2].content, "Third valid message");
    }

    #[test]
    fn test_read_since_cursor_skips_malformed_lines() {
        // Similar to test_read_all_skips_malformed_lines but tests the cursor-based
        // reading path which uses a different code path (byte offset seeking).
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        // Write initial messages and read to set cursor position
        channel
            .send(&Message::text("agent1", "First message"))
            .unwrap();
        let _ = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();

        // Write more valid messages
        channel
            .send(&Message::text("agent2", "Second message"))
            .unwrap();

        // Inject a malformed line
        let channel_file = temp_dir.path().join("channel.jsonl");
        {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&channel_file)
                .unwrap();
            writeln!(file, "This is raw text, not JSON").unwrap();
        }

        // Write another valid message after the corruption
        channel
            .send(&Message::text("agent3", "Third message"))
            .unwrap();

        // read_since_cursor should skip the malformed line and return the 2 new valid messages
        let messages = read_since_cursor_with_retry(&channel, "reader", 5).unwrap();
        assert_eq!(
            messages.len(),
            2,
            "Should skip malformed line and read 2 new valid messages"
        );
        assert_eq!(messages[0].content, "Second message");
        assert_eq!(messages[1].content, "Third message");
    }
}
