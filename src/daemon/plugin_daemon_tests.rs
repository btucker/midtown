use super::*;

#[test]
fn backoff_duration_exponential() {
    assert_eq!(backoff_duration(0), Duration::from_millis(500));
    assert_eq!(backoff_duration(1), Duration::from_millis(1000));
    assert_eq!(backoff_duration(2), Duration::from_millis(2000));
    assert_eq!(backoff_duration(3), Duration::from_millis(4000));
}

#[test]
fn backoff_duration_caps_at_max() {
    assert_eq!(backoff_duration(20), MAX_BACKOFF);
    assert_eq!(backoff_duration(100), MAX_BACKOFF);
}

#[tokio::test]
async fn manager_no_plugins_returns_false() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        vec![], // no plugin dirs
        "/tmp/sdk".into(),
    );
    assert!(!manager.ensure_running().await);
    assert!(!manager.is_running().await);
    assert!(!manager.has_plugins());
}

#[tokio::test]
async fn manager_shutdown_noop_when_not_running() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        vec![],
        "/tmp/sdk".into(),
    );
    manager.shutdown().await; // Should not panic.
}

#[tokio::test]
async fn manager_check_health_noop_when_not_running() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        vec![],
        "/tmp/sdk".into(),
    );
    manager.check_health().await; // Should not panic.
}

#[tokio::test]
async fn manager_has_plugins_reflects_dirs() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        vec!["/tmp/plugins".into()],
        "/tmp/sdk".into(),
    );
    assert!(manager.has_plugins());
}

#[tokio::test]
async fn manager_socket_path_accessible() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon-sp.sock".into(),
        vec![],
        "/tmp/sdk".into(),
    );
    assert_eq!(
        manager.socket_path(),
        PathBuf::from("/tmp/test-plugin-daemon-sp.sock")
    );
}

#[tokio::test]
async fn manager_update_plugin_dirs_resets_backoff() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        vec!["/tmp/old-plugins".into()],
        "/tmp/sdk".into(),
    );

    // Simulate a crash to create backoff state.
    {
        let mut inner = manager.inner.lock().await;
        inner.crash_count = 5;
        inner.last_crash = Some(Instant::now());
    }

    // Update dirs should reset backoff.
    manager
        .update_plugin_dirs(vec!["/tmp/new-plugins".into()])
        .await;

    let inner = manager.inner.lock().await;
    assert_eq!(inner.crash_count, 0);
    assert!(inner.last_crash.is_none());
    assert_eq!(inner.plugin_dirs, vec![PathBuf::from("/tmp/new-plugins")]);
}

#[tokio::test]
async fn manager_update_plugin_dirs_noop_when_same() {
    let dirs = vec![PathBuf::from("/tmp/plugins")];
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        dirs.clone(),
        "/tmp/sdk".into(),
    );

    // Set crash state.
    {
        let mut inner = manager.inner.lock().await;
        inner.crash_count = 3;
        inner.last_crash = Some(Instant::now());
    }

    // Same dirs — should not reset backoff.
    manager.update_plugin_dirs(dirs).await;

    let inner = manager.inner.lock().await;
    assert_eq!(inner.crash_count, 3);
}

#[tokio::test]
async fn manager_backoff_prevents_immediate_respawn() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon-backoff.sock".into(),
        vec!["/tmp/plugins".into()],
        "/tmp/nonexistent-sdk".into(),
    );

    // Simulate a crash with very recent timestamp.
    {
        let mut inner = manager.inner.lock().await;
        inner.crash_count = 5;
        inner.last_crash = Some(Instant::now());
    }

    // Should not try to spawn because we're in backoff.
    assert!(!manager.ensure_running().await);
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn manager_spawn_and_ready_handshake() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();

    // Create a fake Python daemon that outputs {"ready":true} then sleeps.
    let sdk_dir = dir.path().join("sdk");
    std::fs::create_dir_all(&sdk_dir).unwrap();

    // Create a fake "uv" that runs a shell script outputting the ready signal.
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    // The fake uv ignores all args and just outputs the ready signal then sleeps.
    std::fs::write(
        &fake_uv,
        r#"#!/bin/sh
echo '{"ready":true}'
# Keep the process alive so it doesn't immediately exit.
sleep 60
"#,
    )
    .unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let socket_path = dir.path().join("test-daemon.sock");
    let plugin_dir = dir.path().join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("test.py"), "# test plugin").unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path.clone(), vec![plugin_dir], sdk_dir);

    let result = manager.ensure_running().await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    assert!(result, "Daemon should be running after successful spawn");
    assert!(manager.is_running().await);

    // Check health should report still running.
    manager.check_health().await;
    assert!(manager.is_running().await);

    // Shutdown should kill it.
    manager.shutdown().await;
    assert!(!manager.is_running().await);
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn manager_detects_crash_and_records_backoff() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let sdk_dir = dir.path().join("sdk");
    std::fs::create_dir_all(&sdk_dir).unwrap();

    // Create a fake uv that outputs ready then immediately exits.
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    std::fs::write(&fake_uv, "#!/bin/sh\necho '{\"ready\":true}'\nexit 0\n").unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let socket_path = dir.path().join("test-crash.sock");
    let plugin_dir = dir.path().join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("test.py"), "# plugin").unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path, vec![plugin_dir], sdk_dir);

    // Spawn — the process will exit right after ready.
    assert!(manager.ensure_running().await);

    // Wait a moment for the process to exit.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // check_health should detect the exit.
    manager.check_health().await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    assert!(!manager.is_running().await);

    // Crash state should be recorded.
    let inner = manager.inner.lock().await;
    assert_eq!(inner.crash_count, 1);
    assert!(inner.last_crash.is_some());
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn manager_spawn_fails_without_ready() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let sdk_dir = dir.path().join("sdk");
    std::fs::create_dir_all(&sdk_dir).unwrap();

    // Create a fake uv that exits without outputting ready.
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    std::fs::write(&fake_uv, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let socket_path = dir.path().join("test-no-ready.sock");
    let plugin_dir = dir.path().join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("test.py"), "# plugin").unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path, vec![plugin_dir], sdk_dir);

    let result = manager.ensure_running().await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    assert!(!result, "Should fail when daemon doesn't send ready");
    assert!(!manager.is_running().await);

    // Should record crash for backoff.
    let inner = manager.inner.lock().await;
    assert_eq!(inner.crash_count, 1);
    assert!(inner.last_crash.is_some());
}

#[allow(clippy::await_holding_lock)] // Intentionally hold PATH_LOCK across await.
#[tokio::test]
async fn manager_shutdown_sends_sigterm() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let sdk_dir = dir.path().join("sdk");
    std::fs::create_dir_all(&sdk_dir).unwrap();

    let marker_path = dir.path().join("sigterm-received");
    let marker_str = marker_path.to_str().unwrap();

    // Create a fake uv that traps SIGTERM and writes a marker file.
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let fake_uv = bin_dir.join("uv");
    std::fs::write(
        &fake_uv,
        format!(
            r#"#!/bin/sh
cleanup() {{
    echo "sigterm" > "{marker_str}"
    exit 0
}}
trap cleanup TERM
echo '{{"ready":true}}'
# Sleep in a loop so trap can fire (sleep is not interruptible in all shells).
while true; do sleep 1; done
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake_uv, std::fs::Permissions::from_mode(0o755)).unwrap();

    let socket_path = dir.path().join("test-sigterm.sock");
    let plugin_dir = dir.path().join("plugins");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("test.py"), "# plugin").unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path, vec![plugin_dir], sdk_dir);

    assert!(manager.ensure_running().await);
    assert!(manager.is_running().await);

    // Shutdown should send SIGTERM, which triggers the trap handler.
    manager.shutdown().await;

    unsafe {
        std::env::set_var("PATH", &original_path);
    }

    assert!(!manager.is_running().await);
    assert!(
        marker_path.exists(),
        "SIGTERM handler should have written marker file"
    );
    let content = std::fs::read_to_string(&marker_path).unwrap();
    assert_eq!(content.trim(), "sigterm");
}
