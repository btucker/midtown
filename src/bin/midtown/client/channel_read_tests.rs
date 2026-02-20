//! Tests for channel_read client method parameter construction.
//!
//! These tests verify that channel_read correctly assembles the JSON params
//! passed to the daemon RPC, particularly the `channel` field priority logic.

use std::sync::Mutex;

/// Guard for tests that mutate the `MIDTOWN_CHANNEL` env var.
/// Rust runs tests in parallel; without serialization, concurrent
/// set_var/remove_var calls cause flaky failures.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Helper to build the params JSON that channel_read would send, without
/// actually connecting to a socket. We replicate the param-assembly logic
/// so we can unit-test it in isolation.
fn build_channel_read_params(
    all: bool,
    last: Option<&usize>,
    since: Option<&str>,
    channel: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({ "all": all });
    if let Some(n) = last {
        params["last"] = serde_json::json!(n);
    }
    if let Some(duration) = since {
        params["since"] = serde_json::json!(duration);
    }
    let resolved_channel = channel
        .map(|s| s.to_string())
        .or_else(|| std::env::var("MIDTOWN_CHANNEL").ok());
    if let Some(ch) = resolved_channel {
        params["channel"] = serde_json::json!(ch);
    }
    params
}

#[test]
fn channel_read_explicit_channel_sets_param() {
    let params = build_channel_read_params(false, None, None, Some("ops"));
    assert_eq!(
        params["channel"].as_str(),
        Some("ops"),
        "Explicit --channel flag should set params[\"channel\"]"
    );
}

#[test]
fn channel_read_no_channel_no_env_omits_param() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::remove_var("MIDTOWN_CHANNEL") };
    let params = build_channel_read_params(false, None, None, None);
    assert!(
        params.get("channel").is_none(),
        "No --channel and no MIDTOWN_CHANNEL should omit the channel param"
    );
}

#[test]
fn channel_read_env_var_used_when_no_explicit_channel() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("MIDTOWN_CHANNEL", "infra") };
    let params = build_channel_read_params(false, None, None, None);
    unsafe { std::env::remove_var("MIDTOWN_CHANNEL") };
    assert_eq!(
        params["channel"].as_str(),
        Some("infra"),
        "MIDTOWN_CHANNEL env var should be used when no --channel flag is provided"
    );
}

#[test]
fn channel_read_explicit_channel_overrides_env_var() {
    let _guard = ENV_MUTEX.lock().unwrap();
    unsafe { std::env::set_var("MIDTOWN_CHANNEL", "main") };
    let params = build_channel_read_params(false, None, None, Some("ops"));
    unsafe { std::env::remove_var("MIDTOWN_CHANNEL") };
    assert_eq!(
        params["channel"].as_str(),
        Some("ops"),
        "Explicit --channel flag should take priority over MIDTOWN_CHANNEL env var"
    );
}

#[test]
fn channel_read_all_flag_included_in_params() {
    let params = build_channel_read_params(true, None, None, Some("ops"));
    assert_eq!(params["all"].as_bool(), Some(true));
    assert_eq!(params["channel"].as_str(), Some("ops"));
}

#[test]
fn channel_read_last_and_since_params_included() {
    let last = 10_usize;
    let params = build_channel_read_params(false, Some(&last), Some("5m"), Some("ops"));
    assert_eq!(params["last"].as_u64(), Some(10));
    assert_eq!(params["since"].as_str(), Some("5m"));
    assert_eq!(params["channel"].as_str(), Some("ops"));
}
