//! Message types for channel communication

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Types of messages that can be sent through a channel.
///
/// # Examples
///
/// ```
/// use midtown::MessageType;
///
/// let msg_type = MessageType::default();
/// assert_eq!(msg_type, MessageType::Text);
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    /// Regular text message
    #[default]
    Text,
    /// System notification
    System,
    /// Command or instruction
    Command,
    /// Status update
    Status,
    /// Error notification
    Error,
}

/// A message in the channel log.
///
/// Messages are the primary unit of communication in midtown channels.
/// Each message has a unique ID, timestamp, sender, content, and type.
///
/// # Examples
///
/// Creating different message types:
///
/// ```
/// use midtown::{Message, MessageType};
///
/// // Text message from an agent
/// let text = Message::text("agent1", "Hello, team!");
/// assert_eq!(text.from, "agent1");
/// assert_eq!(text.message_type, MessageType::Text);
///
/// // System notification
/// let sys = Message::system("Build completed");
/// assert_eq!(sys.from, "system");
/// assert_eq!(sys.message_type, MessageType::System);
///
/// // Status update
/// let status = Message::status("agent2", "Working on task #42");
/// assert_eq!(status.message_type, MessageType::Status);
/// ```
///
/// Messages serialize to JSON for storage:
///
/// ```
/// use midtown::Message;
///
/// let msg = Message::text("agent1", "Hello");
/// let json = serde_json::to_string(&msg).unwrap();
/// assert!(json.contains("agent1"));
/// assert!(json.contains("Hello"));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message identifier
    pub id: String,
    /// When the message was created
    pub timestamp: DateTime<Utc>,
    /// Who sent the message (agent name or role)
    pub from: String,
    /// Message content
    pub content: String,
    /// Type of message
    #[serde(rename = "type")]
    pub message_type: MessageType,
}

impl Message {
    /// Create a new message with auto-generated ID and timestamp.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::new("agent1", "Task completed", MessageType::Status);
    /// assert_eq!(msg.from, "agent1");
    /// assert_eq!(msg.content, "Task completed");
    /// assert!(!msg.id.is_empty()); // UUID auto-generated
    /// ```
    pub fn new(
        from: impl Into<String>,
        content: impl Into<String>,
        message_type: MessageType,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            from: from.into(),
            content: content.into(),
            message_type,
        }
    }

    /// Create a text message.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::text("alice", "Hello world");
    /// assert_eq!(msg.message_type, MessageType::Text);
    /// ```
    pub fn text(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Text)
    }

    /// Create a system message.
    ///
    /// System messages automatically use "system" as the sender.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::Message;
    ///
    /// let msg = Message::system("Daemon started");
    /// assert_eq!(msg.from, "system");
    /// ```
    pub fn system(content: impl Into<String>) -> Self {
        Self::new("system", content, MessageType::System)
    }

    /// Create a command message.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::command("lead", "Build the feature");
    /// assert_eq!(msg.message_type, MessageType::Command);
    /// ```
    pub fn command(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Command)
    }

    /// Create a status message.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::status("worker1", "Compiling...");
    /// assert_eq!(msg.message_type, MessageType::Status);
    /// ```
    pub fn status(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Status)
    }

    /// Create an error message.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::error("worker1", "Build failed");
    /// assert_eq!(msg.message_type, MessageType::Error);
    /// ```
    pub fn error(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_creation() {
        let msg = Message::text("agent1", "Hello, world!");
        assert_eq!(msg.from, "agent1");
        assert_eq!(msg.content, "Hello, world!");
        assert_eq!(msg.message_type, MessageType::Text);
        assert!(!msg.id.is_empty());
    }

    #[test]
    fn test_message_serialization() {
        let msg = Message::text("agent1", "Hello");
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.from, msg.from);
        assert_eq!(parsed.content, msg.content);
    }

    #[test]
    fn test_system_message() {
        let msg = Message::system("System initialized");
        assert_eq!(msg.from, "system");
        assert_eq!(msg.message_type, MessageType::System);
    }
}
