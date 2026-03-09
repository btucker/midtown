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
/// into a single message to avoid channel flooding. Also extracts structured
/// tool data (`ToolBlock`s) from tool_use/tool_result pairs and attaches them
/// to the posted channel messages for client-side rendering.
///
/// - The main lead's text and tool data are posted to the main channel (`channel: None`).
/// - Each channel lead's text and tool data are posted to its respective topic channel.
/// - Fork sessions' text and tool data are posted to their bound topic channels.
/// - Coworker text is handled separately by [`process_agent_output()`].
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
                tool_use_id: None,
                parent_tool_use_id: None,
            });
        }
        append_tool_data_effects(
            &mut effects,
            lead_events,
            main_lead_session_name.to_string(),
            None,
        );
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
                    tool_use_id: None,
                    parent_tool_use_id: None,
                });
            }
            append_tool_data_effects(
                &mut effects,
                cl_events,
                channel_name.clone(),
                Some(channel_name.clone()),
            );
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
                    tool_use_id: None,
                    parent_tool_use_id: None,
                });
            }
            append_tool_data_effects(
                &mut effects,
                fork_events,
                fork_name.clone(),
                Some(channel_name.clone()),
            );
        }
    }

    effects
}

/// Create PostToChannel effects carrying `tool_data` for topic channel messages.
///
/// Extracts tool blocks from the session's events and posts them as a separate
/// message with a `[ToolName, ...]` summary for TUI visibility. Thread routing
/// for fork sessions is handled by the effect executor via `fork_bound_threads`.
fn append_tool_data_effects(
    effects: &mut Vec<Effect>,
    session_events: &[StreamEvent],
    sender: String,
    channel: Option<String>,
) {
    let blocks = extract_tool_blocks(session_events);
    if blocks.is_empty() {
        return;
    }
    let tool_summary = format!(
        "[{}]",
        blocks
            .iter()
            .map(|b| b.tool_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    effects.push(Effect::PostToChannel {
        sender,
        message: tool_summary,
        channel,
        auto_output: true,
        message_type: None,
        nudge_type: None,
        tool_data: Some(blocks),
        provider: None,
        tool_use_id: None,
        parent_tool_use_id: None,
    });
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

/// Extract `parent_tool_use_id` from a stream event's `extra` field.
///
/// Claude Code's `--output-format stream-json` emits this on every
/// assistant/user event that originates inside a sub-agent (Agent/Task/Skill),
/// linking it back to the parent tool_use block's `id`.
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
fn extract_tool_blocks(events: &[StreamEvent]) -> Vec<ToolBlock> {
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
    // Progress events with data.type == "agent_progress" and data.message.type == "user"
    // carry tool_result blocks from sub-agent execution.
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
    // Progress events with data.type == "agent_progress" and data.message.type == "assistant"
    // carry tool_use blocks from sub-agent execution, with parentToolUseID on the event.
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

/// Extract assistant text, split by parentToolUseID.
///
/// Returns `(top_level_text, sub_agent_texts)` where `sub_agent_texts` is
/// a map of `parent_tool_use_id → aggregated text` for sub-agent prose.
fn extract_assistant_text_split(events: &[StreamEvent]) -> (String, HashMap<String, String>) {
    let mut top_level = String::new();
    let mut sub_agent: HashMap<String, String> = HashMap::new();

    for event in events {
        match event {
            StreamEvent::Assistant { message, extra, .. } => {
                let text = extract_text_blocks(message);
                if text.is_empty() {
                    continue;
                }
                if let Some(parent_id) = get_parent_tool_use_id(extra) {
                    sub_agent.entry(parent_id).or_default().push_str(&text);
                } else {
                    top_level.push_str(&text);
                }
            }
            // Sub-agent text from progress events (agent_progress with assistant text).
            StreamEvent::Progress {
                data,
                parent_tool_use_id: Some(parent_id),
                ..
            } if data.get("type").and_then(|t| t.as_str()) == Some("agent_progress")
                && data
                    .get("message")
                    .and_then(|m| m.get("type"))
                    .and_then(|t| t.as_str())
                    == Some("assistant") =>
            {
                if let Some(inner_msg) = data.get("message").and_then(|m| m.get("message")) {
                    let text = extract_text_blocks(inner_msg);
                    if !text.is_empty() {
                        sub_agent
                            .entry(parent_id.clone())
                            .or_default()
                            .push_str(&text);
                    }
                }
            }
            _ => {}
        }
    }

    (top_level, sub_agent)
}

/// Strip tool-call label lines from text destined for DM channels.
///
/// Claude Code emits `[ToolName]` text blocks alongside `tool_use` blocks in
/// assistant messages. In DM channels, the tool calls are rendered separately
/// via `extract_tool_blocks()`, so these labels are noise. This function removes
/// lines that consist solely of a bracketed PascalCase/camelCase word (e.g.,
/// `[Bash]`, `[ToolSearch]`) while preserving all other text.
fn strip_tool_labels(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            // Keep non-empty lines that aren't bare tool labels like [Bash], [ToolSearch]
            if trimmed.is_empty() {
                return false;
            }
            !(trimmed.starts_with('[')
                && trimmed.ends_with(']')
                && trimmed.len() > 2
                && trimmed[1..trimmed.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Process agent output and generate DM channel posting effects.
///
/// DM channels are a complete stream of the agent — text AND tool calls.
/// All content uses `auto_output: false` because DM channels echo everything
/// (there's no distinction between "auto" and "explicit" output).
///
/// For each agent session, posts:
/// - Top-level text output (assistant prose) with provider metadata
/// - Extracted `★ Insight` blocks as `PostInsight` effects (routed to the task's channel)
/// - Top-level tool calls as `tool_data`, tagged with `tool_use_id` for thread parent lookup
/// - Sub-agent text and tool calls with `parent_tool_use_id` for thread reply resolution
///
/// Effects are ordered: top-level (thread parents) first, sub-agent (thread children) second,
/// so the effect executor can resolve thread parents before children reference them.
///
/// `agent_names` is the set of session names whose output should be posted
/// to DM channels.
pub fn process_agent_output(
    events: &HashMap<String, Vec<StreamEvent>>,
    agent_names: &HashSet<String>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for name in agent_names {
        if let Some(coworker_events) = events.get(name.as_str()) {
            let dm_channel = format!("dm-{}", name);
            let provider = detect_provider(coworker_events);

            // Split text by parentToolUseID.
            let (top_text, sub_agent_texts) = extract_assistant_text_split(coworker_events);

            // Post top-level text output (assistant prose).
            // Strip tool-call labels (e.g. "[Bash]") that duplicate rendered tool blocks.
            let trimmed = strip_tool_labels(&top_text).trim().to_string();
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
                    tool_use_id: None,
                    parent_tool_use_id: None,
                });
            }

            // Split tool blocks into top-level vs sub-agent.
            let all_blocks = extract_tool_blocks(coworker_events);
            if !all_blocks.is_empty() {
                let sub_count = all_blocks
                    .iter()
                    .filter(|b| b.parent_tool_use_id.is_some())
                    .count();
                if sub_count > 0 {
                    tracing::debug!(
                        coworker = %name,
                        total_blocks = all_blocks.len(),
                        sub_agent_blocks = sub_count,
                        "Splitting tool blocks into top-level vs sub-agent"
                    );
                }
            }
            let mut top_level_blocks = Vec::new();
            let mut sub_agent_blocks: HashMap<String, Vec<ToolBlock>> = HashMap::new();

            for block in all_blocks {
                if let Some(ref parent_id) = block.parent_tool_use_id {
                    sub_agent_blocks
                        .entry(parent_id.clone())
                        .or_default()
                        .push(block);
                } else {
                    top_level_blocks.push(block);
                }
            }

            // Post top-level tool blocks, tagged with tool_use_id from the first block.
            // Include a tool name summary (e.g. "[Bash, Read]") for TUI visibility,
            // since the terminal renderer only displays msg.content, not tool_data.
            if !top_level_blocks.is_empty() {
                let tool_use_id = top_level_blocks.first().and_then(|b| b.call_id.clone());
                let tool_summary = format!(
                    "[{}]",
                    top_level_blocks
                        .iter()
                        .map(|b| b.tool_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                effects.push(Effect::PostToChannel {
                    sender: name.clone(),
                    message: tool_summary,
                    channel: Some(dm_channel.clone()),
                    auto_output: false,
                    message_type: None,
                    nudge_type: None,
                    tool_data: Some(top_level_blocks),
                    provider: provider.clone(),
                    tool_use_id,
                    parent_tool_use_id: None,
                });
            }

            // Post sub-agent text as thread replies (grouped by parent_tool_use_id).
            for (parent_id, text) in &sub_agent_texts {
                let trimmed = strip_tool_labels(text).trim().to_string();
                if !trimmed.is_empty() {
                    effects.push(Effect::PostToChannel {
                        sender: name.clone(),
                        message: trimmed,
                        channel: Some(dm_channel.clone()),
                        auto_output: false,
                        message_type: None,
                        nudge_type: None,
                        tool_data: None,
                        provider: provider.clone(),
                        tool_use_id: None,
                        parent_tool_use_id: Some(parent_id.clone()),
                    });
                }
            }

            // Post sub-agent tool blocks as thread replies (grouped by parent_tool_use_id).
            for (parent_id, blocks) in sub_agent_blocks {
                let tool_summary = format!(
                    "[{}]",
                    blocks
                        .iter()
                        .map(|b| b.tool_name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                effects.push(Effect::PostToChannel {
                    sender: name.clone(),
                    message: tool_summary,
                    channel: Some(dm_channel.clone()),
                    auto_output: false,
                    message_type: None,
                    nudge_type: None,
                    tool_data: Some(blocks),
                    provider: provider.clone(),
                    tool_use_id: None,
                    parent_tool_use_id: Some(parent_id),
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
    agent_names: &HashSet<String>,
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
    for name in agent_names {
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
