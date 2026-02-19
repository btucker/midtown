use super::super::super::app::tests::test_app;
use super::*;

// --- Bug !1616: click-map regression for phase label on wrapped task titles ---

/// When a task title wraps, the first continuation line (line index 1) has the phase label
/// merged into it. That line must be registered in task_line_map so clicking it triggers
/// click-to-attach.
#[test]
fn test_task_line_map_registers_continuation_line_with_phase_label() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    // Use a subject long enough to wrap at narrow render width (width=30 forces wrap)
    app.tasks = vec![KanbanTask {
        id: "100".to_string(),
        subject: "A very long task title that will definitely wrap when rendered at a narrow width in the sidebar".to_string(),
        owner: Some("york".to_string()),
        status: TaskStatus::InProgress,
        modified_at: None,
        channel: None,
        blocked_by: vec![],
    }];

    let backend = TestBackend::new(30, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    // The channel header is line 0, the task title first line is line 1.
    // The continuation line (with phase label merged) is line 2.
    // All lines that belong to this task must be in task_line_map.
    let task_lines: Vec<u16> = app
        .task_line_map
        .iter()
        .filter(|(_, (id, _))| id == "100")
        .map(|(line, _)| *line)
        .collect();

    // Must have at least 2 registered lines for this task (title line + continuation line)
    assert!(
        task_lines.len() >= 2,
        "wrapped task title: continuation line with phase label must be registered in task_line_map, got lines: {:?}",
        task_lines
    );
}

// --- draw_ops_mini_channel tests ---

#[test]
fn test_draw_ops_mini_channel_renders_without_panic() {
    use midtown::Message;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(40, 8);
    let mut terminal = Terminal::new(backend).unwrap();

    let msgs: Vec<midtown::Message> = vec![
        {
            let mut m = Message::system("CI checks passed");
            m.from = "github".to_string();
            m
        },
        {
            let mut m = Message::system("Task !1583 assigned to york");
            m.from = "daemon".to_string();
            m
        },
    ];
    let refs: Vec<&midtown::Message> = msgs.iter().collect();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_ops_mini_channel(f, &refs, area);
        })
        .unwrap();
    // No panic = success
}

#[test]
fn test_draw_ops_mini_channel_empty_messages() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let backend = TestBackend::new(40, 5);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_ops_mini_channel(f, &[], area);
        })
        .unwrap();
    // Empty state renders without panic
}
