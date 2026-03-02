use super::app;
use super::app::tests::test_app;
use super::tests::key_press;
use super::{EventResult, handle_event};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

// ── State transition tests ───────────────────────────────────────────

#[test]
fn test_slash_opens_search_when_not_in_input() {
    let mut app = test_app();
    // Focus on Board (not InputBar), empty input — `/` should open search
    app.focused_pane = app::FocusedPane::Board;
    app.input_text.clear();

    assert!(!app.search.show, "Search should start hidden");

    let result = handle_event(&mut app, key_press(KeyCode::Char('/')));
    assert!(matches!(result, EventResult::Continue));
    assert!(app.search.show, "/ should open search overlay");
}

#[test]
fn test_slash_does_not_open_search_when_in_input_bar() {
    let mut app = test_app();
    app.focused_pane = app::FocusedPane::InputBar;

    let result = handle_event(&mut app, key_press(KeyCode::Char('/')));
    assert!(matches!(result, EventResult::Continue));
    assert!(
        !app.search.show,
        "/ should not open search when InputBar is focused"
    );
    assert_eq!(app.input_text, "/", "/ should be inserted as text input");
}

#[test]
fn test_slash_does_not_open_search_when_draft_exists() {
    let mut app = test_app();
    app.focused_pane = app::FocusedPane::Board;
    app.input_text = "draft message".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Char('/')));
    assert!(matches!(result, EventResult::Continue));
    assert!(
        !app.search.show,
        "/ should not open search when input has draft text"
    );
}

#[test]
fn test_esc_dismisses_search() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input = "test query".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Esc));
    assert!(matches!(result, EventResult::Continue));
    assert!(!app.search.show, "Esc should dismiss search");
    assert!(app.search.input.is_empty(), "Esc should clear search input");
}

// ── Input handling tests ─────────────────────────────────────────────

#[test]
fn test_search_character_input() {
    let mut app = test_app();
    app.search.show = true;

    let result = handle_event(&mut app, key_press(KeyCode::Char('h')));
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(app.search.input, "h");

    let _ = handle_event(&mut app, key_press(KeyCode::Char('i')));
    assert_eq!(app.search.input, "hi");
}

#[test]
fn test_search_backspace() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input = "test".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Backspace));
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(app.search.input, "tes", "Backspace should remove last char");
}

#[test]
fn test_search_backspace_on_empty_input() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input.clear();

    let result = handle_event(&mut app, key_press(KeyCode::Backspace));
    assert!(matches!(result, EventResult::Continue));
    assert!(
        app.search.input.is_empty(),
        "Backspace on empty input should be a no-op"
    );
}

// ── Selection navigation tests ───────────────────────────────────────

#[test]
fn test_search_arrow_navigation() {
    let mut app = test_app();
    app.search.show = true;
    app.search.results = vec![
        midtown::search::SearchResult {
            id: "1".to_string(),
            from: "alice".to_string(),
            content: "hello world".to_string(),
            timestamp: "2025-01-01T00:00:00Z".to_string(),
            channel: "general".to_string(),
            message_type: "text".to_string(),
            snippet: "hello world".to_string(),
        },
        midtown::search::SearchResult {
            id: "2".to_string(),
            from: "bob".to_string(),
            content: "goodbye world".to_string(),
            timestamp: "2025-01-01T00:01:00Z".to_string(),
            channel: "general".to_string(),
            message_type: "text".to_string(),
            snippet: "goodbye world".to_string(),
        },
        midtown::search::SearchResult {
            id: "3".to_string(),
            from: "charlie".to_string(),
            content: "search test".to_string(),
            timestamp: "2025-01-01T00:02:00Z".to_string(),
            channel: "dev".to_string(),
            message_type: "text".to_string(),
            snippet: "search test".to_string(),
        },
    ];
    app.search.selected_index = 0;

    // Down arrow
    let result = handle_event(&mut app, key_press(KeyCode::Down));
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(app.search.selected_index, 1);

    // Down again
    let _ = handle_event(&mut app, key_press(KeyCode::Down));
    assert_eq!(app.search.selected_index, 2);

    // Down wraps to 0
    let _ = handle_event(&mut app, key_press(KeyCode::Down));
    assert_eq!(app.search.selected_index, 0);

    // Up wraps to end
    let _ = handle_event(&mut app, key_press(KeyCode::Up));
    assert_eq!(app.search.selected_index, 2);
}

// ── Enter behavior tests ─────────────────────────────────────────────

#[test]
fn test_enter_with_results_selects_and_closes() {
    let mut app = test_app();
    app.search.show = true;
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "auth".to_string(),
        message_type: "text".to_string(),
        snippet: "hello".to_string(),
    }];
    app.search.selected_index = 0;
    app.selected_channel = "midtown".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Enter));
    assert!(matches!(result, EventResult::Continue));
    assert!(!app.search.show, "Enter should close search");
    assert_eq!(
        app.selected_channel, "auth",
        "Enter should switch to the result's channel"
    );
    assert_eq!(
        app.board_selection,
        Some(app::BoardSelection::Channel("auth".to_string())),
        "board_selection should match"
    );
}

// ── Toggle tests ─────────────────────────────────────────────────────

#[test]
fn test_toggle_search_opens_and_closes() {
    let mut app = test_app();

    app.toggle_search();
    assert!(app.search.show, "toggle_search should open");

    app.search.input = "query".to_string();
    app.toggle_search();
    assert!(!app.search.show, "toggle_search should close");
    assert!(
        app.search.input.is_empty(),
        "toggle_search should clear input"
    );
}

// ── Selection bounds tests ───────────────────────────────────────────

#[test]
fn test_search_select_prev_on_empty_results() {
    let mut app = test_app();
    app.search.show = true;
    app.search.results.clear();
    app.search.selected_index = 0;

    // Should not panic or change index
    app.search_select_prev();
    assert_eq!(app.search.selected_index, 0);
}

#[test]
fn test_search_select_next_on_empty_results() {
    let mut app = test_app();
    app.search.show = true;
    app.search.results.clear();
    app.search.selected_index = 0;

    // Should not panic or change index
    app.search_select_next();
    assert_eq!(app.search.selected_index, 0);
}

#[test]
fn test_dismiss_search_clears_all_state() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input = "query".to_string();
    app.search.selected_index = 5;
    app.search.loading = true;
    app.search.error = Some("error".to_string());

    app.dismiss_search();

    assert!(!app.search.show);
    assert!(app.search.input.is_empty());
    assert_eq!(app.search.selected_index, 0);
    assert!(!app.search.loading);
    assert!(app.search.error.is_none());
    assert!(app.search.results.is_empty());
}

// ── execute_search tests ─────────────────────────────────────────────

#[test]
fn test_execute_search_empty_query_clears_results() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input = "   ".to_string(); // whitespace-only
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "old result".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "general".to_string(),
        message_type: "text".to_string(),
        snippet: "old result".to_string(),
    }];
    app.search.selected_index = 0;

    app.execute_search();

    assert!(
        app.search.results.is_empty(),
        "Empty query should clear results"
    );
    assert_eq!(app.search.selected_index, 0);
}

#[test]
fn test_execute_search_with_query_runs_search() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input = "hello".to_string();

    // In test mode, the project dir won't have channels, so we get empty results
    app.execute_search();

    assert!(
        !app.search.loading,
        "Loading should be cleared after search"
    );
    // Either results are empty (no channels dir) or error is set — both are valid
    // The important thing is that loading was properly set and cleared
    assert_eq!(app.search.selected_index, 0);
}

#[test]
fn test_enter_with_no_results_executes_search() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input = "query".to_string();
    app.search.results.clear();

    let result = handle_event(&mut app, key_press(KeyCode::Enter));
    assert!(matches!(result, EventResult::Continue));
    // Enter with no results should trigger execute_search()
    // (search completes synchronously, results may be empty due to no channels)
    assert!(!app.search.loading);
}

// ── search_select tests ──────────────────────────────────────────────

#[test]
fn test_search_select_no_op_when_hidden() {
    let mut app = test_app();
    app.search.show = false;
    app.selected_channel = "original".to_string();

    app.search_select();

    assert_eq!(
        app.selected_channel, "original",
        "search_select should no-op when hidden"
    );
}

#[test]
fn test_search_select_no_op_when_empty_results() {
    let mut app = test_app();
    app.search.show = true;
    app.search.results.clear();
    app.selected_channel = "original".to_string();

    app.search_select();

    assert_eq!(
        app.selected_channel, "original",
        "search_select should no-op with empty results"
    );
}

#[test]
fn test_search_select_calls_load_channel_messages() {
    let mut app = test_app();
    app.search.show = true;
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "frontend".to_string(),
        message_type: "text".to_string(),
        snippet: "hello".to_string(),
    }];
    app.search.selected_index = 0;

    assert!(!app.load_channel_messages_called);

    app.search_select();

    assert!(
        app.load_channel_messages_called,
        "search_select must call load_channel_messages()"
    );
    assert_eq!(app.selected_channel, "frontend");
    assert!(!app.search.show, "search_select should close overlay");
}

// ── Ctrl key guard tests ─────────────────────────────────────────────

#[test]
fn test_ctrl_u_is_no_op_when_search_open() {
    let mut app = test_app();
    app.search.show = true;
    app.focused_pane = app::FocusedPane::InputBar;
    app.input_text = "some text".to_string();
    app.input_cursor = 4;

    let event = Event::Key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    let result = handle_event(&mut app, event);
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.input_text, "some text",
        "Ctrl+U should not modify input when search is open"
    );
}

#[test]
fn test_ctrl_d_is_no_op_when_search_open() {
    let mut app = test_app();
    app.search.show = true;
    app.focused_pane = app::FocusedPane::InputBar;
    app.input_text = "some text".to_string();
    app.input_cursor = 2;

    let event = Event::Key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    let result = handle_event(&mut app, event);
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.input_text, "some text",
        "Ctrl+D should not modify input when search is open"
    );
}

#[test]
fn test_ctrl_w_is_no_op_when_search_open() {
    let mut app = test_app();
    app.search.show = true;
    app.focused_pane = app::FocusedPane::InputBar;
    app.input_text = "hello world".to_string();
    app.input_cursor = 11;

    let event = Event::Key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
    let result = handle_event(&mut app, event);
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.input_text, "hello world",
        "Ctrl+W should not modify input when search is open"
    );
}

// ── Paste routing test ───────────────────────────────────────────────

#[test]
fn test_paste_routes_to_search_input() {
    let mut app = test_app();
    app.search.show = true;
    app.search.input = "hel".to_string();

    let event = Event::Paste("lo world".to_string());
    let result = handle_event(&mut app, event);
    assert!(matches!(result, EventResult::Continue));
    assert_eq!(
        app.search.input, "hello world",
        "Pasted text should be appended to search input"
    );
}

// ── search_input/search_backspace guard tests ────────────────────────

#[test]
fn test_search_input_no_op_when_hidden() {
    let mut app = test_app();
    app.search.show = false;
    app.search.input.clear();

    app.search_input('x');
    assert!(
        app.search.input.is_empty(),
        "search_input should no-op when hidden"
    );
}

#[test]
fn test_search_backspace_no_op_when_hidden() {
    let mut app = test_app();
    app.search.show = false;
    app.search.input = "test".to_string();

    app.search_backspace();
    assert_eq!(
        app.search.input, "test",
        "search_backspace should no-op when hidden"
    );
}

// ── Stale results tests ──────────────────────────────────────────────

#[test]
fn test_enter_re_executes_when_query_changed() {
    let mut app = test_app();
    app.search.show = true;
    // Simulate: searched for "hello", got results, then changed input to "world"
    app.search.last_query = "hello".to_string();
    app.search.input = "world".to_string();
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "general".to_string(),
        message_type: "text".to_string(),
        snippet: "hello".to_string(),
    }];
    app.search.selected_index = 0;
    app.selected_channel = "midtown".to_string();

    let result = handle_event(&mut app, key_press(KeyCode::Enter));
    assert!(matches!(result, EventResult::Continue));

    // Should have re-executed (not selected the old "general" result)
    // After execute_search, last_query should now be "world"
    assert_eq!(app.search.last_query, "world");
    // The old result should NOT have caused a channel switch to "general"
    assert_eq!(
        app.selected_channel, "midtown",
        "Stale results should not navigate; search should re-execute instead"
    );
}

#[test]
fn test_results_are_stale_detects_changed_query() {
    let mut app = test_app();
    app.search.show = true;
    app.search.last_query = "hello".to_string();
    app.search.input = "world".to_string();
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "general".to_string(),
        message_type: "text".to_string(),
        snippet: "hello".to_string(),
    }];

    assert!(
        app.search.results_are_stale(),
        "Results should be stale when input differs from last_query"
    );
}

#[test]
fn test_results_are_not_stale_when_query_matches() {
    let mut app = test_app();
    app.search.show = true;
    app.search.last_query = "hello".to_string();
    app.search.input = "hello".to_string();
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "hello".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "general".to_string(),
        message_type: "text".to_string(),
        snippet: "hello".to_string(),
    }];

    assert!(
        !app.search.results_are_stale(),
        "Results should not be stale when input matches last_query"
    );
}

// ── Archived channel stripping test ──────────────────────────────────

#[test]
fn test_search_select_strips_archived_suffix() {
    let mut app = test_app();
    app.search.show = true;
    app.search.results = vec![midtown::search::SearchResult {
        id: "1".to_string(),
        from: "alice".to_string(),
        content: "old message".to_string(),
        timestamp: "2025-01-01T00:00:00Z".to_string(),
        channel: "auth.archived".to_string(), // Archived channel from rg
        message_type: "text".to_string(),
        snippet: "old message".to_string(),
    }];
    app.search.selected_index = 0;

    app.search_select();

    assert_eq!(
        app.selected_channel, "auth",
        "search_select should strip .archived suffix from channel name"
    );
    assert_eq!(
        app.board_selection,
        Some(app::BoardSelection::Channel("auth".to_string())),
        "board_selection should use stripped channel name"
    );
}
