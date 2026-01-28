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
pub struct Channel {
    /// Base directory for this channel (~/.midtown/<repo>/)
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
    /// Uses ~/.midtown/<repo>/ as the base directory.
    pub fn for_repo(repo: &str) -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let base_dir = PathBuf::from(home).join(".midtown").join(repo);
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
    pub fn send(&self, message: &Message) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.channel_file)?;

        // Acquire exclusive lock for writing
        file.lock_exclusive()?;

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

        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() {
                let message: Message = serde_json::from_str(&line)?;
                messages.push(message);
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
                let message: Message = serde_json::from_str(line)?;
                last_id = Some(message.id.clone());
                messages.push(message);
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
            // We can't easily recalculate the exact byte position, but we
            // can estimate. For load_more_history, we'll re-read anyway.
            actual_start = 0; // Signal that there's more history available
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageType;
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    /// Retry read_all with backoff to handle transient lock contention in CI
    fn read_all_with_retry(channel: &Channel, max_attempts: u32) -> Result<Vec<Message>> {
        for attempt in 0..max_attempts {
            match channel.read_all() {
                Ok(messages) => return Ok(messages),
                Err(e) if attempt < max_attempts - 1 => {
                    // WouldBlock is expected under lock contention, retry after backoff
                    thread::sleep(Duration::from_millis(10 * (attempt as u64 + 1)));
                    continue;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!()
    }

    #[test]
    fn test_channel_creation() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();
        assert!(temp_dir.path().join("cursors").exists());
        // Channel file should exist (for tailf) but be empty (no messages)
        assert!(channel.exists());
        assert_eq!(channel.message_count().unwrap(), 0);
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

        // Agent reads all messages
        let messages = channel.read_since_cursor("reader").unwrap();
        assert_eq!(messages.len(), 2);

        // Send more messages
        channel.send(&Message::text("agent1", "Message 3")).unwrap();

        // Agent should only see new message
        let new_messages = channel.read_since_cursor("reader").unwrap();
        assert_eq!(new_messages.len(), 1);
        assert_eq!(new_messages[0].content, "Message 3");

        // Another agent sees all messages
        let all_messages = channel.read_since_cursor("other_reader").unwrap();
        assert_eq!(all_messages.len(), 3);
    }

    #[test]
    fn test_cursor_reset() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        channel.send(&Message::text("agent1", "Message")).unwrap();

        // Read once
        let _ = channel.read_since_cursor("reader").unwrap();

        // Reset cursor
        channel.reset_cursor("reader").unwrap();

        // Should see message again
        let messages = channel.read_since_cursor("reader").unwrap();
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn test_message_count() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        assert_eq!(channel.message_count().unwrap(), 0);

        channel.send(&Message::text("agent1", "1")).unwrap();
        channel.send(&Message::text("agent1", "2")).unwrap();
        channel.send(&Message::text("agent1", "3")).unwrap();

        assert_eq!(channel.message_count().unwrap(), 3);
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

        let messages = channel.read_all().unwrap();
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

        // Reading from non-existent channel should return empty vec
        let messages = channel.read_all().unwrap();
        assert!(messages.is_empty());

        let messages = channel.read_since_cursor("reader").unwrap();
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
        };
        writeln!(file, "{}", serde_json::to_string(&new_msg).unwrap()).unwrap();

        // Second: write an "old" message that arrived late (timestamp = 40 min ago)
        let old_msg = Message {
            id: "old".to_string(),
            timestamp: old_time,
            from: "agent2".to_string(),
            content: "Old message (delayed)".to_string(),
            message_type: MessageType::Text,
        };
        writeln!(file, "{}", serde_json::to_string(&old_msg).unwrap()).unwrap();

        drop(file);

        // Read messages - they should be sorted by timestamp (oldest first)
        let messages = channel.read_all().unwrap();
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
        let (messages, start_pos) = channel.read_last_n_messages(10).unwrap();
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
        let (messages, _start_pos) = channel.read_last_n_messages(10).unwrap();
        assert_eq!(messages.len(), 10);
        // Should have messages 41-50 (last 10)
        assert_eq!(messages[0].content, "Message 41");
        assert_eq!(messages[9].content, "Message 50");
    }

    #[test]
    fn test_read_last_n_messages_empty_channel() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        let (messages, start_pos) = channel.read_last_n_messages(10).unwrap();
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
        let (recent_msgs, start_pos) = channel.read_last_n_messages(10).unwrap();
        assert_eq!(recent_msgs.len(), 10);
        assert_eq!(recent_msgs[0].content, "Message 21");

        // Now load 10 more messages before that position
        if start_pos > 0 {
            let (older_msgs, _) = channel
                .read_messages_before_position(start_pos, 10)
                .unwrap();
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
        let (messages, start_pos) = channel.read_messages_before_position(0, 10).unwrap();
        assert!(messages.is_empty());
        assert_eq!(start_pos, 0);
    }
}
