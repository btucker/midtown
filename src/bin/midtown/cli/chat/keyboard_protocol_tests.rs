//! Tests for keyboard protocol configuration.
//!
//! Verifies that the KEYBOARD_ENHANCEMENT_FLAGS constant includes the
//! required flags for proper key decoding via the kitty protocol.

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

/// Verify that REPORT_ALL_KEYS_AS_ESCAPE_CODES is NOT enabled.
///
/// This flag causes the kitty protocol to report ALL keys as escape codes
/// with the base (lowercase/unshifted) key plus modifier flags. This breaks
/// shifted character input: Shift+A reports 'a'+SHIFT instead of 'A', and
/// Shift+1 reports '1'+SHIFT instead of '!'. The terminal can no longer
/// perform keyboard-layout-aware character translation.
#[test]
fn test_enhancement_flags_exclude_report_all_keys() {
    assert!(
        !super::KEYBOARD_ENHANCEMENT_FLAGS
            .contains(KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES),
        "KEYBOARD_ENHANCEMENT_FLAGS must NOT include REPORT_ALL_KEYS_AS_ESCAPE_CODES"
    );
}
