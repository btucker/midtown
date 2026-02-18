//! Syntax highlighting for fenced code blocks in the chat TUI.
//!
//! Uses syntect with the base16-ocean.dark theme to colorize code.
//! Falls back to plain text styling for unknown languages.

use once_cell::sync::Lazy;
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use syntect::{
    easy::HighlightLines, highlighting::ThemeSet, parsing::SyntaxSet, util::LinesWithEndings,
};

static SYNTAX_SET: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEME_SET: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

/// Highlight source code and return ratatui Lines with colored spans.
///
/// Uses syntect with the base16-ocean.dark theme. Falls back to plain text
/// syntax highlighting if the language is unknown.
pub fn highlight_code(language: &str, source: &str) -> Vec<Line<'static>> {
    if source.is_empty() {
        return Vec::new();
    }

    let syntax = find_syntax(language);

    let theme = &THEME_SET.themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);

    let mut result = Vec::new();

    for line in LinesWithEndings::from(source) {
        let ranges = h.highlight_line(line, &SYNTAX_SET).unwrap_or_default();

        let spans: Vec<Span<'static>> = ranges
            .into_iter()
            .map(|(style, text)| {
                let fg = syntect_to_ratatui_color(style.foreground);
                let content = text.trim_end_matches('\n').to_string();
                Span::styled(content, Style::default().fg(fg))
            })
            .filter(|s| !s.content.is_empty())
            .collect();

        result.push(Line::from(spans));
    }

    result
}

/// Find syntax definition for a language name or common alias.
///
/// Tries the exact name first, then a set of well-known aliases.
/// Returns a plain text syntax as fallback for unknown languages.
fn find_syntax(language: &str) -> &'static syntect::parsing::SyntaxReference {
    let ss = &*SYNTAX_SET;

    // Try exact name lookup
    if let Some(syntax) = ss.find_syntax_by_name(language) {
        return syntax;
    }

    // Try extension lookup (handles "rs", "js", "py", etc.)
    if let Some(syntax) = ss.find_syntax_by_extension(language) {
        return syntax;
    }

    // Common aliases
    let alias = match language.to_lowercase().as_str() {
        "rust" => "Rust",
        "javascript" | "js" => "JavaScript",
        "typescript" | "ts" => "TypeScript",
        "python" | "py" => "Python",
        "bash" | "sh" | "shell" => "Bash",
        "json" => "JSON",
        "yaml" | "yml" => "YAML",
        "toml" => "TOML",
        "sql" => "SQL",
        "html" => "HTML",
        "css" => "CSS",
        "c" => "C",
        "cpp" | "c++" => "C++",
        "java" => "Java",
        "go" => "Go",
        "ruby" | "rb" => "Ruby",
        "swift" => "Swift",
        "kotlin" | "kt" => "Kotlin",
        "scala" => "Scala",
        "haskell" | "hs" => "Haskell",
        "xml" => "XML",
        "markdown" | "md" => "Markdown",
        "makefile" | "make" => "Makefile",
        "dockerfile" => "Dockerfile",
        _ => "",
    };

    if !alias.is_empty()
        && let Some(syntax) = ss.find_syntax_by_name(alias)
    {
        return syntax;
    }

    // Fall back to plain text
    ss.find_syntax_plain_text()
}

/// Convert syntect RGBA color to ratatui Color
fn syntect_to_ratatui_color(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

#[path = "highlight_tests.rs"]
#[cfg(test)]
mod tests;
