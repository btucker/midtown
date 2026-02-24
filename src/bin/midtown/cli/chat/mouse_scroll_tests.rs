//! Tests for mouse wheel scroll behavior in the main chat and thread panels.
//!
//! These tests cover two modes:
//! - **Immediate mode**: Events arriving > MOUSE_INERTIA_THRESHOLD apart are treated as
//!   deliberate mouse-wheel clicks and scroll SCROLL_STEP lines immediately.
//! - **Inertia/accumulator mode**: Events arriving ≤ MOUSE_INERTIA_THRESHOLD apart (trackpad
//!   or momentum scrolling) use the accumulator; MOUSE_SCROLL_THRESHOLD events = MOUSE_SCROLL_STEP.

use std::time::{Duration, Instant};

use midtown::Message;

use super::tests::test_app;
use super::{MOUSE_INERTIA_THRESHOLD, MOUSE_SCROLL_STEP, MOUSE_SCROLL_THRESHOLD, SCROLL_STEP};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Make `app.last_scroll_event` look like it was set "just now" so subsequent
/// mouse scroll calls enter inertia/accumulator mode (elapsed ≤ threshold).
fn set_recent_scroll(app: &mut super::App) {
    app.last_scroll_event = Some(Instant::now());
}

/// Make `app.last_thread_scroll_event` look like it was set "just now".
fn set_recent_thread_scroll(app: &mut super::App) {
    app.last_thread_scroll_event = Some(Instant::now());
}

/// Make `app.last_scroll_event` look like it happened long ago so subsequent
/// mouse scroll calls enter immediate mode (elapsed > threshold).
fn set_old_scroll(app: &mut super::App) {
    app.last_scroll_event =
        Some(Instant::now() - MOUSE_INERTIA_THRESHOLD - Duration::from_millis(10));
}

/// Make `app.last_thread_scroll_event` look like it happened long ago.
fn set_old_thread_scroll(app: &mut super::App) {
    app.last_thread_scroll_event =
        Some(Instant::now() - MOUSE_INERTIA_THRESHOLD - Duration::from_millis(10));
}

// ---------------------------------------------------------------------------
// Immediate mode (deliberate mouse-wheel click)
// ---------------------------------------------------------------------------

#[test]
fn test_mouse_scroll_immediate_when_no_recent_event() {
    // First-ever scroll event (last_scroll_event = None): treated as deliberate → SCROLL_STEP.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.scroll_offset = 0;
    // last_scroll_event starts as None in test_app

    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset, SCROLL_STEP,
        "First scroll event (no prior event) should scroll SCROLL_STEP immediately"
    );
}

#[test]
fn test_mouse_scroll_immediate_when_last_event_was_old() {
    // Event arriving > MOUSE_INERTIA_THRESHOLD after the previous one → deliberate click.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.scroll_offset = 0;
    set_old_scroll(&mut app);

    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset, SCROLL_STEP,
        "Slow event (> threshold apart) should scroll SCROLL_STEP immediately"
    );
}

#[test]
fn test_mouse_scroll_down_immediate() {
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {i}")));
    }
    app.scroll_offset = SCROLL_STEP * 2;
    set_old_scroll(&mut app);

    app.mouse_scroll_down();
    assert_eq!(
        app.scroll_offset, SCROLL_STEP,
        "Slow scroll-down event should decrease offset by SCROLL_STEP"
    );
}

// ---------------------------------------------------------------------------
// Inertia / accumulator mode (trackpad momentum)
// ---------------------------------------------------------------------------

#[test]
fn test_inertia_scrolling_with_occasional_reversals() {
    // Regression test: with threshold=8, trackpad inertia caused scroll to fail entirely.
    // Inertia produces mostly up events but occasional small reversals (down). When a
    // reversal resets the accumulator mid-count before threshold is reached, scroll never fires.
    // With MOUSE_SCROLL_THRESHOLD=3 this pattern still produces scrolls per cycle.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 0;
    set_recent_scroll(&mut app);

    // Simulate inertia: MOUSE_SCROLL_THRESHOLD+1 up events (fires once at event THRESHOLD,
    // then 1 more starts the next batch), then 1 reversal (resets that partial credit).
    let cycle_up = MOUSE_SCROLL_THRESHOLD as usize + 1;
    for _ in 0..5 {
        for _ in 0..cycle_up {
            app.mouse_scroll_up();
        }
        app.mouse_scroll_down(); // inertia reversal resets accumulator
    }

    assert!(
        app.scroll_offset > 0,
        "Should have scrolled despite inertia reversals (MOUSE_SCROLL_THRESHOLD+1 up + 1 down, repeated)"
    );
}

#[test]
fn test_mouse_scroll_accumulator() {
    // Mouse scrolling in inertia mode requires MOUSE_SCROLL_THRESHOLD same-direction events.
    // Each step moves MOUSE_SCROLL_STEP lines (finer than keyboard SCROLL_STEP).
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("Test message {}", i)));
    }
    app.scroll_offset = 0;
    set_recent_scroll(&mut app);

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
    // When direction changes, up credits are discarded. The first down event resets the
    // positive accumulator to 0 and counts as -1 simultaneously. The scroll should fire
    // on exactly the MOUSE_SCROLL_THRESHOLD-th down event — not before.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 10;
    set_recent_scroll(&mut app);

    let sub_threshold = MOUSE_SCROLL_THRESHOLD as usize - 1;

    // Accumulate sub_threshold up credits (not enough to fire)
    for _ in 0..sub_threshold {
        app.mouse_scroll_up();
    }
    assert_eq!(
        app.scroll_offset, 10,
        "sub_threshold up events should not scroll"
    );

    // Down events: first resets the up credits and counts as -1; subsequent events
    // continue accumulating. Scroll fires exactly at the MOUSE_SCROLL_THRESHOLD-th event.
    for i in 1..MOUSE_SCROLL_THRESHOLD as usize {
        app.mouse_scroll_down();
        assert_eq!(
            app.scroll_offset, 10,
            "Down event {} should not scroll yet (need {} total to fire)",
            i, MOUSE_SCROLL_THRESHOLD
        );
    }
    // The MOUSE_SCROLL_THRESHOLD-th down event completes a full batch and fires
    app.mouse_scroll_down();
    assert_eq!(
        app.scroll_offset,
        10 - MOUSE_SCROLL_STEP,
        "Scroll must fire on exactly the {}th down event after direction reset",
        MOUSE_SCROLL_THRESHOLD
    );
}

#[test]
fn test_mouse_wheel_scroll_is_slower_than_keyboard_in_inertia_mode() {
    // In inertia mode, mouse uses a finer step (MOUSE_SCROLL_STEP) and requires
    // MOUSE_SCROLL_THRESHOLD events per step, vs keyboard which scrolls SCROLL_STEP per call.
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

    // Mouse in inertia mode takes MOUSE_SCROLL_THRESHOLD events to move MOUSE_SCROLL_STEP lines
    set_recent_scroll(&mut app);
    let sub_threshold = MOUSE_SCROLL_THRESHOLD as usize - 1;
    for i in 1..=sub_threshold {
        app.mouse_scroll_up();
        assert_eq!(app.scroll_offset, 0, "Event {} shouldn't scroll yet", i);
    }
    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset, MOUSE_SCROLL_STEP,
        "{}th event should scroll MOUSE_SCROLL_STEP lines in inertia mode",
        MOUSE_SCROLL_THRESHOLD
    );
}

#[test]
fn test_mouse_wheel_scroll_equals_keyboard_in_immediate_mode() {
    // In immediate mode (slow events), mouse scrolls by SCROLL_STEP just like keyboard.
    let mut app = test_app();
    for i in 0..30 {
        app.messages
            .push_back(Message::text("test", format!("msg {}", i)));
    }
    app.scroll_offset = 0;

    set_old_scroll(&mut app);
    app.mouse_scroll_up();
    assert_eq!(
        app.scroll_offset, SCROLL_STEP,
        "Immediate-mode mouse scroll should equal keyboard SCROLL_STEP"
    );
}

// ---------------------------------------------------------------------------
// Thread panel scroll
// ---------------------------------------------------------------------------

#[test]
fn test_thread_scroll_up_moves_offset() {
    // thread_mouse_scroll_up should increase thread_scroll_offset (scroll toward older messages)
    // and must NOT affect the main scroll_offset.
    let mut app = test_app();
    app.scroll_offset = 5;
    app.thread_scroll_offset = 0;
    set_recent_thread_scroll(&mut app);

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
    set_recent_thread_scroll(&mut app);

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
    set_recent_scroll(&mut app);
    set_recent_thread_scroll(&mut app);

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

#[test]
fn test_thread_immediate_mode() {
    // Thread panel also uses immediate mode for slow events.
    let mut app = test_app();
    app.thread_scroll_offset = 0;
    set_old_thread_scroll(&mut app);

    app.thread_mouse_scroll_up();
    assert_eq!(
        app.thread_scroll_offset, SCROLL_STEP,
        "Thread immediate-mode scroll should move by SCROLL_STEP"
    );
}
