//! Per-agent cursor tracking for channel reading

use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Cursor state for an agent's read position in a channel.
///
/// Each agent maintains a cursor that tracks where they've read up to
/// in the channel log. This enables incremental reading where agents
/// only receive new messages since their last read.
///
/// # Examples
///
/// Creating and updating a cursor:
///
/// ```
/// use midtown::Cursor;
///
/// let mut cursor = Cursor::new("agent1");
/// assert_eq!(cursor.position, 0);
/// assert!(cursor.last_message_id.is_none());
///
/// // After reading some messages
/// cursor.update(256, Some("msg-123".to_string()));
/// assert_eq!(cursor.position, 256);
/// assert_eq!(cursor.last_message_id, Some("msg-123".to_string()));
///
/// // Reset to re-read from beginning
/// cursor.reset();
/// assert_eq!(cursor.position, 0);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// The agent this cursor belongs to
    pub agent: String,
    /// Byte offset in the channel file (position after last read message)
    pub position: u64,
    /// ID of the last message read (for validation)
    pub last_message_id: Option<String>,
    /// When the cursor was last updated
    pub updated_at: DateTime<Utc>,
}

impl Cursor {
    /// Create a new cursor at position 0.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::Cursor;
    ///
    /// let cursor = Cursor::new("my-agent");
    /// assert_eq!(cursor.agent, "my-agent");
    /// assert_eq!(cursor.position, 0);
    /// ```
    pub fn new(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            position: 0,
            last_message_id: None,
            updated_at: Utc::now(),
        }
    }

    /// Get the path for a cursor file
    ///
    /// For backward compatibility:
    /// - If channel is "midtown" and cursors/<agent>.json exists (legacy), use that
    /// - Otherwise use cursors/<channel>/<agent>.json
    pub fn file_path(base_dir: &Path, channel: &str, agent: &str) -> PathBuf {
        // Legacy path for midtown channel
        if channel == "midtown" {
            let legacy_path = base_dir.join("cursors").join(format!("{}.json", agent));
            if legacy_path.exists() {
                return legacy_path;
            }
        }

        // New per-channel path
        base_dir
            .join("cursors")
            .join(channel)
            .join(format!("{}.json", agent))
    }

    /// Load a cursor from disk, or create a new one if it doesn't exist
    pub fn load_or_create(base_dir: &Path, channel: &str, agent: &str) -> Result<Self> {
        let path = Self::file_path(base_dir, channel, agent);
        if path.exists() {
            Self::load(&path)
        } else {
            Ok(Self::new(agent))
        }
    }

    /// Load a cursor from a file
    pub fn load(path: &Path) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        Ok(serde_json::from_str(&contents)?)
    }

    /// Save the cursor to disk
    ///
    /// Note: The channel name must be provided since it's not stored in the Cursor struct.
    pub fn save(&self, base_dir: &Path, channel: &str) -> Result<()> {
        let path = Self::file_path(base_dir, channel, &self.agent);

        // Ensure cursors directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Write atomically by writing to temp file then renaming
        let temp_path = path.with_extension("json.tmp");
        let mut file = File::create(&temp_path)?;
        let json = serde_json::to_string_pretty(self)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        crate::paths::atomic_rename(&temp_path, &path)?;

        Ok(())
    }

    /// Update the cursor position
    pub fn update(&mut self, position: u64, last_message_id: Option<String>) {
        self.position = position;
        self.last_message_id = last_message_id;
        self.updated_at = Utc::now();
    }

    /// Reset the cursor to the beginning
    pub fn reset(&mut self) {
        self.position = 0;
        self.last_message_id = None;
        self.updated_at = Utc::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_cursor_creation() {
        let cursor = Cursor::new("agent1");
        assert_eq!(cursor.agent, "agent1");
        assert_eq!(cursor.position, 0);
        assert!(cursor.last_message_id.is_none());
    }

    #[test]
    fn test_cursor_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let mut cursor = Cursor::new("test_agent");
        cursor.update(100, Some("msg-123".to_string()));

        cursor.save(temp_dir.path(), "midtown").unwrap();

        let loaded = Cursor::load_or_create(temp_dir.path(), "midtown", "test_agent").unwrap();
        assert_eq!(loaded.agent, "test_agent");
        assert_eq!(loaded.position, 100);
        assert_eq!(loaded.last_message_id, Some("msg-123".to_string()));
    }

    #[test]
    fn test_cursor_load_or_create_new() {
        let temp_dir = TempDir::new().unwrap();
        let cursor = Cursor::load_or_create(temp_dir.path(), "midtown", "new_agent").unwrap();
        assert_eq!(cursor.agent, "new_agent");
        assert_eq!(cursor.position, 0);
    }

    #[test]
    fn test_cursor_per_channel() {
        let temp_dir = TempDir::new().unwrap();

        // Create cursors for same agent on different channels
        let mut cursor1 = Cursor::new("agent1");
        cursor1.update(100, Some("msg-1".to_string()));
        cursor1.save(temp_dir.path(), "channel-a").unwrap();

        let mut cursor2 = Cursor::new("agent1");
        cursor2.update(200, Some("msg-2".to_string()));
        cursor2.save(temp_dir.path(), "channel-b").unwrap();

        // Load and verify they're independent
        let loaded1 = Cursor::load_or_create(temp_dir.path(), "channel-a", "agent1").unwrap();
        assert_eq!(loaded1.position, 100);
        assert_eq!(loaded1.last_message_id, Some("msg-1".to_string()));

        let loaded2 = Cursor::load_or_create(temp_dir.path(), "channel-b", "agent1").unwrap();
        assert_eq!(loaded2.position, 200);
        assert_eq!(loaded2.last_message_id, Some("msg-2".to_string()));
    }

    #[test]
    fn test_cursor_reset() {
        let mut cursor = Cursor::new("agent1");
        cursor.update(500, Some("msg-xyz".to_string()));
        cursor.reset();
        assert_eq!(cursor.position, 0);
        assert!(cursor.last_message_id.is_none());
    }
}
