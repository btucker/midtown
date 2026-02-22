use super::derive_thread_id;

#[test]
fn derive_thread_id_prefers_cli_value() {
    let result = derive_thread_id(Some("cli-thread"), Some("env-thread"));
    assert_eq!(result.as_deref(), Some("cli-thread"));
}

#[test]
fn derive_thread_id_uses_env_when_cli_missing() {
    let result = derive_thread_id(None, Some("env-thread"));
    assert_eq!(result.as_deref(), Some("env-thread"));
}

#[test]
fn derive_thread_id_falls_back_when_cli_empty() {
    let result = derive_thread_id(Some("  "), Some("env-thread"));
    assert_eq!(result.as_deref(), Some("env-thread"));
}

#[test]
fn derive_thread_id_returns_none_when_no_values() {
    let result = derive_thread_id(None, None);
    assert!(result.is_none());
}

#[test]
fn derive_thread_id_ignores_empty_env_value() {
    let result = derive_thread_id(None, Some("   "));
    assert!(result.is_none());
}

#[test]
fn derive_thread_id_preserves_original_cli_value() {
    let raw = " thread-123 ";
    let result = derive_thread_id(Some(raw), None);
    assert_eq!(result.as_deref(), Some(raw));
}
