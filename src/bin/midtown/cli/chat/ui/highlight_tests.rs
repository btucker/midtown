use ratatui::style::Color;

use super::highlight_code;

#[test]
fn test_highlight_rust_code() {
    let source = "fn main() {\n    let x = 42;\n    println!(\"{}\", x);\n}";
    let lines = highlight_code("rust", source);

    assert!(!lines.is_empty(), "Should produce at least one line");

    // Rust highlighting should produce spans with varied colors (not all the same)
    let all_spans: Vec<_> = lines.iter().flat_map(|l| l.spans.iter()).collect();
    assert!(!all_spans.is_empty(), "Should produce at least one span");

    // At least some spans should have color styling (not all default)
    let colored_spans = all_spans
        .iter()
        .filter(|s| s.style.fg.is_some() && s.style.fg != Some(Color::Reset))
        .count();
    assert!(
        colored_spans > 0,
        "Rust highlighting should produce colored spans"
    );

    // Colors should vary (not all the same)
    let unique_colors: std::collections::HashSet<Option<Color>> =
        all_spans.iter().map(|s| s.style.fg).collect();
    assert!(
        unique_colors.len() > 1,
        "Rust highlighting should produce multiple distinct colors, got: {:?}",
        unique_colors
    );
}

#[test]
fn test_highlight_unknown_language_fallback() {
    let source = "some unknown language content\nwith multiple lines";
    // Should not panic; produces plain output
    let lines = highlight_code("unknownlanguagexyz", source);

    assert!(
        !lines.is_empty(),
        "Unknown language should still produce lines"
    );

    let all_text: String = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        all_text.contains("some unknown language content"),
        "Text content should be preserved for unknown language, got: {}",
        all_text
    );
}

#[test]
fn test_highlight_empty_source() {
    let lines = highlight_code("rust", "");
    // Empty source should produce empty lines without panicking
    assert!(lines.is_empty(), "Empty source should produce no lines");
}

#[test]
fn test_code_block_segment_renders_with_borders() {
    use std::collections::HashMap;

    use midtown::{Message, MessageType};

    use super::super::messages_mermaid::render_message_with_mermaid;
    use crate::cli::chat::mermaid::{ContentSegment, MermaidCache};

    let msg = Message {
        id: "1".to_string(),
        from: "park".to_string(),
        content: "ignored".to_string(),
        timestamp: chrono::Utc::now(),
        message_type: MessageType::Text,
        channel: None,
        source_channel: None,
        session_id: None,
        thread_parent_id: None,
    };

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
        None,
        &current_tasks,
        None,
        &[],
        &cache,
        &mut lines,
        &mut diagram_sources,
        &mut mermaid_to_render,
    );

    // Collect all text per line to check content
    let all_lines_text: Vec<String> = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    let all_text = all_lines_text.join("\n");

    assert!(
        all_text.contains("--- rust ---"),
        "Expected top border '--- rust ---', got: {}",
        all_text
    );
    assert!(
        all_text.contains("--- end ---"),
        "Expected bottom border '--- end ---', got: {}",
        all_text
    );
    // Check that fn main() {} appears somewhere across the highlighted spans
    let code_lines: Vec<String> = lines
        .iter()
        .skip(1) // skip border line
        .take_while(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            !text.contains("--- end ---")
        })
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect();
    let code_text = code_lines.join("");
    assert!(
        code_text.contains("fn") && code_text.contains("main"),
        "Expected code content with 'fn' and 'main', got: {}",
        code_text
    );
}
