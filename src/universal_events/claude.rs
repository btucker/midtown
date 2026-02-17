//! Claude-specific converter from `StreamEvent` to `UniversalItem`.
//!
//! Extracts tool calls from Claude Code's JSON streaming output and converts
//! them into provider-agnostic `UniversalItem` values.

use super::{ContentPart, ItemKind, ItemStatus, UniversalItem};
use crate::headless::StreamEvent;

#[path = "claude_tests.rs"]
#[cfg(test)]
mod tests;

/// Extract tool call items from Claude stream events.
///
/// Iterates over `StreamEvent::Assistant` events, inspects each content block,
/// and produces a `UniversalItem` for every `tool_use` block found.
///
/// Non-assistant events and non-tool-use content blocks are skipped.
pub fn extract_tool_calls(events: &[StreamEvent]) -> Vec<UniversalItem> {
    let mut items = Vec::new();

    for event in events {
        if let StreamEvent::Assistant { message, .. } = event
            && let Some(content) = message.get("content")
            && let Some(arr) = content.as_array()
        {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    let name = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = block
                        .get("input")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let call_id = block
                        .get("id")
                        .and_then(|id| id.as_str())
                        .unwrap_or("")
                        .to_string();

                    items.push(UniversalItem {
                        item_id: uuid::Uuid::new_v4().to_string(),
                        kind: ItemKind::ToolCall,
                        content: vec![ContentPart::ToolCall {
                            name,
                            input,
                            call_id,
                        }],
                        status: ItemStatus::Completed,
                        timestamp: chrono::Utc::now(),
                    });
                }
            }
        }
    }

    items
}
