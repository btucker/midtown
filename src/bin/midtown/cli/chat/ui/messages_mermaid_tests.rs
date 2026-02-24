use std::collections::HashMap;

use ratatui::style::{Color, Modifier};

use super::*;
use crate::cli::chat::mermaid::{self, ContentSegment, MermaidCache, RenderedDiagram};
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
        session_id: None,
        thread_parent_id: None,
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
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
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
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
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
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
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
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
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
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
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
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
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
            &[],
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
            false,
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
        session_id: None,
        thread_parent_id: None,
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
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
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
        &[],
        &cache,
        &mut normal_lines,
        &mut normal_diagram_sources,
        &mut normal_mermaid_to_render,
        false,
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
            &[],
            &cache,
            &mut lines,
            &mut diagram_sources,
            &mut mermaid_to_render,
            false,
        );

        assert!(!lines.is_empty(), "Should produce lines at width {}", width);
    }
}

/// When a message's first (and only) segment is a CodeBlock, the top border line
/// must carry the timestamp gutter — i.e., it must be built with
/// `build_first_content_line` rather than `build_continuation_line`.
#[test]
fn test_code_block_first_segment_has_timestamp_gutter() {
    let msg = test_message("ignored");
    let segments = vec![ContentSegment::CodeBlock {
        language: "rust".to_string(),
        source: "fn main() {}".to_string(),
    }];

    let cache = MermaidCache::new();
    let current_tasks = HashMap::new();
    let mut lines = Vec::new();
    let mut diagram_sources = Vec::new();
    let mut mermaid_to_render = Vec::new();

    // Pass the same sender as prev_sender so show_sender=false and the first
    // rendered line is the code block top border (not a sender header).
    render_message_with_mermaid(
        &msg,
        &segments,
        80,
        Some("park"),
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
    );

    assert!(!lines.is_empty(), "Should produce at least one line");

    // The first rendered line must be the top border with timestamp gutter.
    // build_first_content_line prepends a timestamp span; build_continuation_line
    // prepends only spaces. A timestamp span will have non-empty content that is
    // NOT just whitespace (it contains "HH:MM" digits).
    let first_line = &lines[0];
    let first_span_text = first_line
        .spans
        .first()
        .map(|s| s.content.as_ref())
        .unwrap_or("");

    // The gutter span produced by build_first_content_line contains time digits,
    // while build_continuation_line produces only spaces.
    assert!(
        first_span_text.chars().any(|c| c.is_ascii_digit()),
        "First line of code-block-first message should contain timestamp digits in gutter, got: {:?}",
        first_span_text
    );
}

/// Code lines longer than content_width must be truncated to prevent overflow
/// on narrow terminals.
#[test]
fn test_code_block_long_line_truncated_to_content_width() {
    let long_line = "x".repeat(200);
    let msg = test_message("ignored");
    let segments = vec![ContentSegment::CodeBlock {
        language: "".to_string(),
        source: long_line.clone(),
    }];

    let content_width = 40_usize;
    let cache = MermaidCache::new();
    let current_tasks = HashMap::new();
    let mut lines = Vec::new();
    let mut diagram_sources = Vec::new();
    let mut mermaid_to_render = Vec::new();

    render_message_with_mermaid(
        &msg,
        &segments,
        // total width; content_width accounts for the indent
        content_width + TIMESTAMP_GUTTER_WIDTH,
        None,
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
    );

    // Find the line that contains the code content (not borders)
    let code_line = lines.iter().find(|l| {
        let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        !text.contains("---") && text.contains('x')
    });

    assert!(code_line.is_some(), "Should have a code content line");

    let code_line_text: String = code_line
        .unwrap()
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    // The content portion (excluding indent) must not exceed content_width chars
    let indent_len = TIMESTAMP_GUTTER_WIDTH; // Text message indent
    let content_chars = code_line_text
        .chars()
        .skip(indent_len)
        .filter(|c| *c == 'x')
        .count();
    assert!(
        content_chars <= content_width,
        "Code line should be truncated to content_width={}, got {} 'x' chars",
        content_width,
        content_chars
    );
}

/// Code block with a language should show just the bare name (e.g. "rust"),
/// not the old "--- rust ---" format.
#[test]
fn test_code_block_lang_label_is_bare_name() {
    let msg = test_message("ignored");
    let segments = vec![ContentSegment::CodeBlock {
        language: "rust".to_string(),
        source: "fn main() {}".to_string(),
    }];

    let cache = MermaidCache::new();
    let current_tasks = HashMap::new();
    let mut lines = Vec::new();
    let mut diagram_sources = Vec::new();
    let mut mermaid_to_render = Vec::new();

    render_message_with_mermaid(
        &msg,
        &segments,
        80,
        Some("park"),
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
    );

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !all_text.contains("--- rust ---"),
        "Should not show '--- rust ---' border, got: {}",
        all_text
    );
    assert!(
        all_text.contains("rust"),
        "Should show bare language name 'rust', got: {}",
        all_text
    );
}

/// Code block should have no "--- end ---" bottom border.
#[test]
fn test_code_block_no_end_border() {
    let msg = test_message("ignored");
    let segments = vec![ContentSegment::CodeBlock {
        language: "typescript".to_string(),
        source: "const x = 1;".to_string(),
    }];

    let cache = MermaidCache::new();
    let current_tasks = HashMap::new();
    let mut lines = Vec::new();
    let mut diagram_sources = Vec::new();
    let mut mermaid_to_render = Vec::new();

    render_message_with_mermaid(
        &msg,
        &segments,
        80,
        Some("park"),
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
    );

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !all_text.contains("--- end ---"),
        "Should not show '--- end ---' border, got: {}",
        all_text
    );
}

/// Code block with empty language should omit the label line entirely.
#[test]
fn test_code_block_empty_lang_no_label() {
    let msg = test_message("ignored");
    let segments = vec![ContentSegment::CodeBlock {
        language: "".to_string(),
        source: "some code".to_string(),
    }];

    let cache = MermaidCache::new();
    let current_tasks = HashMap::new();
    let mut lines = Vec::new();
    let mut diagram_sources = Vec::new();
    let mut mermaid_to_render = Vec::new();

    render_message_with_mermaid(
        &msg,
        &segments,
        80,
        Some("park"),
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
    );

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !all_text.contains("--- code ---"),
        "Should not show '--- code ---' fallback label, got: {}",
        all_text
    );
    // Only code content line(s) should be present, no lang label
    assert!(
        all_text.contains("some code") || lines.len() == 1,
        "Should have code content but no label, got: {}",
        all_text
    );
}

/// Language label line should be styled DarkGray with no leading indent.
#[test]
fn test_code_block_lang_label_style_and_no_indent() {
    let msg = test_message("ignored");
    let segments = vec![ContentSegment::CodeBlock {
        language: "python".to_string(),
        source: "x = 1".to_string(),
    }];

    let cache = MermaidCache::new();
    let current_tasks = HashMap::new();
    let mut lines = Vec::new();
    let mut diagram_sources = Vec::new();
    let mut mermaid_to_render = Vec::new();

    render_message_with_mermaid(
        &msg,
        &segments,
        80,
        Some("park"),
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
    );

    // Find the span containing just "python"
    let lang_span = lines.iter().flat_map(|l| l.spans.iter()).find(|s| {
        let text = s.content.as_ref();
        text == "python" || text.trim() == "python"
    });

    assert!(
        lang_span.is_some(),
        "Should have a span with 'python' label"
    );
    let span = lang_span.unwrap();
    assert_eq!(
        span.style.fg,
        Some(Color::DarkGray),
        "Language label should be DarkGray"
    );
    // The label itself should not be indented (no leading spaces in the span)
    assert!(
        !span.content.starts_with(' '),
        "Language label span should not start with spaces, got: {:?}",
        span.content
    );
}

/// When a code block with no language is the first content segment, the first
/// code line must not be double-indented. The timestamp gutter IS the indent —
/// the code should start at the same column as a language label would.
#[test]
fn test_code_block_empty_lang_first_line_not_double_indented() {
    let msg = test_message("ignored");
    let segments = vec![ContentSegment::CodeBlock {
        language: "".to_string(),
        source: "let x = 1;".to_string(),
    }];

    let cache = MermaidCache::new();
    let current_tasks = HashMap::new();
    let mut lines = Vec::new();
    let mut diagram_sources = Vec::new();
    let mut mermaid_to_render = Vec::new();

    // Same sender suppresses the sender header so first line is the code line.
    render_message_with_mermaid(
        &msg,
        &segments,
        80,
        Some("park"),
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
        false,
    );

    assert!(!lines.is_empty(), "Should produce at least one line");

    // The first line is the code line carrying the timestamp gutter.
    // Its first span must be the timestamp (has digits) — not a blank indent span.
    let first_line = &lines[0];
    let first_span_text = first_line
        .spans
        .first()
        .map(|s| s.content.as_ref())
        .unwrap_or("");

    assert!(
        first_span_text.chars().any(|c| c.is_ascii_digit()),
        "First span of first line should be the timestamp gutter (contains digits), got: {:?}",
        first_span_text
    );

    // The second span must NOT start with spaces — it should be the code content
    // directly, not another indent block.
    if let Some(second_span) = first_line.spans.get(1) {
        let text = second_span.content.as_ref();
        assert!(
            !text.starts_with("       "),
            "Second span should be code content, not a blank indent (double-indent bug), got: {:?}",
            text
        );
    }
}

// ── render_header_content_segments ───────────────────────────────────────────

/// A code block with empty language in a thread header should produce no label line —
/// matching render_code_block_segment which omits the label when language is empty.
#[test]
fn test_header_code_block_empty_language_has_no_label() {
    use ratatui::style::Style;

    let segments = vec![ContentSegment::CodeBlock {
        language: "".to_string(),
        source: "x = 1".to_string(),
    }];
    let lines = render_header_content_segments(&segments, 40, Style::default(), false);

    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join("");
    // No label span for empty language — only code content
    assert!(
        !text.contains("code"),
        "Empty language should produce no label line, got: {text}"
    );
    assert!(
        text.contains("x = 1") || !lines.is_empty(),
        "Should still produce code content lines, got: {text}"
    );
}

/// Code lines in a thread header that exceed content_width must be truncated.
/// This covers the span-truncation break path in render_header_content_segments.
#[test]
fn test_header_code_block_long_line_truncated_to_content_width() {
    use ratatui::style::Style;

    let content_width = 10;
    // Source line is much wider than content_width
    let source = "x".repeat(50);
    let segments = vec![ContentSegment::CodeBlock {
        language: "rust".to_string(),
        source: source.clone(),
    }];
    let lines = render_header_content_segments(&segments, content_width, Style::default(), false);

    // Each rendered code line must not exceed content_width chars
    for line in &lines {
        let total_chars: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
        assert!(
            total_chars <= content_width,
            "Header code line should be truncated to content_width={}, got {}",
            content_width,
            total_chars
        );
    }
}

/// A mermaid segment in a thread header should render as a "[diagram]" placeholder
/// (async diagram rendering is not available in the pre-computation context).
#[test]
fn test_header_mermaid_segment_shows_placeholder() {
    use ratatui::style::Style;

    let segments = vec![ContentSegment::Mermaid("graph TD\n  A-->B".to_string())];
    let lines = render_header_content_segments(&segments, 40, Style::default(), false);

    let text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("[diagram]"),
        "Mermaid segment in header should show '[diagram]' placeholder, got: {text}"
    );
}
