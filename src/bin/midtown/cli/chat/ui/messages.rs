//! Message rendering: sender headers, timestamp lines, and content layout.

use std::collections::HashMap;

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use midtown::MessageType;

use super::TIMESTAMP_GUTTER_WIDTH;
use super::styles::{get_sender_color, is_dim_sender, is_system_like_sender};
use super::text::{parse_markdown, wrap_content};

/// Precomputed values shared by message rendering functions.
///
/// Avoids duplicating display name resolution, color lookup, sender visibility,
/// content style, and extra indent calculations across `render_message` and
/// `render_message_with_mermaid`.
pub struct MessageRenderContext {
    pub time: String,
    pub display_from: String,
    pub color: Color,
    pub show_sender: bool,
    pub content_style: Style,
    /// Extra indent beyond the timestamp gutter (2 for action "* ", crosspost prefix len, or 0).
    pub extra_indent: usize,
}

impl MessageRenderContext {
    pub fn new(
        msg: &midtown::Message,
        prev_sender: Option<&str>,
        user_display_name: Option<&str>,
    ) -> Self {
        use chrono::Local;

        let local_time = msg.timestamp.with_timezone(&Local);
        let time = local_time.format("%H:%M").to_string();

        let display_from: String = if msg.from == "user" {
            user_display_name.unwrap_or("user").to_string()
        } else {
            msg.from.clone()
        };

        let color = get_sender_color(&display_from);
        let show_sender = prev_sender.is_none_or(|prev| prev != msg.from);

        let content_style = match msg.message_type {
            MessageType::Action => Style::default().fg(color),
            MessageType::System => Style::default().fg(Color::DarkGray),
            _ if is_dim_sender(&msg.from) => Style::default().fg(Color::DarkGray),
            _ => Style::default().fg(Color::White),
        };

        let extra_indent = if msg.message_type == MessageType::Action {
            2 // "* "
        } else if let Some(ref source_channel) = msg.source_channel {
            2 + 6 + source_channel.chars().count() + 3 // "★ from #channel | "
        } else {
            0
        };

        Self {
            time,
            display_from,
            color,
            show_sender,
            content_style,
            extra_indent,
        }
    }

    /// Content width available after timestamp gutter and extra indent.
    pub fn content_width(&self, width: usize) -> usize {
        width.saturating_sub(TIMESTAMP_GUTTER_WIDTH + self.extra_indent)
    }

    /// Total indent width (timestamp gutter + extra indent).
    pub fn indent_width(&self) -> usize {
        TIMESTAMP_GUTTER_WIDTH + self.extra_indent
    }
}

/// Push the sender header (optional blank line + sender name line) into `lines`.
///
/// The blank-line logic differs slightly for action messages vs. regular messages:
/// - Action messages: blank line unless prev sender was system-like
/// - Regular messages: blank line unless both prev and current are system-like
pub fn push_sender_header(
    msg: &midtown::Message,
    ctx: &MessageRenderContext,
    prev_sender: Option<&str>,
    current_tasks: &HashMap<String, String>,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let add_blank = if msg.message_type == MessageType::Action {
        prev_sender.is_some_and(|prev| !is_system_like_sender(prev))
    } else if let Some(prev) = prev_sender {
        !(is_system_like_sender(prev) && is_system_like_sender(&msg.from))
    } else {
        false
    };
    if add_blank {
        lines.push(Line::from(""));
    }
    let current_task = current_tasks.get(&msg.from.to_lowercase());
    lines.push(build_sender_line(
        &ctx.display_from,
        &msg.message_type,
        ctx.color,
        current_task,
        width,
    ));
}

/// Build the first content line with appropriate timestamp prefix.
///
/// Dispatches to action ("* "), crosspost ("★ from #channel | "), or plain timestamp format.
pub fn build_first_content_line(
    msg: &midtown::Message,
    ctx: &MessageRenderContext,
    content: &str,
) -> Line<'static> {
    if msg.message_type == MessageType::Action {
        build_action_timestamp_line(&ctx.time, content, ctx.color, ctx.content_style)
    } else if let Some(ref source_channel) = msg.source_channel {
        build_crosspost_timestamp_line(&ctx.time, content, source_channel, ctx.content_style)
    } else {
        build_timestamp_line(&ctx.time, content, ctx.content_style)
    }
}

/// Build a continuation line (non-first content line) with proper indentation.
pub fn build_continuation_line(ctx: &MessageRenderContext, content: &str) -> Line<'static> {
    let indent = " ".repeat(ctx.indent_width());
    let mut spans = vec![Span::raw(indent)];
    spans.extend(parse_markdown(content, ctx.content_style));
    Line::from(spans)
}

/// Render a single message into one or more Lines.
///
/// Handles three message variants (action, crosspost, regular) through a unified
/// flow: compute context → optional sender header → timestamp first line →
/// indented continuation lines.
pub fn render_message(
    msg: &midtown::Message,
    width: usize,
    prev_sender: Option<&str>,
    current_tasks: &HashMap<String, String>,
    user_display_name: Option<&str>,
) -> Vec<Line<'static>> {
    let ctx = MessageRenderContext::new(msg, prev_sender, user_display_name);

    let content_width = ctx.content_width(width);
    if content_width == 0 {
        return vec![];
    }

    let content_lines = wrap_content(&msg.content, content_width);
    let mut result = Vec::new();

    if ctx.show_sender {
        push_sender_header(msg, &ctx, prev_sender, current_tasks, width, &mut result);
    }

    for (i, content) in content_lines.iter().enumerate() {
        if i == 0 {
            result.push(build_first_content_line(msg, &ctx, content));
        } else {
            result.push(build_continuation_line(&ctx, content));
        }
    }

    result
}

/// Build a line with the sender name and optionally their current task
///
/// Format: "**name**" or "**name** - Task subject" (task is not bold)
fn build_sender_line(
    display_name: &str,
    message_type: &MessageType,
    color: Color,
    current_task: Option<&String>,
    width: usize,
) -> Line<'static> {
    match message_type {
        MessageType::System => Line::from(vec![Span::styled(
            String::from("<system>"),
            Style::default().fg(Color::DarkGray),
        )]),
        _ => {
            let mut spans = vec![Span::styled(
                display_name.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )];

            // Add current task if available
            if let Some(task) = current_task {
                // Calculate available space for task (width - name - " - ")
                // Use chars().count() for UTF-8 safe length calculation
                let prefix_len = display_name.chars().count() + 3; // " - " = 3 chars
                let available = width.saturating_sub(prefix_len);

                if available > 5 {
                    // Only show if we have reasonable space
                    // Use chars() for UTF-8 safe truncation to avoid panics on multi-byte chars
                    let truncated_task = if task.chars().count() > available {
                        let truncated: String =
                            task.chars().take(available.saturating_sub(1)).collect();
                        format!("{}…", truncated)
                    } else {
                        task.clone()
                    };

                    spans.push(Span::styled(
                        format!(" - {}", truncated_task),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
            }

            Line::from(spans)
        }
    }
}

/// Build a timestamp line with message content: " HH:MM message"
fn build_timestamp_line(time: &str, content: &str, content_style: Style) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {} ", time),
        Style::default().fg(Color::DarkGray),
    )];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

/// Build a timestamp line for action messages: " HH:MM * message"
/// The "*" is in the actor's color to indicate this is an action/status message
fn build_action_timestamp_line(
    time: &str,
    content: &str,
    actor_color: Color,
    content_style: Style,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {} ", time), Style::default().fg(Color::DarkGray)),
        Span::styled("* ", Style::default().fg(actor_color)),
    ];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

/// Build a timestamp line for cross-posted insights: " HH:MM ★ from #channel | message"
/// The "★" and channel attribution are styled distinctly to indicate cross-posting
fn build_crosspost_timestamp_line(
    time: &str,
    content: &str,
    source_channel: &str,
    content_style: Style,
) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!(" {} ", time), Style::default().fg(Color::DarkGray)),
        Span::styled("★ ", Style::default().fg(Color::Yellow)),
        Span::styled(
            format!("from #{} | ", source_channel),
            Style::default().fg(Color::DarkGray),
        ),
    ];
    spans.extend(parse_markdown(content, content_style));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use midtown::Message;

    use super::super::styles::is_system_like_sender;

    #[test]
    fn test_continuation_lines_have_consistent_indent() {
        use chrono::Utc;

        // Create messages from users with different name lengths (3 content lines)
        let short_name_msg = Message {
            id: "1".to_string(),
            from: "a".to_string(),
            content: "line1\nline2\nline3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let long_name_msg = Message {
            id: "2".to_string(),
            from: "lexington".to_string(),
            content: "line1\nline2\nline3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // New layout: name line, then 3 content lines (timestamp + 2 continuations)
        // Total = 4 lines: sender, timestamp+line1, indent+line2, indent+line3
        let short_lines = render_message(&short_name_msg, 80, None, &current_tasks, None);
        let long_lines = render_message(&long_name_msg, 80, None, &current_tasks, None);

        assert_eq!(short_lines.len(), 4, "Expected 4 lines: sender + 3 content");
        assert_eq!(long_lines.len(), 4, "Expected 4 lines: sender + 3 content");

        // Extract the indent from continuation lines (3rd and 4th line, first span)
        let short_indent = &short_lines[2].spans[0].content;
        let long_indent = &long_lines[2].spans[0].content;

        // Continuation lines should have the SAME indent (7 spaces) regardless of username length
        assert_eq!(
            short_indent.len(),
            TIMESTAMP_GUTTER_WIDTH,
            "Indent should be {} chars, got {}",
            TIMESTAMP_GUTTER_WIDTH,
            short_indent.len()
        );
        assert_eq!(
            short_indent.len(),
            long_indent.len(),
            "Continuation indent should be consistent"
        );
    }

    #[test]
    fn test_consecutive_messages_skip_sender_name() {
        use chrono::Utc;

        let msg1 = Message {
            id: "1".to_string(),
            from: "columbus".to_string(),
            content: "first message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let msg2 = Message {
            id: "2".to_string(),
            from: "columbus".to_string(),
            content: "second message".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // First message (no previous sender) - shows sender line + timestamp line
        let lines1 = render_message(&msg1, 80, None, &current_tasks, None);
        assert_eq!(lines1.len(), 2); // sender line + timestamp+content line

        // Second message from same sender - shows only timestamp + content (no sender)
        let lines2 = render_message(&msg2, 80, Some("columbus"), &current_tasks, None);
        assert_eq!(lines2.len(), 1); // just timestamp + content

        // Different sender - shows blank line + sender line + timestamp line
        let lines3 = render_message(&msg2, 80, Some("lexington"), &current_tasks, None);
        assert_eq!(lines3.len(), 3); // blank + sender line + timestamp+content line

        // Verify first message has sender name on first line
        let first_line_content: String =
            lines1[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(first_line_content.contains("columbus"));

        // Verify same-sender message has timestamp, not actor
        let same_sender_content: String =
            lines2[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!same_sender_content.contains("columbus"));
        assert!(same_sender_content.contains(":")); // Has timestamp like "10:12"
    }

    #[test]
    fn test_action_message_format() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "completed task 3".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Action,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // Action messages now follow standard format:
        // Line 0: actor name (when sender changes)
        // Line 1: " HH:MM * message" with * in actor color
        let lines = render_message(&msg, 80, None, &current_tasks, None);
        assert_eq!(lines.len(), 2, "Expected 2 lines: actor name + message");

        // First line should be actor name
        let first_line: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first_line.contains("park"),
            "First line should contain actor name, got: {}",
            first_line
        );

        // Second line should have format " HH:MM * message"
        let second_line: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            second_line.contains("* "),
            "Message line should contain '* ', got: {}",
            second_line
        );
        assert!(
            second_line.contains("completed task 3"),
            "Message line should contain content, got: {}",
            second_line
        );
        assert!(
            second_line.contains(":"),
            "Message line should contain timestamp, got: {}",
            second_line
        );

        // Verify the spans on the message line: timestamp, "* ", content
        assert!(
            lines[1].spans.len() >= 3,
            "Expected at least 3 spans: timestamp, '* ', content"
        );
        // First span should be timestamp " HH:MM "
        assert!(
            lines[1].spans[0].content.contains(":"),
            "First span should be timestamp"
        );
        // Second span should be "* "
        assert_eq!(
            lines[1].spans[1].content, "* ",
            "Second span should be '* '"
        );
    }

    #[test]
    fn test_system_message_format() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "system".to_string(),
            content: "Session started".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::System,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let current_tasks = HashMap::new();

        // System messages now render through standard path: sender line + timestamp line
        let lines = render_message(&msg, 80, None, &current_tasks, None);
        assert_eq!(lines.len(), 2); // sender line + content line

        // First line is the sender name (<system>)
        let sender: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(sender, "<system>");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::DarkGray));

        // Second line has timestamp + content in DarkGray
        let content: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(content.contains("Session started"));
    }

    #[test]
    fn test_chat_shows_newest_messages_when_content_exceeds_height() {
        use chrono::Utc;

        let messages: Vec<Message> = (0..10)
            .map(|i| Message {
                id: i.to_string(),
                from: format!("user{}", i),
                content: format!("message content {}", i),
                timestamp: Utc::now(),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
            })
            .collect();

        let current_tasks = HashMap::new();

        let mut all_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let msg_lines = render_message(msg, 80, prev_sender, &current_tasks, None);
            all_lines.extend(msg_lines);
            prev_sender = Some(&msg.from);
        }

        assert!(
            all_lines.len() > 10,
            "Expected more than 10 lines, got {}",
            all_lines.len()
        );

        let last_line: String = all_lines
            .last()
            .unwrap()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            last_line.contains("message content 9"),
            "Last line should contain newest message, got: {}",
            last_line
        );

        let visible_height = 10;

        let buggy_visible: Vec<_> = all_lines.iter().take(visible_height).collect();
        let fixed_visible: Vec<_> = all_lines
            .iter()
            .skip(all_lines.len().saturating_sub(visible_height))
            .collect();

        let buggy_content: String = buggy_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        let fixed_content: String = fixed_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        assert!(
            buggy_content.contains("message content 0"),
            "Buggy version should contain oldest message"
        );
        assert!(
            !buggy_content.contains("message content 9"),
            "Buggy version should NOT contain newest message"
        );
        assert!(
            fixed_content.contains("message content 9"),
            "Fixed version should contain newest message"
        );
    }

    #[test]
    fn test_smooth_scrolling_always_shows_last_lines() {
        use chrono::Utc;

        let messages: Vec<Message> = (0..20)
            .map(|i| Message {
                id: i.to_string(),
                from: format!("user{}", i),
                content: format!("message content {}", i),
                timestamp: Utc::now(),
                message_type: MessageType::Text,
                channel: None,
                source_channel: None,
                session_id: None,
            })
            .collect();

        let current_tasks = HashMap::new();

        let at_bottom_messages = &messages[10..20];
        let mut at_bottom_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in at_bottom_messages {
            at_bottom_lines.extend(render_message(msg, 80, prev_sender, &current_tasks, None));
            prev_sender = Some(&msg.from);
        }

        let scrolled_one_messages = &messages[9..19];
        let mut scrolled_one_lines: Vec<Line> = Vec::new();
        let mut prev_sender: Option<&str> = None;
        for msg in scrolled_one_messages {
            scrolled_one_lines.extend(render_message(msg, 80, prev_sender, &current_tasks, None));
            prev_sender = Some(&msg.from);
        }

        let visible_height = 10;

        let bottom_visible: Vec<_> = if at_bottom_lines.len() > visible_height {
            at_bottom_lines
                .iter()
                .skip(at_bottom_lines.len() - visible_height)
                .collect()
        } else {
            at_bottom_lines.iter().collect()
        };

        let scrolled_visible: Vec<_> = if scrolled_one_lines.len() > visible_height {
            scrolled_one_lines
                .iter()
                .skip(scrolled_one_lines.len() - visible_height)
                .collect()
        } else {
            scrolled_one_lines.iter().collect()
        };

        let bottom_content: String = bottom_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();
        let scrolled_content: String = scrolled_visible
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect();

        assert!(
            bottom_content.contains("message content 19")
                || bottom_content.contains("message content 18"),
            "At bottom should show newest messages, got: {}",
            bottom_content
        );

        assert!(
            scrolled_content.contains("message content 18")
                || scrolled_content.contains("message content 17"),
            "Scrolled by 1 should show slightly older messages, got: {}",
            scrolled_content
        );

        let bottom_has_19 = bottom_content.contains("message content 19");
        let scrolled_has_19 = scrolled_content.contains("message content 19");

        assert!(
            bottom_has_19,
            "At bottom should show message 19 (newest in 10..20 range), got: {}",
            bottom_content
        );

        assert!(
            !scrolled_has_19,
            "Scrolled view should NOT show message 19 (not in 9..19 range), got: {}",
            scrolled_content
        );

        assert!(
            scrolled_content.contains("message content 18"),
            "Scrolled view should show message 18 (near end of 9..19 range), got: {}",
            scrolled_content
        );
    }

    #[test]
    fn test_system_like_messages_grouped_together() {
        use chrono::Utc;

        fn count_blank_lines(lines: &[Line]) -> usize {
            lines
                .iter()
                .filter(|l| l.spans.iter().all(|s| s.content.is_empty()))
                .count()
        }

        let current_tasks = HashMap::new();

        // Test 1: Regular -> daemon (system-like) should add blank before daemon
        let daemon_msg = Message {
            id: "2".to_string(),
            from: "daemon".to_string(),
            content: "Called in coworker".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let daemon_lines = render_message(&daemon_msg, 80, Some("madison"), &current_tasks, None);
        assert!(
            count_blank_lines(&daemon_lines) == 1,
            "Should have blank line before daemon message after regular sender"
        );

        // Test 2: daemon -> daemon (both system-like) should NOT add blank
        let daemon_msg2 = Message {
            id: "3".to_string(),
            from: "daemon".to_string(),
            content: "Another daemon event".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let daemon_lines2 = render_message(&daemon_msg2, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&daemon_lines2),
            0,
            "Should NOT have blank line between consecutive daemon messages"
        );

        // Test 3: daemon -> github should add blank (github is not system-like)
        let github_msg = Message {
            id: "4".to_string(),
            from: "github".to_string(),
            content: "Check passed".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let github_lines = render_message(&github_msg, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&github_lines),
            1,
            "Should have blank line between daemon and github messages"
        );

        // Test 4: daemon -> regular (park) SHOULD add blank line
        let park_msg = Message {
            id: "5".to_string(),
            from: "park".to_string(),
            content: "back to work".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let park_lines = render_message(&park_msg, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&park_lines),
            1,
            "Should have blank line when transitioning from system-like to regular"
        );

        // Test 5: Verify is_system_like_sender helper
        assert!(is_system_like_sender("daemon"));
        assert!(!is_system_like_sender("github"));
        assert!(is_system_like_sender("system"));
        assert!(is_system_like_sender("DAEMON"));
        assert!(!is_system_like_sender("madison"));
        assert!(!is_system_like_sender("park"));
    }

    #[test]
    fn test_github_messages_have_blank_line_spacing() {
        use chrono::Utc;

        fn count_blank_lines(lines: &[Line]) -> usize {
            lines
                .iter()
                .filter(|l| l.spans.iter().all(|s| s.content.is_empty()))
                .count()
        }

        let current_tasks = HashMap::new();

        let github_msg = Message {
            id: "1".to_string(),
            from: "github".to_string(),
            content: "Check passed".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let github_lines = render_message(&github_msg, 80, Some("daemon"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&github_lines),
            1,
            "Should have blank line between daemon and github messages"
        );

        let daemon_msg = Message {
            id: "2".to_string(),
            from: "daemon".to_string(),
            content: "Called in coworker".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let daemon_lines = render_message(&daemon_msg, 80, Some("github"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&daemon_lines),
            1,
            "Should have blank line between github and daemon messages"
        );

        let github_lines2 = render_message(&github_msg, 80, Some("park"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&github_lines2),
            1,
            "Should have blank line between coworker and github messages"
        );

        let park_msg = Message {
            id: "3".to_string(),
            from: "park".to_string(),
            content: "working".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };
        let park_lines = render_message(&park_msg, 80, Some("github"), &current_tasks, None);
        assert_eq!(
            count_blank_lines(&park_lines),
            1,
            "Should have blank line between github and coworker messages"
        );

        // Test: github content should still be DarkGray
        let github_lines3 = render_message(&github_msg, 80, None, &current_tasks, None);
        assert!(
            github_lines3.len() >= 2,
            "Github message should have sender + content lines"
        );
        let content_line = &github_lines3[1];
        let has_dark_gray_content = content_line
            .spans
            .iter()
            .any(|s| s.style.fg == Some(Color::DarkGray) && !s.content.contains(':'));
        assert!(
            has_dark_gray_content,
            "Github message content should be DarkGray"
        );
    }

    #[test]
    fn test_sender_line_shows_current_task() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "working on feature".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let mut current_tasks = HashMap::new();
        current_tasks.insert(
            "park".to_string(),
            "Fix chat TUI timestamp formatting".to_string(),
        );

        let lines = render_message(&msg, 80, None, &current_tasks, None);

        let first_line_content: String =
            lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first_line_content.contains("park"),
            "Should contain sender name"
        );
        assert!(
            first_line_content.contains("Fix chat TUI timestamp formatting"),
            "Should contain current task"
        );
        assert!(
            first_line_content.contains(" - "),
            "Should have separator between name and task"
        );

        let empty_tasks = HashMap::new();
        let lines_no_task = render_message(&msg, 80, None, &empty_tasks, None);
        let first_line_no_task: String = lines_no_task[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            first_line_no_task.contains("park"),
            "Should contain sender name"
        );
        assert!(
            !first_line_no_task.contains(" - "),
            "Should NOT have task separator when no task"
        );
    }

    #[test]
    fn test_sender_line_truncates_long_task() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: "test".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let mut current_tasks = HashMap::new();
        current_tasks.insert(
            "park".to_string(),
            "This is a very long task description that should be truncated".to_string(),
        );

        let lines = render_message(&msg, 30, None, &current_tasks, None);
        let first_line_content: String =
            lines[0].spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(
            first_line_content.contains("…") || first_line_content.len() <= 30,
            "Long task should be truncated"
        );
    }

    #[test]
    fn test_sender_line_case_insensitive_lookup() {
        use chrono::Utc;

        let msg = Message {
            id: "1".to_string(),
            from: "Park".to_string(),
            content: "test".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        };

        let mut current_tasks = HashMap::new();
        current_tasks.insert("park".to_string(), "Fix something".to_string());

        let lines = render_message(&msg, 80, None, &current_tasks, None);
        let first_line_content: String =
            lines[0].spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(
            first_line_content.contains("Fix something"),
            "Should find task with case-insensitive lookup"
        );
    }

    #[test]
    fn test_render_crosspost_message() {
        let mut msg = test_message("The tower::Layer stack composes auth providers independently.");
        msg.source_channel = Some("auth-refactor".to_string());

        let current_tasks = HashMap::new();

        let lines = render_message(&msg, 80, None, &current_tasks, None);

        assert!(
            lines.len() >= 2,
            "Expected at least 2 lines, got {}",
            lines.len()
        );

        let content_line = &lines[1];
        let spans = &content_line.spans;

        let star_span = spans.iter().find(|s| s.content.contains('★'));
        assert!(
            star_span.is_some(),
            "Expected to find ★ prefix in content line"
        );

        let channel_span = spans.iter().find(|s| s.content.contains("#auth-refactor"));
        assert!(
            channel_span.is_some(),
            "Expected to find #auth-refactor channel attribution"
        );
    }

    #[test]
    fn test_render_crosspost_utf8_channel_name() {
        let long_content = "First line of content that is long enough to wrap onto a second continuation line for testing alignment";
        let mut msg = test_message(long_content);
        msg.source_channel = Some("design-café".to_string());

        let current_tasks = HashMap::new();
        let width = 60;

        let lines = render_message(&msg, width, None, &current_tasks, None);

        assert!(
            lines.len() >= 3,
            "Expected at least 3 lines (sender + 2 content), got {}",
            lines.len()
        );

        let continuation_line = &lines[2];
        let first_span_content = &continuation_line.spans[0].content;

        // "design-café" has 11 chars, so prefix_len = 2+6+11+3 = 22
        let expected_indent = 7 + 22; // TIMESTAMP_GUTTER_WIDTH + prefix_len
        assert_eq!(
            first_span_content.len(),
            expected_indent,
            "Continuation indent should be {} spaces, got {} (possible byte/char mismatch)",
            expected_indent,
            first_span_content.len()
        );
    }

    /// Helper to create a test Message
    fn test_message(content: &str) -> Message {
        Message {
            id: "1".to_string(),
            from: "park".to_string(),
            content: content.to_string(),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
        }
    }
}
