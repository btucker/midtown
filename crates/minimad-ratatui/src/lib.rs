//! Bridge between the minimad markdown parser and ratatui types.
//!
//! Provides two public functions:
//! - [`from_str`]: Parse markdown into a multi-line `ratatui::text::Text`
//! - [`inline`]: Parse markdown as a single inline line for single-line rendering

use minimad::{CompositeStyle, Line as MadLine, Text as MadText};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

#[path = "tests.rs"]
#[cfg(test)]
mod tests;

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

/// Convert a minimad `Line` to a ratatui `Line<'static>`.
///
/// Line variants handled:
/// - `Normal`: use base_style
/// - `CodeFence`: dark background style
/// - `TableRow`: cells joined with `│` separators
/// - `TableRule`: horizontal rule with `─` characters
/// - `HorizontalRule`: 40 `─` characters
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
            let mut spans: Vec<Span<'static>> = Vec::new();
            for (i, cell) in table_row.cells.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::styled(" \u{2502} ", base_style)); // │
                }
                spans.extend(composite_to_spans(cell, base_style));
            }
            Line::from(spans)
        }
        MadLine::TableRule(_) => {
            Line::from(Span::styled("\u{2500}".repeat(40), base_style)) // ─
        }
        MadLine::HorizontalRule => {
            Line::from(Span::styled("\u{2500}".repeat(40), base_style)) // ─
        }
    }
}

/// Parse markdown into a multi-line ratatui `Text`.
///
/// Each line of markdown is converted to a ratatui `Line` with appropriate styling:
/// - Bold, italic, strikeout use ratatui modifiers
/// - Code spans use cyan foreground
/// - Code fence blocks use a dark background
/// - Table rows are rendered with `│` separators between cells
/// - Horizontal rules render as 40 `─` characters
///
/// The `base_style` is applied to all unstyled text.
pub fn from_str(markdown: &str, base_style: Style) -> Text<'static> {
    let mad_text = MadText::from(markdown);
    let lines: Vec<Line<'static>> = mad_text
        .lines
        .iter()
        .map(|l| mad_line_to_line(l, base_style))
        .collect();
    Text::from(lines)
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
