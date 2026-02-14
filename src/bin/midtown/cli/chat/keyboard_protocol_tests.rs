//! Tests for keyboard protocol configuration.
//!
//! Verifies that the KEYBOARD_ENHANCEMENT_FLAGS constant includes the
//! required flags for proper Shift+Enter detection via the kitty protocol.

use crossterm::event::KeyboardEnhancementFlags;

/// Verify that the enhancement flags constant includes DISAMBIGUATE_ESCAPE_CODES.
///
/// Without this flag, terminals send ambiguous escape sequences for special
/// keys and crossterm may decode them incorrectly (e.g., Shift+Enter as 'j').
#[test]
fn test_enhancement_flags_include_disambiguate() {
    assert!(
        super::KEYBOARD_ENHANCEMENT_FLAGS
            .contains(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
        "KEYBOARD_ENHANCEMENT_FLAGS must include DISAMBIGUATE_ESCAPE_CODES"
    );
}

/// Verify that the enhancement flags constant includes REPORT_ALL_KEYS_AS_ESCAPE_CODES.
///
/// Without this flag, modifier state (Shift, Ctrl, etc.) is not reliably
/// reported for all key combinations.
#[test]
fn test_enhancement_flags_include_report_all_keys() {
    assert!(
        super::KEYBOARD_ENHANCEMENT_FLAGS
            .contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES),
        "KEYBOARD_ENHANCEMENT_FLAGS must include REPORT_ALL_KEYS_AS_ESCAPE_CODES"
    );
}
