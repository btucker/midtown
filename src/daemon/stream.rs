//! Process headless session stream events and generate effects.
//!
//! This module contains pure decision functions that analyze stream events
//! from headless sessions (Lead, channel leads, and coworkers) and produce
//! channel posting and universal event broadcast effects.

use super::effects::Effect;
use crate::headless::StreamEvent;
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
pub fn extract_assistant_text(events: &[StreamEvent]) -> String {
    let mut aggregated = String::new();

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
                aggregated.push_str(&text);
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
                });
            }
        }
    }

    effects
}

/// Process coworker output and generate DM channel posting effects.
///
/// For each coworker session that produced text output, posts the aggregated text
/// to the coworker's DM channel (`dm-<name>`). This mirrors how channel leads
/// stream their output to topic channels.
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
            let trimmed = extract_assistant_text(coworker_events).trim().to_string();
            if !trimmed.is_empty() {
                effects.push(Effect::PostToChannel {
                    sender: name.clone(),
                    message: trimmed,
                    channel: Some(format!("dm-{}", name)),
                });
            }
        }
    }

    effects
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
/// Coworker tool calls are never shown.
pub fn process_universal_events(
    events: &HashMap<String, Vec<StreamEvent>>,
    channel_lead_sessions: &HashMap<String, String>,
    main_lead_session_name: &str,
    fork_bound_channels: &HashMap<String, String>,
    fork_bound_threads: &HashMap<String, String>,
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

    effects
}

#[path = "stream_tests.rs"]
#[cfg(test)]
mod tests;
