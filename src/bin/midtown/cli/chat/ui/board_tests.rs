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
        description: None,
        owner: Some("york".to_string()),
        status: TaskStatus::InProgress,
        modified_at: None,
        channel: None,
        blocked_by: vec![],
        message_id: None,
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

// --- render_channel_header tests ---

#[test]
fn test_channel_header_no_unread() {
    let app = super::super::super::app::tests::test_app();
    let mut lines = Vec::new();
    render_channel_header(&app, "midtown", &mut lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].spans[0].content.as_ref(), "#midtown");
}

#[test]
fn test_channel_header_with_unread() {
    let mut app = super::super::super::app::tests::test_app();
    app.channel_unread_counts.insert("auth".to_string(), 5);
    let mut lines = Vec::new();
    render_channel_header(&app, "auth", &mut lines);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].spans[0].content.as_ref(), "#auth (5)");
}

#[test]
fn test_channel_header_no_task_count() {
    // Task count should not appear in the header regardless of task presence
    let app = super::super::super::app::tests::test_app();
    let mut lines = Vec::new();
    render_channel_header(&app, "tui", &mut lines);
    let content = lines[0].spans[0].content.as_ref();
    assert!(
        !content.contains("tasks"),
        "header should not mention tasks: {content}"
    );
    assert!(
        !content.contains("—"),
        "header should not contain separator: {content}"
    );
}

// --- coworker_line_map tests ---

fn make_active_coworker(name: &str, phase: &str) -> CoworkerInfo {
    CoworkerInfo {
        name: name.to_string(),
        task_id: None,
        phase: Some(phase.to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
        progress: None,
        time_estimate: None,
    }
}

/// draw_board_panel must populate coworker_line_map so that clicking a coworker row
/// in the status table triggers click-to-attach.
#[test]
fn test_draw_board_panel_populates_coworker_line_map() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.coworkers = vec![
        make_active_coworker("park", "developing"),
        make_active_coworker("york", "testing"),
    ];
    app.max_coworkers = 4;

    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    // Both coworkers should be present in the map
    let names: Vec<&String> = app.coworker_line_map.values().collect();
    assert!(
        names.iter().any(|n| n.as_str() == "park"),
        "park should be in coworker_line_map, got: {:?}",
        names
    );
    assert!(
        names.iter().any(|n| n.as_str() == "york"),
        "york should be in coworker_line_map, got: {:?}",
        names
    );

    // The two coworkers should be on consecutive y-lines
    let mut y_lines: Vec<u16> = app.coworker_line_map.keys().cloned().collect();
    y_lines.sort();
    assert_eq!(
        y_lines.len(),
        2,
        "expected exactly 2 lines in coworker_line_map"
    );
    assert_eq!(
        y_lines[1] - y_lines[0],
        1,
        "coworker rows should be on consecutive y-lines"
    );
}

/// Idle coworkers should NOT appear in coworker_line_map (they are excluded from the table).
#[test]
fn test_coworker_line_map_excludes_idle() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.coworkers = vec![
        CoworkerInfo {
            name: "park".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
            progress: None,
            time_estimate: None,
        },
        make_active_coworker("york", "developing"),
    ];

    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    let names: Vec<&String> = app.coworker_line_map.values().collect();
    assert!(
        !names.iter().any(|n| n.as_str() == "park"),
        "idle coworker 'park' should NOT be in coworker_line_map"
    );
    assert!(
        names.iter().any(|n| n.as_str() == "york"),
        "active coworker 'york' should be in coworker_line_map"
    );
}

/// The project lead (legacy name "lead") should NOT appear in the coworkers sidebar,
/// even when their phase is active (non-idle). Regression test for !1723.
#[test]
fn test_coworker_line_map_excludes_project_lead() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.coworkers = vec![
        make_active_coworker("lead", "developing"),
        make_active_coworker("york", "developing"),
    ];

    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    let names: Vec<&String> = app.coworker_line_map.values().collect();
    assert!(
        !names.iter().any(|n| n.as_str() == "lead"),
        "project lead 'lead' should NOT be in coworker_line_map, got: {:?}",
        names
    );
    assert!(
        names.iter().any(|n| n.as_str() == "york"),
        "active coworker 'york' should be in coworker_line_map"
    );
}

// --- DM channel separation tests ---

/// DM channels should appear after regular channels, separated by a "DIRECT MESSAGES" header.
#[test]
fn test_dm_channels_separated_from_regular_channels() {
    use midtown::ChannelInfo;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.available_channels = vec![
        ChannelInfo {
            name: "tui".to_string(),
            is_archived: false,
            is_dm: false,
        },
        ChannelInfo {
            name: "ops".to_string(),
            is_archived: false,
            is_dm: false,
        },
        ChannelInfo {
            name: "dm-park".to_string(),
            is_archived: false,
            is_dm: true,
        },
        ChannelInfo {
            name: "dm-york".to_string(),
            is_archived: false,
            is_dm: true,
        },
    ];

    let backend = TestBackend::new(40, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    let mut rendered_lines = Vec::new();
    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    // Extract rendered text from the terminal buffer
    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        rendered_lines.push(line.trim_end().to_string());
    }

    // Find the "DIRECT MESSAGES" header line
    let dm_header_line = rendered_lines
        .iter()
        .position(|l| l.contains("DIRECT MESSAGES"));
    assert!(
        dm_header_line.is_some(),
        "should have a 'DIRECT MESSAGES' section header, rendered:\n{}",
        rendered_lines.join("\n")
    );
    let dm_header_y = dm_header_line.unwrap();

    // Regular channels (#tui, #ops) should appear before the DM header
    let tui_line = rendered_lines.iter().position(|l| l.contains("#tui"));
    let ops_line = rendered_lines.iter().position(|l| l.contains("#ops"));
    assert!(tui_line.is_some(), "should render #tui");
    assert!(ops_line.is_some(), "should render #ops");
    assert!(
        tui_line.unwrap() < dm_header_y,
        "#tui should be above DIRECT MESSAGES"
    );
    assert!(
        ops_line.unwrap() < dm_header_y,
        "#ops should be above DIRECT MESSAGES"
    );

    // DM channels (@park, @york) should appear after the DM header
    let park_line = rendered_lines.iter().position(|l| l.contains("@park"));
    let york_line = rendered_lines.iter().position(|l| l.contains("@york"));
    assert!(park_line.is_some(), "should render @park");
    assert!(york_line.is_some(), "should render @york");
    assert!(
        park_line.unwrap() > dm_header_y,
        "@park should be below DIRECT MESSAGES"
    );
    assert!(
        york_line.unwrap() > dm_header_y,
        "@york should be below DIRECT MESSAGES"
    );
}

/// When there are no DM channels, the "DIRECT MESSAGES" header should not appear.
#[test]
fn test_no_dm_header_when_no_dm_channels() {
    use midtown::ChannelInfo;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.available_channels = vec![
        ChannelInfo {
            name: "tui".to_string(),
            is_archived: false,
            is_dm: false,
        },
        ChannelInfo {
            name: "ops".to_string(),
            is_archived: false,
            is_dm: false,
        },
    ];

    let backend = TestBackend::new(40, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer();
    for y in 0..buffer.area.height {
        let mut line = String::new();
        for x in 0..buffer.area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        assert!(
            !line.contains("DIRECT MESSAGES"),
            "DIRECT MESSAGES header should not appear when there are no DM channels"
        );
    }
}

/// DM channels should be clickable via channel_line_map even when in the DM section.
#[test]
fn test_dm_channels_in_channel_line_map() {
    use midtown::ChannelInfo;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    app.available_channels = vec![
        ChannelInfo {
            name: "tui".to_string(),
            is_archived: false,
            is_dm: false,
        },
        ChannelInfo {
            name: "dm-park".to_string(),
            is_archived: false,
            is_dm: true,
        },
    ];

    let backend = TestBackend::new(40, 20);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    let channel_names: Vec<&String> = app.channel_line_map.values().collect();
    assert!(
        channel_names.iter().any(|n| n.as_str() == "tui"),
        "tui should be in channel_line_map"
    );
    assert!(
        channel_names.iter().any(|n| n.as_str() == "dm-park"),
        "dm-park should be in channel_line_map"
    );
}

/// The project lead named by repo name (canonical naming, e.g. "midtown") should NOT
/// appear in the coworkers sidebar. test_app() uses project_name = "test".
/// Regression test for !1723 (Codex review feedback).
#[test]
fn test_coworker_line_map_excludes_repo_named_lead() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut app = test_app();
    // The canonical lead session is named after the repo (app.project_name = "test")
    app.coworkers = vec![
        make_active_coworker("test", "developing"),
        make_active_coworker("york", "developing"),
    ];

    let backend = TestBackend::new(80, 40);
    let mut terminal = Terminal::new(backend).unwrap();

    terminal
        .draw(|f| {
            let area = f.area();
            draw_board_panel(f, &mut app, area);
        })
        .unwrap();

    let names: Vec<&String> = app.coworker_line_map.values().collect();
    assert!(
        !names.iter().any(|n| n.as_str() == "test"),
        "repo-named project lead 'test' should NOT be in coworker_line_map, got: {:?}",
        names
    );
    assert!(
        names.iter().any(|n| n.as_str() == "york"),
        "active coworker 'york' should be in coworker_line_map"
    );
}
