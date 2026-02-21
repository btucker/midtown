//! Tests for mouse wheel scroll accumulator behavior in the main chat and thread panels.

use midtown::Message;

use super::SCROLL_STEP;
use super::tests::test_app;

#[test]
fn test_mouse_scroll_accumulator() {
    // Test that mouse wheel scrolling requires multiple events per line
    // for smooth scrolling (reduces scroll speed compared to keyboard).
    // Each 8 mouse events triggers one scroll_up/down which moves by SCROLL_STEP.
    let mut app = test_app();

    // Add enough messages to make scrolling possible
    // visible_height = 20, so we need > 20 messages
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("Test message {}", i)));
    }

    // Start at the bottom (scroll_offset = 0)
    app.scroll_offset = 0;

    // Test scroll up: should require 8 events to scroll SCROLL_STEP lines
    let initial_offset = app.scroll_offset;

    // First 7 events should not scroll
    for _ in 0..7 {
        app.mouse_scroll_up();
    }
    assert_eq!(
        app.scroll_offset, initial_offset,
        "Should not scroll with <8 events"
    );

    // 8th event should trigger scroll by SCROLL_STEP
    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset,
        initial_offset + SCROLL_STEP,
        "Should scroll after 8 events"
    );

    // Accumulator should reset, so another 8 events needed
    for _ in 0..7 {
        app.mouse_scroll_up();
    }
    assert_eq!(
        app.scroll_offset,
        initial_offset + SCROLL_STEP,
        "Should not scroll with <8 events after reset"
    );

    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset,
        initial_offset + SCROLL_STEP * 2,
        "Should scroll after another 8 events"
    );

    // Test scroll down
    let current_offset = app.scroll_offset;
    for _ in 0..8 {
        app.mouse_scroll_down();
    }
    assert_eq!(
        app.scroll_offset,
        current_offset - SCROLL_STEP,
        "Scroll down should work after 8 events"
    );
}

#[test]
fn test_mouse_scroll_accumulator_resets_on_direction_change() {
    // Bug: accumulator was shared between up/down directions.
    // 4 up events + 4 down events should NOT fire a scroll because
    // the direction changed — each direction must start fresh.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 10;

    // 4 up events — not enough to trigger (need 8)
    for _ in 0..4 {
        app.mouse_scroll_up();
    }
    assert_eq!(app.scroll_offset, 10, "4 up events should not scroll yet");

    // 4 down events — direction changed, so accumulator resets to 0 first,
    // then accumulates to 4. Should still NOT fire scroll.
    for _ in 0..4 {
        app.mouse_scroll_down();
    }
    assert_eq!(
        app.scroll_offset, 10,
        "Direction change must reset accumulator; 4 down after 4 up should not scroll"
    );
}

#[test]
fn test_thread_scroll_up_moves_offset() {
    // thread_mouse_scroll_up should increase thread_scroll_offset (scroll toward older messages)
    // and must NOT affect the main scroll_offset.
    let mut app = test_app();
    app.scroll_offset = 5;
    app.thread_scroll_offset = 0;

    // 8 events should move by SCROLL_STEP
    for _ in 0..8 {
        app.thread_mouse_scroll_up();
    }
    assert_eq!(
        app.thread_scroll_offset, SCROLL_STEP,
        "Thread scroll offset should increase after 8 up events"
    );
    assert_eq!(
        app.scroll_offset, 5,
        "Main scroll_offset must not change when scrolling thread"
    );
}

#[test]
fn test_thread_scroll_down_moves_offset() {
    // thread_mouse_scroll_down should decrease thread_scroll_offset (back toward newest)
    let mut app = test_app();
    app.thread_scroll_offset = SCROLL_STEP;

    for _ in 0..8 {
        app.thread_mouse_scroll_down();
    }
    assert_eq!(
        app.thread_scroll_offset, 0,
        "Thread scroll offset should return to 0 after 8 down events"
    );
}

#[test]
fn test_thread_scroll_accumulator_independent_from_main() {
    // The thread panel's scroll accumulator must be independent of the main chat's.
    // Partial credits earned while scrolling one panel must not bleed into the other.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 5;
    app.thread_scroll_offset = 0;

    // Earn 7 up credits in main chat (not enough to fire)
    for _ in 0..7 {
        app.mouse_scroll_up();
    }
    assert_eq!(app.scroll_offset, 5, "7 main-chat events should not scroll");

    // Now do 1 up event in the thread panel — should NOT fire because the thread
    // accumulator starts at 0, not at 7.
    app.thread_mouse_scroll_up();
    assert_eq!(
        app.thread_scroll_offset, 0,
        "1 thread event should not scroll; thread accumulator is independent"
    );
    assert_eq!(app.scroll_offset, 5, "Main chat must remain unchanged");
}
