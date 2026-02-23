//! ANSI rendering for `midtown session view`.
//!
//! Converts the rich-text output produced by `format_events_as_rich_text`
//! (which contains markdown prose + labeled code fences) into ANSI-escaped
//! terminal output with:
//!
//! - Syntax-highlighted code blocks (via syntect, same theme as the TUI)
//! - Bold headers (`##`, `**...**`)
//! - Italic (`*...*`)
//! - Inline code (`` `...` ``)
//! - Tool-call headers formatted distinctively

use once_cell::sync::Lazy;
use syntect::{
    easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet, util::LinesWithEndings,
};

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

// ANSI escape codes
const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const ITALIC: &str = "\x1b[3m";
const DIM: &str = "\x1b[2m";
const FG_CYAN: &str = "\x1b[36m";
const FG_YELLOW: &str = "\x1b[33m";
const FG_GREEN: &str = "\x1b[32m";
const BG_DARK: &str = "\x1b[48;2;43;48;59m"; // base16-ocean.dark background

/// Render a rich-text session output string to ANSI-escaped terminal output.
///
/// The input is expected to be the output of `format_events_as_rich_text`:
/// markdown prose interspersed with labeled code fences like:
/// ```
/// **[Bash]**
/// ```bash
/// ls -la
/// ```
///
/// **[result]**
/// ```
/// file1.txt
/// ```
/// ```
pub fn render_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len() * 2);
    let mut in_code_fence = false;
    let mut fence_lang: Option<String> = None;
    let mut code_block_lines: Vec<String> = Vec::new();

    for line in input.lines() {
        if in_code_fence {
            if line == "```" || line == "~~~" {
                // End of code block — flush with syntax highlighting
                let rendered =
                    render_code_block(fence_lang.as_deref().unwrap_or(""), &code_block_lines);
                output.push_str(&rendered);
                in_code_fence = false;
                fence_lang = None;
                code_block_lines.clear();
                output.push('\n');
            } else {
                code_block_lines.push(line.to_string());
            }
        } else if let Some(lang) = line.strip_prefix("```") {
            in_code_fence = true;
            fence_lang = if lang.is_empty() {
                None
            } else {
                Some(lang.to_string())
            };
            code_block_lines.clear();
        } else if let Some(lang) = line.strip_prefix("~~~") {
            in_code_fence = true;
            fence_lang = if lang.is_empty() {
                None
            } else {
                Some(lang.to_string())
            };
            code_block_lines.clear();
        } else {
            output.push_str(&render_prose_line(line));
            output.push('\n');
        }
    }

    // Unclosed code fence — render what we have
    if in_code_fence && !code_block_lines.is_empty() {
        let rendered = render_code_block(fence_lang.as_deref().unwrap_or(""), &code_block_lines);
        output.push_str(&rendered);
    }

    output
}

/// Render a code block with syntect syntax highlighting.
fn render_code_block(lang: &str, lines: &[String]) -> String {
    let ss = &*SYNTAX_SET;
    let theme = &THEME_SET.themes["base16-ocean.dark"];

    let syntax = if lang.is_empty() {
        ss.find_syntax_plain_text()
    } else {
        ss.find_syntax_by_token(lang)
            .or_else(|| ss.find_syntax_by_name(lang))
            .or_else(|| ss.find_syntax_by_extension(lang))
            .unwrap_or_else(|| ss.find_syntax_plain_text())
    };

    let mut h = HighlightLines::new(syntax, theme);
    let source = lines.join("\n") + "\n";
    let mut result = String::new();

    // Opening fence line with dim language label
    if !lang.is_empty() {
        result.push_str(&format!("{DIM}── {lang} ──{RESET}\n"));
    } else {
        result.push_str(&format!("{DIM}────────{RESET}\n"));
    }

    for line in LinesWithEndings::from(&source) {
        let ranges = match h.highlight_line(line, ss) {
            Ok(r) => r,
            Err(_) => {
                result.push_str(line);
                continue;
            }
        };

        result.push_str(BG_DARK);
        for (style, text) in &ranges {
            let c = style.foreground;
            result.push_str(&format!(
                "\x1b[38;2;{};{};{}m{}{RESET}{BG_DARK}",
                c.r, c.g, c.b, text
            ));
        }
        result.push_str(RESET);
    }

    // Closing fence line
    result.push_str(&format!("{DIM}────────{RESET}\n"));
    result
}

/// Render a single prose line with inline markdown formatting.
///
/// Handles:
/// - `# ## ###` headers → bold + color
/// - `**text**` bold → bold
/// - `*text*` italic → italic
/// - `` `code` `` inline code → dim + color
/// - `**[ToolName]**` tool headers → bold + cyan
fn render_prose_line(line: &str) -> String {
    // Headers
    if let Some(rest) = line.strip_prefix("### ") {
        return format!("{BOLD}{FG_YELLOW}{rest}{RESET}");
    }
    if let Some(rest) = line.strip_prefix("## ") {
        return format!("{BOLD}{FG_YELLOW}{rest}{RESET}");
    }
    if let Some(rest) = line.strip_prefix("# ") {
        return format!("{BOLD}{FG_YELLOW}{rest}{RESET}");
    }

    // Horizontal rules
    if line == "---" || line == "***" || line == "===" {
        return format!("{DIM}─────────────────────────────────────{RESET}");
    }

    // Apply inline formatting
    apply_inline_formatting(line)
}

/// Apply inline formatting (bold, italic, code, tool headers) to a string.
fn apply_inline_formatting(input: &str) -> String {
    let mut result = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Check for **[ToolName]** pattern — tool call header
        // e.g. "**[Bash]**": chars[i]='*', chars[i+1]='*', chars[i+2]='['
        // find_substr returns the position *after* the closing "**"
        if chars[i] == '*'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && i + 2 < chars.len()
            && chars[i + 2] == '['
            && let Some(end) = find_substr(&chars, i + 3, "**")
        {
            // inner = chars[i+2..end-2] captures "[ToolName]"
            let inner: String = chars[i + 2..end - 2].iter().collect();
            result.push_str(&format!("{BOLD}{FG_GREEN}{inner}{RESET}"));
            i = end;
            continue;
        }

        // Check for **bold**
        if chars[i] == '*'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && let Some(end) = find_close_double_star(&chars, i + 2)
        {
            let inner: String = chars[i + 2..end].iter().collect();
            result.push_str(&format!("{BOLD}{inner}{RESET}"));
            i = end + 2;
            continue;
        }

        // Check for *italic*
        if chars[i] == '*'
            && (i == 0 || chars[i - 1] != '*')
            && i + 1 < chars.len()
            && chars[i + 1] != '*'
            && let Some(end) = find_close_single_star(&chars, i + 1)
        {
            let inner: String = chars[i + 1..end].iter().collect();
            result.push_str(&format!("{ITALIC}{inner}{RESET}"));
            i = end + 1;
            continue;
        }

        // Check for `inline code`
        if chars[i] == '`'
            && (i == 0 || chars[i - 1] != '`')
            && let Some(end) = chars[i + 1..].iter().position(|&c| c == '`')
        {
            let inner: String = chars[i + 1..i + 1 + end].iter().collect();
            result.push_str(&format!("{DIM}{FG_CYAN}{inner}{RESET}"));
            i = i + 1 + end + 1;
            continue;
        }

        result.push(chars[i]);
        i += 1;
    }

    result
}

/// Find the position of `**` closing a bold span, starting at `from`.
fn find_close_double_star(chars: &[char], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < chars.len() {
        if chars[i] == '*' && chars[i + 1] == '*' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Find the position of `*` closing an italic span, starting at `from`.
fn find_close_single_star(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == '*' && (i + 1 >= chars.len() || chars[i + 1] != '*'))
}

/// Find the end position of `pattern` in `chars`, starting at `from`.
/// Returns the position immediately after the pattern ends.
fn find_substr(chars: &[char], from: usize, pattern: &str) -> Option<usize> {
    let pat: Vec<char> = pattern.chars().collect();
    let pat_len = pat.len();
    if pat_len == 0 || from + pat_len > chars.len() {
        return None;
    }
    for i in from..=(chars.len() - pat_len) {
        if chars[i..i + pat_len] == pat[..] {
            return Some(i + pat_len);
        }
    }
    None
}

/// Render a single JSONL event line as ANSI-formatted output, for `--watch` mode.
///
/// Returns `None` if the line is not a displayable event (e.g., System/Result events).
pub fn render_event_line(jsonl_line: &str) -> Option<String> {
    let event: midtown::headless::StreamEvent = serde_json::from_str(jsonl_line).ok()?;

    let rich = match event {
        midtown::headless::StreamEvent::Assistant { message, .. } => {
            let content = message.get("content")?.as_array()?;
            let mut parts: Vec<String> = Vec::new();
            for block in content {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str())
                            && !text.trim().is_empty()
                        {
                            parts.push(text.to_string());
                        }
                    }
                    Some("tool_use") => {
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown");
                        let input = block.get("input").unwrap_or(&serde_json::Value::Null);
                        let input_str = serde_json::to_string_pretty(input).unwrap_or_default();
                        let lang = if name == "Bash" { "bash" } else { "json" };
                        parts.push(format!("**[{name}]**\n```{lang}\n{input_str}\n```"));
                    }
                    _ => {}
                }
            }
            if parts.is_empty() {
                return None;
            }
            parts.join("\n\n")
        }
        midtown::headless::StreamEvent::User { message, .. } => {
            let content = message.get("content")?.as_array()?;
            let mut parts: Vec<String> = Vec::new();
            for block in content {
                if block.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                    let result_text = extract_tool_result_text(block);
                    if !result_text.trim().is_empty() {
                        parts.push(format!("**[result]**\n```\n{result_text}\n```"));
                    }
                }
            }
            if parts.is_empty() {
                return None;
            }
            parts.join("\n\n")
        }
        _ => return None,
    };

    Some(render_ansi(&rich))
}

/// Extract text from a tool_result content block.
fn extract_tool_result_text(block: &serde_json::Value) -> String {
    match block.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[path = "session_render_tests.rs"]
#[cfg(test)]
mod tests;
