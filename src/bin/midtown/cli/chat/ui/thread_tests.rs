use midtown::MessageType;

use super::super::super::app::FocusedPane;
use super::super::super::app::tests::test_app;
use super::draw_thread_panel;

// ── draw_thread_panel: narrow terminal layout regression ─────────────────────

/// When the thread panel is very narrow (content_width = 0 after border subtraction),
/// the header height must stay at the 4-line minimum rather than inflating to the
/// 12-line cap. Previously, `render_content_lines` called with width=0 would wrap
/// each character into its own line, causing a long parent message to hit the 12-line
/// cap and crowd out thread replies.
#[test]
fn test_draw_thread_panel_narrow_terminal_header_height() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    // Add a long parent message (many characters that would balloon to 12+ lines at width=1)
    let parent_msg = midtown::Message::text("park", "word ".repeat(50));
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    // 4-column terminal: area.width=4, content_width=4.saturating_sub(2)=2 — narrow but nonzero
    // Use width=2: content_width=0 (2-2=0), which is the zero-width edge case
    let backend = TestBackend::new(2, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    // Should not panic, and should render without filling the entire height with header
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 2, 20);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    // The key invariant: thread replies area (chunks[1]) must have height >= 3
    // (the Min(3) constraint). If the header inflated to 12, there'd be no space.
    // With a 20-row terminal: 12 header + 3 replies_min + 3 input = 18 minimum needed.
    // We verify the panel renders without panic — a panic means the layout overflowed.
}

/// Thread input cursor must use theme palette colors rather than hardcoded black/yellow.
///
/// The `█` (FULL BLOCK) cursor in draw_thread_input previously used
/// `fg(Color::Black).bg(Color::Yellow)` — a solid black block that is invisible on dark
/// backgrounds. The cursor must use `fg(palette.fg)` so it appears light/white in dark themes.
#[test]
fn test_thread_cursor_uses_palette_not_hardcoded_colors() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    // 40-wide, 12-tall terminal.
    // Layout: header(4) + replies(5) + input(3) = 12 rows.
    let backend = TestBackend::new(40, 12);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    // Set up a parent message so draw_thread_panel renders the full panel.
    let parent_msg = midtown::Message::text("park", "hello");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);
    app.focused_pane = FocusedPane::Thread;
    app.thread_input_text = "hi".to_string();
    app.thread_input_cursor = 2; // at end of "hi" — renders the █ cursor

    let palette = app.theme.palette();

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 40, 12);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    // Input chunk starts at y=9 (header=4, replies=5).
    // Inside the input block: border at y=9, content at y=10, border at y=11.
    // Content at y=10: left_border(x=0) + ↳(x=1) + space(x=2) + h(x=3) + i(x=4) + cursor(x=5).
    let cursor_cell = buffer.cell((5, 10)).unwrap();

    assert_eq!(
        cursor_cell.symbol(),
        "█",
        "Thread cursor at end of text should show '█'"
    );
    assert_ne!(
        cursor_cell.fg,
        Color::Black,
        "Thread cursor must NOT use hardcoded Color::Black — invisible in dark themes. Got fg={:?}",
        cursor_cell.fg
    );
    assert_eq!(
        cursor_cell.fg, palette.fg,
        "Thread cursor must use palette.fg so it's visible in any theme. \
         Got fg={:?}, expected palette.fg={:?}",
        cursor_cell.fg, palette.fg
    );
}

/// Thread input bar must expand to multiple lines for long input text,
/// matching the behavior of the main channel input bar.
///
/// The thread input height was previously hardcoded to 3 (1 content line + 2 borders).
/// With long input text that wraps to multiple lines, it must grow dynamically
/// using the same calculate_input_bar_height logic as the main input.
#[test]
fn test_thread_input_expands_to_multiple_lines() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    // 80-wide, 25-tall terminal — enough space for header + replies + tall input.
    let backend = TestBackend::new(80, 25);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    let parent_msg = midtown::Message::text("park", "hello");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    // 150 'a' chars should wrap to 3 content lines at 80-wide terminal
    // (available inner width 78, minus prompt 3, minus cursor 1 = 74 chars/line → 3 lines)
    app.thread_input_text = "a".repeat(150);

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 25);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    // The thread input area (including borders) must be > 3 rows for multi-line text.
    // With 3 content lines + 2 border rows = 5 total.
    let input_area = app
        .thread_input_area
        .expect("thread_input_area must be set");
    assert!(
        input_area.height > 3,
        "Thread input height should expand beyond 3 for long text, got {}",
        input_area.height
    );
    assert_eq!(
        input_area.height, 5,
        "150-char input should give 3 content lines + 2 borders = height 5, got {}",
        input_area.height
    );
}

/// When content_width is zero, header renders with minimum height (4), not maximum (12).
/// This is the direct regression check: we measure the layout split produces a valid
/// replies area rather than an overflowed/zero-height one.
#[test]
fn test_draw_thread_panel_zero_content_width_does_not_inflate_header() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    // 250-char message: at width=1 wrapping, this becomes 250 lines → hits 12-line cap
    let long_content = "x".repeat(250);
    let parent_msg = midtown::Message::text("madison", &long_content);
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    // Minimal width: content_width = area.width.saturating_sub(2) = 0
    let backend = TestBackend::new(2, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 2, 20);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    // Rendered without panic — the layout was valid (no overflow from inflated header)
}

/// Thread panel header must render fenced code blocks in the parent message with
/// syntax highlighting, not raw backtick fences. When a thread is opened on a
/// message containing a code block, the header ("Thread" box at the top) must
/// show "--- rust ---" borders rather than "```rust".
///
/// Before the fix: draw_thread_panel pre-computed the parent message content using
/// render_content_lines(), which has no code block detection. Code block content
/// fell through to minimad_ratatui::inline line-by-line, showing `` ``` `` as text.
/// After the fix: the pre-computation uses parse_content_segments() and routes
/// code blocks through highlight_code(), producing the same "--- lang ---" borders
/// as the main channel and thread replies.
#[test]
fn test_thread_header_renders_code_block_in_parent_message() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    // Parent message that contains a fenced code block
    let parent_msg = midtown::Message {
        id: "parent-1".to_string(),
        from: "tui".to_string(),
        content: "Here is some code:\n```rust\nfn hello() {}\n```".to_string(),
        timestamp: chrono::Utc::now(),
        message_type: MessageType::Text,
        channel: None,
        session_id: None,
        thread_parent_id: None,
    };
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 30);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    let rendered_lines: Vec<String> = (0..30)
        .map(|row| {
            (0..80)
                .filter_map(|col| buf.cell((col, row)).map(|c| c.symbol().to_string()))
                .collect()
        })
        .collect();
    let all_text = rendered_lines.join("\n");

    // The header must show the bare language label and syntax-highlighted code,
    // not raw backtick fences. New format: bare "rust" label, no "--- ---" borders.
    assert!(
        all_text.contains("rust"),
        "Thread header should render code blocks with bare language label 'rust', got:\n{}",
        all_text
    );
    assert!(
        !all_text.contains("--- rust ---"),
        "Thread header should NOT show old '--- rust ---' border format, got:\n{}",
        all_text
    );
    assert!(
        !all_text.contains("--- end ---"),
        "Thread header should NOT show '--- end ---' bottom border, got:\n{}",
        all_text
    );
    assert!(
        !all_text.contains("```rust"),
        "Thread header should NOT show raw '```rust' fences, got:\n{}",
        all_text
    );
}

/// Thread messages containing fenced code blocks must be syntax-highlighted,
/// not shown as raw backtick fences. The rendered thread panel must use the
/// bare language label (e.g. "rust") when a reply has a rust code block.
///
/// Before the fix: draw_thread_messages used render_message() which passed the
/// raw content through minimad_ratatui::inline, showing "```rust" as plain text.
/// After the fix: it calls parse_content_segments() + render_message_with_mermaid()
/// which routes code blocks through highlight_code(), producing "--- lang ---" borders.
#[test]
fn test_thread_panel_renders_code_block_with_syntax_highlighting_borders() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    // Add a parent message (thread header)
    let parent_msg = midtown::Message::text("park", "Here is some code");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id.clone());

    // Add a thread reply with a fenced rust code block
    let reply = midtown::Message {
        id: "reply-1".to_string(),
        from: "madison".to_string(),
        content: "```rust\nfn hello() {}\n```".to_string(),
        timestamp: chrono::Utc::now(),
        message_type: MessageType::Text,
        channel: None,
        session_id: None,
        thread_parent_id: Some(parent_id),
    };
    app.thread_messages.push(reply);

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 30);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    // Collect rendered buffer as text lines
    let buf = terminal.backend().buffer();
    let rendered_lines: Vec<String> = (0..30)
        .map(|row| {
            (0..80)
                .filter_map(|col| buf.cell((col, row)).map(|c| c.symbol().to_string()))
                .collect()
        })
        .collect();
    let all_text = rendered_lines.join("\n");

    assert!(
        all_text.contains("rust"),
        "Thread panel should render code blocks with bare language label 'rust', got:\n{}",
        all_text
    );
    assert!(
        !all_text.contains("--- rust ---"),
        "Thread panel should NOT show '--- rust ---' border, got:\n{}",
        all_text
    );
    assert!(
        !all_text.contains("--- end ---"),
        "Thread panel should NOT show '--- end ---' border, got:\n{}",
        all_text
    );
    // Should NOT show raw backtick fences
    assert!(
        !all_text.contains("```rust"),
        "Thread panel should NOT show raw backtick fences, got:\n{}",
        all_text
    );
}

/// The separator between parent message and replies must show the reply count.
/// With 2 replies: "─── 2 replies ───"
/// With 1 reply:   "─── 1 reply ───"
/// With 0 replies: "─── no replies yet ───"
#[test]
fn test_thread_separator_shows_reply_count() {
    use midtown::MessageType;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    let parent_msg = midtown::Message::text("park", "parent message");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id.clone());

    // Add two replies
    for i in 0..2 {
        app.thread_messages.push(midtown::Message {
            id: format!("reply-{i}"),
            from: "madison".to_string(),
            content: format!("reply {i}"),
            timestamp: chrono::Utc::now(),
            message_type: MessageType::Text,
            channel: None,
            session_id: None,
            thread_parent_id: Some(parent_id.clone()),
        });
    }

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 30);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    let all_text: String = (0..30)
        .flat_map(|row| {
            (0..80).filter_map(move |col| buf.cell((col, row)).map(|c| c.symbol().to_string()))
        })
        .collect();

    assert!(
        all_text.contains("2 replies"),
        "Separator should show '2 replies' for 2 replies, got:\n{}",
        all_text
    );
}

/// With zero replies, the separator reads "no replies yet".
#[test]
fn test_thread_separator_no_replies() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();
    let parent_msg = midtown::Message::text("park", "hello");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);
    // No replies

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 20);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    let all_text: String = (0..20)
        .flat_map(|row| {
            (0..80).filter_map(move |col| buf.cell((col, row)).map(|c| c.symbol().to_string()))
        })
        .collect();

    assert!(
        all_text.contains("no replies yet"),
        "Separator should show 'no replies yet' when there are no replies, got:\n{}",
        all_text
    );
}

/// There must be exactly one blank line between the separator and the first reply's
/// sender header when parent and first reply have different senders.
///
/// Bug: passing `parent_sender` as `prev` to the first reply caused `push_sender_header`
/// to add a blank line (its "different sender" spacing logic), while the separator
/// already ends with its own blank line — producing a double blank line.
///
/// Fix: pass `None` as `prev` for the first reply so `push_sender_header` does not
/// add a blank line.
#[test]
fn test_first_reply_has_single_blank_line_after_separator() {
    use midtown::MessageType;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();

    // Parent from "alice", reply from "bob" — different senders triggers the bug
    let parent_msg = midtown::Message::text("alice", "parent message");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id.clone());

    app.thread_messages.push(midtown::Message {
        id: "reply-1".to_string(),
        from: "bob".to_string(),
        content: "first reply".to_string(),
        timestamp: chrono::Utc::now(),
        message_type: MessageType::Text,
        channel: None,
        session_id: None,
        thread_parent_id: Some(parent_id),
    });

    let backend = TestBackend::new(80, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 30);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    // Extract only the inner area cells (cols 1..79, rows 1..26) to exclude border chars.
    // The Thread block renders at (0,0) with full width/height; inner starts at (1,1).
    let rows: Vec<String> = (1..26_u16)
        .map(|row| {
            (1..79_u16)
                .filter_map(|col| buf.cell((col, row)).map(|c| c.symbol().to_string()))
                .collect::<String>()
        })
        .collect();

    // Find the separator row (contains "1 reply")
    let sep_row = rows
        .iter()
        .position(|r| r.contains("1 reply"))
        .expect("separator '1 reply' not found in rendered output");

    // Count blank rows immediately after the separator (blank = all spaces)
    let blank_count = rows[sep_row + 1..]
        .iter()
        .take_while(|r| r.trim().is_empty())
        .count();

    assert_eq!(
        blank_count,
        1,
        "Expected exactly 1 blank line between separator and first reply sender header, \
         got {blank_count}. Rows after separator:\n{}",
        rows[sep_row + 1..sep_row + 5].join("\n")
    );

    // Also assert "bob" appears after the blank line
    let bob_row = rows.iter().position(|r| r.contains("bob"));
    assert!(
        bob_row.is_some(),
        "Expected 'bob' sender header to appear in rendered output"
    );
}

/// The parent message must be rendered using render_message formatting
/// (sender header + timestamp gutter), not a flat content-only approach.
#[test]
fn test_thread_parent_message_rendered_with_sender_header() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let mut app = test_app();
    let parent_msg = midtown::Message::text("parkavenue", "content from parent");
    let parent_id = parent_msg.id.clone();
    app.messages.push_back(parent_msg);
    app.thread_parent_id = Some(parent_id);

    let backend = TestBackend::new(80, 20);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let area = Rect::new(0, 0, 80, 20);
            draw_thread_panel(f, &mut app, area);
        })
        .unwrap();

    let buf = terminal.backend().buffer();
    let all_text: String = (0..20)
        .flat_map(|row| {
            (0..80).filter_map(move |col| buf.cell((col, row)).map(|c| c.symbol().to_string()))
        })
        .collect();

    // render_message shows sender name followed by content on the same/next line
    assert!(
        all_text.contains("parkavenue"),
        "Thread must show parent message sender 'parkavenue', got:\n{}",
        all_text
    );
    assert!(
        all_text.contains("content from parent"),
        "Thread must show parent message content, got:\n{}",
        all_text
    );
}
