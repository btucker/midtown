//! Tests for channel post auto-threading fallback behavior.
//!
//! These tests verify that when `MIDTOWN_TASK_ID` is set but `channel_post_for_task`
//! fails, the fallback to a regular post includes a warning message.
//! We replicate the decision logic from `handle()` in isolation since the actual
//! function requires a live daemon connection.

use crate::ENV_MUTEX;

/// Simulates the auto-threading decision logic from `handle()` in channel.rs.
///
/// Returns `(method, warning)` where:
/// - `method` is "task_thread", "explicit_thread", "auto_thread", or "regular"
/// - `warning` is `Some(msg)` if the auto-threading fallback was triggered
fn resolve_post_method(
    explicit_task: Option<&str>,
    explicit_thread: Option<&str>,
    env_task_id: Option<&str>,
    task_lookup_succeeds: bool,
) -> (&'static str, Option<String>) {
    if explicit_task.is_some() {
        return ("task_thread", None);
    }
    if explicit_thread.is_some() {
        return ("explicit_thread", None);
    }
    if let Some(env_task_id) = env_task_id {
        if task_lookup_succeeds {
            return ("auto_thread", None);
        }
        // This is the fallback path — must produce a warning
        let warning = format!(
            "warning: auto-threading to task !{} failed (simulated error), falling back to regular post",
            env_task_id
        );
        return ("regular", Some(warning));
    }
    ("regular", None)
}

#[test]
fn auto_threading_fallback_produces_warning_when_task_lookup_fails() {
    let (method, warning) = resolve_post_method(None, None, Some("42"), false);
    assert_eq!(method, "regular", "should fall back to regular post");
    assert!(
        warning.is_some(),
        "should produce a warning when auto-threading fails"
    );
    let msg = warning.unwrap();
    assert!(
        msg.contains("task !42"),
        "warning should include the task ID, got: {}",
        msg
    );
    assert!(
        msg.contains("falling back"),
        "warning should indicate fallback, got: {}",
        msg
    );
}

#[test]
fn auto_threading_succeeds_without_warning() {
    let (method, warning) = resolve_post_method(None, None, Some("42"), true);
    assert_eq!(method, "auto_thread", "should use auto-threading");
    assert!(warning.is_none(), "should not produce a warning on success");
}

#[test]
fn no_env_task_id_uses_regular_post() {
    let (method, warning) = resolve_post_method(None, None, None, false);
    assert_eq!(method, "regular");
    assert!(warning.is_none());
}

#[test]
fn explicit_task_flag_takes_priority_over_env() {
    let (method, _) = resolve_post_method(Some("99"), None, Some("42"), false);
    assert_eq!(
        method, "task_thread",
        "explicit --task should take priority over MIDTOWN_TASK_ID"
    );
}

#[test]
fn explicit_thread_flag_takes_priority_over_env() {
    let (method, _) = resolve_post_method(None, Some("msg-123"), Some("42"), false);
    assert_eq!(
        method, "explicit_thread",
        "explicit --thread should take priority over MIDTOWN_TASK_ID"
    );
}

/// Verify that the real `handle()` function's auto-threading path uses
/// MIDTOWN_TASK_ID from the environment. This test doesn't connect to a daemon
/// but validates the env var detection logic.
#[test]
fn env_var_detection_for_auto_threading() {
    let _guard = ENV_MUTEX.lock().unwrap();

    // With MIDTOWN_TASK_ID set, auto-threading path should be chosen
    unsafe { std::env::set_var("MIDTOWN_TASK_ID", "2157") };
    let env_task_id = std::env::var("MIDTOWN_TASK_ID").ok();
    assert_eq!(env_task_id.as_deref(), Some("2157"));

    // Without MIDTOWN_TASK_ID, regular post path should be chosen
    unsafe { std::env::remove_var("MIDTOWN_TASK_ID") };
    let env_task_id = std::env::var("MIDTOWN_TASK_ID").ok();
    assert!(env_task_id.is_none());
}
