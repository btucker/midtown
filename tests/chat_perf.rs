//! Performance tests for the chat TUI.
//!
//! These tests measure rendering performance and ensure the chat TUI can handle
//! large numbers of messages without degradation. Each test has clear pass/fail
//! thresholds and can run in CI.
//!
//! Performance targets:
//! - Single message render: < 1ms
//! - 100 messages render: < 50ms
//! - 1000 messages render: < 500ms
//! - Scroll operation: < 1ms
//!
//! Run with: `cargo test --test chat_perf`

use chrono::Utc;
use midtown::{Message, MessageType};
use std::collections::HashMap;
use std::time::Instant;

// Re-create the core rendering functions for testing without importing private modules.
// This mirrors the logic in src/bin/midtown/cli/chat/ui.rs.

/// Timestamp gutter width: " HH:MM " = 7 chars
const TIMESTAMP_GUTTER_WIDTH: usize = 7;

/// Check if sender is system-like (for grouping)
fn is_system_like_sender(sender: &str) -> bool {
    matches!(sender.to_lowercase().as_str(), "daemon" | "system")
}

/// Wrap a single line of text to fit within the given width.
fn wrap_line(text: &str, width: usize) -> Vec<&str> {
    if text.is_empty() {
        return vec![""];
    }
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

        let byte_pos = remaining
            .char_indices()
            .nth(width)
            .map(|(i, _)| i)
            .unwrap_or(remaining.len());

        let break_at = remaining[..byte_pos]
            .rfind(' ')
            .map(|pos| pos + 1)
            .unwrap_or(byte_pos);

        let (line, rest) = remaining.split_at(break_at);
        result.push(line.trim_end());
        remaining = rest.trim_start();
    }

    result
}

/// Wrap content text into lines that fit the given width.
fn wrap_content(content: &str, width: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in content.split('\n') {
        let wrapped = wrap_line(line, width);
        for w in wrapped {
            result.push(w.to_string());
        }
    }
    result
}

/// Simulated message rendering (mirrors render_message logic).
/// Returns the number of output lines for the message.
///
/// This mirrors the actual render_message() function in ui.rs, including
/// special handling for Action messages which have a "* " prefix.
fn render_message_line_count(
    msg: &Message,
    width: usize,
    prev_sender: Option<&str>,
    _current_tasks: &HashMap<String, String>,
) -> usize {
    let show_sender = prev_sender.is_none_or(|prev| prev != msg.from);

    // Action messages have a "* " prefix that consumes 2 extra characters
    // See ui.rs render_message() for the actual implementation
    let extra_prefix = if msg.message_type == MessageType::Action {
        2 // "* " prefix
    } else {
        0
    };

    // Content width after timestamp gutter (and action prefix if applicable)
    let content_width = width.saturating_sub(TIMESTAMP_GUTTER_WIDTH + extra_prefix);
    if content_width == 0 {
        return 0;
    }

    let content_lines = wrap_content(&msg.content, content_width);
    let mut line_count = content_lines.len();

    if show_sender {
        // Add blank line before new sender (except between system-like senders)
        if let Some(prev) = prev_sender
            && !(is_system_like_sender(prev) && is_system_like_sender(&msg.from))
        {
            line_count += 1;
        }
        // Sender name line
        line_count += 1;
    }

    line_count
}

/// Generate test messages with varying content lengths.
fn generate_messages(count: usize) -> Vec<Message> {
    let senders = [
        "park",
        "columbus",
        "lexington",
        "madison",
        "broadway",
        "amsterdam",
    ];
    let contents = [
        "Short message",
        "This is a medium length message that has some content",
        "This is a longer message that contains more text and will likely need to be wrapped when rendered in a narrow terminal window",
        "A message with **markdown** and `code` formatting that needs to be parsed",
        "Multi-line message\nwith embedded\nnewlines that need handling",
    ];

    (0..count)
        .map(|i| Message {
            id: i.to_string(),
            from: senders[i % senders.len()].to_string(),
            content: contents[i % contents.len()].to_string(),
            timestamp: Utc::now(),
            message_type: if i % 10 == 0 {
                MessageType::Action
            } else {
                MessageType::Text
            },
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        })
        .collect()
}

/// Generate messages with long content to stress wrapping.
fn generate_long_messages(count: usize) -> Vec<Message> {
    let senders = ["park", "columbus"];
    let long_content = "This is a very long message that will definitely need multiple lines of wrapping. It contains lots of text that simulates a detailed explanation or a long status update from a coworker. The rendering system needs to handle this efficiently without introducing significant latency. ".repeat(5);

    (0..count)
        .map(|i| Message {
            id: i.to_string(),
            from: senders[i % senders.len()].to_string(),
            content: long_content.clone(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        })
        .collect()
}

// =============================================================================
// Message Rendering Performance Tests
// =============================================================================

/// Test that rendering a single message is fast (< 1ms).
#[test]
fn test_single_message_render_latency() {
    let msg = Message {
        id: "1".to_string(),
        from: "park".to_string(),
        content: "This is a test message with **markdown** and `code`".to_string(),
        timestamp: Utc::now(),
        message_type: MessageType::Text,
        channel: None,
        source_channel: None,
        session_id: None,
        thread_parent_id: None,
    };

    let current_tasks = HashMap::new();
    let width = 80;

    // Warm up
    for _ in 0..100 {
        let _ = render_message_line_count(&msg, width, None, &current_tasks);
    }

    // Measure
    let iterations = 1000;
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = render_message_line_count(&msg, width, None, &current_tasks);
    }
    let elapsed = start.elapsed();

    let avg_micros = elapsed.as_micros() as f64 / iterations as f64;
    let avg_millis = avg_micros / 1000.0;

    println!(
        "Single message render: {:.3}ms average ({} iterations)",
        avg_millis, iterations
    );

    // Threshold: single message should render in < 1ms
    assert!(
        avg_millis < 1.0,
        "Single message render too slow: {:.3}ms (threshold: 1ms)",
        avg_millis
    );
}

/// Test that rendering 100 messages is fast (< 50ms total).
#[test]
fn test_100_messages_render_latency() {
    let messages = generate_messages(100);
    let current_tasks = HashMap::new();
    let width = 80;

    // Warm up
    for _ in 0..10 {
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let _ = render_message_line_count(msg, width, prev_sender, &current_tasks);
            prev_sender = Some(&msg.from);
        }
    }

    // Measure
    let iterations = 100;
    let start = Instant::now();
    for _ in 0..iterations {
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let _ = render_message_line_count(msg, width, prev_sender, &current_tasks);
            prev_sender = Some(&msg.from);
        }
    }
    let elapsed = start.elapsed();

    let avg_millis = elapsed.as_millis() as f64 / iterations as f64;

    println!(
        "100 messages render: {:.2}ms average ({} iterations)",
        avg_millis, iterations
    );

    // Threshold: 100 messages should render in < 50ms
    assert!(
        avg_millis < 50.0,
        "100 messages render too slow: {:.2}ms (threshold: 50ms)",
        avg_millis
    );
}

/// Test that rendering 1000 messages is acceptable (< 500ms total).
#[test]
fn test_1000_messages_render_latency() {
    let messages = generate_messages(1000);
    let current_tasks = HashMap::new();
    let width = 80;

    // Measure
    let iterations = 10;
    let start = Instant::now();
    for _ in 0..iterations {
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let _ = render_message_line_count(msg, width, prev_sender, &current_tasks);
            prev_sender = Some(&msg.from);
        }
    }
    let elapsed = start.elapsed();

    let avg_millis = elapsed.as_millis() as f64 / iterations as f64;

    println!(
        "1000 messages render: {:.2}ms average ({} iterations)",
        avg_millis, iterations
    );

    // Threshold: 1000 messages should render in < 500ms
    assert!(
        avg_millis < 500.0,
        "1000 messages render too slow: {:.2}ms (threshold: 500ms)",
        avg_millis
    );
}

// =============================================================================
// Text Wrapping Performance Tests
// =============================================================================

/// Test that wrap_line handles long text efficiently.
#[test]
fn test_wrap_line_performance() {
    let long_text = "a".repeat(10000);
    let width = 80;

    // Warm up
    for _ in 0..10 {
        let _ = wrap_line(&long_text, width);
    }

    // Measure
    let iterations = 100;
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = wrap_line(&long_text, width);
    }
    let elapsed = start.elapsed();

    let avg_micros = elapsed.as_micros() as f64 / iterations as f64;
    let avg_millis = avg_micros / 1000.0;

    println!(
        "wrap_line (10000 chars): {:.3}ms average ({} iterations)",
        avg_millis, iterations
    );

    // Threshold: wrapping 10000 chars should take < 5ms
    assert!(
        avg_millis < 5.0,
        "wrap_line too slow for long text: {:.3}ms (threshold: 5ms)",
        avg_millis
    );
}

/// Test wrap_content with realistic message content.
#[test]
fn test_wrap_content_performance() {
    let content = "This is a message with multiple paragraphs.\n\nIt contains **markdown** formatting and `code spans` that need to be preserved.\n\nThe wrapping should handle all of this efficiently.\n".repeat(10);
    let width = 80;

    // Warm up
    for _ in 0..10 {
        let _ = wrap_content(&content, width);
    }

    // Measure
    let iterations = 1000;
    let start = Instant::now();
    for _ in 0..iterations {
        let _ = wrap_content(&content, width);
    }
    let elapsed = start.elapsed();

    let avg_micros = elapsed.as_micros() as f64 / iterations as f64;
    let avg_millis = avg_micros / 1000.0;

    println!(
        "wrap_content (multi-paragraph): {:.3}ms average ({} iterations)",
        avg_millis, iterations
    );

    // Threshold: wrapping realistic content should take < 1ms
    assert!(
        avg_millis < 1.0,
        "wrap_content too slow: {:.3}ms (threshold: 1ms)",
        avg_millis
    );
}

// =============================================================================
// Scroll Performance Tests
// =============================================================================

/// Simulate visible_messages slice calculation (mirrors App::visible_messages).
fn visible_messages(
    messages: &[Message],
    scroll_offset: usize,
    visible_height: usize,
) -> &[Message] {
    let total = messages.len();
    if total == 0 {
        return &[];
    }

    let end = total.saturating_sub(scroll_offset);
    let start = end.saturating_sub(visible_height);

    &messages[start..end]
}

/// Test that scroll operations are instantaneous.
#[test]
fn test_scroll_operation_latency() {
    let messages = generate_messages(10000);
    let visible_height = 50;

    // Warm up
    for offset in 0..100 {
        let _ = visible_messages(&messages, offset, visible_height);
    }

    // Measure scroll operations at various positions
    let offsets = [0, 100, 500, 1000, 5000, 9000];
    let iterations = 10000;

    let start = Instant::now();
    for _ in 0..iterations {
        for &offset in &offsets {
            let _ = visible_messages(&messages, offset, visible_height);
        }
    }
    let elapsed = start.elapsed();

    let total_ops = iterations * offsets.len();
    let avg_nanos = elapsed.as_nanos() as f64 / total_ops as f64;
    let avg_micros = avg_nanos / 1000.0;

    println!(
        "Scroll operation: {:.3}us average ({} operations)",
        avg_micros, total_ops
    );

    // Threshold: scroll should be essentially instant (< 10us)
    assert!(
        avg_micros < 10.0,
        "Scroll operation too slow: {:.3}us (threshold: 10us)",
        avg_micros
    );
}

/// Test rapid scrolling simulation.
#[test]
fn test_rapid_scrolling_performance() {
    let messages = generate_messages(5000);
    let visible_height = 30;
    let current_tasks = HashMap::new();
    let width = 80;

    // Simulate rapid scrolling: get visible messages and render them
    let iterations = 100;
    let start = Instant::now();

    for _ in 0..iterations {
        // Scroll from bottom to top
        for offset in (0..1000).step_by(10) {
            let visible = visible_messages(&messages, offset, visible_height);
            let mut prev_sender: Option<&str> = None;
            for msg in visible {
                let _ = render_message_line_count(msg, width, prev_sender, &current_tasks);
                prev_sender = Some(&msg.from);
            }
        }
    }
    let elapsed = start.elapsed();

    let scroll_ops = iterations * 100; // 100 scroll positions per iteration
    let avg_millis = elapsed.as_millis() as f64 / scroll_ops as f64;

    println!(
        "Rapid scroll (visible + render): {:.3}ms per scroll ({} scrolls)",
        avg_millis, scroll_ops
    );

    // Threshold: each scroll + render should take < 5ms for smooth 60fps scrolling
    assert!(
        avg_millis < 5.0,
        "Rapid scrolling too slow: {:.3}ms per scroll (threshold: 5ms for 60fps)",
        avg_millis
    );
}

// =============================================================================
// Memory Usage Tests
// =============================================================================

/// Test that message storage grows linearly with message count.
#[test]
fn test_message_storage_memory() {
    // Measure base allocation size for messages
    let small_messages = generate_messages(100);
    let small_size: usize = small_messages
        .iter()
        .map(|m| m.id.len() + m.from.len() + m.content.len())
        .sum();

    let large_messages = generate_messages(10000);
    let large_size: usize = large_messages
        .iter()
        .map(|m| m.id.len() + m.from.len() + m.content.len())
        .sum();

    // Size should scale linearly (within 20% of expected ratio)
    let ratio = large_size as f64 / small_size as f64;
    let expected_ratio = 100.0; // 10000 / 100

    println!(
        "Memory scaling: 100 msgs = {} bytes, 10000 msgs = {} bytes, ratio = {:.2}x (expected: {:.0}x)",
        small_size, large_size, ratio, expected_ratio
    );

    assert!(
        (ratio - expected_ratio).abs() < expected_ratio * 0.2,
        "Memory does not scale linearly: ratio {:.2}x vs expected {:.0}x",
        ratio,
        expected_ratio
    );
}

/// Test that rendering long messages doesn't cause excessive allocations.
#[test]
fn test_long_message_rendering_memory() {
    let long_messages = generate_long_messages(100);
    let current_tasks = HashMap::new();
    let width = 80;

    // Count total lines generated (proxy for allocation)
    let mut total_lines = 0;
    let mut prev_sender: Option<&str> = None;

    for msg in &long_messages {
        total_lines += render_message_line_count(msg, width, prev_sender, &current_tasks);
        prev_sender = Some(&msg.from);
    }

    println!(
        "Long messages: {} messages -> {} total lines",
        long_messages.len(),
        total_lines
    );

    // Sanity check: long messages should produce many lines but not explosively so
    // Each message has ~2500 chars (5x repeated 500 char paragraph)
    // At 80 char width with 7 char gutter = 73 usable chars, that's ~35 lines per message
    // Plus sender lines and blank separators
    // 100 messages * ~22 lines = ~2200 lines expected (some sender lines are grouped)
    assert!(
        total_lines > 1000 && total_lines < 5000,
        "Unexpected line count for long messages: {} (expected 1000-5000)",
        total_lines
    );
}

// =============================================================================
// Stress Tests
// =============================================================================

/// Stress test with very narrow terminal width.
#[test]
fn test_narrow_terminal_rendering() {
    let messages = generate_messages(100);
    let current_tasks = HashMap::new();
    let width = 40; // Very narrow

    let start = Instant::now();
    let iterations = 100;

    for _ in 0..iterations {
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let _ = render_message_line_count(msg, width, prev_sender, &current_tasks);
            prev_sender = Some(&msg.from);
        }
    }
    let elapsed = start.elapsed();

    let avg_millis = elapsed.as_millis() as f64 / iterations as f64;

    println!(
        "Narrow terminal (width=40): {:.2}ms for 100 messages ({} iterations)",
        avg_millis, iterations
    );

    // Narrow terminals may need more wrapping, but should still be fast
    assert!(
        avg_millis < 100.0,
        "Narrow terminal rendering too slow: {:.2}ms (threshold: 100ms)",
        avg_millis
    );
}

/// Stress test with very wide terminal width.
#[test]
fn test_wide_terminal_rendering() {
    let messages = generate_messages(100);
    let current_tasks = HashMap::new();
    let width = 200; // Very wide

    let start = Instant::now();
    let iterations = 100;

    for _ in 0..iterations {
        let mut prev_sender: Option<&str> = None;
        for msg in &messages {
            let _ = render_message_line_count(msg, width, prev_sender, &current_tasks);
            prev_sender = Some(&msg.from);
        }
    }
    let elapsed = start.elapsed();

    let avg_millis = elapsed.as_millis() as f64 / iterations as f64;

    println!(
        "Wide terminal (width=200): {:.2}ms for 100 messages ({} iterations)",
        avg_millis, iterations
    );

    // Wide terminals should be faster (less wrapping)
    assert!(
        avg_millis < 50.0,
        "Wide terminal rendering unexpectedly slow: {:.2}ms (threshold: 50ms)",
        avg_millis
    );
}

/// Test with messages containing only Unicode/emoji.
#[test]
fn test_unicode_message_rendering() {
    let unicode_messages: Vec<Message> = (0..100)
        .map(|i| Message {
            id: i.to_string(),
            from: "park".to_string(),
            content: "This message has emoji: \u{1F389}\u{1F680}\u{2728} and CJK: \u{4E2D}\u{6587}\u{6D4B}\u{8BD5} and symbols: \u{2192}\u{2713}\u{2717}".to_string(),
            timestamp: Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            source_channel: None,
            session_id: None,
            thread_parent_id: None,
        })
        .collect();

    let current_tasks = HashMap::new();
    let width = 80;

    let start = Instant::now();
    let iterations = 100;

    for _ in 0..iterations {
        let mut prev_sender: Option<&str> = None;
        for msg in &unicode_messages {
            let _ = render_message_line_count(msg, width, prev_sender, &current_tasks);
            prev_sender = Some(&msg.from);
        }
    }
    let elapsed = start.elapsed();

    let avg_millis = elapsed.as_millis() as f64 / iterations as f64;

    println!(
        "Unicode messages: {:.2}ms for 100 messages ({} iterations)",
        avg_millis, iterations
    );

    // Unicode handling should not significantly impact performance
    assert!(
        avg_millis < 50.0,
        "Unicode message rendering too slow: {:.2}ms (threshold: 50ms)",
        avg_millis
    );
}

// =============================================================================
// Threshold Summary
// =============================================================================

/// Summary test that prints all performance thresholds.
#[test]
fn test_performance_thresholds_summary() {
    println!("\n=== Chat TUI Performance Thresholds ===");
    println!("Single message render:     < 1ms");
    println!("100 messages render:       < 50ms");
    println!("1000 messages render:      < 500ms");
    println!("wrap_line (10000 chars):   < 5ms");
    println!("wrap_content:              < 1ms");
    println!("Scroll operation:          < 10us");
    println!("Rapid scroll + render:     < 5ms per scroll");
    println!("Narrow terminal (w=40):    < 100ms for 100 msgs");
    println!("Wide terminal (w=200):     < 50ms for 100 msgs");
    println!("Unicode messages:          < 50ms for 100 msgs");
    println!("========================================\n");
}
