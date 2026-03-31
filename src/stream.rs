//! Stream event extraction utilities.
//!
//! Pure functions that extract structured data from headless session stream events.
//! Used by the daemon executor to post text, tool blocks, and insights to channels.

use crate::headless::StreamEvent;
use crate::json_ext::ValueExt;
use crate::message::ToolBlock;
use std::collections::HashMap;

fn extract_text_blocks(message: &serde_json::Value) -> String {
    let mut text = String::new();
    if let Some(content) = message.get("content")
        && let Some(arr) = content.as_array()
    {
        for block in arr {
            if block.get("type").and_then(|t| t.as_str()) == Some("text")
                && let Some(chunk) = block.get("text").and_then(|t| t.as_str())
            {
                text.push_str(chunk);
            }
        }
    }
    text
}

/// Extract and aggregate text content from `StreamEvent::Assistant` events.
///
/// Collects all text blocks from all Assistant events in a single drain cycle,
/// returning a single aggregated string. This avoids flooding the channel with
/// per-token or per-event messages during streaming.
///
/// For Codex sessions, prefers `item/completed` assistant events (which contain
/// the full message text) over per-delta events to avoid duplication. When no
/// `item/completed` events are available (the normal Codex flow is deltas →
/// `turn/completed`), falls back to the result text from `StreamEvent::Result`.
pub fn extract_assistant_text(events: &[StreamEvent]) -> String {
    let mut aggregated = String::new();
    let mut has_codex_completed = false;
    let mut has_codex_deltas = false;

    for event in events {
        if let StreamEvent::Assistant { message, extra, .. } = event {
            let text = extract_text_blocks(message);
            if text.is_empty() {
                continue;
            }

            let is_codex = extra.str_field("provider") == Some("codex");
            if !is_codex {
                aggregated.push_str(&text);
                continue;
            }

            let is_completed = extra.str_field("event") == Some("item/completed");
            if is_completed {
                has_codex_completed = true;
                aggregated.push_str(&text);
            } else {
                has_codex_deltas = true;
            }
        }
    }

    // Codex fallback: the normal Codex protocol flow is deltas → turn/completed
    // without a separate item/completed for agentMessage items. When no
    // item/completed text was found, use the turn/completed result text instead.
    //
    // Guard: only fall back when Codex deltas were present in this drain cycle.
    // A bare Result without deltas likely means item/completed was already posted
    // in a previous drain — using Result here would duplicate the post.
    if aggregated.is_empty() && !has_codex_completed && has_codex_deltas {
        for event in events {
            if let StreamEvent::Result {
                result: Some(text),
                is_error: false,
                extra,
                ..
            } = event
            {
                let is_codex = extra.str_field("provider") == Some("codex");
                if is_codex && !text.is_empty() {
                    aggregated.push_str(text);
                }
            }
        }
    }

    aggregated
}

/// Extract `parent_tool_use_id` from a stream event's `extra` field.
///
/// Checks both `parent_tool_use_id` (current format, snake_case) and
/// `parentToolUseID` (legacy camelCase) for forward/backward compatibility.
fn get_parent_tool_use_id(extra: &serde_json::Value) -> Option<String> {
    extra
        .get("parent_tool_use_id")
        .or_else(|| extra.get("parentToolUseID"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Extract structured ToolBlock data from stream events.
///
/// Pairs `tool_use` blocks from Assistant events with `tool_result` blocks
/// from User events by `call_id`. The raw input/output JSON is preserved
/// so the client can render tool-specific UI.
///
/// Each ToolBlock carries `call_id` (the tool_use block's `id`) and
/// `parent_tool_use_id` (from the event's `parentToolUseID` field) for
/// sub-agent thread resolution.
pub fn extract_tool_blocks(events: &[StreamEvent]) -> Vec<ToolBlock> {
    // Collect tool results keyed by call_id from top-level User events.
    let mut results: HashMap<String, (serde_json::Value, bool)> = HashMap::new();
    for event in events {
        if let StreamEvent::User { message, .. } = event
            && let Some(content) = message.get("content")
            && let Some(arr) = content.as_array()
        {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    let call_id = block
                        .get("tool_use_id")
                        .and_then(json_value_as_string)
                        .unwrap_or_default();
                    let is_error = block
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let output = extract_tool_result_json(block);
                    results.insert(call_id, (output, is_error));
                }
            }
        }
    }

    // Also collect tool results from sub-agent progress events.
    for event in events {
        if let StreamEvent::Progress { data, .. } = event
            && data.get("type").and_then(|t| t.as_str()) == Some("agent_progress")
            && let Some(inner_msg) = data.get("message").and_then(|m| m.get("message"))
            && data
                .get("message")
                .and_then(|m| m.get("type"))
                .and_then(|t| t.as_str())
                == Some("user")
            && let Some(arr) = inner_msg.get("content").and_then(|c| c.as_array())
        {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    let call_id = block
                        .get("tool_use_id")
                        .and_then(json_value_as_string)
                        .unwrap_or_default();
                    let is_error = block
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let output = extract_tool_result_json(block);
                    results.insert(call_id, (output, is_error));
                }
            }
        }
    }

    // Extract tool calls with their results from top-level Assistant events.
    let mut blocks = Vec::new();
    for event in events {
        if let StreamEvent::Assistant { message, extra, .. } = event
            && let Some(content) = message.get("content")
            && let Some(arr) = content.as_array()
        {
            let parent_tool_use_id = get_parent_tool_use_id(extra);
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
                        .and_then(json_value_as_string)
                        .unwrap_or_default();
                    let (output, error) = match results.get(&call_id) {
                        Some((out, is_err)) => (Some(out.clone()), *is_err),
                        None => (None, false),
                    };
                    blocks.push(ToolBlock {
                        tool_name: name,
                        input,
                        output,
                        error,
                        call_id: Some(call_id),
                        parent_tool_use_id: parent_tool_use_id.clone(),
                    });
                }
            }
        }
    }

    // Extract tool calls from sub-agent progress events.
    for event in events {
        if let StreamEvent::Progress {
            data,
            parent_tool_use_id,
            ..
        } = event
            && data.get("type").and_then(|t| t.as_str()) == Some("agent_progress")
            && parent_tool_use_id.is_some()
            && let Some(inner_msg) = data.get("message").and_then(|m| m.get("message"))
            && data
                .get("message")
                .and_then(|m| m.get("type"))
                .and_then(|t| t.as_str())
                == Some("assistant")
            && let Some(arr) = inner_msg.get("content").and_then(|c| c.as_array())
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
                        .and_then(json_value_as_string)
                        .unwrap_or_default();
                    let (output, error) = match results.get(&call_id) {
                        Some((out, is_err)) => (Some(out.clone()), *is_err),
                        None => (None, false),
                    };
                    blocks.push(ToolBlock {
                        tool_name: name,
                        input,
                        output,
                        error,
                        call_id: Some(call_id),
                        parent_tool_use_id: parent_tool_use_id.clone(),
                    });
                }
            }
        }
    }

    blocks
}

/// Extract tool result content as a JSON value for structured storage.
fn extract_tool_result_json(block: &serde_json::Value) -> serde_json::Value {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => {
            let truncated = truncate_str(s, MAX_DM_TOOL_OUTPUT_BYTES);
            serde_json::Value::String(truncated.to_string())
        }
        Some(serde_json::Value::Array(arr)) => {
            let text: String = arr
                .iter()
                .filter_map(|item| {
                    if item.get("type").and_then(|t| t.as_str()) == Some("text") {
                        item.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");
            let truncated = truncate_str(&text, MAX_DM_TOOL_OUTPUT_BYTES);
            serde_json::Value::String(truncated.to_string())
        }
        _ => serde_json::Value::Null,
    }
}

/// Maximum bytes for tool result output in DM channel messages.
const MAX_DM_TOOL_OUTPUT_BYTES: usize = 4096;

/// Coerce a JSON value to a String (handles string, number, bool).
fn json_value_as_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Truncate a string to at most `max_bytes` at a char boundary.
fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        s
    } else {
        let boundary = s.floor_char_boundary(max_bytes);
        &s[..boundary]
    }
}

/// Extract insight blocks from text.
///
/// Looks for `★ Insight` blocks delimited by lines of dashes (with optional
/// backtick wrappers). Returns the trimmed content of each insight block.
pub fn extract_insights(text: &str) -> Vec<String> {
    let mut insights = Vec::new();

    let start_marker = "★ Insight";
    let end_markers = [
        "`─────────────────────────────────────────────────`",
        "─────────────────────────────────────────────────",
    ];

    let mut pos = 0;
    while let Some(start) = text[pos..].find(start_marker) {
        let start_abs = pos + start;
        if let Some(header_end) = text[start_abs..].find('\n') {
            let content_start = start_abs + header_end + 1;
            let end_pos = end_markers
                .iter()
                .filter_map(|marker| text[content_start..].find(marker))
                .min();

            if let Some(end) = end_pos {
                let insight = text[content_start..content_start + end].trim().to_string();
                if !insight.is_empty() {
                    insights.push(insight);
                }
                pos = content_start + end;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    insights
}
