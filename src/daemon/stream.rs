//! Process headless session stream events and generate effects.
//!
//! This module contains pure decision functions that analyze stream events
//! from headless sessions (Lead, channel leads, and coworkers) and produce
//! channel posting and universal event broadcast effects.

use super::effects::Effect;
use crate::headless::StreamEvent;
use crate::message::ToolBlock;
use std::collections::{HashMap, HashSet};

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

            let is_codex = extra.get("provider").and_then(|v| v.as_str()) == Some("codex");
            if !is_codex {
                aggregated.push_str(&text);
                continue;
            }

            let is_completed =
                extra.get("event").and_then(|v| v.as_str()) == Some("item/completed");
            if is_completed {
                // Codex emits per-delta assistant chunks and then a full completed
                // assistant message. For channel posting, emit completed text only
                // to avoid duplicate posts when delta/completed split across drains.
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
                let is_codex = extra.get("provider").and_then(|v| v.as_str()) == Some("codex");
                if is_codex && !text.is_empty() {
                    aggregated.push_str(text);
                }
            }
        }
    }

    aggregated
}

/// Process headless Lead and channel lead output and generate channel posting effects.
///
/// Aggregates all text content from Assistant events in the current drain cycle
/// into a single message to avoid channel flooding.
///
/// - The main lead's text is posted to the main channel (`channel: None`).
/// - Each channel lead's text is posted to its respective topic channel.
/// - Coworker text is handled separately by [`process_coworker_output()`].
///
/// `channel_lead_sessions` maps channel name → session ID for active channel leads.
pub fn process_lead_output(
    events: &HashMap<String, Vec<StreamEvent>>,
    channel_lead_sessions: &HashMap<String, String>,
    main_lead_session_name: &str,
    fork_bound_channels: &HashMap<String, String>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Main lead → posts to main channel.
    if let Some(lead_events) = events.get(main_lead_session_name) {
        let trimmed = extract_assistant_text(lead_events).trim().to_string();
        if !trimmed.is_empty() {
            effects.push(Effect::PostToChannel {
                sender: main_lead_session_name.to_string(),
                message: trimmed,
                channel: None,
                auto_output: true,
                message_type: None,
                nudge_type: None,
                tool_data: None,
                provider: None,
            });
        }
    }

    // Channel leads → each posts to its respective topic channel.
    for channel_name in channel_lead_sessions.keys() {
        if let Some(cl_events) = events.get(channel_name.as_str()) {
            let trimmed = extract_assistant_text(cl_events).trim().to_string();
            if !trimmed.is_empty() {
                effects.push(Effect::PostToChannel {
                    sender: channel_name.clone(),
                    message: trimmed,
                    channel: Some(channel_name.clone()),
                    auto_output: true,
                    message_type: None,
                    nudge_type: None,
                    tool_data: None,
                    provider: None,
                });
            }
        }
    }

    // Forked channel leads → posts to the channel they were inherited from.
    for (fork_name, channel_name) in fork_bound_channels {
        if let Some(fork_events) = events.get(fork_name.as_str()) {
            let trimmed = extract_assistant_text(fork_events).trim().to_string();
            if !trimmed.is_empty() {
                effects.push(Effect::PostToChannel {
                    sender: fork_name.clone(),
                    message: trimmed,
                    channel: Some(channel_name.clone()),
                    auto_output: true,
                    message_type: None,
                    nudge_type: None,
                    tool_data: None,
                    provider: None,
                });
            }
        }
    }

    effects
}

/// Detect the AI provider from stream events.
///
/// Returns `Some("codex")` if any event carries `extra.provider == "codex"`,
/// otherwise `Some("claude")` if any Assistant events are present, or `None`.
fn detect_provider(events: &[StreamEvent]) -> Option<String> {
    let mut has_assistant = false;
    for event in events {
        if let StreamEvent::Assistant { extra, .. } = event {
            has_assistant = true;
            if extra.get("provider").and_then(|v| v.as_str()) == Some("codex") {
                return Some("codex".to_string());
            }
        }
    }
    if has_assistant {
        Some("claude".to_string())
    } else {
        None
    }
}

/// Extract structured ToolBlock data from stream events.
///
/// Pairs `tool_use` blocks from Assistant events with `tool_result` blocks
/// from User events by `call_id`. The raw input/output JSON is preserved
/// so the client can render tool-specific UI.
fn extract_tool_blocks(events: &[StreamEvent]) -> Vec<ToolBlock> {
    // Collect tool results keyed by call_id.
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

    // Extract tool calls with their results.
    let mut blocks = Vec::new();
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
                    });
                }
            }
        }
    }

    blocks
}

/// Extract tool result content as a JSON value for structured storage.
///
/// Returns the content as-is (string or array of text blocks), truncated
/// to the DM channel size limit.
fn extract_tool_result_json(block: &serde_json::Value) -> serde_json::Value {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => {
            let truncated = truncate_str(s, MAX_DM_TOOL_OUTPUT_BYTES);
            serde_json::Value::String(truncated.to_string())
        }
        Some(serde_json::Value::Array(arr)) => {
            // Extract text from text blocks and concatenate
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

/// Process coworker output and generate DM channel posting effects.
///
/// DM channels are a complete stream of the agent — text AND tool calls.
/// All content uses `auto_output: false` because DM channels echo everything
/// (there's no distinction between "auto" and "explicit" output).
///
/// For each coworker session, posts:
/// - Text output (assistant prose) with provider metadata
/// - Structured tool calls as `tool_data` on the message (raw JSON for client rendering)
/// - Extracted `★ Insight` blocks as `PostInsight` effects (routed to the task's channel)
///
/// `coworker_names` is the set of active coworker session names (excluding the
/// main lead, channel leads, and fork-bound sessions).
pub fn process_coworker_output(
    events: &HashMap<String, Vec<StreamEvent>>,
    coworker_names: &HashSet<String>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for name in coworker_names {
        if let Some(coworker_events) = events.get(name.as_str()) {
            let dm_channel = format!("dm-{}", name);
            let provider = detect_provider(coworker_events);

            // Post text output (assistant prose).
            let trimmed = extract_assistant_text(coworker_events).trim().to_string();
            if !trimmed.is_empty() {
                // Extract insights from the text before posting to DM.
                for insight in extract_insights(&trimmed) {
                    effects.push(Effect::PostInsight {
                        agent: name.clone(),
                        insight,
                    });
                }

                effects.push(Effect::PostToChannel {
                    sender: name.clone(),
                    message: trimmed,
                    channel: Some(dm_channel.clone()),
                    auto_output: false,
                    message_type: None,
                    nudge_type: None,
                    tool_data: None,
                    provider: provider.clone(),
                });
            }

            // Post structured tool calls for client-side rendering.
            let tool_blocks = extract_tool_blocks(coworker_events);
            if !tool_blocks.is_empty() {
                effects.push(Effect::PostToChannel {
                    sender: name.clone(),
                    message: String::new(),
                    channel: Some(dm_channel),
                    auto_output: false,
                    message_type: None,
                    nudge_type: None,
                    tool_data: Some(tool_blocks),
                    provider: provider.clone(),
                });
            }
        }
    }

    effects
}

/// Maximum bytes for tool result output in DM channel messages.
/// More generous than the 256-byte limit used for RPC/WebSocket activity items.
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

/// Process lead and channel lead stream events and generate universal event broadcast effects.
///
/// For the main channel, only the lead's tool calls are shown. For each topic channel
/// that has an active channel lead, that lead's tool calls are broadcast with the
/// channel name so the web UI can display them only when viewing that topic channel.
///
/// Forked leads (thread-bound sessions) include `thread_parent_id` so the frontend
/// routes their tool calls to the thread panel instead of the main channel activity strip.
///
/// Coworker tool calls are shown in their DM channels (`dm-<name>`).
pub fn process_universal_events(
    events: &HashMap<String, Vec<StreamEvent>>,
    channel_lead_sessions: &HashMap<String, String>,
    main_lead_session_name: &str,
    fork_bound_channels: &HashMap<String, String>,
    fork_bound_threads: &HashMap<String, String>,
    coworker_names: &HashSet<String>,
) -> Vec<Effect> {
    let timestamp = chrono::Utc::now();
    let mut effects = Vec::new();

    // Main lead → shown in the main channel (channel = None).
    if let Some(lead_events) = events.get(main_lead_session_name) {
        let items = crate::universal_events::claude::extract_tool_events(lead_events, timestamp);
        if !items.is_empty() {
            effects.push(Effect::BroadcastUniversalItems {
                agent_name: main_lead_session_name.to_string(),
                channel: None,
                thread_parent_id: None,
                items,
            });
        }
    }

    // Channel leads → shown only in their respective topic channels.
    // Each channel lead's session name equals the channel name (see launch::channel_lead_session_name).
    for channel_name in channel_lead_sessions.keys() {
        if let Some(cl_events) = events.get(channel_name.as_str()) {
            let items = crate::universal_events::claude::extract_tool_events(cl_events, timestamp);
            if !items.is_empty() {
                effects.push(Effect::BroadcastUniversalItems {
                    agent_name: channel_name.clone(),
                    channel: Some(channel_name.clone()),
                    thread_parent_id: None,
                    items,
                });
            }
        }
    }

    // Forked lead tool calls: tagged with thread_parent_id so the frontend routes them
    // to the thread panel. The channel is still set so the frontend knows which channel
    // the thread belongs to (used for the thread activity drawer).
    for (fork_name, channel_name) in fork_bound_channels {
        if let Some(fork_events) = events.get(fork_name.as_str()) {
            let items =
                crate::universal_events::claude::extract_tool_events(fork_events, timestamp);
            if !items.is_empty() {
                let thread_parent_id = fork_bound_threads.get(fork_name).cloned();
                effects.push(Effect::BroadcastUniversalItems {
                    agent_name: fork_name.clone(),
                    channel: Some(channel_name.clone()),
                    thread_parent_id,
                    items,
                });
            }
        }
    }

    // Coworkers → shown in their DM channels (dm-<name>).
    for name in coworker_names {
        if let Some(cw_events) = events.get(name.as_str()) {
            let items = crate::universal_events::claude::extract_tool_events(cw_events, timestamp);
            if !items.is_empty() {
                effects.push(Effect::BroadcastUniversalItems {
                    agent_name: name.clone(),
                    channel: Some(format!("dm-{}", name)),
                    thread_parent_id: None,
                    items,
                });
            }
        }
    }

    effects
}

/// Extract insight blocks from text.
///
/// Looks for `★ Insight` blocks delimited by lines of dashes (with optional
/// backtick wrappers). Returns the trimmed content of each insight block.
pub(crate) fn extract_insights(text: &str) -> Vec<String> {
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

#[path = "stream_tests.rs"]
#[cfg(test)]
mod tests;
