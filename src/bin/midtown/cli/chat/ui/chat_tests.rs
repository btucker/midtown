//! Tests for `draw_lead_indicator` rendering.

use super::super::super::app::ToolActivityEntry;
use super::super::super::app::tests::test_app;
use super::*;

fn make_tool_entry(header: &str, completed: bool) -> ToolActivityEntry {
    ToolActivityEntry {
        header: header.to_string(),
        completed_at: if completed {
            Some(std::time::Instant::now())
        } else {
            None
        },
    }
}

fn buffer_row(buffer: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
    (0..width)
        .map(|x| {
            buffer
                .cell((x, y))
                .map(|c| c.symbol().to_string())
                .unwrap_or_default()
        })
        .collect()
}

#[test]
fn test_draw_lead_indicator_agent_name_on_last_line() {
    // Agent name should appear on the LAST (bottom) line, not the first.
    // New entries should come in at the bottom (CLI-style ordering).
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 2);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    // Two entries: older completed, newer in-progress
    app.tool_activity = std::collections::HashMap::from([(
        "lead".to_string(),
        vec![
            make_tool_entry("\u{2713} Read foo.rs", true), // older, completed
            make_tool_entry("\u{203a} Write bar.rs", false), // newer, in-progress
        ],
    )]);
    app.lead_working = true;

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 2,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row0 = buffer_row(buffer, 0, 20);
    let row1 = buffer_row(buffer, 1, 20);

    assert!(
        row1.contains("lead"),
        "Agent name 'lead' should be on the LAST line (row 1), got row1={row1:?} row0={row0:?}",
    );
    assert!(
        !row0.contains("lead"),
        "Agent name 'lead' should NOT be on the first line (row 0), got row0={row0:?}",
    );
}

#[test]
fn test_draw_lead_indicator_older_entries_on_top() {
    // Older entries should be displayed above newer entries (chronological order).
    // The completed (✓) older entry should be on row 0; in-progress (›) newer on row 1.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(80, 2);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.tool_activity = std::collections::HashMap::from([(
        "lead".to_string(),
        vec![
            make_tool_entry("\u{2713} Read foo.rs", true), // older
            make_tool_entry("\u{203a} Write bar.rs", false), // newer
        ],
    )]);
    app.lead_working = true;

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 2,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row0 = buffer_row(buffer, 0, 80);
    let row1 = buffer_row(buffer, 1, 80);

    assert!(
        row0.contains("foo"),
        "Older entry (foo.rs) should be on row 0 (top), got row0={row0:?}",
    );
    assert!(
        row1.contains("bar"),
        "Newer entry (bar.rs) should be on row 1 (bottom), got row1={row1:?}",
    );
}

#[test]
fn test_draw_lead_indicator_no_ellipsis_truncation() {
    // Descriptions should NOT be truncated with "..." when rendered.
    // The text should just be clipped at the terminal edge, not have "..." added.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    let backend = TestBackend::new(30, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.tool_activity = std::collections::HashMap::from([(
        "lead".to_string(),
        vec![make_tool_entry("\u{203a} ABCDEFGHIJKLMNOPQRSTUVWXY", false)],
    )]);
    app.lead_working = true;

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 30,
                height: 1,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row = buffer_row(buffer, 0, 30);

    assert!(
        !row.contains("..."),
        "Lead indicator should NOT use '...' truncation — just clip at terminal edge. Got: {row:?}",
    );
}

#[test]
fn test_draw_lead_indicator_spinner_shows_without_lead_working() {
    // When lead_working is false but tool entries are in-progress,
    // the spinner glyph must still appear (not a space). This ensures the visual
    // state is consistent with any_spinner_visible(), which fires the animation
    // timer for in-progress entries regardless of lead_working.
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;

    const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

    let backend = TestBackend::new(80, 1);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut app = test_app();
    app.lead_working = false; // explicitly NOT working
    app.tool_activity = std::collections::HashMap::from([(
        "lead".to_string(),
        vec![make_tool_entry("\u{203a} Read foo.rs", false)], // in-progress
    )]);

    terminal
        .draw(|f| {
            let area = Rect {
                x: 0,
                y: 0,
                width: 80,
                height: 1,
            };
            draw_lead_indicator(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    let row = buffer_row(buffer, 0, 80);

    let has_spinner = SPINNER_FRAMES.iter().any(|&frame| row.contains(frame));
    assert!(
        has_spinner,
        "Spinner glyph should appear when tool entries are in-progress, even if lead_working=false. Got: {row:?}",
    );
}
