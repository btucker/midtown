//! Per-agent cursor tracking for channel reading

use crate::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Cursor state for an agent's read position in a channel.
///
/// Each agent session maintains a cursor that tracks where they've read up to
/// in the channel log. Cursors are session-scoped: each session gets its own
/// cursor file, preventing sessions from inheriting stale positions from
/// previous sessions.
///
/// # Examples
///
/// Creating and updating a cursor:
///
/// ```
/// use midtown::Cursor;
///
/// let mut cursor = Cursor::new("agent1", "session-abc");
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
    /// The session this cursor belongs to
    pub session_id: String,
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
    /// let cursor = Cursor::new("my-agent", "session-123");
    /// assert_eq!(cursor.agent, "my-agent");
    /// assert_eq!(cursor.session_id, "session-123");
    /// assert_eq!(cursor.position, 0);
    /// ```
    pub fn new(agent: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            session_id: session_id.into(),
            position: 0,
            last_message_id: None,
            updated_at: Utc::now(),
        }
    }

    /// Get the path for a cursor file
    ///
    /// Cursors are stored at `channels/<channel>/cursors/<agent>/<session_id>.json`,
    /// colocated with the channel directory they track. The per-agent subdirectory
    /// isolates sessions from each other: each session_id gets its own file, so
    /// concurrent or successive sessions with the same name don't share state.
    pub fn file_path(base_dir: &Path, channel: &str, agent: &str, session_id: &str) -> PathBuf {
        base_dir
            .join("channels")
            .join(channel)
            .join("cursors")
            .join(agent)
            .join(format!("{}.json", session_id))
    }

    /// Load a cursor from disk, or create a new one at position 0 if it doesn't exist.
    ///
    /// Returns a fresh cursor at position 0 when no file exists. For callers that
    /// want to start at end-of-file (e.g., new lead sessions that should only see
    /// messages from their own lifetime), use [`Channel::set_cursor_to_end`] after
    /// loading.
    pub fn load_or_create(
        base_dir: &Path,
        channel: &str,
        agent: &str,
        session_id: &str,
    ) -> Result<Self> {
        let path = Self::file_path(base_dir, channel, agent, session_id);
        if path.exists() {
            Self::load(&path)
        } else {
            Ok(Self::new(agent, session_id))
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
        let path = Self::file_path(base_dir, channel, &self.agent, &self.session_id);

        // Ensure cursors/<agent>/ directory exists
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

    /// Delete this cursor's file from disk (for cleanup on session end).
    ///
    /// Silently ignores errors (e.g., file already deleted).
    pub fn delete(&self, base_dir: &Path, channel: &str) {
        let path = Self::file_path(base_dir, channel, &self.agent, &self.session_id);
        let _ = fs::remove_file(&path);

        // Remove the agent directory if it's now empty
        if let Some(agent_dir) = path.parent() {
            let _ = fs::remove_dir(agent_dir); // only succeeds if empty
        }
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
        let cursor = Cursor::new("agent1", "session-abc");
        assert_eq!(cursor.agent, "agent1");
        assert_eq!(cursor.session_id, "session-abc");
        assert_eq!(cursor.position, 0);
        assert!(cursor.last_message_id.is_none());
    }

    #[test]
    fn test_cursor_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let mut cursor = Cursor::new("test_agent", "session-abc");
        cursor.update(100, Some("msg-123".to_string()));

        cursor.save(temp_dir.path(), "midtown").unwrap();

        let loaded =
            Cursor::load_or_create(temp_dir.path(), "midtown", "test_agent", "session-abc")
                .unwrap();
        assert_eq!(loaded.agent, "test_agent");
        assert_eq!(loaded.session_id, "session-abc");
        assert_eq!(loaded.position, 100);
        assert_eq!(loaded.last_message_id, Some("msg-123".to_string()));
    }

    #[test]
    fn test_cursor_load_or_create_new() {
        let temp_dir = TempDir::new().unwrap();
        let cursor =
            Cursor::load_or_create(temp_dir.path(), "midtown", "new_agent", "session-xyz").unwrap();
        assert_eq!(cursor.agent, "new_agent");
        assert_eq!(cursor.session_id, "session-xyz");
        assert_eq!(cursor.position, 0);
    }

    #[test]
    fn test_cursor_per_channel() {
        let temp_dir = TempDir::new().unwrap();

        // Create cursors for same agent on different channels
        let mut cursor1 = Cursor::new("agent1", "session-abc");
        cursor1.update(100, Some("msg-1".to_string()));
        cursor1.save(temp_dir.path(), "channel-a").unwrap();

        let mut cursor2 = Cursor::new("agent1", "session-abc");
        cursor2.update(200, Some("msg-2".to_string()));
        cursor2.save(temp_dir.path(), "channel-b").unwrap();

        // Load and verify they're independent
        let loaded1 =
            Cursor::load_or_create(temp_dir.path(), "channel-a", "agent1", "session-abc").unwrap();
        assert_eq!(loaded1.position, 100);
        assert_eq!(loaded1.last_message_id, Some("msg-1".to_string()));

        let loaded2 =
            Cursor::load_or_create(temp_dir.path(), "channel-b", "agent1", "session-abc").unwrap();
        assert_eq!(loaded2.position, 200);
        assert_eq!(loaded2.last_message_id, Some("msg-2".to_string()));
    }

    #[test]
    fn test_cursor_session_isolation() {
        let temp_dir = TempDir::new().unwrap();

        // Two sessions for same agent on same channel are independent
        let mut cursor1 = Cursor::new("lead", "session-1");
        cursor1.update(100, Some("msg-old".to_string()));
        cursor1.save(temp_dir.path(), "midtown").unwrap();

        // Session 2 starts fresh — gets position 0, not session-1's position
        let cursor2 =
            Cursor::load_or_create(temp_dir.path(), "midtown", "lead", "session-2").unwrap();
        assert_eq!(cursor2.position, 0);
        assert!(cursor2.last_message_id.is_none());

        // Verify session-1's cursor is unchanged
        let loaded1 =
            Cursor::load_or_create(temp_dir.path(), "midtown", "lead", "session-1").unwrap();
        assert_eq!(loaded1.position, 100);
    }

    #[test]
    fn test_cursor_file_path() {
        use std::path::Path;
        let base = Path::new("/tmp/test");
        let path = Cursor::file_path(base, "midtown", "lead", "session-abc");
        assert_eq!(
            path,
            Path::new("/tmp/test/channels/midtown/cursors/lead/session-abc.json")
        );
    }

    #[test]
    fn test_cursor_delete() {
        let temp_dir = TempDir::new().unwrap();
        let mut cursor = Cursor::new("agent1", "session-abc");
        cursor.update(100, Some("msg-1".to_string()));
        cursor.save(temp_dir.path(), "midtown").unwrap();

        // Verify file exists
        let path = Cursor::file_path(temp_dir.path(), "midtown", "agent1", "session-abc");
        assert!(path.exists());

        // Delete and verify gone
        cursor.delete(temp_dir.path(), "midtown");
        assert!(!path.exists());
    }

    #[test]
    fn test_cursor_reset() {
        let mut cursor = Cursor::new("agent1", "session-abc");
        cursor.update(500, Some("msg-xyz".to_string()));
        cursor.reset();
        assert_eq!(cursor.position, 0);
        assert!(cursor.last_message_id.is_none());
    }
}
