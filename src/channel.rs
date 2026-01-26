//! Channel management for append-only message logs

use crate::cursor::Cursor;
use crate::message::Message;
use crate::Result;
use fs2::FileExt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// A channel for agent communication
///
/// Channels use an append-only JSONL file for messages and per-agent cursor
/// tracking for read positions. File locking ensures thread-safe concurrent access.
pub struct Channel {
    /// Base directory for this channel (~/.midtown/<repo>/)
    base_dir: PathBuf,
    /// Path to the channel.jsonl file
    channel_file: PathBuf,
}

impl Channel {
    /// Create a new channel at the specified base directory
    ///
    /// Creates the directory structure if it doesn't exist.
    pub fn new(base_dir: impl Into<PathBuf>) -> Result<Self> {
        let base_dir = base_dir.into();
        fs::create_dir_all(&base_dir)?;
        fs::create_dir_all(base_dir.join("cursors"))?;

        let channel_file = base_dir.join("channel.jsonl");

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
    pub fn read_all(&self) -> Result<Vec<Message>> {
        if !self.channel_file.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(&self.channel_file)?;
        // Acquire shared lock for reading
        file.lock_shared()?;

        let reader = BufReader::new(file);
        let mut messages = Vec::new();

        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() {
                let message: Message = serde_json::from_str(&line)?;
                messages.push(message);
            }
        }

        Ok(messages)
    }

    /// Read messages since the agent's cursor position
    ///
    /// Returns new messages and updates the cursor.
    pub fn read_since_cursor(&self, agent: &str) -> Result<Vec<Message>> {
        if !self.channel_file.exists() {
            return Ok(Vec::new());
        }

        let mut cursor = Cursor::load_or_create(&self.base_dir, agent)?;

        let file = File::open(&self.channel_file)?;
        // Acquire shared lock for reading
        file.lock_shared()?;

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

    /// Get the total number of messages in the channel
    pub fn message_count(&self) -> Result<usize> {
        Ok(self.read_all()?.len())
    }

    /// Check if the channel file exists
    pub fn exists(&self) -> bool {
        self.channel_file.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageType;
    use tempfile::TempDir;

    #[test]
    fn test_channel_creation() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();
        assert!(temp_dir.path().join("cursors").exists());
        assert!(!channel.exists()); // No messages yet
    }

    #[test]
    fn test_send_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        let msg1 = Message::text("agent1", "Hello");
        let msg2 = Message::text("agent2", "World");

        channel.send(&msg1).unwrap();
        channel.send(&msg2).unwrap();

        let messages = channel.read_all().unwrap();
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
    fn test_different_message_types() {
        let temp_dir = TempDir::new().unwrap();
        let channel = Channel::new(temp_dir.path()).unwrap();

        channel.send(&Message::text("agent1", "text")).unwrap();
        channel.send(&Message::system("system notification")).unwrap();
        channel.send(&Message::command("agent1", "run test")).unwrap();
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
}
