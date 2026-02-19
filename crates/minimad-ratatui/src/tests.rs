use ratatui::style::{Color, Modifier, Style};

use super::{from_str, inline};

// ── table helper ──────────────────────────────────────────────────────────────

/// Collect the full string content of a ratatui Line.
fn line_content(line: &ratatui::text::Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

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

// ── table column alignment tests ───────────────────────────────────────────────

#[test]
fn test_table_columns_are_padded_to_equal_widths() {
    // Header "Feature" is 7 chars, "Status" is 6 chars, "Owner" is 5 chars.
    // Data row "Universal events pipeline" is 25 chars — forces that column wider.
    let md =
        "| Feature | Status | Owner |\n|---|---|---|\n| Universal events pipeline | Done | alice |";
    let text = from_str(md, base());

    // Find the header line (contains "Feature")
    let header_line = text
        .lines
        .iter()
        .find(|l| line_content(l).contains("Feature"));
    // Find the data line (contains "Universal")
    let data_line = text
        .lines
        .iter()
        .find(|l| line_content(l).contains("Universal"));

    assert!(header_line.is_some(), "Should have a header line");
    assert!(data_line.is_some(), "Should have a data line");

    let header_content = line_content(header_line.unwrap());
    let data_content = line_content(data_line.unwrap());

    // Both rows must have the same total rendered width (padding makes them equal)
    assert_eq!(
        header_content.chars().count(),
        data_content.chars().count(),
        "Header and data rows should have equal rendered width. header={:?} data={:?}",
        header_content,
        data_content
    );
}

#[test]
fn test_table_rule_width_matches_table_width() {
    let md = "| Feature | Status |\n|---|---|\n| Universal events pipeline | Done |";
    let text = from_str(md, base());

    let header_line = text
        .lines
        .iter()
        .find(|l| line_content(l).contains("Feature"));
    let rule_line = text.lines.iter().find(|l| {
        let c = line_content(l);
        c.contains('\u{2500}') && !c.contains("Feature") && !c.contains("Universal")
    });

    assert!(header_line.is_some(), "Should have a header line");
    assert!(rule_line.is_some(), "Should have a table rule line");

    let header_width = line_content(header_line.unwrap()).chars().count();
    let rule_width = line_content(rule_line.unwrap()).chars().count();

    assert_eq!(
        header_width, rule_width,
        "TableRule width ({}) should match header row width ({})",
        rule_width, header_width
    );
}

#[test]
fn test_table_header_row_is_bold() {
    let md = "| Col A | Col B |\n|---|---|\n| a | b |";
    let text = from_str(md, base());

    // Header row (first TableRow) should have bold spans for cell content
    let header_line = text
        .lines
        .iter()
        .find(|l| line_content(l).contains("Col A"));
    assert!(header_line.is_some(), "Should have a header line");

    let header_line = header_line.unwrap();
    let has_bold = header_line
        .spans
        .iter()
        .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
    assert!(
        has_bold,
        "Header row should have at least one bold span. spans: {:?}",
        header_line
            .spans
            .iter()
            .map(|s| (&s.content, s.style.add_modifier))
            .collect::<Vec<_>>()
    );
}

#[test]
fn test_table_alignment_right() {
    // Right-aligned column: content should be right-padded with spaces on left
    let md = "| Name | Count |\n|---|---:|\n| Alice | 42 |";
    let text = from_str(md, base());

    let data_line = text
        .lines
        .iter()
        .find(|l| line_content(l).contains("Alice"));
    assert!(data_line.is_some(), "Should have a data line");

    let data_content = line_content(data_line.unwrap());
    // In right-aligned column, "42" should be preceded by spaces within its cell
    // The second cell content should have leading spaces before "42"
    // We check by finding where "42" appears and verifying a space precedes it
    let idx = data_content.find("42").expect("Should contain '42'");
    assert!(
        idx > 0 && data_content.as_bytes()[idx - 1] == b' ',
        "Right-aligned '42' should be preceded by a space. content={:?}",
        data_content
    );
}

#[test]
fn test_table_alignment_center() {
    let md = "| Name | Count |\n|---|:---:|\n| Hi | 5 |";
    let text = from_str(md, base());

    let data_line = text.lines.iter().find(|l| line_content(l).contains("Hi"));
    assert!(data_line.is_some(), "Should have a data line");

    let data_content = line_content(data_line.unwrap());
    // Center-aligned "5" in a wider column should have spaces on both sides
    let idx = data_content.find('5').expect("Should contain '5'");
    let before = idx > 0 && data_content.as_bytes()[idx - 1] == b' ';
    let after = data_content.len() > idx + 1 && data_content.as_bytes()[idx + 1] == b' ';
    assert!(
        before || after,
        "Center-aligned '5' should have surrounding spaces. content={:?}",
        data_content
    );
}

#[test]
fn test_inline_table_rule_width_scales_with_columns() {
    // The fallback path in mad_line_to_line() should produce a rule width
    // proportional to the column count, not a hardcoded value.
    use super::mad_line_to_line;
    use minimad::Text as MadText;

    let md = "|---|---|---|";
    let mad_text = MadText::from(md);
    let mad_line = &mad_text.lines[0];
    let line = mad_line_to_line(mad_line, base());
    let content = line_content(&line);

    // 3 columns → 3*3 + 2*3 = 15 chars of ─
    assert_eq!(
        content.chars().count(),
        15,
        "TableRule with 3 columns should produce 15-char rule, got: {:?}",
        content
    );

    // Verify it scales: 2-column table rule should be shorter
    let md2 = "|---|---|";
    let mad_text2 = MadText::from(md2);
    let mad_line2 = &mad_text2.lines[0];
    let line2 = mad_line_to_line(mad_line2, base());
    let content2 = line_content(&line2);

    // 2 columns → 2*3 + 1*3 = 9 chars of ─
    assert_eq!(
        content2.chars().count(),
        9,
        "TableRule with 2 columns should produce 9-char rule, got: {:?}",
        content2
    );
}

// ── syntax highlighting tests ──────────────────────────────────────────────────

#[test]
fn test_from_str_code_fence_without_language_uses_dark_bg() {
    let markdown = "```\nsome code\n```";
    let text = from_str(markdown, base());

    assert_eq!(
        text.lines.len(),
        1,
        "Should have exactly 1 line (no fence delimiters)"
    );
    let code_line = &text.lines[0];

    let content = line_content(code_line);
    assert!(content.contains("some code"), "Should have code content");

    let has_dark_bg = code_line
        .spans
        .iter()
        .any(|s| s.style.bg == Some(Color::DarkGray));
    assert!(
        has_dark_bg,
        "Should have dark gray background without syntax highlighting"
    );
}

#[test]
fn test_from_str_code_fence_with_language_has_dark_bg() {
    let markdown = "```rust\nlet x = 1;\n```";
    let text = from_str(markdown, base());

    assert!(!text.lines.is_empty(), "Should have at least one line");
    let code_line = &text.lines[0];

    let all_dark_bg = code_line
        .spans
        .iter()
        .all(|s| s.style.bg == Some(Color::DarkGray));
    assert!(
        all_dark_bg,
        "All code spans should have dark gray background"
    );

    let content = line_content(code_line);
    assert!(content.contains("let"), "Should contain 'let' keyword");
}

#[test]
fn test_from_str_code_fence_with_language_has_rgb_colors() {
    let markdown = "```rust\nlet x = 1;\n```";
    let text = from_str(markdown, base());

    let code_line = &text.lines[0];
    let has_rgb = code_line
        .spans
        .iter()
        .any(|s| matches!(s.style.fg, Some(Color::Rgb(_, _, _))));
    assert!(has_rgb, "Rust code should have RGB syntax highlight colors");
}

#[test]
fn test_from_str_code_fence_no_fence_delimiter_lines() {
    let markdown = "```rust\nfn main() {}\n```";
    let text = from_str(markdown, base());

    for line in &text.lines {
        let content = line_content(line);
        assert!(
            !content.trim_start_matches('`').is_empty() || content.contains("fn"),
            "Fence delimiter lines should not appear in output, got: {:?}",
            content
        );
    }

    let has_fn = text.lines.iter().any(|l| line_content(l).contains("fn"));
    assert!(has_fn, "Should have the code content");
    assert_eq!(text.lines.len(), 1, "Should have exactly 1 line of code");
}

#[test]
fn test_from_str_code_fence_multiline_code() {
    let markdown = "```rust\nlet a = 1;\nlet b = 2;\n```";
    let text = from_str(markdown, base());

    assert_eq!(text.lines.len(), 2, "Should have 2 lines of code");

    let line0 = line_content(&text.lines[0]);
    let line1 = line_content(&text.lines[1]);
    assert!(line0.contains('a'), "First line should contain 'a'");
    assert!(line1.contains('b'), "Second line should contain 'b'");
}

#[test]
fn test_from_str_code_fence_followed_by_text() {
    let markdown = "```rust\nlet x = 1;\n```\n\nNormal text after.";
    let text = from_str(markdown, base());

    let has_code = text.lines.iter().any(|l| line_content(l).contains("let"));
    let has_text = text
        .lines
        .iter()
        .any(|l| line_content(l).contains("Normal text after"));

    assert!(has_code, "Should have the code line");
    assert!(has_text, "Should have the normal text after the fence");
}

#[test]
fn test_from_str_text_before_code_fence() {
    let markdown = "Before text.\n\n```rust\nlet x = 1;\n```";
    let text = from_str(markdown, base());

    let has_before = text
        .lines
        .iter()
        .any(|l| line_content(l).contains("Before text"));
    let has_code = text.lines.iter().any(|l| line_content(l).contains("let"));

    assert!(has_before, "Should have text before the code fence");
    assert!(has_code, "Should have the code content");
}

#[test]
fn test_from_str_unknown_language_uses_plain_text_highlighting() {
    let markdown = "```unknownlang123\nsome code here\n```";
    let text = from_str(markdown, base());

    assert!(!text.lines.is_empty(), "Should produce output");
    let code_line = &text.lines[0];
    let has_dark_bg = code_line
        .spans
        .iter()
        .any(|s| s.style.bg == Some(Color::DarkGray));
    assert!(
        has_dark_bg,
        "Unknown language should still have dark background"
    );
}

#[test]
fn test_from_str_table_after_code_fence() {
    let markdown = "```rust\nlet x = 1;\n```\n\n| Col A | Col B |\n|---|---|\n| a | b |";
    let text = from_str(markdown, base());

    let has_code = text.lines.iter().any(|l| line_content(l).contains("let"));
    let has_table = text.lines.iter().any(|l| {
        let c = line_content(l);
        c.contains('a') && c.contains('\u{2502}')
    });

    assert!(has_code, "Should have the code line");
    assert!(has_table, "Should have the table row with separator");
}

#[test]
fn test_from_str_table_header_bold_after_code_fence() {
    let markdown = "```rust\nlet x = 1;\n```\n\n| Feature | Status |\n|---|---|\n| Auth | Done |";
    let text = from_str(markdown, base());

    let header_line = text
        .lines
        .iter()
        .find(|l| line_content(l).contains("Feature"));
    assert!(header_line.is_some(), "Should have table header line");

    let header_line = header_line.unwrap();
    let has_bold = header_line
        .spans
        .iter()
        .any(|s| s.style.add_modifier.contains(Modifier::BOLD));
    assert!(
        has_bold,
        "Table header should be bold even after a code fence"
    );
}

// ── table outer border tests ───────────────────────────────────────────────────

#[test]
fn test_table_has_outer_border() {
    let md = "| Name | Status |\n|---|---|\n| alice | done |";
    let text = from_str(md, base());

    let contents: Vec<String> = text.lines.iter().map(|l| line_content(l)).collect();

    // Should have 5 lines: top border, header, rule, data row, bottom border
    assert_eq!(
        contents.len(),
        5,
        "Table should have 5 lines (top border, header, rule, data, bottom border). Got: {:?}",
        contents
    );

    // Top border starts with ┌ and ends with ┐
    assert!(
        contents[0].starts_with('┌'),
        "Top border should start with ┌, got: {:?}",
        contents[0]
    );
    assert!(
        contents[0].ends_with('┐'),
        "Top border should end with ┐, got: {:?}",
        contents[0]
    );

    // Header row starts and ends with │
    assert!(
        contents[1].starts_with('│'),
        "Header should start with │, got: {:?}",
        contents[1]
    );
    assert!(
        contents[1].ends_with('│'),
        "Header should end with │, got: {:?}",
        contents[1]
    );

    // Rule row starts with ├ and ends with ┤
    assert!(
        contents[2].starts_with('├'),
        "Rule should start with ├, got: {:?}",
        contents[2]
    );
    assert!(
        contents[2].ends_with('┤'),
        "Rule should end with ┤, got: {:?}",
        contents[2]
    );

    // Data row starts and ends with │
    assert!(
        contents[3].starts_with('│'),
        "Data row should start with │, got: {:?}",
        contents[3]
    );
    assert!(
        contents[3].ends_with('│'),
        "Data row should end with │, got: {:?}",
        contents[3]
    );

    // Bottom border starts with └ and ends with ┘
    assert!(
        contents[4].starts_with('└'),
        "Bottom border should start with └, got: {:?}",
        contents[4]
    );
    assert!(
        contents[4].ends_with('┘'),
        "Bottom border should end with ┘, got: {:?}",
        contents[4]
    );
}

#[test]
fn test_table_border_width_consistent() {
    let md = "| Name | Status |\n|---|---|\n| alice | done |";
    let text = from_str(md, base());

    let contents: Vec<String> = text.lines.iter().map(|l| line_content(l)).collect();
    assert_eq!(contents.len(), 5, "Should have 5 lines");

    // All lines should have the same char width
    let widths: Vec<usize> = contents.iter().map(|l| l.chars().count()).collect();
    assert!(
        widths.iter().all(|&w| w == widths[0]),
        "All table lines should have the same width. Got widths: {:?}, contents: {:?}",
        widths,
        contents
    );
}
