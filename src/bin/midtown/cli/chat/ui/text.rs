//! Text processing utilities: markdown parsing, line wrapping, and content formatting.

use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

/// Parse markdown in text and return styled spans
///
/// Handles:
/// - **bold** -> BOLD modifier
/// - *italic* -> ITALIC modifier
/// - `code` -> Cyan color
pub fn parse_markdown(text: &str, base_style: Style) -> Vec<Span<'static>> {
    if text.is_empty() {
        return vec![Span::styled(String::new(), base_style)];
    }

    let mut spans = Vec::new();
    let mut chars = text.char_indices().peekable();
    let mut current_pos = 0;

    while let Some((i, c)) = chars.next() {
        match c {
            '`' => {
                // Code span - look for closing backtick
                if let Some(end) = text[i + 1..].find('`') {
                    // Add any text before this marker
                    if i > current_pos {
                        spans.push(Span::styled(text[current_pos..i].to_string(), base_style));
                    }
                    // Add the code span (without backticks)
                    let code_text = &text[i + 1..i + 1 + end];
                    spans.push(Span::styled(
                        code_text.to_string(),
                        Style::default().fg(Color::Cyan),
                    ));
                    // Skip past the closing backtick
                    current_pos = i + 1 + end + 1;
                    // Advance the iterator
                    for _ in 0..end + 1 {
                        chars.next();
                    }
                }
            }
            '*' => {
                // Check for ** (bold) or single * (italic)
                if chars.peek().is_some_and(|(_, next_c)| *next_c == '*') {
                    // Bold: **text**
                    chars.next(); // consume second *
                    if let Some(end) = text[i + 2..].find("**") {
                        if i > current_pos {
                            spans.push(Span::styled(text[current_pos..i].to_string(), base_style));
                        }
                        let bold_text = &text[i + 2..i + 2 + end];
                        spans.push(Span::styled(
                            bold_text.to_string(),
                            base_style.add_modifier(Modifier::BOLD),
                        ));
                        current_pos = i + 2 + end + 2;
                        // Skip past closing **
                        for _ in 0..end + 2 {
                            chars.next();
                        }
                    }
                } else {
                    // Italic: *text*
                    if let Some(end) = text[i + 1..].find('*') {
                        // Make sure it's not ** (start of bold)
                        if end > 0 && !text[i + 1..].starts_with('*') {
                            if i > current_pos {
                                spans.push(Span::styled(
                                    text[current_pos..i].to_string(),
                                    base_style,
                                ));
                            }
                            let italic_text = &text[i + 1..i + 1 + end];
                            spans.push(Span::styled(
                                italic_text.to_string(),
                                base_style.add_modifier(Modifier::ITALIC),
                            ));
                            current_pos = i + 1 + end + 1;
                            // Skip past closing *
                            for _ in 0..end + 1 {
                                chars.next();
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Add any remaining text
    if current_pos < text.len() {
        spans.push(Span::styled(text[current_pos..].to_string(), base_style));
    }

    // If no spans were added (e.g., no markdown found), return the whole text
    if spans.is_empty() {
        return vec![Span::styled(text.to_string(), base_style)];
    }

    spans
}

/// Wrap content text into lines that fit the given width
pub fn wrap_content(content: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in content.split('\n') {
        let wrapped = wrap_line(line, width);
        for w in wrapped {
            result.push(w.to_string());
        }
    }
    result
}

/// Wrap a single line of text to fit within the given width
///
/// Uses word boundaries when possible, falls back to character wrapping.
/// Handles UTF-8 multi-byte characters correctly by using character indices.
pub fn wrap_line(text: &str, width: usize) -> Vec<&str> {
    // Clamp width to minimum 1 to prevent infinite loop
    let width = width.max(1);

    if text.is_empty() {
        return vec![""];
    }
    // Use character count, not byte length (UTF-8 chars can be multi-byte)
    if text.chars().count() <= width {
        return vec![text];
    }

    let mut result = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        let char_count = remaining.chars().count();
        if char_count <= width {
            result.push(remaining);
            break;
        }

        // Find the byte position of the width-th character (safe boundary)
        let byte_pos = remaining
            .char_indices()
            .nth(width)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());

        // Try to find a word boundary within the width limit
        let break_at = remaining[..byte_pos]
            .rfind(' ')
            .map(|pos| pos + 1) // Include the space in current line
            .unwrap_or(byte_pos); // Fall back to hard break at char boundary

        let (line, rest) = remaining.split_at(break_at);
        result.push(line.trim_end()); // Remove trailing space from wrapped line
        remaining = rest.trim_start(); // Remove leading space from next line
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrap_line_empty() {
        assert_eq!(wrap_line("", 40), vec![""]);
    }

    #[test]
    fn test_wrap_line_fits() {
        assert_eq!(wrap_line("hello world", 40), vec!["hello world"]);
    }

    #[test]
    fn test_wrap_line_word_boundary() {
        // "hello world" is 11 chars, with width 7 it should wrap at the space
        assert_eq!(wrap_line("hello world", 7), vec!["hello", "world"]);
    }

    #[test]
    fn test_wrap_line_hard_break() {
        // Word too long to fit, must hard break
        assert_eq!(wrap_line("abcdefghij", 5), vec!["abcde", "fghij"]);
    }

    #[test]
    fn test_wrap_line_multiple_wraps() {
        let text = "this is a longer message that needs multiple wraps";
        let wrapped = wrap_line(text, 15);
        // Each line should be at most 15 characters (not bytes)
        for line in &wrapped {
            assert!(
                line.chars().count() <= 15,
                "Line too long: {} ({} chars)",
                line,
                line.chars().count()
            );
        }
        // Reassembling should give us the original (minus spaces at wrap points)
        let rejoined: String = wrapped.join(" ");
        assert_eq!(rejoined.replace("  ", " "), text);
    }

    #[test]
    fn test_wrap_line_utf8_box_drawing() {
        // Box-drawing characters are 3 bytes each in UTF-8
        let text = "┌─ Team ─────────────────────────────────────────┐";
        // This should NOT panic - the bug was slicing at byte boundaries
        let wrapped = wrap_line(text, 40);
        // Verify we get valid UTF-8 strings back
        for line in &wrapped {
            assert!(line.chars().count() <= 40);
        }
    }

    #[test]
    fn test_wrap_line_zero_width() {
        // Zero width should not cause infinite loop - clamp to minimum 1
        let wrapped = wrap_line("hello", 0);
        // Should produce single-character chunks
        assert_eq!(wrapped, vec!["h", "e", "l", "l", "o"]);
    }

    #[test]
    fn test_parse_markdown_plain_text() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello world", base);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].content, "hello world");
    }

    #[test]
    fn test_parse_markdown_bold() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello **bold** world", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "bold");
        assert!(spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[2].content, " world");
    }

    #[test]
    fn test_parse_markdown_italic() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("hello *italic* world", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "hello ");
        assert_eq!(spans[1].content, "italic");
        assert!(spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(spans[2].content, " world");
    }

    #[test]
    fn test_parse_markdown_code() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("run `cargo test` now", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "run ");
        assert_eq!(spans[1].content, "cargo test");
        assert_eq!(spans[1].style.fg, Some(Color::Cyan));
        assert_eq!(spans[2].content, " now");
    }

    #[test]
    fn test_parse_markdown_mixed() {
        let base = Style::default().fg(Color::White);
        let spans = parse_markdown("**bold** and `code`", base);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "bold");
        assert!(spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(spans[1].content, " and ");
        assert_eq!(spans[2].content, "code");
        assert_eq!(spans[2].style.fg, Some(Color::Cyan));
    }
}
