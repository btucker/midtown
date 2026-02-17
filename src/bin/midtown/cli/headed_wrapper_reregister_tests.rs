use super::is_lease_error;

#[test]
fn test_is_lease_error_detects_no_active_adapter() {
    assert!(is_lease_error(
        "No active headed adapter for session 'lead'"
    ));
    assert!(is_lease_error("No active headed adapter"));
}

#[test]
fn test_is_lease_error_detects_lease_expired() {
    assert!(is_lease_error("lease expired for adapter abc-123"));
    assert!(is_lease_error("lease expired"));
}

#[test]
fn test_is_lease_error_ignores_other_errors() {
    assert!(!is_lease_error("connection refused"));
    assert!(!is_lease_error("timeout"));
    assert!(!is_lease_error(""));
    assert!(!is_lease_error("invalid response"));
    assert!(!is_lease_error("daemon not running"));
}

#[test]
fn test_is_lease_error_case_sensitive() {
    // The error strings from the daemon are lowercase — case sensitivity is intentional
    assert!(!is_lease_error("no active headed adapter"));
    assert!(!is_lease_error("Lease Expired"));
}
