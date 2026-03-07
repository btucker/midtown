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
        PathBuf::new(), // empty workflows dir
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
        PathBuf::new(),
        "/tmp/sdk".into(),
    );
    manager.shutdown().await; // Should not panic.
}

#[tokio::test]
async fn manager_check_health_noop_when_not_running() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        PathBuf::new(),
        "/tmp/sdk".into(),
    );
    manager.check_health().await; // Should not panic.
}

#[tokio::test]
async fn manager_has_plugins_reflects_workflows_dir() {
    let dir = tempfile::tempdir().unwrap();
    let workflows_dir = dir.path().join("workflows");
    std::fs::create_dir_all(workflows_dir.join("my-workflow")).unwrap();
    std::fs::write(workflows_dir.join("my-workflow/workflow.py"), "# hooks").unwrap();

    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        workflows_dir,
        "/tmp/sdk".into(),
    );
    assert!(manager.has_plugins());
}

#[tokio::test]
async fn manager_has_plugins_false_for_empty_dir() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        PathBuf::new(),
        "/tmp/sdk".into(),
    );
    assert!(!manager.has_plugins());
}

#[tokio::test]
async fn manager_has_plugins_false_when_no_workflows_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let workflows_dir = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();
    // Empty workflows dir — no subdirectories with workflow.py

    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        workflows_dir,
        "/tmp/sdk".into(),
    );
    assert!(!manager.has_plugins());
}

#[tokio::test]
async fn manager_refresh_has_plugins_detects_new_workflow() {
    let dir = tempfile::tempdir().unwrap();
    let workflows_dir = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon.sock".into(),
        workflows_dir.clone(),
        "/tmp/sdk".into(),
    );
    assert!(!manager.has_plugins());

    // Add a workflow on disk.
    std::fs::create_dir_all(workflows_dir.join("new-wf")).unwrap();
    std::fs::write(workflows_dir.join("new-wf/workflow.py"), "# hooks").unwrap();

    manager.refresh_has_plugins().await;
    assert!(manager.has_plugins());
}

#[tokio::test]
async fn manager_socket_path_accessible() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon-sp.sock".into(),
        PathBuf::new(),
        "/tmp/sdk".into(),
    );
    assert_eq!(
        manager.socket_path(),
        PathBuf::from("/tmp/test-plugin-daemon-sp.sock")
    );
}

#[tokio::test]
async fn manager_backoff_prevents_immediate_respawn() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-plugin-daemon-backoff.sock".into(),
        "/tmp/workflows".into(),
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
    let workflows_dir = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path.clone(), workflows_dir, sdk_dir);

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
    let workflows_dir = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path, workflows_dir, sdk_dir);

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
    let workflows_dir = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path, workflows_dir, sdk_dir);

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
    let workflows_dir = dir.path().join("workflows");
    std::fs::create_dir_all(&workflows_dir).unwrap();

    let _path_guard = crate::daemon::PATH_LOCK.lock().unwrap();
    let original_path = std::env::var("PATH").unwrap_or_default();
    unsafe {
        std::env::set_var("PATH", format!("{}:{}", bin_dir.display(), original_path));
    }

    let manager = PluginDaemonManager::new(socket_path, workflows_dir, sdk_dir);

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

#[tokio::test]
async fn send_event_returns_none_when_not_running() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-send-event-norun.sock".into(),
        "/tmp/workflows".into(),
        "/tmp/sdk".into(),
    );

    let result = manager
        .send_event(r#"{"type":"timer.tick","event":{}}"#)
        .await;
    assert!(
        result.is_none(),
        "send_event should return None when daemon is not running"
    );
}

#[tokio::test]
async fn send_reload_returns_false_when_not_running() {
    let manager = PluginDaemonManager::new(
        "/tmp/test-send-reload-norun.sock".into(),
        "/tmp/workflows".into(),
        "/tmp/sdk".into(),
    );

    let result = manager.send_reload().await;
    assert!(
        !result,
        "send_reload should return false when daemon is not running"
    );
}

#[tokio::test]
async fn send_reload_round_trip_via_socket() {
    // Stand up a minimal Unix socket server that returns a canned reload response.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("test-reload.sock");
    let sock_clone = socket_path.clone();

    // Spawn a fake Python daemon server that handles reload.
    let server_handle = tokio::spawn(async move {
        let listener = tokio::net::UnixListener::bind(&sock_clone).unwrap();
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut line = String::new();
        buf_reader.read_line(&mut line).await.unwrap();

        // Verify we received a reload command.
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["type"], "reload");

        // Send back a reload response.
        let response = r#"{"ok":true,"reloaded":true,"loaded_plugins":["test.py"]}"#;
        use tokio::io::AsyncWriteExt;
        writer.write_all(response.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();
    });

    // Give server time to bind.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let manager = PluginDaemonManager::new(
        socket_path.clone(),
        "/tmp/workflows".into(),
        "/tmp/sdk".into(),
    );
    // Inject a fake "running" state.
    {
        let mut inner = manager.inner.lock().await;
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout_drain = tokio::spawn(async {});
        let stderr_drain = tokio::spawn(async {});
        inner.process = Some(DaemonProcess {
            child,
            _stdout_drain: stdout_drain,
            _stderr_drain: stderr_drain,
        });
    }

    let result = manager.send_reload().await;
    server_handle.await.unwrap();

    assert!(result, "send_reload should return true on success");

    // Cleanup.
    manager.shutdown().await;
}

#[tokio::test]
async fn send_event_round_trip_via_socket() {
    // Stand up a minimal Unix socket server that returns a canned response.
    let dir = tempfile::tempdir().unwrap();
    let socket_path = dir.path().join("test-roundtrip.sock");
    let sock_clone = socket_path.clone();

    // Spawn a fake Python daemon server.
    let server_handle = tokio::spawn(async move {
        let listener = tokio::net::UnixListener::bind(&sock_clone).unwrap();
        // Accept one connection.
        let (stream, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = stream.into_split();
        let mut buf_reader = tokio::io::BufReader::new(reader);
        let mut line = String::new();
        buf_reader.read_line(&mut line).await.unwrap();

        // Parse the request to verify we received it.
        let _request: serde_json::Value = serde_json::from_str(&line).unwrap();

        // Send back a response with actions.
        let response = r#"{"ok":true,"actions":[{"method":"channel.post","params":{"message":"hello"}}],"default_prevented":true}"#;
        use tokio::io::AsyncWriteExt;
        writer.write_all(response.as_bytes()).await.unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.shutdown().await.unwrap();
    });

    // Give server time to bind.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Create a manager that thinks it's running (we fake the process state).
    let manager = PluginDaemonManager::new(
        socket_path.clone(),
        "/tmp/workflows".into(),
        "/tmp/sdk".into(),
    );
    // Inject a fake "running" state so send_event doesn't bail early.
    {
        let mut inner = manager.inner.lock().await;
        // Create a dummy child process (just `sleep 60`).
        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let stdout_drain = tokio::spawn(async {});
        let stderr_drain = tokio::spawn(async {});
        inner.process = Some(DaemonProcess {
            child,
            _stdout_drain: stdout_drain,
            _stderr_drain: stderr_drain,
        });
    }

    let result = manager
        .send_event(r#"{"type":"timer.tick","event":{"type":"timer.tick","channel":"test"}}"#)
        .await;

    server_handle.await.unwrap();

    let result = result.expect("send_event should return a result");
    assert!(result.ok);
    assert!(result.default_prevented);
    assert_eq!(result.actions.len(), 1);
    assert_eq!(result.actions[0].method, "channel.post");

    // Cleanup.
    manager.shutdown().await;
}
