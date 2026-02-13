use super::app;
use super::app::tests::test_app;
use super::tests::key_press;
use super::{EventResult, handle_event};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[test]
fn test_ctrl_k_toggles_channel_switcher() {
    let mut app = test_app();
    assert!(
        !app.channel_switcher.show,
        "Channel switcher should start hidden"
    );

    let event = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    let result = handle_event(&mut app, event);

    assert!(matches!(result, EventResult::Continue));
    assert!(
        app.channel_switcher.show,
        "Ctrl+K should show channel switcher"
    );

    // Toggle again to hide
    let event = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
    let result = handle_event(&mut app, event);

    assert!(matches!(result, EventResult::Continue));
    assert!(
        !app.channel_switcher.show,
        "Ctrl+K should hide channel switcher"
    );
}

#[test]
fn test_esc_dismisses_channel_switcher() {
    let mut app = test_app();
    app.channel_switcher.show = true;
    app.channel_switcher.input = "test".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Esc));

    assert!(matches!(result, EventResult::Continue));
    assert!(
        !app.channel_switcher.show,
        "Esc should dismiss channel switcher"
    );
    assert!(
        app.channel_switcher.input.is_empty(),
        "Esc should clear input"
    );
}

#[test]
fn test_channel_switcher_input() {
    let mut app = test_app();
    app.channel_switcher.show = true;

    let result = handle_event(&mut app, key_press(KeyCode::Char('a')));

    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.channel_switcher.input, "a",
        "Character should be added to channel switcher input"
    );

    let _result = handle_event(&mut app, key_press(KeyCode::Char('u')));
    assert_eq!(app.channel_switcher.input, "au");
}

#[test]
fn test_channel_switcher_backspace() {
    let mut app = test_app();
    app.channel_switcher.show = true;
    app.channel_switcher.input = "test".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Backspace));

    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.channel_switcher.input, "tes",
        "Backspace should remove last character"
    );
}

#[test]
fn test_channel_switcher_arrow_navigation() {
    let mut app = test_app();
    app.channel_switcher.show = true;
    app.channel_switcher.filtered_channels = vec![
        app::ChannelSwitcherItem {
            name: "channel1".to_string(),
            unread_count: 0,
        },
        app::ChannelSwitcherItem {
            name: "channel2".to_string(),
            unread_count: 0,
        },
        app::ChannelSwitcherItem {
            name: "channel3".to_string(),
            unread_count: 0,
        },
    ];
    app.channel_switcher.selected_index = 0;

    // Down arrow
    let result = handle_event(&mut app, key_press(KeyCode::Down));
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(app.channel_switcher.selected_index, 1);

    // Down again
    let _result = handle_event(&mut app, key_press(KeyCode::Down));
    assert_eq!(app.channel_switcher.selected_index, 2);

    // Down wraps to 0
    let _result = handle_event(&mut app, key_press(KeyCode::Down));
    assert_eq!(app.channel_switcher.selected_index, 0);

    // Up arrow
    let _result = handle_event(&mut app, key_press(KeyCode::Up));
    assert_eq!(app.channel_switcher.selected_index, 2);
}

#[test]
fn test_channel_switcher_enter_selects() {
    let mut app = test_app();
    app.channel_switcher.show = true;
    app.channel_switcher.filtered_channels = vec![
        app::ChannelSwitcherItem {
            name: "auth".to_string(),
            unread_count: 2,
        },
        app::ChannelSwitcherItem {
            name: "frontend".to_string(),
            unread_count: 0,
        },
    ];
    app.channel_switcher.selected_index = 1;
    app.selected_channel = "midtown".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Enter));

    assert!(matches!(result, EventResult::Continue));
    assert!(
        !app.channel_switcher.show,
        "Enter should close channel switcher"
    );
    assert_eq!(
        app.selected_channel, "frontend",
        "Selected channel should change"
    );
    // Issue #4: Verify board_selection is also set
    assert_eq!(
        app.board_selection,
        Some(app::BoardSelection::Channel("frontend".to_string())),
        "board_selection should be set to match selected_channel"
    );
}

#[test]
fn test_channel_switcher_navigation_with_many_channels() {
    // Regression test for issue #2: navigation should work correctly
    // even when there are more than 10 channels (max visible items)
    let mut app = test_app();
    app.channel_switcher.show = true;

    // Create 15 test channels (more than the 10 visible limit)
    app.channel_switcher.filtered_channels = (0..15)
        .map(|i| app::ChannelSwitcherItem {
            name: format!("channel{}", i),
            unread_count: 0,
        })
        .collect();

    app.channel_switcher.selected_index = 0;

    // Navigate down to index 11 (beyond the 10 visible items)
    for _ in 0..11 {
        let _ = handle_event(&mut app, key_press(KeyCode::Down));
    }

    assert_eq!(
        app.channel_switcher.selected_index, 11,
        "Should be at index 11"
    );

    // Select the channel at index 11
    let result = handle_event(&mut app, key_press(KeyCode::Enter));

    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.selected_channel, "channel11",
        "Should select channel11 even though it's beyond the visible window"
    );
    assert_eq!(
        app.board_selection,
        Some(app::BoardSelection::Channel("channel11".to_string())),
        "board_selection should match"
    );
}
