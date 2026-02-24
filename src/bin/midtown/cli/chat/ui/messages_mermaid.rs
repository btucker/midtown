//! Mermaid diagram rendering within chat messages.

use std::collections::HashMap;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::cli::chat::mermaid::{ContentSegment, MermaidCache};

use super::highlight::highlight_code;
use super::messages::{
    MessageRenderContext, build_continuation_line, build_first_content_line, push_sender_header,
    render_content_lines,
};
use super::text::wrap_content;

/// Render a message that contains mermaid code fences.
///
/// Splits the message content into text and mermaid segments, rendering
/// text normally and inserting selectable placeholders for mermaid diagrams.
/// Each diagram gets a numbered label that the user can select to open
/// in a fullscreen viewer.
#[allow(clippy::too_many_arguments)]
pub fn render_message_with_mermaid(
    msg: &midtown::Message,
    segments: &[ContentSegment],
    width: usize,
    prev_sender: Option<&str>,
    current_tasks: &HashMap<String, String>,
    user_display_name: Option<&str>,
    channel_lead_names: &[String],
    mermaid_cache: &MermaidCache,
    lines: &mut Vec<Line<'static>>,
    diagram_sources: &mut Vec<String>,
    mermaid_to_render: &mut Vec<String>,
    use_light_theme: bool,
) {
    let ctx = MessageRenderContext::new(msg, prev_sender, user_display_name, channel_lead_names);

    let content_width = ctx.content_width(width);
    if content_width == 0 {
        return;
    }

    if ctx.show_sender {
        push_sender_header(msg, &ctx, prev_sender, current_tasks, width, lines);
    }

    let mut is_first_content_line = true;

    for segment in segments {
        match segment {
            ContentSegment::Text(text) => {
                let content_lines = wrap_content(text, content_width);
                for content in &content_lines {
                    let parsed = minimad_ratatui::inline(content, ctx.content_style);
                    if is_first_content_line {
                        lines.push(build_first_content_line(msg, &ctx, parsed));
                        is_first_content_line = false;
                    } else {
                        lines.push(build_continuation_line(&ctx, parsed));
                    }
                }
            }
            ContentSegment::Mermaid(source) => {
                render_mermaid_segment(
                    source,
                    &ctx,
                    content_width,
                    mermaid_cache,
                    lines,
                    diagram_sources,
                    mermaid_to_render,
                );
                is_first_content_line = false;
            }
            ContentSegment::CodeBlock { language, source } => {
                render_code_block_segment(
                    msg,
                    language,
                    source,
                    &ctx,
                    content_width,
                    &mut is_first_content_line,
                    lines,
                    use_light_theme,
                );
            }
        }
    }
}

/// Render a single mermaid diagram segment (cached, pending, or unqueued).
fn render_mermaid_segment(
    source: &str,
    ctx: &MessageRenderContext,
    content_width: usize,
    mermaid_cache: &MermaidCache,
    lines: &mut Vec<Line<'static>>,
    diagram_sources: &mut Vec<String>,
    mermaid_to_render: &mut Vec<String>,
) {
    let diagram_type = source
        .lines()
        .next()
        .unwrap_or("diagram")
        .split_whitespace()
        .next()
        .unwrap_or("diagram");

    let indent = " ".repeat(ctx.indent_width());

    if let Some(diagram) = mermaid_cache.get_cached(source) {
        // Diagram is ready: show inline ASCII art with separators
        let diagram_num = diagram_sources.len() + 1;
        diagram_sources.push(source.to_string());

        // Top separator
        let top_sep = format!("{}--- {} ---", indent, diagram_type);
        lines.push(Line::from(Span::styled(
            top_sep,
            Style::default().fg(Color::DarkGray),
        )));

        // ASCII art lines (cyan, indented, truncated to content_width)
        for art_line in diagram.ascii_art.lines() {
            let truncated: String = art_line.chars().take(content_width).collect();
            lines.push(Line::from(Span::styled(
                format!("{}{}", indent, truncated),
                Style::default().fg(Color::Cyan),
            )));
        }

        // Bottom separator with browser hint
        let bottom_sep = if diagram_num <= 9 {
            format!("{}--- press {} to open in browser ---", indent, diagram_num)
        } else {
            format!("{}--- {} ---", indent, diagram_type)
        };
        lines.push(Line::from(Span::styled(
            bottom_sep,
            Style::default().fg(Color::DarkGray),
        )));
    } else if mermaid_cache.is_pending(source) {
        // Rendering in progress
        let placeholder = format!("{}[rendering {}...]", indent, diagram_type);
        lines.push(Line::from(Span::styled(
            placeholder,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        // Not yet queued: show placeholder and queue for rendering
        let placeholder = format!("{}[{} diagram]", indent, diagram_type);
        lines.push(Line::from(Span::styled(
            placeholder,
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
        mermaid_to_render.push(source.to_string());
    }
}

/// Render a fenced code block with syntax highlighting.
///
/// Emits an optional bare language label (e.g. `rust`) in dim color before the
/// code lines, with no bottom border. Handles the timestamp gutter for the first
/// line of the message when `is_first_content_line` is true (sets it to false
/// after emitting the first line). Long code lines are truncated to
/// `content_width` to prevent overflow on narrow terminals.
#[allow(clippy::too_many_arguments)]
fn render_code_block_segment(
    msg: &midtown::Message,
    language: &str,
    source: &str,
    ctx: &MessageRenderContext,
    content_width: usize,
    is_first_content_line: &mut bool,
    lines: &mut Vec<Line<'static>>,
    use_light_theme: bool,
) {
    let indent = " ".repeat(ctx.indent_width());

    // Language label: bare name in dim color, no indent (e.g. "rust").
    // Omitted entirely when language is empty.
    if !language.is_empty() {
        let label_span = Span::styled(language.to_string(), Style::default().fg(Color::DarkGray));
        let label_line = Line::from(vec![label_span]);
        if *is_first_content_line {
            lines.push(build_first_content_line(msg, ctx, label_line));
            *is_first_content_line = false;
        } else {
            lines.push(build_continuation_line(ctx, label_line));
        }
    }

    // Highlighted code lines, truncated to content_width
    let highlighted = highlight_code(language, source, use_light_theme);
    for hl_line in highlighted {
        let mut truncated_spans = Vec::new();
        let mut remaining = content_width;
        for span in hl_line.spans {
            if remaining == 0 {
                break;
            }
            let text: String = span.content.chars().take(remaining).collect();
            remaining = remaining.saturating_sub(span.content.chars().count());
            truncated_spans.push(Span::styled(text, span.style));
        }
        if *is_first_content_line {
            // build_first_content_line provides the timestamp gutter — don't
            // prepend indent here or the first line ends up double-indented.
            lines.push(build_first_content_line(
                msg,
                ctx,
                Line::from(truncated_spans),
            ));
            *is_first_content_line = false;
        } else {
            let mut spans = vec![Span::raw(indent.clone())];
            spans.extend(truncated_spans);
            lines.push(Line::from(spans));
        }
    }
}

/// Render content segments as flat lines with no message framing.
///
/// Used by the thread header to render the parent message content without
/// sender prefix or continuation-line indentation. Handles text, code blocks,
/// and mermaid placeholders.
pub fn render_header_content_segments(
    segments: &[ContentSegment],
    content_width: usize,
    content_style: Style,
    use_light_theme: bool,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for segment in segments {
        match segment {
            ContentSegment::Text(text) => {
                // Use render_content_lines to preserve block-level table detection:
                // text segments can contain markdown tables (|...|), and the line-by-line
                // table parser in render_content_lines must see them as a contiguous block.
                lines.extend(render_content_lines(text, content_width, content_style));
            }
            ContentSegment::CodeBlock { language, source } => {
                // Language label: bare name in dim color (e.g. "rust").
                // Omitted entirely when language is empty, matching render_code_block_segment.
                if !language.is_empty() {
                    lines.push(Line::from(Span::styled(
                        language.to_string(),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                let highlighted = highlight_code(language, source, use_light_theme);
                for hl_line in highlighted {
                    let mut truncated_spans = Vec::new();
                    let mut remaining = content_width;
                    for span in hl_line.spans {
                        if remaining == 0 {
                            break;
                        }
                        let text: String = span.content.chars().take(remaining).collect();
                        remaining = remaining.saturating_sub(span.content.chars().count());
                        truncated_spans.push(Span::styled(text, span.style));
                    }
                    lines.push(Line::from(truncated_spans));
                }
            }
            ContentSegment::Mermaid(_) => {
                lines.push(Line::from(Span::styled(
                    "[diagram]",
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }
    }

    lines
}

#[path = "messages_mermaid_tests.rs"]
#[cfg(test)]
mod tests;
