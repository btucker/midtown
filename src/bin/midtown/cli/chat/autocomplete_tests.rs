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
            progress: None,
            time_estimate: None,
        },
        CoworkerInfo {
            name: "pleasant".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
            progress: None,
            time_estimate: None,
        },
        CoworkerInfo {
            name: "lexington".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
            progress: None,
            time_estimate: None,
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
            progress: None,
            time_estimate: None,
        },
        CoworkerInfo {
            name: "pleasant".to_string(),
            task_id: None,
            phase: Some("idle".to_string()),
            pr_number: None,
            health: "green".to_string(),
            provider: "claude".to_string(),
            profile: "default".to_string(),
            progress: None,
            time_estimate: None,
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
        progress: None,
        time_estimate: None,
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
        progress: None,
        time_estimate: None,
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
        progress: None,
        time_estimate: None,
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

#[test]
fn test_channel_autocomplete_uses_prefix_matching() {
    // Issue #2 from review: Channel autocomplete should use prefix matching (consistent with @mentions)
    use midtown::Channel;
    use tempfile::TempDir;

    let mut app = test_app();

    // Create temporary channel directory
    let temp_dir = TempDir::new().unwrap();
    let base_dir = temp_dir.path().to_path_buf();

    // Create test channels in channels/ subdirectory
    let channels_dir = base_dir.join("channels");
    std::fs::create_dir_all(&channels_dir).unwrap();
    std::fs::write(channels_dir.join("midtown.jsonl"), "").unwrap();
    std::fs::write(channels_dir.join("madison.jsonl"), "").unwrap();
    std::fs::write(channels_dir.join("admin.jsonl"), "").unwrap();

    // Set up channel
    app.channel = Some(Channel::new(base_dir.clone(), "test").unwrap());

    // Test prefix matching: "m" should match "midtown" and "madison", not "admin"
    let items = app.get_channel_items("m");
    assert_eq!(items.len(), 2, "Should match 2 channels starting with 'm'");
    assert!(items.iter().any(|i| i.value == "#midtown"));
    assert!(items.iter().any(|i| i.value == "#madison"));
    assert!(!items.iter().any(|i| i.value == "#admin"));

    // Test "ad" should match only "admin", not "madison"
    let items = app.get_channel_items("ad");
    assert_eq!(items.len(), 1, "Should match 1 channel starting with 'ad'");
    assert_eq!(items[0].value, "#admin");
}

#[test]
fn test_task_autocomplete_uses_prefix_matching() {
    // Issue #2 from review: Task autocomplete should use prefix matching (consistent with @mentions)
    use super::{KanbanTask, TaskStatus};

    let mut app = test_app();

    // Add test tasks
    app.tasks = vec![
        KanbanTask {
            id: "1224".to_string(),
            subject: "Fix chat TUI autocomplete".to_string(),
            description: None,
            owner: None,
            status: TaskStatus::InProgress,
            modified_at: None,
            channel: None,
            blocked_by: Vec::new(),
            pr_number: None,
        },
        KanbanTask {
            id: "1234".to_string(),
            subject: "Add new feature".to_string(),
            description: None,
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: None,
            blocked_by: Vec::new(),
            pr_number: None,
        },
        KanbanTask {
            id: "2245".to_string(),
            subject: "Debug issue".to_string(),
            description: None,
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: None,
            blocked_by: Vec::new(),
            pr_number: None,
        },
    ];

    // Test prefix matching by ID: "12" should match 1224 and 1234, not 2245
    let items = app.get_task_items("12");
    assert_eq!(items.len(), 2, "Should match 2 tasks starting with '12'");
    assert!(items.iter().any(|i| i.value == "!1224"));
    assert!(items.iter().any(|i| i.value == "!1234"));
    assert!(!items.iter().any(|i| i.value == "!2245"));

    // Test prefix matching by subject: "fix" should match "Fix chat TUI", not "Debug" or "Add"
    let items = app.get_task_items("fix");
    assert_eq!(
        items.len(),
        1,
        "Should match 1 task with subject starting with 'fix'"
    );
    assert_eq!(items[0].value, "!1224");

    // Test prefix matching by subject: "add" should match "Add new feature"
    let items = app.get_task_items("add");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].value, "!1234");
}

#[test]
fn test_task_autocomplete_empty_query_shows_in_progress_first() {
    use super::{KanbanTask, TaskStatus};

    let mut app = test_app();

    app.tasks = vec![
        KanbanTask {
            id: "100".to_string(),
            subject: "Pending task A".to_string(),
            description: None,
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: None,
            blocked_by: Vec::new(),
            pr_number: None,
        },
        KanbanTask {
            id: "200".to_string(),
            subject: "In-progress task B".to_string(),
            description: None,
            owner: Some("park".to_string()),
            status: TaskStatus::InProgress,
            modified_at: None,
            channel: None,
            blocked_by: Vec::new(),
            pr_number: None,
        },
        KanbanTask {
            id: "300".to_string(),
            subject: "Pending task C".to_string(),
            description: None,
            owner: None,
            status: TaskStatus::Pending,
            modified_at: None,
            channel: None,
            blocked_by: Vec::new(),
            pr_number: None,
        },
        KanbanTask {
            id: "400".to_string(),
            subject: "In-progress task D".to_string(),
            description: None,
            owner: Some("madison".to_string()),
            status: TaskStatus::InProgress,
            modified_at: None,
            channel: None,
            blocked_by: Vec::new(),
            pr_number: None,
        },
    ];

    // Empty query should show all tasks, with in_progress first
    let items = app.get_task_items("");
    assert_eq!(items.len(), 4, "Should show all tasks");
    // First two should be in_progress tasks
    assert_eq!(items[0].value, "!200", "First should be in_progress task");
    assert_eq!(items[1].value, "!400", "Second should be in_progress task");
    // Then pending tasks
    assert_eq!(items[2].value, "!100", "Third should be pending task");
    assert_eq!(items[3].value, "!300", "Fourth should be pending task");
}

#[test]
fn test_slash_autocomplete_empty_query_shows_all_commands() {
    let app = test_app();

    // Empty query after "/" should show all commands
    let items = app.get_slash_items("");
    assert!(!items.is_empty(), "Should show commands for empty query");
    assert!(
        items.iter().any(|i| i.value == "/channel create"),
        "Should include /channel create"
    );
    assert!(items.iter().any(|i| i.value == "/me"), "Should include /me");
}

#[test]
fn test_slash_autocomplete_filters_by_prefix() {
    let app = test_app();

    // "ch" should match "/channel create"
    let items = app.get_slash_items("ch");
    assert_eq!(items.len(), 1, "Should match 1 command starting with 'ch'");
    assert_eq!(items[0].value, "/channel create");

    // "m" should match "/me"
    let items = app.get_slash_items("m");
    assert_eq!(items.len(), 1, "Should match 1 command starting with 'm'");
    assert_eq!(items[0].value, "/me");

    // "x" should match nothing
    let items = app.get_slash_items("x");
    assert!(items.is_empty(), "Should match no commands");
}

#[test]
fn test_slash_autocomplete_triggers_at_start_of_input() {
    let mut app = test_app();

    // Type "/" at start — should trigger slash autocomplete
    app.input_text = "/".to_string();
    app.input_cursor = 1;
    app.detect_autocomplete_trigger();

    assert!(
        app.autocomplete.show,
        "Autocomplete should show after '/' at start"
    );
    assert_eq!(app.autocomplete.trigger_type, Some('/'));
    assert_eq!(app.autocomplete.query, "");
}

#[test]
fn test_slash_autocomplete_does_not_trigger_mid_message() {
    let mut app = test_app();

    // "/" in the middle of text should NOT trigger slash autocomplete
    app.input_text = "hello /me".to_string();
    app.input_cursor = 9;
    app.detect_autocomplete_trigger();

    assert!(
        !app.autocomplete.show,
        "Autocomplete should NOT trigger for '/' after a space mid-message"
    );
}

#[test]
fn test_slash_autocomplete_query_updates_as_user_types() {
    let mut app = test_app();

    // Type "/ch" — query should be "ch", filtered to /channel create
    app.input_text = "/ch".to_string();
    app.input_cursor = 3;
    app.detect_autocomplete_trigger();

    assert!(app.autocomplete.show, "Autocomplete should show for '/ch'");
    assert_eq!(app.autocomplete.query, "ch");
    assert_eq!(app.autocomplete.items.len(), 1);
    assert_eq!(app.autocomplete.items[0].value, "/channel create");
}

#[test]
fn test_slash_autocomplete_insert_replaces_partial_command() {
    let mut app = test_app();

    // Set up: user typed "/ch" and selected "/channel create"
    app.input_text = "/ch".to_string();
    app.input_cursor = 3;
    app.autocomplete.show = true;
    app.autocomplete.trigger_type = Some('/');
    app.autocomplete.trigger_start_pos = 0;
    app.autocomplete.query = "ch".to_string();
    app.autocomplete.selected_index = 0;
    app.autocomplete.items = vec![super::AutocompleteItem {
        value: "/channel create".to_string(),
        description: Some("Create a new channel".to_string()),
    }];

    app.insert_autocomplete_item();

    assert_eq!(
        app.input_text, "/channel create ",
        "Should expand to full command with trailing space"
    );
    assert!(!app.autocomplete.show, "Autocomplete should be hidden");
}
