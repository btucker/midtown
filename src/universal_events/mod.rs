//! Universal event types for provider-agnostic session event processing.
//!
//! These types normalize events from different AI providers (Claude, etc.)
//! into a common representation for downstream consumers.

pub mod claude;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A normalized event item from any AI provider session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniversalItem {
    /// Unique identifier for this item.
    pub item_id: String,
    /// The kind of item (message, tool call, etc.).
    pub kind: ItemKind,
    /// Content blocks within this item.
    pub content: Vec<ContentPart>,
    /// Current status of the item.
    pub status: ItemStatus,
    /// When this item was created/observed.
    pub timestamp: DateTime<Utc>,
}

/// The kind of universal item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ItemKind {
    /// A text message from the model.
    Message,
    /// A tool/function call.
    ToolCall,
}

/// A content block within a universal item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ContentPart {
    /// Plain text content.
    Text { text: String },
    /// A tool invocation with name, input arguments, provider call ID, and a human-readable header.
    ToolCall {
        name: String,
        input: serde_json::Value,
        call_id: String,
        /// Human-readable summary of the tool call (e.g., `$ git status`, `read src/main.rs`).
        semantic_header: String,
    },
    /// The result of a tool invocation, matched by call ID.
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
    },
}

/// Status of a universal item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ItemStatus {
    /// The item is still being streamed / awaiting completion.
    InProgress,
    /// The item is fully received.
    Completed,
}
