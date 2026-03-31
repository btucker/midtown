//! Text utility functions.

/// Truncate a string to max length with ellipsis.
/// Uses `floor_char_boundary` to avoid panicking on multi-byte UTF-8 characters.
pub fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let end = s.floor_char_boundary(max_len.saturating_sub(3));
        format!("{}...", &s[..end])
    }
}
