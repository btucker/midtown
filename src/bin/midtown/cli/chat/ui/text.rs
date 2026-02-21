//! Text processing utilities: line wrapping and content formatting.

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

/// Count how many wrapped lines `content` produces at the given width,
/// without allocating any intermediate strings.
pub fn count_wrapped_lines(content: &str, width: usize) -> usize {
    let width = width.max(1);
    content
        .split('\n')
        .map(|line| count_line_wraps(line, width))
        .sum::<usize>()
        .max(1)
}

/// Count how many lines a single (newline-free) text segment produces when
/// word-wrapped to `width` columns — mirrors the logic in `wrap_line` but
/// returns only the count with no heap allocation.
fn count_line_wraps(text: &str, width: usize) -> usize {
    let width = width.max(1);
    if text.is_empty() {
        return 1;
    }
    let char_count = text.chars().count();
    if char_count <= width {
        return 1;
    }

    let mut count = 0;
    let mut remaining = text;

    while !remaining.is_empty() {
        let rem_chars = remaining.chars().count();
        if rem_chars <= width {
            count += 1;
            break;
        }

        // Find the byte position of the width-th character
        let byte_pos = remaining
            .char_indices()
            .nth(width)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());

        // Try to find a word boundary within the width limit
        let break_at = remaining[..byte_pos]
            .rfind(' ')
            .map(|pos| pos + 1)
            .unwrap_or(byte_pos);

        count += 1;
        remaining = remaining[break_at..].trim_start();
    }

    count
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
    use ratatui::style::{Color, Modifier, Style};

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
    fn test_count_wrapped_lines_matches_wrap_content() {
        // count_wrapped_lines must return the same count as wrap_content().len()
        // for all cases — it is a zero-allocation shadow of wrap_content.
        let cases: &[(&str, usize)] = &[
            ("", 40),
            ("hello", 40),
            ("hello world", 7),
            ("hello world", 5),
            ("abcdefghij", 5),
            ("this is a longer message that needs multiple wraps", 15),
            ("single line no wrap needed at all", 80),
            ("a b c d e f g", 3),
        ];
        for &(text, width) in cases {
            let expected = wrap_content(text, width).len();
            let actual = count_wrapped_lines(text, width);
            assert_eq!(
                actual, expected,
                "count_wrapped_lines({:?}, {}) = {} but wrap_content().len() = {}",
                text, width, actual, expected
            );
        }
    }

    #[test]
    fn test_count_wrapped_lines_multiline() {
        // Newlines in the input create separate paragraphs — each is wrapped independently.
        let text =
            "line one is short\nthis is a much longer second line that will need to wrap\nshort";
        let width = 20;
        let expected = wrap_content(text, width).len();
        let actual = count_wrapped_lines(text, width);
        assert_eq!(
            actual, expected,
            "Multiline equivalence failed: expected {}, got {}",
            expected, actual
        );
    }

    #[test]
    fn test_count_wrapped_lines_zero_width_no_hang() {
        // width=0 must not hang — it is clamped to 1 inside count_line_wraps.
        let result = count_wrapped_lines("hello world", 0);
        assert!(
            result > 0,
            "Must return at least 1 line for non-empty input"
        );
    }

    #[test]
    fn test_inline_plain_text() {
        let base = Style::default().fg(Color::White);
        let line = minimad_ratatui::inline("hello world", base);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "hello world");
    }

    #[test]
    fn test_inline_bold() {
        let base = Style::default().fg(Color::White);
        let line = minimad_ratatui::inline("hello **bold** world", base);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "hello ");
        assert_eq!(line.spans[1].content, "bold");
        assert!(line.spans[1].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[2].content, " world");
    }

    #[test]
    fn test_inline_italic() {
        let base = Style::default().fg(Color::White);
        let line = minimad_ratatui::inline("hello *italic* world", base);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "hello ");
        assert_eq!(line.spans[1].content, "italic");
        assert!(line.spans[1].style.add_modifier.contains(Modifier::ITALIC));
        assert_eq!(line.spans[2].content, " world");
    }

    #[test]
    fn test_inline_code() {
        let base = Style::default().fg(Color::White);
        let line = minimad_ratatui::inline("run `cargo test` now", base);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "run ");
        assert_eq!(line.spans[1].content, "cargo test");
        assert_eq!(line.spans[1].style.fg, Some(Color::Cyan));
        assert_eq!(line.spans[2].content, " now");
    }

    #[test]
    fn test_inline_mixed() {
        let base = Style::default().fg(Color::White);
        let line = minimad_ratatui::inline("**bold** and `code`", base);
        assert_eq!(line.spans.len(), 3);
        assert_eq!(line.spans[0].content, "bold");
        assert!(line.spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[1].content, " and ");
        assert_eq!(line.spans[2].content, "code");
        assert_eq!(line.spans[2].style.fg, Some(Color::Cyan));
    }
}
