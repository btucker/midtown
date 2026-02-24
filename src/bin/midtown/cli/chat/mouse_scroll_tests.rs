//! Tests for mouse wheel scroll accumulator behavior in the main chat and thread panels.

use midtown::Message;

use super::tests::test_app;
use super::{MOUSE_SCROLL_STEP, MOUSE_SCROLL_THRESHOLD, SCROLL_STEP};

#[test]
fn test_inertia_scrolling_with_occasional_reversals() {
    // Regression test: with threshold=8, trackpad inertia causes scroll to fail entirely.
    // Inertia produces mostly up events but occasional small reversals (down). When a
    // reversal resets the accumulator mid-count, 7 up + 1 down + 7 up + 1 down = no scroll.
    // With threshold=3 this pattern produces multiple scrolls.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 0;

    // Simulate inertia: 7 up events, then 1 reversal, repeated 5 times.
    // With threshold=8 this never scrolls (accumulator resets to 0 on every reversal at count 7).
    // With threshold<=7 we complete at least one scroll despite the reversals.
    for _ in 0..5 {
        for _ in 0..7 {
            app.mouse_scroll_up();
        }
        app.mouse_scroll_down(); // inertia reversal resets accumulator
    }

    assert!(
        app.scroll_offset > 0,
        "Should have scrolled despite inertia reversals (7 up + 1 down, repeated)"
    );
}

#[test]
fn test_mouse_scroll_accumulator() {
    // Mouse scrolling requires MOUSE_SCROLL_THRESHOLD same-direction events per step.
    // Each step moves MOUSE_SCROLL_STEP lines (finer than keyboard SCROLL_STEP).
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("Test message {}", i)));
    }
    app.scroll_offset = 0;

    // (threshold - 1) events should not scroll
    let sub_threshold = MOUSE_SCROLL_THRESHOLD as usize - 1;
    for _ in 0..sub_threshold {
        app.mouse_scroll_up();
    }
    assert_eq!(
        app.scroll_offset, 0,
        "Should not scroll with <MOUSE_SCROLL_THRESHOLD events"
    );

    // threshold-th event triggers scroll by MOUSE_SCROLL_STEP
    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset, MOUSE_SCROLL_STEP,
        "Should scroll MOUSE_SCROLL_STEP after threshold events"
    );

    // Accumulator resets; another MOUSE_SCROLL_THRESHOLD events needed
    for _ in 0..sub_threshold {
        app.mouse_scroll_up();
    }
    assert_eq!(
        app.scroll_offset, MOUSE_SCROLL_STEP,
        "Should not scroll with <threshold events after reset"
    );
    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset,
        MOUSE_SCROLL_STEP * 2,
        "Should scroll another step after threshold more events"
    );

    // Scroll down: accumulator is 0 after last scroll trigger.
    // Need MOUSE_SCROLL_THRESHOLD down events to trigger scroll down.
    let current_offset = app.scroll_offset;
    for _ in 0..sub_threshold {
        app.mouse_scroll_down();
    }
    assert_eq!(
        app.scroll_offset, current_offset,
        "Should not scroll down with <threshold events"
    );
    app.mouse_scroll_down();
    assert_eq!(
        app.scroll_offset,
        current_offset - MOUSE_SCROLL_STEP,
        "Scroll down should work after threshold events"
    );
}

#[test]
fn test_mouse_scroll_accumulator_resets_on_direction_change() {
    // When direction changes mid-batch, credits in the old direction are discarded.
    // (threshold-1) up + (threshold-1) down should NOT fire a scroll.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 10;

    let sub_threshold = MOUSE_SCROLL_THRESHOLD as usize - 1;

    // sub_threshold up events — not enough to trigger
    for _ in 0..sub_threshold {
        app.mouse_scroll_up();
    }
    assert_eq!(
        app.scroll_offset, 10,
        "sub_threshold up events should not scroll yet"
    );

    // sub_threshold down events — direction changed on first, so accumulator resets to 0,
    // then accumulates sub_threshold-1 more. Still not enough to trigger.
    for _ in 0..sub_threshold {
        app.mouse_scroll_down();
    }
    assert_eq!(
        app.scroll_offset, 10,
        "Direction change must reset accumulator; sub_threshold down after sub_threshold up should not scroll"
    );
}

#[test]
fn test_mouse_wheel_scroll_is_slower_than_keyboard() {
    // Mouse uses a finer step (MOUSE_SCROLL_STEP) and requires MOUSE_SCROLL_THRESHOLD
    // events per step, vs keyboard which scrolls SCROLL_STEP per call.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 0;

    // Keyboard moves SCROLL_STEP per call
    app.scroll_up();
    assert_eq!(
        app.scroll_offset, SCROLL_STEP,
        "Keyboard scroll = SCROLL_STEP"
    );

    // Reset
    app.scroll_offset = 0;

    // Mouse takes MOUSE_SCROLL_THRESHOLD events to move MOUSE_SCROLL_STEP lines
    let sub_threshold = MOUSE_SCROLL_THRESHOLD as usize - 1;
    for i in 1..=sub_threshold {
        app.mouse_scroll_up();
        assert_eq!(app.scroll_offset, 0, "Event {} shouldn't scroll yet", i);
    }
    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset, MOUSE_SCROLL_STEP,
        "{}th event should scroll MOUSE_SCROLL_STEP lines",
        MOUSE_SCROLL_THRESHOLD
    );
}

#[test]
fn test_thread_scroll_up_moves_offset() {
    // thread_mouse_scroll_up should increase thread_scroll_offset (scroll toward older messages)
    // and must NOT affect the main scroll_offset.
    let mut app = test_app();
    app.scroll_offset = 5;
    app.thread_scroll_offset = 0;

    for _ in 0..MOUSE_SCROLL_THRESHOLD as usize {
        app.thread_mouse_scroll_up();
    }
    assert_eq!(
        app.thread_scroll_offset, MOUSE_SCROLL_STEP,
        "Thread scroll offset should increase after MOUSE_SCROLL_THRESHOLD up events"
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
    app.thread_scroll_offset = MOUSE_SCROLL_STEP;

    for _ in 0..MOUSE_SCROLL_THRESHOLD as usize {
        app.thread_mouse_scroll_down();
    }
    assert_eq!(
        app.thread_scroll_offset, 0,
        "Thread scroll offset should return to 0 after MOUSE_SCROLL_THRESHOLD down events"
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

    // Earn (threshold-1) up credits in main chat — not enough to fire
    let sub_threshold = MOUSE_SCROLL_THRESHOLD as usize - 1;
    for _ in 0..sub_threshold {
        app.mouse_scroll_up();
    }
    assert_eq!(
        app.scroll_offset, 5,
        "sub_threshold main-chat events should not scroll"
    );

    // 1 up event in the thread panel — should NOT fire because the thread
    // accumulator starts at 0, independent of main's partial count.
    app.thread_mouse_scroll_up();
    assert_eq!(
        app.thread_scroll_offset, 0,
        "1 thread event should not scroll; thread accumulator is independent"
    );
    assert_eq!(app.scroll_offset, 5, "Main chat must remain unchanged");
}
