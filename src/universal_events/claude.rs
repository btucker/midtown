//! Claude-specific converter from `StreamEvent` to `UniversalItem`.
//!
//! Extracts tool calls and tool results from Claude Code's JSON streaming output
//! and converts them into provider-agnostic `UniversalItem` values.

use super::{ContentPart, ItemKind, ItemStatus, UniversalItem};
use crate::headless::StreamEvent;

#[path = "claude_tests.rs"]
#[cfg(test)]
mod tests;

/// Compute a human-readable summary header for a tool call.
///
/// Inspects the tool name and input arguments to produce a concise,
/// Pi-agent-style description of what the tool is doing.
///
/// # Format table
///
/// | Tool name    | Header format                            |
/// |--------------|------------------------------------------|
/// | `Bash`       | `$ <command>` (truncated at 60 chars)    |
/// | `Edit`       | `edit <file_path>`                       |
/// | `Write`      | `write <file_path>`                      |
/// | `Read`       | `read <file_path>`                       |
/// | `Glob`       | `glob <pattern>`                         |
/// | `Grep`       | `grep /<pattern>/`                       |
/// | `Task`       | `task: <description>` (first 40 chars)   |
/// | `NotebookEdit` | `notebook edit <notebook_path>`        |
/// | `WebFetch`   | `fetch <host>` (domain only)             |
/// | `WebSearch`  | `search "<query>"`                       |
/// | `TodoWrite`  | `todo: update`                           |
/// | `ExitPlanMode` | `exit plan mode`                       |
/// | `MultiEdit`  | `multi-edit <file_path>`                 |
/// | (default)    | lowercase tool name                      |
pub fn semantic_header(name: &str, input: &serde_json::Value) -> String {
    match name {
        "Bash" => {
            let command = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let char_count = command.chars().count();
            if char_count > 60 {
                let truncated = truncate_chars(command, 59);
                format!("$ {truncated}\u{2026}")
            } else {
                format!("$ {command}")
            }
        }
        "Edit" => {
            let path = first_path_field(input);
            format!("edit {path}")
        }
        "Write" => {
            let path = first_path_field(input);
            format!("write {path}")
        }
        "Read" => {
            let path = first_path_field(input);
            format!("read {path}")
        }
        "Glob" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            format!("glob {pattern}")
        }
        "Grep" => {
            let pattern = input.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            format!("grep /{pattern}/")
        }
        "Task" => {
            let desc = input
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let truncated = truncate_chars(desc, 40);
            format!("task: {truncated}")
        }
        "NotebookEdit" => {
            let path = first_path_field(input);
            format!("notebook edit {path}")
        }
        "WebFetch" => {
            let url_str = input.get("url").and_then(|v| v.as_str()).unwrap_or("");
            // Extract just the host from the URL (e.g. "https://example.com/path" → "example.com").
            // We strip the scheme prefix and take the first path component.
            let host = extract_url_host(url_str).unwrap_or_else(|| truncate_chars(url_str, 60));
            format!("fetch {host}")
        }
        "WebSearch" => {
            let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");
            format!("search \"{query}\"")
        }
        "TodoWrite" => "todo: update".to_string(),
        "ExitPlanMode" => "exit plan mode".to_string(),
        "MultiEdit" => {
            let path = first_path_field(input);
            format!("multi-edit {path}")
        }
        _ => name.to_lowercase(),
    }
}

/// Return the first path-like field found in the input object.
///
/// Tries `file_path`, `notebook_path`, `path` in order; returns empty string if none found.
fn first_path_field(input: &serde_json::Value) -> &str {
    for key in &["file_path", "notebook_path", "path"] {
        if let Some(v) = input.get(key).and_then(|v| v.as_str()) {
            return v;
        }
    }
    ""
}

/// Truncate a string to at most `max_chars` Unicode scalar values.
///
/// If truncated, the returned slice ends before the character that would exceed the limit.
/// Callers that need ellipsis should append it themselves.
fn truncate_chars(s: &str, max_chars: usize) -> &str {
    let mut char_indices = s.char_indices();
    // Advance past `max_chars` characters.
    let cutoff = char_indices.nth(max_chars).map(|(i, _)| i);
    match cutoff {
        // String is longer than max_chars — slice to the byte offset of char at max_chars.
        Some(end) => &s[..end],
        None => s,
    }
}

/// Extract only the host portion from a URL string.
///
/// Handles common URL forms: `https://host/path`, `http://host/path`.
/// Returns `None` if extraction fails (e.g. relative URLs, malformed input).
fn extract_url_host(url: &str) -> Option<&str> {
    // Strip scheme (e.g. "https://")
    let after_scheme = url.find("://").map(|pos| &url[pos + 3..])?;
    // The host ends at the first '/', '?', '#', or ':' (port separator).
    let host_end = after_scheme
        .find(['/', '?', '#', ':'])
        .unwrap_or(after_scheme.len());
    let host = &after_scheme[..host_end];
    if host.is_empty() { None } else { Some(host) }
}

/// Extract both tool call and tool result items from Claude stream events.
///
/// Iterates over:
/// - `StreamEvent::Assistant` events: extracts `tool_use` content blocks → `ContentPart::ToolCall`
/// - `StreamEvent::User` events: extracts `tool_result` content blocks → `ContentPart::ToolResult`
///
/// Uses Claude's `call_id` / `tool_use_id` as the `item_id` for deterministic output.
/// The provided `timestamp` is applied to all extracted items.
///
/// Non-matching events and non-matching content blocks are skipped.
pub fn extract_tool_events(
    events: &[StreamEvent],
    timestamp: chrono::DateTime<chrono::Utc>,
) -> Vec<UniversalItem> {
    let mut items = Vec::new();

    for event in events {
        match event {
            StreamEvent::Assistant { message, .. } => {
                if let Some(content) = message.get("content")
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
                            let header = semantic_header(&name, &input);

                            items.push(UniversalItem {
                                item_id: call_id.clone(),
                                kind: ItemKind::ToolCall,
                                content: vec![ContentPart::ToolCall {
                                    name,
                                    input,
                                    call_id,
                                    semantic_header: header,
                                }],
                                status: ItemStatus::Completed,
                                timestamp,
                            });
                        }
                    }
                }
            }
            StreamEvent::User { message, .. } => {
                if let Some(content) = message.get("content")
                    && let Some(arr) = content.as_array()
                {
                    for block in arr {
                        if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            let call_id = block
                                .get("tool_use_id")
                                .and_then(|id| id.as_str())
                                .unwrap_or("")
                                .to_string();
                            let is_error = block
                                .get("is_error")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                            let output = extract_tool_result_content(block);

                            items.push(UniversalItem {
                                item_id: format!("result:{call_id}"),
                                kind: ItemKind::ToolResult,
                                content: vec![ContentPart::ToolResult {
                                    call_id,
                                    output,
                                    is_error,
                                }],
                                status: ItemStatus::Completed,
                                timestamp,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    items
}

/// Maximum bytes retained from a tool result's output string.
///
/// The TUI only uses `is_error` from ToolResult items — the raw output is never
/// displayed but is serialized over RPC on every `kanban.data` poll. Capping at
/// 256 bytes prevents large Read/Bash/Grep outputs from bloating daemon memory
/// and RPC payloads.
const MAX_TOOL_RESULT_OUTPUT_BYTES: usize = 256;

/// Extract the output string from a `tool_result` content block.
///
/// The `content` field can be:
/// - A string: returned directly.
/// - An array of text blocks: joined together.
/// - Missing/null: returns an empty string.
///
/// Output is truncated to `MAX_TOOL_RESULT_OUTPUT_BYTES` to bound memory usage.
fn extract_tool_result_content(block: &serde_json::Value) -> String {
    let full = match block.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|item| {
                if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                    item.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    if full.len() > MAX_TOOL_RESULT_OUTPUT_BYTES {
        let boundary = full.floor_char_boundary(MAX_TOOL_RESULT_OUTPUT_BYTES);
        full[..boundary].to_string()
    } else {
        full
    }
}
