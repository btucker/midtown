use super::*;

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
