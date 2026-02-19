//! Process headless session stream events and generate effects.
//!
//! This module contains pure decision functions that analyze stream events
//! from headless sessions (Lead, channel leads, and coworkers) and produce
//! channel posting and universal event broadcast effects.

use super::effects::Effect;
use crate::headless::StreamEvent;
use std::collections::HashMap;

/// Extract and aggregate text content from headless lead `StreamEvent::Assistant` events.
///
/// Collects all text blocks from all Assistant events in a single drain cycle,
/// returning a single aggregated string. This avoids flooding the channel with
/// per-token or per-event messages during streaming.
pub fn extract_lead_text(events: &[StreamEvent]) -> String {
    let mut aggregated = String::new();
    for event in events {
        if let StreamEvent::Assistant { message, .. } = event
            && let Some(content) = message.get("content")
            && let Some(arr) = content.as_array()
        {
            for block in arr {
                if block.get("type").and_then(|t| t.as_str()) == Some("text")
                    && let Some(text) = block.get("text").and_then(|t| t.as_str())
                {
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
/// - Coworker text is never posted.
///
/// `channel_lead_sessions` maps channel name → session ID for active channel leads.
pub fn process_lead_output(
    events: &HashMap<String, Vec<StreamEvent>>,
    channel_lead_sessions: &HashMap<String, String>,
) -> Vec<Effect> {
    let mut effects = Vec::new();

    // Main lead → posts to main channel.
    if let Some(lead_events) = events.get("lead") {
        let trimmed = extract_lead_text(lead_events).trim().to_string();
        if !trimmed.is_empty() {
            effects.push(Effect::PostToChannel {
                sender: "lead".to_string(),
                message: trimmed,
                channel: None,
            });
        }
    }

    // Channel leads → each posts to its respective topic channel.
    for channel_name in channel_lead_sessions.keys() {
        if let Some(cl_events) = events.get(channel_name.as_str()) {
            let trimmed = extract_lead_text(cl_events).trim().to_string();
            if !trimmed.is_empty() {
                effects.push(Effect::PostToChannel {
                    sender: channel_name.clone(),
                    message: trimmed,
                    channel: Some(channel_name.clone()),
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
/// Coworker tool calls are never shown.
pub fn process_universal_events(
    events: &HashMap<String, Vec<StreamEvent>>,
    channel_lead_sessions: &HashMap<String, String>,
) -> Vec<Effect> {
    let timestamp = chrono::Utc::now();
    let mut effects = Vec::new();

    // Main lead → shown in the main channel (channel = None).
    if let Some(lead_events) = events.get("lead") {
        let items = crate::universal_events::claude::extract_tool_events(lead_events, timestamp);
        if !items.is_empty() {
            effects.push(Effect::BroadcastUniversalItems {
                agent_name: "lead".to_string(),
                channel: None,
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
