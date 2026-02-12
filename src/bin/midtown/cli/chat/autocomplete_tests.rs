use super::CoworkerInfo;
use super::tests::test_app;
use crate::cli::chat::{EventResult, handle_event};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

#[test]
fn test_autocomplete_starts_with_matching() {
    // Bug #1: @p should show both 'park' and 'pleasant', not just 'park'
    let mut app = test_app();

    // Add test coworkers
    app.coworkers = vec![
        CoworkerInfo {
            name: "park".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
        },
        CoworkerInfo {
            name: "pleasant".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
        },
        CoworkerInfo {
            name: "lexington".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
        },
    ];

    // Test @p should match both park and pleasant
    let items = app.get_mention_items("p");
    assert_eq!(items.len(), 2, "Should match 2 coworkers starting with 'p'");
    assert!(
        items.iter().any(|i| i.value == "@park"),
        "Should include @park"
    );
    assert!(
        items.iter().any(|i| i.value == "@pleasant"),
        "Should include @pleasant"
    );
    assert!(
        !items.iter().any(|i| i.value == "@lexington"),
        "Should NOT include @lexington"
    );

    // Test @pl should match only pleasant
    let items = app.get_mention_items("pl");
    assert_eq!(items.len(), 1, "Should match 1 coworker starting with 'pl'");
    assert_eq!(items[0].value, "@pleasant");

    // Test @pa should match only park
    let items = app.get_mention_items("pa");
    assert_eq!(items.len(), 1, "Should match 1 coworker starting with 'pa'");
    assert_eq!(items[0].value, "@park");

    // Test @l should match lead and lexington
    let items = app.get_mention_items("l");
    assert_eq!(items.len(), 2, "Should match lead and lexington");
    assert_eq!(items[0].value, "@lead", "Lead should be first");
    assert_eq!(items[1].value, "@lexington");
}

#[test]
fn test_autocomplete_empty_query_shows_all() {
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
        },
        CoworkerInfo {
            name: "pleasant".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
        },
    ];

    // Empty query should show all (lead + all coworkers)
    let items = app.get_mention_items("");
    assert_eq!(items.len(), 3, "Empty query should show lead + 2 coworkers");
    assert_eq!(items[0].value, "@lead");
}

#[test]
fn test_autocomplete_case_insensitive() {
    let mut app = test_app();

    app.coworkers = vec![CoworkerInfo {
        name: "Park".to_string(), // Capitalized
        task_id: None,
        phase: Some("idle".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
    }];

    // Lowercase query should match capitalized name
    let items = app.get_mention_items("p");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].value, "@Park");

    let items = app.get_mention_items("pa");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].value, "@Park");
}

#[test]
fn test_shift_enter_inserts_newline() {
    // Bug #3: Shift+Enter should insert newline, not send message
    let mut app = test_app();
    app.input_text = "Hello".to_string();
    app.input_cursor = 5;

    // Simulate Shift+Enter
    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    let result = handle_event(&mut app, event);

    // Should continue (not send message)
    assert!(
        matches!(result, EventResult::Continue),
        "Shift+Enter should not send message"
    );

    // Should have inserted newline
    assert_eq!(app.input_text, "Hello\n", "Should have newline appended");
    assert_eq!(app.input_cursor, 6, "Cursor should advance past newline");
}

#[test]
fn test_shift_enter_multi_line() {
    let mut app = test_app();
    app.input_text = "Line 1".to_string();
    app.input_cursor = 6;

    // First Shift+Enter
    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    handle_event(&mut app, event);

    assert_eq!(app.input_text, "Line 1\n");
    assert_eq!(app.input_cursor, 7);

    // Add more text
    app.input_text.push_str("Line 2");
    app.input_cursor = 13;

    // Second Shift+Enter
    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    handle_event(&mut app, event);

    assert_eq!(app.input_text, "Line 1\nLine 2\n");
    assert_eq!(app.input_cursor, 14);
}

#[test]
fn test_enter_without_shift_focuses_input() {
    // Verify normal Enter still works (not broken by Shift+Enter fix)
    // When not focused on input bar, Enter should focus it
    let mut app = test_app();
    app.input_text = "".to_string();
    app.input_cursor = 0;
    use super::FocusedPane;
    app.focused_pane = FocusedPane::Board;

    // Normal Enter (no modifiers)
    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let result = handle_event(&mut app, event);

    // Should continue and focus input
    assert!(matches!(result, EventResult::Continue));
    assert!(matches!(app.focused_pane, FocusedPane::InputBar));
}

#[test]
fn test_shift_enter_with_autocomplete_shown() {
    // Shift+Enter should insert newline even if autocomplete is showing
    let mut app = test_app();
    app.input_text = "@p".to_string();
    app.input_cursor = 2;
    app.autocomplete.show = true;
    app.autocomplete.selected_index = 0;

    app.coworkers = vec![CoworkerInfo {
        name: "park".to_string(),
        task_id: None,
        phase: Some("idle".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
    }];

    // Shift+Enter should insert newline, not select autocomplete
    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    handle_event(&mut app, event);

    assert_eq!(
        app.input_text, "@p\n",
        "Should insert newline, not autocomplete"
    );
    assert_eq!(app.input_cursor, 3);
}

#[test]
fn test_enter_selects_autocomplete_when_shown() {
    // Normal Enter should select autocomplete when shown
    let mut app = test_app();
    app.input_text = "@p".to_string();
    app.input_cursor = 2;
    app.autocomplete.show = true;
    app.autocomplete.selected_index = 0;
    use super::FocusedPane;
    app.focused_pane = FocusedPane::InputBar;

    app.coworkers = vec![CoworkerInfo {
        name: "park".to_string(),
        task_id: None,
        phase: Some("idle".to_string()),
        pr_number: None,
        health: "green".to_string(),
        provider: "claude".to_string(),
        profile: "default".to_string(),
    }];

    // Get autocomplete items to populate the list
    app.autocomplete.items = app.get_mention_items("p");

    // Normal Enter should select autocomplete item
    let event = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    handle_event(&mut app, event);

    assert!(
        app.input_text.starts_with("@park"),
        "Should insert autocomplete item, got: {}",
        app.input_text
    );
    assert!(!app.autocomplete.show, "Autocomplete should be hidden");
}
