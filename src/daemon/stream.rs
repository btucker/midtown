//! Process headless session stream events and generate effects.
//!
//! This module contains pure decision functions that analyze stream events
//! from headless sessions (Lead and coworkers) and produce channel posting
//! and universal event broadcast effects.

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

/// Process headless Lead output and generate channel posting effects.
///
/// Aggregates all text content from Assistant events in the current drain cycle
/// into a single message to avoid channel flooding.
///
/// Returns an effect to post the aggregated text to the main channel if any
/// Assistant text was found.
pub fn process_lead_output(events: &HashMap<String, Vec<StreamEvent>>) -> Vec<Effect> {
    let mut effects = Vec::new();

    if let Some(lead_events) = events.get("lead") {
        let aggregated = extract_lead_text(lead_events);
        if !aggregated.is_empty() {
            effects.push(Effect::PostToChannel {
                sender: "lead".to_string(),
                message: aggregated,
                channel: None, // Posts to main channel
            });
        }
    }

    effects
}

/// Process all agent stream events and generate universal event broadcast effects.
///
/// Processes the lead and all coworker agents. Extracts tool calls and results
/// using the Claude converter and returns broadcast effects for any agent that
/// produced tool events.
pub fn process_universal_events(events: &HashMap<String, Vec<StreamEvent>>) -> Vec<Effect> {
    let timestamp = chrono::Utc::now();
    let mut effects = Vec::new();
    for (agent_name, agent_events) in events {
        let items = crate::universal_events::claude::extract_tool_events(agent_events, timestamp);
        if !items.is_empty() {
            effects.push(Effect::BroadcastUniversalItems {
                agent_name: agent_name.clone(),
                items,
            });
        }
    }
    effects
}

#[path = "stream_tests.rs"]
#[cfg(test)]
mod tests;
