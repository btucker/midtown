//! Message types for channel communication

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Types of messages that can be sent through a channel
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

/// A message in the channel log
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
    /// Create a new message with auto-generated ID and timestamp
    pub fn new(from: impl Into<String>, content: impl Into<String>, message_type: MessageType) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            from: from.into(),
            content: content.into(),
            message_type,
        }
    }

    /// Create a text message
    pub fn text(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Text)
    }

    /// Create a system message
    pub fn system(content: impl Into<String>) -> Self {
        Self::new("system", content, MessageType::System)
    }

    /// Create a command message
    pub fn command(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Command)
    }

    /// Create a status message
    pub fn status(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Status)
    }

    /// Create an error message
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
