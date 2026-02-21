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
    /// Action message (IRC-style /me)
    Action,
    /// Insight message (architectural diagram, codebase learning)
    Insight,
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
/// assert_eq!(sys.from, "daemon");
/// assert_eq!(sys.message_type, MessageType::System);
///
/// // Status update
/// let status = Message::status("agent2", "Working on task !42");
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
    /// Channel name (defaults to "midtown" for backward compatibility).
    /// Stored as Option for backward compat with existing struct literals,
    /// but always initialized in constructors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Optional source channel for cross-posts (None if not a cross-post)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_channel: Option<String>,
    /// Optional Claude session ID for disambiguation when multiple sessions
    /// share the same coworker name. `None` for messages from system, lead,
    /// or legacy messages before session tracking was added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Optional thread parent message ID. When set, this message is a reply
    /// in a thread started by the message with this ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_parent_id: Option<String>,
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
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        }
    }

    /// Create a new message for a specific channel.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::for_channel("pr-discussion", "agent1", "Let's review", MessageType::Text);
    /// assert_eq!(msg.channel_name(), "pr-discussion");
    /// ```
    pub fn for_channel(
        channel: impl Into<String>,
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
            channel: Some(channel.into()),
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        }
    }

    /// Create a thread reply message for a specific channel.
    pub fn thread_reply(
        channel: impl Into<String>,
        from: impl Into<String>,
        content: impl Into<String>,
        thread_parent_id: impl Into<String>,
        message_type: MessageType,
    ) -> Self {
        let mut msg = Self::for_channel(channel, from, content, message_type);
        msg.thread_parent_id = Some(thread_parent_id.into());
        msg
    }

    /// Get the channel name (defaults to "midtown" if not set).
    pub fn channel_name(&self) -> &str {
        self.channel.as_deref().unwrap_or("midtown")
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
    /// System messages automatically use "daemon" as the sender.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::Message;
    ///
    /// let msg = Message::system("Daemon started");
    /// assert_eq!(msg.from, "daemon");
    /// ```
    pub fn system(content: impl Into<String>) -> Self {
        Self::new("daemon", content, MessageType::System)
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

    /// Create an action message (IRC-style /me).
    ///
    /// Action messages are displayed as `* name action` in chat,
    /// following IRC convention.
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::action("lexington", "investigating the auth bug");
    /// assert_eq!(msg.message_type, MessageType::Action);
    /// assert_eq!(msg.from, "lexington");
    /// // Displays as: * lexington investigating the auth bug
    /// ```
    pub fn action(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Action)
    }

    /// Create an insight message (architectural diagram, codebase learning).
    ///
    /// Insight messages contain analysis or visualizations generated by
    /// specialized headless sessions (architect, etc.).
    ///
    /// # Examples
    ///
    /// ```
    /// use midtown::{Message, MessageType};
    ///
    /// let msg = Message::insight("architect", "```mermaid\ngraph TD\n...");
    /// assert_eq!(msg.message_type, MessageType::Insight);
    /// ```
    pub fn insight(from: impl Into<String>, content: impl Into<String>) -> Self {
        Self::new(from, content, MessageType::Insight)
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
        assert_eq!(msg.from, "daemon");
        assert_eq!(msg.message_type, MessageType::System);
    }

    #[test]
    fn test_action_message() {
        let msg = Message::action("lexington", "investigating the auth bug");
        assert_eq!(msg.from, "lexington");
        assert_eq!(msg.content, "investigating the auth bug");
        assert_eq!(msg.message_type, MessageType::Action);
    }

    #[test]
    fn test_insight_message() {
        let msg = Message::insight("architect", "```mermaid\ngraph TD\nA-->B");
        assert_eq!(msg.from, "architect");
        assert_eq!(msg.message_type, MessageType::Insight);
        assert!(msg.content.contains("mermaid"));
    }

    #[test]
    fn test_channel_defaults_to_midtown() {
        let msg = Message::text("agent1", "Hello");
        assert_eq!(msg.channel_name(), "midtown");
        assert_eq!(msg.source_channel, None);
    }

    #[test]
    fn test_for_channel() {
        let msg =
            Message::for_channel("pr-discussion", "agent1", "Let's review", MessageType::Text);
        assert_eq!(msg.channel_name(), "pr-discussion");
        assert_eq!(msg.from, "agent1");
    }

    #[test]
    fn test_backward_compatibility_deserialize() {
        // Simulate an old message JSON without channel field
        let old_json = r#"{
            "id": "test-id",
            "timestamp": "2026-01-01T00:00:00Z",
            "from": "agent1",
            "content": "Hello",
            "type": "text"
        }"#;

        let msg: Message = serde_json::from_str(old_json).unwrap();
        assert_eq!(msg.channel_name(), "midtown"); // Should default
        assert_eq!(msg.from, "agent1");
        assert_eq!(msg.content, "Hello");
    }

    #[test]
    fn test_backward_compatibility_struct_literal() {
        // Verify that struct literal construction with all fields compiles and
        // that channel_name() returns the default when channel is None.
        let msg = Message {
            id: "test".to_string(),
            timestamp: Utc::now(),
            from: "agent1".to_string(),
            content: "Test".to_string(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        };
        assert_eq!(msg.channel_name(), "midtown"); // channel_name() handles None
    }

    #[test]
    fn test_new_format_serialize_deserialize() {
        let msg = Message::for_channel("pr-discussion", "agent1", "Test", MessageType::Text);
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.channel_name(), "pr-discussion");
        assert_eq!(parsed.from, "agent1");
    }

    #[test]
    fn test_thread_parent_id_serialization() {
        let mut msg = Message::text("agent1", "Reply in thread");
        msg.thread_parent_id = Some("parent-uuid-123".to_string());
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("thread_parent_id"));
        assert!(json.contains("parent-uuid-123"));
        let parsed: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.thread_parent_id, Some("parent-uuid-123".to_string()));
    }

    #[test]
    fn test_thread_parent_id_defaults_to_none() {
        let msg = Message::text("agent1", "Top-level message");
        assert_eq!(msg.thread_parent_id, None);
        let json = serde_json::to_string(&msg).unwrap();
        assert!(!json.contains("thread_parent_id")); // skip_serializing_if = None
    }

    #[test]
    fn test_backward_compat_no_thread_parent_id() {
        let old_json = r#"{
            "id": "test-id",
            "timestamp": "2026-01-01T00:00:00Z",
            "from": "agent1",
            "content": "Hello",
            "type": "text"
        }"#;
        let msg: Message = serde_json::from_str(old_json).unwrap();
        assert_eq!(msg.thread_parent_id, None);
    }
}
