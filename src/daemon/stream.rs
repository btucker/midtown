//! Process headless session stream events and generate effects.
//!
//! This module contains pure decision functions that analyze stream events
//! from headless sessions (Lead and coworkers) and produce channel posting
//! effects.

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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── extract_lead_text tests ─────────────────────────────────────────

    #[test]
    fn test_extract_lead_text_single_text_block() {
        let events = vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "text", "text": "Hello world"}]
            }),
            session_id: None,
            extra: json!(null),
        }];
        assert_eq!(extract_lead_text(&events), "Hello world");
    }

    #[test]
    fn test_extract_lead_text_aggregates_multiple_events() {
        let events = vec![
            StreamEvent::Assistant {
                message: json!({
                    "content": [{"type": "text", "text": "Hello "}]
                }),
                session_id: None,
                extra: json!(null),
            },
            StreamEvent::Assistant {
                message: json!({
                    "content": [{"type": "text", "text": "world"}]
                }),
                session_id: None,
                extra: json!(null),
            },
        ];
        assert_eq!(extract_lead_text(&events), "Hello world");
    }

    #[test]
    fn test_extract_lead_text_skips_non_text_blocks() {
        let events = vec![StreamEvent::Assistant {
            message: json!({
                "content": [
                    {"type": "tool_use", "id": "123", "name": "Read"},
                    {"type": "text", "text": "Reading file..."}
                ]
            }),
            session_id: None,
            extra: json!(null),
        }];
        assert_eq!(extract_lead_text(&events), "Reading file...");
    }

    #[test]
    fn test_extract_lead_text_empty_content_array() {
        let events = vec![StreamEvent::Assistant {
            message: json!({"content": []}),
            session_id: None,
            extra: json!(null),
        }];
        assert_eq!(extract_lead_text(&events), "");
    }

    #[test]
    fn test_extract_lead_text_no_text_blocks() {
        let events = vec![StreamEvent::Assistant {
            message: json!({
                "content": [{"type": "tool_use", "id": "123", "name": "Read"}]
            }),
            session_id: None,
            extra: json!(null),
        }];
        assert_eq!(extract_lead_text(&events), "");
    }

    #[test]
    fn test_extract_lead_text_non_assistant_events() {
        let events = vec![
            StreamEvent::System {
                subtype: "init".to_string(),
                session_id: Some("abc-123".to_string()),
                model: Some("sonnet".to_string()),
                extra: json!({}),
            },
            StreamEvent::User {
                message: json!({"content": "user input"}),
                extra: json!({}),
            },
        ];
        assert_eq!(extract_lead_text(&events), "");
    }

    // ── process_lead_output tests ───────────────────────────────────────

    #[test]
    fn test_process_lead_output_no_events() {
        let events = HashMap::new();
        let effects = process_lead_output(&events);
        assert!(effects.is_empty());
    }

    #[test]
    fn test_process_lead_output_no_lead_events() {
        let mut events = HashMap::new();
        events.insert("coworker".to_string(), vec![]);
        let effects = process_lead_output(&events);
        assert!(effects.is_empty());
    }

    #[test]
    fn test_process_lead_output_returns_post_effect() {
        let mut events = HashMap::new();
        events.insert(
            "lead".to_string(),
            vec![StreamEvent::Assistant {
                message: json!({
                    "content": [{"type": "text", "text": "Hello from lead"}]
                }),
                session_id: None,
                extra: json!(null),
            }],
        );

        let effects = process_lead_output(&events);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            Effect::PostToChannel {
                sender,
                message,
                channel,
            } => {
                assert_eq!(sender, "lead");
                assert_eq!(message, "Hello from lead");
                assert!(channel.is_none());
            }
            _ => panic!("Expected PostToChannel effect"),
        }
    }

    #[test]
    fn test_process_lead_output_aggregates_multiple_events() {
        let mut events = HashMap::new();
        events.insert(
            "lead".to_string(),
            vec![
                StreamEvent::Assistant {
                    message: json!({
                        "content": [{"type": "text", "text": "First "}]
                    }),
                    session_id: None,
                    extra: json!(null),
                },
                StreamEvent::Assistant {
                    message: json!({
                        "content": [{"type": "text", "text": "Second"}]
                    }),
                    session_id: None,
                    extra: json!(null),
                },
            ],
        );

        let effects = process_lead_output(&events);
        assert_eq!(effects.len(), 1);

        match &effects[0] {
            Effect::PostToChannel { message, .. } => {
                assert_eq!(message, "First Second");
            }
            _ => panic!("Expected PostToChannel effect"),
        }
    }

    #[test]
    fn test_process_lead_output_empty_text_not_posted() {
        let mut events = HashMap::new();
        events.insert(
            "lead".to_string(),
            vec![StreamEvent::Assistant {
                message: json!({
                    "content": [{"type": "tool_use", "name": "Read", "input": {}}]
                }),
                session_id: None,
                extra: json!(null),
            }],
        );

        let effects = process_lead_output(&events);
        assert!(
            effects.is_empty(),
            "Should not post if no text content found"
        );
    }
}
