use super::*;

#[test]
fn backoff_duration_exponential() {
    // First crash: 500ms
    assert_eq!(backoff_duration(0), Duration::from_millis(500));
    // Second crash: 1000ms
    assert_eq!(backoff_duration(1), Duration::from_millis(1000));
    // Third crash: 2000ms
    assert_eq!(backoff_duration(2), Duration::from_millis(2000));
    // Fourth crash: 4000ms
    assert_eq!(backoff_duration(3), Duration::from_millis(4000));
}

#[test]
fn backoff_duration_caps_at_max() {
    // Very high crash count should cap at MAX_BACKOFF (60s).
    assert_eq!(backoff_duration(20), MAX_BACKOFF);
    assert_eq!(backoff_duration(100), MAX_BACKOFF);
}

#[tokio::test]
async fn manager_send_event_no_script_returns_false() {
    // When the script doesn't exist, spawn will fail → should return Ok(false)
    // meaning "fall back to subprocess".
    let manager = WorkflowSidecarManager::new("/tmp/test-sidecar.sock".into());
    let result = manager
        .send_event(
            Path::new("/nonexistent/workflow.py"),
            r#"{"type":"timer.tick","channel":"test"}"#,
            Path::new("/tmp/test-state.json"),
        )
        .await;

    // Should return Ok(false) — sidecar couldn't start, use subprocess fallback.
    assert!(matches!(result, Ok(false)));
}

#[tokio::test]
async fn manager_shutdown_empty_is_noop() {
    let manager = WorkflowSidecarManager::new("/tmp/test-sidecar.sock".into());
    // Should not panic on empty manager.
    manager.shutdown_all().await;
}

#[tokio::test]
async fn manager_check_health_empty_is_noop() {
    let manager = WorkflowSidecarManager::new("/tmp/test-sidecar.sock".into());
    // Should not panic on empty manager.
    manager.check_health().await;
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn sidecar_script_exit_detected_as_not_supported() {
    use std::os::unix::fs::PermissionsExt;

    // A script that exits immediately (single-shot mode) should be detected as
    // not supporting sidecar mode.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("workflow.py");
    // Shell script that just exits — doesn't output {"ready":true}.
    std::fs::write(&script, "#!/bin/sh\nexit 0").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Create a fake uv that strips "run --quiet" and exec's the script.
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    // Skip "run", "--quiet", then exec the script (ignoring --sidecar).
    std::fs::write(
        &fake_uv,
        "#!/bin/sh\nshift; shift; SCRIPT=\"$1\"; shift; exec \"$SCRIPT\"",
    )
    .unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = WorkflowSidecarManager::new("/tmp/test-sidecar.sock".into());
    let result = manager
        .send_event(
            &script,
            r#"{"type":"timer.tick","channel":"test"}"#,
            &dir.path().join("state.json"),
        )
        .await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Should return Ok(false) — script doesn't support sidecar mode.
    assert!(matches!(result, Ok(false)));

    // The script should now be marked as single_shot_only.
    let sidecars = manager.sidecars.lock().await;
    let canonical = script
        .canonicalize()
        .unwrap_or_else(|_| script.to_path_buf());
    assert!(
        sidecars
            .get(&canonical)
            .map(|e| e.single_shot_only)
            .unwrap_or(false),
        "Script should be marked as single_shot_only after failing sidecar probe"
    );
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn sidecar_script_with_ready_signal() {
    use std::os::unix::fs::PermissionsExt;

    // A script that outputs {"ready":true} and then reads stdin and outputs {"ok":true}.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("workflow.py");
    std::fs::write(
        &script,
        r#"#!/bin/sh
echo '{"ready":true}'
while IFS= read -r line; do
    echo '{"ok":true}'
done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Create a fake uv that strips "run --quiet" and exec's the script.
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    std::fs::write(
        &fake_uv,
        "#!/bin/sh\nshift; shift; SCRIPT=\"$1\"; shift; exec \"$SCRIPT\"",
    )
    .unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = WorkflowSidecarManager::new("/tmp/test-sidecar.sock".into());
    let result = manager
        .send_event(
            &script,
            r#"{"type":"timer.tick","channel":"test"}"#,
            &dir.path().join("state.json"),
        )
        .await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    // Should return Ok(true) — sidecar delivered the event successfully.
    assert!(
        matches!(result, Ok(true)),
        "Expected Ok(true) for sidecar delivery, got: {result:?}"
    );

    // Clean up the sidecar process.
    manager.shutdown_all().await;
}

#[tokio::test]
async fn clear_single_shot_flag_allows_retry() {
    let manager = WorkflowSidecarManager::new("/tmp/test-sidecar.sock".into());
    let script = Path::new("/tmp/test-clear-flag.py");

    // Manually insert a single_shot_only entry.
    {
        let mut sidecars = manager.sidecars.lock().await;
        sidecars.insert(
            script.to_path_buf(),
            SidecarEntry {
                process: None,
                script_path: script.to_path_buf(),
                crash_count: 0,
                last_crash: None,
                single_shot_only: true,
                script_mtime: None,
            },
        );
    }

    // Verify it's set.
    {
        let sidecars = manager.sidecars.lock().await;
        assert!(sidecars[script].single_shot_only);
    }

    // Clear the flag.
    manager.clear_single_shot_flag(script).await;

    // Verify it's cleared.
    {
        let sidecars = manager.sidecars.lock().await;
        assert!(!sidecars[script].single_shot_only);
    }
}
