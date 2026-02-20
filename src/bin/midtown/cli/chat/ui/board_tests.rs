use super::*;

// --- is_ops_message tests ---

#[test]
fn test_is_ops_message_midtown_sender() {
    assert!(is_ops_message("midtown", &MessageType::Text, "hello"));
    assert!(is_ops_message("MIDTOWN", &MessageType::Text, "hello"));
}

#[test]
fn test_is_ops_message_github_sender() {
    assert!(is_ops_message("github", &MessageType::System, "CI passed"));
}

#[test]
fn test_is_ops_message_system_sender() {
    assert!(is_ops_message("system", &MessageType::System, "restart"));
    assert!(is_ops_message("daemon", &MessageType::System, "spawned"));
}

#[test]
fn test_is_ops_message_action_type() {
    // Action type (MessageType::Action) from coworkers is an ops message
    assert!(is_ops_message(
        "york",
        &MessageType::Action,
        "/me developing"
    ));
    // lead and user action messages stay in main chat
    assert!(!is_ops_message(
        "lead",
        &MessageType::Action,
        "/me reviewing"
    ));
}

#[test]
fn test_is_ops_message_slash_me_content() {
    // /me prefix in content = ops message even with Text type
    assert!(is_ops_message("park", &MessageType::Text, "/me idle"));
}

#[test]
fn test_is_ops_message_regular_conversation() {
    // Regular conversation from coworkers/lead/user is NOT ops
    assert!(!is_ops_message("york", &MessageType::Text, "Hello team"));
    assert!(!is_ops_message("lead", &MessageType::Text, "Looks good"));
    assert!(!is_ops_message(
        "user",
        &MessageType::Text,
        "What's the status?"
    ));
}

#[test]
fn test_is_ops_message_lead_user_slash_me_not_ops() {
    // lead and user /me actions stay in main chat, not ops
    assert!(!is_ops_message(
        "lead",
        &MessageType::Action,
        "/me reviewing"
    ));
    assert!(!is_ops_message(
        "lead",
        &MessageType::Text,
        "/me checking things"
    ));
    assert!(!is_ops_message(
        "user",
        &MessageType::Action,
        "/me asking a question"
    ));
    assert!(!is_ops_message(
        "user",
        &MessageType::Text,
        "/me doing something"
    ));
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
        Message::new("york", "/me developing task 1583", MessageType::Action),
    ];
    let refs: Vec<&midtown::Message> = msgs.iter().collect();

    terminal
        .draw(|f| {
            use ratatui_themes::{Theme, ThemeName};
            let palette = Theme::new(ThemeName::CatppuccinMocha).palette();
            let area = f.area();
            draw_ops_mini_channel(f, &refs, area, palette);
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
            use ratatui_themes::{Theme, ThemeName};
            let palette = Theme::new(ThemeName::CatppuccinMocha).palette();
            let area = f.area();
            draw_ops_mini_channel(f, &[], area, palette);
        })
        .unwrap();
    // Empty state renders without panic
}
