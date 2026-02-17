//! Mermaid diagram rendering within chat messages.

use std::collections::HashMap;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::cli::chat::mermaid::{ContentSegment, MermaidCache};

use super::messages::{
    MessageRenderContext, build_continuation_line, build_first_content_line, push_sender_header,
};
use super::text::wrap_content;

/// Render a message that contains mermaid code fences.
///
/// Splits the message content into text and mermaid segments, rendering
/// text normally and inserting selectable placeholders for mermaid diagrams.
/// Each diagram gets a numbered label that the user can select to open
/// in a fullscreen viewer.
#[allow(clippy::too_many_arguments)]
pub fn render_message_with_mermaid(
    msg: &midtown::Message,
    segments: &[ContentSegment],
    width: usize,
    prev_sender: Option<&str>,
    current_tasks: &HashMap<String, String>,
    user_display_name: Option<&str>,
    mermaid_cache: &MermaidCache,
    lines: &mut Vec<Line<'static>>,
    diagram_sources: &mut Vec<String>,
    mermaid_to_render: &mut Vec<String>,
) {
    let ctx = MessageRenderContext::new(msg, prev_sender, user_display_name);

    let content_width = ctx.content_width(width);
    if content_width == 0 {
        return;
    }

    if ctx.show_sender {
        push_sender_header(msg, &ctx, prev_sender, current_tasks, width, lines);
    }

    let mut is_first_content_line = true;

    for segment in segments {
        match segment {
            ContentSegment::Text(text) => {
                let content_lines = wrap_content(text, content_width);
                for content in &content_lines {
                    let parsed = minimad_ratatui::inline(content, ctx.content_style);
                    if is_first_content_line {
                        lines.push(build_first_content_line(msg, &ctx, parsed));
                        is_first_content_line = false;
                    } else {
                        lines.push(build_continuation_line(&ctx, parsed));
                    }
                }
            }
            ContentSegment::Mermaid(source) => {
                render_mermaid_segment(
                    source,
                    &ctx,
                    content_width,
                    mermaid_cache,
                    lines,
                    diagram_sources,
                    mermaid_to_render,
                );
                is_first_content_line = false;
            }
        }
    }
}

/// Render a single mermaid diagram segment (cached, pending, or unqueued).
fn render_mermaid_segment(
    source: &str,
    ctx: &MessageRenderContext,
    content_width: usize,
    mermaid_cache: &MermaidCache,
    lines: &mut Vec<Line<'static>>,
    diagram_sources: &mut Vec<String>,
    mermaid_to_render: &mut Vec<String>,
) {
    let diagram_type = source
        .lines()
        .next()
        .unwrap_or("diagram")
        .split_whitespace()
        .next()
        .unwrap_or("diagram");

    let indent = " ".repeat(ctx.indent_width());

    if let Some(diagram) = mermaid_cache.get_cached(source) {
        // Diagram is ready: show inline ASCII art with separators
        let diagram_num = diagram_sources.len() + 1;
        diagram_sources.push(source.to_string());

        // Top separator
        let top_sep = format!("{}--- {} ---", indent, diagram_type);
        lines.push(Line::from(Span::styled(
            top_sep,
            Style::default().fg(Color::DarkGray),
        )));

        // ASCII art lines (cyan, indented, truncated to content_width)
        for art_line in diagram.ascii_art.lines() {
            let truncated: String = art_line.chars().take(content_width).collect();
            lines.push(Line::from(Span::styled(
                format!("{}{}", indent, truncated),
                Style::default().fg(Color::Cyan),
            )));
        }

        // Bottom separator with browser hint
        let bottom_sep = if diagram_num <= 9 {
            format!("{}--- press {} to open in browser ---", indent, diagram_num)
        } else {
            format!("{}--- {} ---", indent, diagram_type)
        };
        lines.push(Line::from(Span::styled(
            bottom_sep,
            Style::default().fg(Color::DarkGray),
        )));
    } else if mermaid_cache.is_pending(source) {
        // Rendering in progress
        let placeholder = format!("{}[rendering {}...]", indent, diagram_type);
        lines.push(Line::from(Span::styled(
            placeholder,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        // Not yet queued: show placeholder and queue for rendering
        let placeholder = format!("{}[{} diagram]", indent, diagram_type);
        lines.push(Line::from(Span::styled(
            placeholder,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
        mermaid_to_render.push(source.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::chat::mermaid::{self, RenderedDiagram};
    use midtown::{Message, MessageType};

    use super::super::TIMESTAMP_GUTTER_WIDTH;

    /// Helper to create a dummy RenderedDiagram for cache injection
    fn dummy_rendered_diagram() -> RenderedDiagram {
        RenderedDiagram {
            ascii_art: "┌───────┐\n│ Hello │\n└───────┘".to_string(),
            svg: "<svg>test</svg>".to_string(),
        }
    }

    /// Helper to create a test Message
    fn test_message(content: &str) -> Message {
        Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        }
    }

    #[test]
    fn test_cached_diagram_shows_inline_ascii_art() {
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let msg = test_message("ignored");
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            all_text.contains("--- graph ---"),
            "Expected top separator, got: {}",
            all_text
        );
        assert!(
            all_text.contains("Hello"),
            "Expected ASCII art content, got: {}",
            all_text
        );
        assert!(
            all_text.contains("--- press 1 to open in browser ---"),
            "Expected bottom separator with hint, got: {}",
            all_text
        );

        let bottom_line = lines.last().unwrap();
        assert_eq!(bottom_line.spans[0].style.fg, Some(Color::DarkGray));

        let ascii_line = lines
            .iter()
            .find(|l| l.spans.iter().any(|s| s.content.contains("Hello")));
        assert!(ascii_line.is_some(), "Should have an ASCII art line");
        assert_eq!(ascii_line.unwrap().spans[0].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn test_pending_diagram_shows_rendering_placeholder() {
        let source = "sequenceDiagram\n  A->>B: hello";
        let mut cache = MermaidCache::new();
        cache.insert_pending(source);

        let msg = test_message("ignored");
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let placeholder_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            placeholder_text.contains("[rendering sequenceDiagram...]"),
            "Expected rendering placeholder, got: {}",
            placeholder_text
        );

        let placeholder_line = lines.last().unwrap();
        assert_eq!(placeholder_line.spans[0].style.fg, Some(Color::DarkGray));
        assert!(
            placeholder_line.spans[0]
                .style
                .add_modifier
                .contains(Modifier::ITALIC)
        );

        assert!(diagram_sources.is_empty());
    }

    #[test]
    fn test_unqueued_diagram_shows_plain_placeholder_and_queues() {
        let source = "flowchart LR\n  A-->B";
        let cache = MermaidCache::new();

        let msg = test_message("ignored");
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let placeholder_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            placeholder_text.contains("[flowchart diagram]"),
            "Expected plain placeholder, got: {}",
            placeholder_text
        );

        assert_eq!(mermaid_to_render.len(), 1);
        assert_eq!(mermaid_to_render[0], source);

        assert!(diagram_sources.is_empty());
    }

    #[test]
    fn test_diagram_numbering_sequential() {
        let sources: Vec<String> = (0..3)
            .map(|i| format!("graph TD\n  A{}-->B{}", i, i))
            .collect();
        let mut cache = MermaidCache::new();
        for s in &sources {
            cache.insert_cached(s, dummy_rendered_diagram());
        }

        let msg = test_message("ignored");
        let segments: Vec<ContentSegment> = sources
            .iter()
            .map(|s| ContentSegment::Mermaid(s.clone()))
            .collect();
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        assert_eq!(diagram_sources.len(), 3);

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(all_text.contains("--- press 1 to open in browser ---"));
        assert!(all_text.contains("--- press 2 to open in browser ---"));
        assert!(all_text.contains("--- press 3 to open in browser ---"));
    }

    #[test]
    fn test_diagram_cap_at_9_shortcuts() {
        let sources: Vec<String> = (0..11)
            .map(|i| format!("graph TD\n  X{}-->Y{}", i, i))
            .collect();
        let mut cache = MermaidCache::new();
        for s in &sources {
            cache.insert_cached(s, dummy_rendered_diagram());
        }

        let msg = test_message("ignored");
        let segments: Vec<ContentSegment> = sources
            .iter()
            .map(|s| ContentSegment::Mermaid(s.clone()))
            .collect();
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        assert_eq!(diagram_sources.len(), 11);

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        for i in 1..=9 {
            assert!(
                all_text.contains(&format!("--- press {} to open in browser ---", i)),
                "Diagram {} should have a numbered browser hint",
                i
            );
        }

        let numbered_count = all_text.matches("to open in browser ---").count();
        assert_eq!(
            numbered_count, 9,
            "Only 9 diagrams should have browser hints"
        );
    }

    #[test]
    fn test_mixed_text_and_mermaid_segments() {
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let msg = test_message("ignored");
        let segments = vec![
            ContentSegment::Text("Before the diagram".to_string()),
            ContentSegment::Mermaid(source.to_string()),
            ContentSegment::Text("After the diagram".to_string()),
        ];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(all_text.contains("Before the diagram"));
        assert!(all_text.contains("--- graph ---"));
        assert!(all_text.contains("--- press 1 to open in browser ---"));
        assert!(all_text.contains("After the diagram"));
    }

    #[test]
    fn test_diagram_type_extracted_from_first_line() {
        let test_cases = vec![
            ("sequenceDiagram\n  A->>B: hello", "sequenceDiagram"),
            ("classDiagram\n  class Animal", "classDiagram"),
            ("flowchart LR\n  A-->B", "flowchart"),
            ("pie title Pets\n  \"Dogs\": 60", "pie"),
        ];

        for (source, expected_type) in test_cases {
            let mut cache = MermaidCache::new();
            cache.insert_cached(source, dummy_rendered_diagram());

            let msg = test_message("ignored");
            let segments = vec![ContentSegment::Mermaid(source.to_string())];
            let current_tasks = HashMap::new();
            let mut lines = Vec::new();
            let mut diagram_sources = Vec::new();
            let mut mermaid_to_render = Vec::new();

            render_message_with_mermaid(
                &msg,
                &segments,
                80,
                None,
                &current_tasks,
                None,
                &cache,
                &mut lines,
                &mut diagram_sources,
                &mut mermaid_to_render,
            );

            let all_text: String = lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("");
            assert!(
                all_text.contains(&format!("--- {} ---", expected_type)),
                "Expected diagram type '{}' in separator, got: {}",
                expected_type,
                all_text
            );
        }
    }

    #[test]
    fn test_action_message_mermaid_placeholder_extra_indent() {
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "ignored".to_string(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Action,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();
        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        let action_indent = " ".repeat(TIMESTAMP_GUTTER_WIDTH + 2);

        let all_text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            all_text.contains(&format!("{}--- graph ---", action_indent)),
            "Action message top separator should have {} chars indent, got:\n{}",
            TIMESTAMP_GUTTER_WIDTH + 2,
            all_text
        );
        assert!(
            all_text.contains(&format!(
                "{}--- press 1 to open in browser ---",
                action_indent
            )),
            "Action message bottom separator should have {} chars indent",
            TIMESTAMP_GUTTER_WIDTH + 2,
        );

        for line in &lines {
            for span in &line.spans {
                if span.style.fg == Some(Color::Cyan) {
                    let text = span.content.as_ref();
                    assert!(
                        text.starts_with(&action_indent),
                        "ASCII art line should have {} chars indent, got: {:?}",
                        TIMESTAMP_GUTTER_WIDTH + 2,
                        text
                    );
                }
            }
        }

        // Compare with normal text message indent
        let normal_msg = test_message("ignored");
        let mut normal_lines = Vec::new();
        let mut normal_diagram_sources = Vec::new();
        let mut normal_mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &normal_msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut normal_lines,
            &mut normal_diagram_sources,
            &mut normal_mermaid_to_render,
        );

        let normal_indent = " ".repeat(TIMESTAMP_GUTTER_WIDTH);
        let normal_text: String = normal_lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            normal_text.contains(&format!("{}--- graph ---", normal_indent)),
            "Normal message top separator should have {} chars indent",
            TIMESTAMP_GUTTER_WIDTH,
        );
        for line in &normal_lines {
            for span in &line.spans {
                if span.style.fg == Some(Color::Cyan) {
                    let text = span.content.as_ref();
                    assert!(
                        text.starts_with(&normal_indent),
                        "Normal ASCII art line should have {} chars indent, got: {:?}",
                        TIMESTAMP_GUTTER_WIDTH,
                        text
                    );
                    assert!(
                        !text.starts_with(&action_indent),
                        "Normal ASCII art line should NOT have action indent, got: {:?}",
                        text
                    );
                }
            }
        }
    }

    #[test]
    fn test_narrow_terminal_does_not_panic_on_unicode_ascii_art() {
        let source = "graph TD\n  A-->B";
        let mut cache = MermaidCache::new();
        cache.insert_cached(
            source,
            mermaid::RenderedDiagram {
                ascii_art: "┌──────────────────┐\n│ A long box label │\n└──────────────────┘"
                    .to_string(),
                svg: "<svg>test</svg>".to_string(),
            },
        );

        let msg = test_message("ignored");
        let segments = vec![ContentSegment::Mermaid(source.to_string())];
        let current_tasks = HashMap::new();

        for width in [15, 18, 20, 25] {
            let mut lines = Vec::new();
            let mut diagram_sources = Vec::new();
            let mut mermaid_to_render = Vec::new();

            render_message_with_mermaid(
                &msg,
                &segments,
                width,
                None,
                &current_tasks,
                None,
                &cache,
                &mut lines,
                &mut diagram_sources,
                &mut mermaid_to_render,
            );

            assert!(!lines.is_empty(), "Should produce lines at width {}", width);
        }
    }

    #[test]
    fn test_render_crosspost_with_mermaid() {
        let source = "graph TD\n  A-->B";
        let mut msg = test_message("ignored");
        msg.source_channel = Some("design".to_string());

        let segments = vec![
            ContentSegment::Text("Architecture insight: ".to_string()),
            ContentSegment::Mermaid(source.to_string()),
        ];

        let mut cache = MermaidCache::new();
        cache.insert_cached(source, dummy_rendered_diagram());

        let current_tasks = HashMap::new();

        let mut lines = Vec::new();
        let mut diagram_sources = Vec::new();
        let mut mermaid_to_render = Vec::new();

        render_message_with_mermaid(
            &msg,
            &segments,
            80,
            None,
            &current_tasks,
            None,
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
        );

        assert!(
            !lines.is_empty(),
            "Expected at least some lines for cross-posted mermaid"
        );

        let has_star = lines
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.contains('★')));
        assert!(
            has_star,
            "Expected to find ★ prefix in cross-posted mermaid message"
        );
    }
}
