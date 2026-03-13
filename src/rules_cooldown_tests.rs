use super::*;

/// Demonstrates the TOCTOU race with separate check()+record() calls.
///
/// When two callers both check() before either records(), both see
/// the cooldown as expired and proceed — a duplicate action.
/// check_and_record() prevents this by atomically checking and recording.
#[test]
fn check_and_record_is_atomic_vs_separate_check_record() {
    let tracker = CooldownTracker::new();
    let duration = Duration::from_secs(60);

    // Simulate TOCTOU with separate check()/record():
    // Both "callers" check before either records.
    let caller_a_sees = tracker.check("nudge", "msg-1", duration);
    let caller_b_sees = tracker.check("nudge", "msg-1", duration);
    // Both see true — race condition allows duplicate action
    assert!(caller_a_sees, "caller A should see cooldown expired");
    assert!(caller_b_sees, "caller B also sees expired (TOCTOU window)");

    // Now verify check_and_record() prevents this:
    let mut tracker2 = CooldownTracker::new();
    let first = tracker2.check_and_record("nudge", "msg-1", duration);
    let second = tracker2.check_and_record("nudge", "msg-1", duration);
    assert!(first, "first call should succeed");
    assert!(!second, "second call should be blocked (atomic)");
}
