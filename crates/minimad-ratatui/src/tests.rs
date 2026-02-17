use ratatui::style::{Color, Modifier, Style};

use super::{from_str, inline};

fn base() -> Style {
    Style::default().fg(Color::White)
}

// ── inline() tests ────────────────────────────────────────────────────────────

#[test]
fn test_inline_plain_text() {
    let line = inline("hello world", base());
    assert_eq!(line.spans.len(), 1);
    assert_eq!(line.spans[0].content, "hello world");
    assert_eq!(line.spans[0].style.fg, Some(Color::White));
}

#[test]
fn test_inline_bold() {
    let line = inline("hello **bold** world", base());
    // minimad produces: ["hello ", "bold", " world"]
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("bold"), "Content should contain 'bold'");

    let bold_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "bold")
        .expect("Should have a 'bold' span");
    assert!(
        bold_span.style.add_modifier.contains(Modifier::BOLD),
        "Bold span should have BOLD modifier"
    );
}

#[test]
fn test_inline_italic() {
    let line = inline("hello *italic* world", base());
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("italic"), "Content should contain 'italic'");

    let italic_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "italic")
        .expect("Should have an 'italic' span");
    assert!(
        italic_span.style.add_modifier.contains(Modifier::ITALIC),
        "Italic span should have ITALIC modifier"
    );
}

#[test]
fn test_inline_code() {
    let line = inline("run `cargo test` now", base());
    let code_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "cargo test")
        .expect("Should have 'cargo test' span");
    assert_eq!(
        code_span.style.fg,
        Some(Color::Cyan),
        "Code span should be cyan"
    );
}

#[test]
fn test_inline_strikeout() {
    let line = inline("~~deleted~~", base());
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.contains("deleted"), "Content should contain 'deleted'");

    let strikeout_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "deleted")
        .expect("Should have 'deleted' span");
    assert!(
        strikeout_span
            .style
            .add_modifier
            .contains(Modifier::CROSSED_OUT),
        "Strikeout span should have CROSSED_OUT modifier"
    );
}

#[test]
fn test_inline_combined_bold_italic() {
    let line = inline("**bold** and *italic* and `code`", base());

    let bold_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "bold")
        .expect("Should have 'bold' span");
    assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));

    let italic_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "italic")
        .expect("Should have 'italic' span");
    assert!(italic_span.style.add_modifier.contains(Modifier::ITALIC));

    let code_span = line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "code")
        .expect("Should have 'code' span");
    assert_eq!(code_span.style.fg, Some(Color::Cyan));
}

#[test]
fn test_inline_base_style_preserved() {
    let base = Style::default().fg(Color::Green);
    let line = inline("plain text", base);
    assert_eq!(line.spans[0].style.fg, Some(Color::Green));
}

#[test]
fn test_inline_empty_string() {
    let line = inline("", base());
    // Should return an empty-content span
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(text.is_empty(), "Empty input should produce empty output");
}

#[test]
fn test_inline_only_first_line() {
    // inline() should only return the first line
    let line = inline("first line\nsecond line", base());
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(
        !text.contains("second line"),
        "inline() should only return the first line"
    );
    assert!(
        text.contains("first line"),
        "inline() should return the first line content"
    );
}

// ── from_str() tests ───────────────────────────────────────────────────────────

#[test]
fn test_from_str_plain_text() {
    let text = from_str("hello world", base());
    assert_eq!(text.lines.len(), 1);
    let text_content: String = text.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert_eq!(text_content, "hello world");
}

#[test]
fn test_from_str_multi_line() {
    let text = from_str("line one\nline two\nline three", base());
    assert_eq!(text.lines.len(), 3, "Should have 3 lines");

    let first: String = text.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    let last: String = text.lines[2]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();

    assert!(first.contains("line one"));
    assert!(last.contains("line three"));
}

#[test]
fn test_from_str_bold_in_multiline() {
    let text = from_str("**bold** text", base());
    let first_line = &text.lines[0];
    let bold_span = first_line
        .spans
        .iter()
        .find(|s| s.content.as_ref() == "bold")
        .expect("Should have 'bold' span");
    assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
}

#[test]
fn test_from_str_code_fence() {
    let markdown = "```\nsome code here\n```";
    let text = from_str(markdown, base());

    // Find a line with code content - look for code_fence lines (dark bg style)
    let has_code_line = text.lines.iter().any(|l| {
        let content: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        content.contains("some code here")
    });
    assert!(has_code_line, "Should have a line with code fence content");

    let code_line = text
        .lines
        .iter()
        .find(|l| {
            let content: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            content.contains("some code here")
        })
        .expect("Should find the code line");

    // Code fence lines should have dark background
    let has_dark_bg = code_line
        .spans
        .iter()
        .any(|s| s.style.bg == Some(Color::DarkGray));
    assert!(
        has_dark_bg,
        "Code fence line should have dark gray background"
    );
}

#[test]
fn test_from_str_horizontal_rule() {
    let text = from_str("before\n---\nafter", base());
    let has_rule = text.lines.iter().any(|l| {
        let content: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        content.contains('\u{2500}') // ─
    });
    assert!(has_rule, "Should have a horizontal rule line with ─ chars");
}

#[test]
fn test_from_str_header() {
    // minimad parses headers as Normal lines with styled compounds
    let text = from_str("# My Header", base());
    assert!(!text.lines.is_empty(), "Should have at least one line");
    let header_line: String = text.lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        header_line.contains("My Header"),
        "Header content should be present, got: {}",
        header_line
    );
}

#[test]
fn test_from_str_table_row() {
    let text = from_str("| Col A | Col B |\n|---|---|\n| a | b |", base());

    // Check that we have lines and that the data row contains both columns
    let data_line = text.lines.iter().find(|l| {
        let content: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        content.contains('a') && content.contains('b')
    });

    assert!(
        data_line.is_some(),
        "Should have a table data row with both cell values"
    );

    // The data row should have a │ separator
    if let Some(line) = data_line {
        let content: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            content.contains('\u{2502}'), // │
            "Table row should contain │ separator, got: {}",
            content
        );
    }
}
