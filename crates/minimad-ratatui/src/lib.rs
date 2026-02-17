//! Bridge between the minimad markdown parser and ratatui types.
//!
//! Provides two public functions:
//! - [`from_str`]: Parse markdown into a multi-line `ratatui::text::Text`
//! - [`inline`]: Parse markdown as a single inline line for single-line rendering

use std::sync::OnceLock;

use minimad::{Alignment, CompositeStyle, Line as MadLine, Text as MadText};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style as SyntectStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;

#[path = "tests.rs"]
#[cfg(test)]
mod tests;

fn syntax_set() -> &'static SyntaxSet {
    static SS: OnceLock<SyntaxSet> = OnceLock::new();
    SS.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn default_theme() -> &'static Theme {
    static THEME: OnceLock<Theme> = OnceLock::new();
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .into_iter()
            .find(|(name, _)| name == "base16-ocean.dark")
            .map(|(_, theme)| theme)
            .expect("base16-ocean.dark theme should exist in syntect defaults")
    })
}

/// Convert a syntect foreground color to a ratatui `Color::Rgb`.
fn syntect_color_to_ratatui(color: syntect::highlighting::Color) -> Color {
    Color::Rgb(color.r, color.g, color.b)
}

/// Highlight a block of code lines using syntect.
///
/// When `lang` is `None` or unrecognized, falls back to plain text (dark bg, no color).
/// When `lang` is a recognized language, applies RGB token colors over a dark background.
fn highlight_code_block(code_lines: &[&str], lang: Option<&str>) -> Vec<Line<'static>> {
    let ss = syntax_set();
    let theme = default_theme();
    let code_bg = Style::default().bg(Color::DarkGray);

    let syntax = lang
        .and_then(|l| {
            ss.find_syntax_by_token(l)
                .or_else(|| ss.find_syntax_by_name(l))
        })
        .unwrap_or_else(|| ss.find_syntax_plain_text());

    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut result = Vec::new();

    for line_text in code_lines {
        match highlighter.highlight_line(line_text, ss) {
            Ok(ranges) => {
                let spans: Vec<Span<'static>> = ranges
                    .into_iter()
                    .map(|(style, text): (SyntectStyle, &str)| {
                        let fg = syntect_color_to_ratatui(style.foreground);
                        Span::styled(
                            text.to_string(),
                            Style::default().fg(fg).bg(Color::DarkGray),
                        )
                    })
                    .collect();
                result.push(Line::from(spans));
            }
            Err(_) => {
                result.push(Line::from(Span::styled(line_text.to_string(), code_bg)));
            }
        }
    }

    result
}

/// Convert a minimad `Compound` to a ratatui `Span`.
///
/// Applies bold, italic, strikeout modifiers and cyan foreground for code spans.
/// The `base_style` is used as the starting style for all spans.
fn compound_to_span(compound: &minimad::Compound<'_>, base_style: Style) -> Span<'static> {
    let mut style = base_style;

    if compound.bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if compound.italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if compound.strikeout {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if compound.code {
        style = style.fg(Color::Cyan);
    }

    Span::styled(compound.src.to_string(), style)
}

/// Convert a minimad `Composite` to a `Vec<Span<'static>>`.
fn composite_to_spans(composite: &minimad::Composite<'_>, base_style: Style) -> Vec<Span<'static>> {
    composite
        .compounds
        .iter()
        .map(|c| compound_to_span(c, base_style))
        .collect()
}

/// Compute the visible character width of a `Composite` (sum of all compound char lengths).
fn composite_char_width(composite: &minimad::Composite<'_>) -> usize {
    composite.char_length()
}

/// Build a ratatui `Line` for a single table row, padding each cell to `col_widths` and
/// applying `alignments`. If `header_style` is provided, cell content uses that style.
fn render_table_row(
    table_row: &minimad::TableRow<'_>,
    col_widths: &[usize],
    alignments: &[Alignment],
    cell_style: Style,
    base_style: Style,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();

    for (i, cell) in table_row.cells.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" \u{2502} ", base_style)); // │
        }

        let target_width = col_widths.get(i).copied().unwrap_or(0);
        let content_width = composite_char_width(cell);
        let padding = target_width.saturating_sub(content_width);

        let alignment = alignments.get(i).copied().unwrap_or(Alignment::Unspecified);

        let (pad_left, pad_right) = match alignment {
            Alignment::Right => (padding, 0),
            Alignment::Center => {
                let left = padding / 2;
                let right = padding - left;
                (left, right)
            }
            // Left or Unspecified: pad on right
            _ => (0, padding),
        };

        if pad_left > 0 {
            spans.push(Span::styled(" ".repeat(pad_left), base_style));
        }
        spans.extend(composite_to_spans(cell, cell_style));
        if pad_right > 0 {
            spans.push(Span::styled(" ".repeat(pad_right), base_style));
        }
    }

    Line::from(spans)
}

/// Compute column widths and alignments for a contiguous block of table lines.
///
/// Returns `(col_widths, alignments)`.
fn compute_table_layout(table_lines: &[&MadLine<'_>]) -> (Vec<usize>, Vec<Alignment>) {
    let mut col_widths: Vec<usize> = Vec::new();
    let mut alignments: Vec<Alignment> = Vec::new();

    for mad_line in table_lines {
        match mad_line {
            MadLine::TableRow(row) => {
                for (i, cell) in row.cells.iter().enumerate() {
                    let w = composite_char_width(cell);
                    if i >= col_widths.len() {
                        col_widths.resize(i + 1, 0);
                    }
                    if w > col_widths[i] {
                        col_widths[i] = w;
                    }
                }
            }
            MadLine::TableRule(rule) => {
                for (i, &align) in rule.cells.iter().enumerate() {
                    if i >= alignments.len() {
                        alignments.resize(i + 1, Alignment::Unspecified);
                    }
                    alignments[i] = align;
                }
            }
            _ => {}
        }
    }

    (col_widths, alignments)
}

/// Total rendered width for a table row given column widths: sum of widths + separators.
///
/// Each separator is " │ " (3 chars). With N columns: N widths + (N-1) * 3.
fn table_total_width(col_widths: &[usize]) -> usize {
    if col_widths.is_empty() {
        return 0;
    }
    col_widths.iter().sum::<usize>() + (col_widths.len() - 1) * 3
}

/// Convert a minimad `Line` to a ratatui `Line<'static>` for non-table lines.
fn mad_line_to_line(mad_line: &MadLine<'_>, base_style: Style) -> Line<'static> {
    let code_style = base_style.bg(Color::DarkGray);

    match mad_line {
        MadLine::Normal(composite) => {
            let style = if composite.style == CompositeStyle::Code {
                code_style
            } else {
                base_style
            };
            let spans = composite_to_spans(composite, style);
            Line::from(spans)
        }
        MadLine::CodeFence(composite) => {
            let spans = composite_to_spans(composite, code_style);
            Line::from(spans)
        }
        MadLine::TableRow(table_row) => {
            // Fallback: no padding, no alignment (used when called outside from_str context)
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (i, cell) in table_row.cells.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" \u{2502} ", base_style));
                }
                spans.extend(composite_to_spans(cell, base_style));
            }
            Line::from(spans)
        }
        MadLine::TableRule(rule) => {
            // Fallback: estimate width from column count (3 chars per cell + separators)
            let n = rule.cells.len().max(1);
            let width = n * 3 + n.saturating_sub(1) * 3;
            Line::from(Span::styled("\u{2500}".repeat(width), base_style))
        }
        MadLine::HorizontalRule => Line::from(Span::styled("\u{2500}".repeat(40), base_style)),
    }
}

/// Render a section of normal (non-fenced) markdown through minimad, preserving
/// table detection and column alignment.
fn render_normal_section(section: &str, base_style: Style, output: &mut Vec<Line<'static>>) {
    if section.is_empty() {
        return;
    }
    let mad_text = MadText::from(section);
    let mad_lines: Vec<&MadLine<'_>> = mad_text.lines.iter().collect();

    let header_style = base_style.add_modifier(Modifier::BOLD);

    let mut i = 0;
    while i < mad_lines.len() {
        if mad_lines[i].is_table_part() {
            let start = i;
            while i < mad_lines.len() && mad_lines[i].is_table_part() {
                i += 1;
            }
            let block = &mad_lines[start..i];

            let (col_widths, alignments) = compute_table_layout(block);
            let total_width = table_total_width(&col_widths);

            let mut header_rendered = false;

            for mad_line in block {
                match mad_line {
                    MadLine::TableRow(row) => {
                        let cell_style = if !header_rendered {
                            header_rendered = true;
                            header_style
                        } else {
                            base_style
                        };
                        output.push(render_table_row(
                            row,
                            &col_widths,
                            &alignments,
                            cell_style,
                            base_style,
                        ));
                    }
                    MadLine::TableRule(_) => {
                        output.push(Line::from(Span::styled(
                            "\u{2500}".repeat(total_width),
                            base_style,
                        )));
                    }
                    _ => {
                        output.push(mad_line_to_line(mad_line, base_style));
                    }
                }
            }
        } else {
            output.push(mad_line_to_line(mad_lines[i], base_style));
            i += 1;
        }
    }
}

/// Parse markdown into a multi-line ratatui `Text`.
///
/// Each line of markdown is converted to a ratatui `Line` with appropriate styling:
/// - Bold, italic, strikeout use ratatui modifiers
/// - Code spans use cyan foreground
/// - Code fence blocks use a dark background; fenced blocks with a language tag get
///   syntect-based RGB syntax highlighting (theme: base16-ocean.dark)
/// - Table rows are rendered with `│` separators between cells, with cells padded
///   to align columns. The header row (first TableRow) is rendered bold. The
///   separator row (TableRule) determines per-column alignment (left/center/right).
/// - Horizontal rules render as 40 `─` characters
///
/// The `base_style` is applied to all unstyled text.
pub fn from_str(markdown: &str, base_style: Style) -> Text<'static> {
    let mut output_lines: Vec<Line<'static>> = Vec::new();
    let mut normal_buf = String::new();
    let mut in_fence = false;
    let mut fence_lang: Option<&str> = None;
    let mut code_lines: Vec<&str> = Vec::new();

    for raw_line in markdown.lines() {
        let trimmed = raw_line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                // Closing fence: flush accumulated normal text, then render code block
                render_normal_section(&normal_buf, base_style, &mut output_lines);
                normal_buf.clear();
                output_lines.extend(highlight_code_block(&code_lines, fence_lang));
                code_lines.clear();
                in_fence = false;
                fence_lang = None;
            } else {
                // Opening fence: flush accumulated normal text first
                render_normal_section(&normal_buf, base_style, &mut output_lines);
                normal_buf.clear();
                in_fence = true;
                let lang_tag = trimmed.trim_start_matches('`').trim();
                fence_lang = if lang_tag.is_empty() {
                    None
                } else {
                    Some(lang_tag)
                };
            }
        } else if in_fence {
            code_lines.push(raw_line);
        } else {
            normal_buf.push_str(raw_line);
            normal_buf.push('\n');
        }
    }

    // Flush any remaining normal content
    render_normal_section(&normal_buf, base_style, &mut output_lines);

    // Unclosed fence: render as plain code
    if in_fence && !code_lines.is_empty() {
        output_lines.extend(highlight_code_block(&code_lines, None));
    }

    Text::from(output_lines)
}

/// Parse markdown as a single inline line for single-line rendering.
///
/// Takes only the first line of parsed output, making it suitable for rendering
/// messages or other content expected to be a single line.
///
/// The `base_style` is applied to all unstyled text.
pub fn inline(markdown: &str, base_style: Style) -> Line<'static> {
    let mad_text = MadText::from(markdown);
    mad_text
        .lines
        .first()
        .map(|l| mad_line_to_line(l, base_style))
        .unwrap_or_else(|| Line::from(Span::styled(String::new(), base_style)))
}
