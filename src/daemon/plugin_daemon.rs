//! Plugin daemon lifecycle manager.
//!
//! Manages a long-running Python process (`uv run python -m midtown`) that
//! hosts workflow plugins via pluggy. Communication happens over a Unix domain
//! socket using newline-delimited JSON.
//!
//! Lifecycle:
//! - Spawned when plugin files are detected in discovery paths.
//! - Waits for `{"ready":true}` on stdout before dispatching events.
//! - Auto-restarts with exponential backoff on crash.
//! - Gracefully stopped (SIGTERM → wait → SIGKILL) on Rust daemon shutdown.
//!
//! The socket path lives at `<state_dir>/plugin-daemon.sock`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// How long to wait for a plugin dispatch response over the socket.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// How long to wait for `{"ready":true}` on stdout after spawning.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum backoff between restart attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Base backoff for the first restart attempt.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// A single action returned by a Python plugin, deserialized from JSON.
///
/// Maps directly to the Python `DaemonAction(method=..., params=...)` dataclass.
/// The `method` field is an RPC method name like `"channel.post"` or `"coworker.nudge"`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PluginAction {
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// Result of dispatching an event to the Python plugin daemon.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PluginDispatchResult {
    /// Whether the dispatch succeeded on the Python side.
    pub ok: bool,
    /// Error message from the Python side (only when `ok` is false).
    #[serde(default)]
    pub error: Option<String>,
    /// Actions returned by all plugins, concatenated.
    #[serde(default)]
    pub actions: Vec<PluginAction>,
    /// Whether any plugin called `ctx.prevent_default()`.
    #[serde(default)]
    pub default_prevented: bool,
}

/// Internal state of the plugin daemon process.
struct DaemonProcess {
    child: Child,
    /// Background task draining stdout to prevent BrokenPipeError in Python.
    _stdout_drain: tokio::task::JoinHandle<()>,
    /// Background task draining stderr to prevent pipe buffer fill.
    _stderr_drain: tokio::task::JoinHandle<()>,
}

/// Manages the lifecycle of the Python plugin daemon.
///
/// At most one Python daemon process runs at a time. The manager spawns it
/// when plugins are detected, monitors its health, and restarts it with
/// exponential backoff on crashes.
pub(crate) struct PluginDaemonManager {
    /// Socket path is immutable after construction, so it lives outside the mutex.
    socket_path: PathBuf,
    /// Whether plugin directories are configured (non-empty). Set once at construction,
    /// readable without acquiring the mutex. Used by sync code in `pr.rs` to decide
    /// whether to take the script-authoritative path.
    has_plugins_flag: AtomicBool,
    inner: Mutex<PluginDaemonInner>,
}

struct PluginDaemonInner {
    /// Running daemon process, if any.
    process: Option<DaemonProcess>,
    /// Plugin directories passed to `--plugin-dirs`.
    plugin_dirs: Vec<PathBuf>,
    /// Path to the Python SDK (for `uv run`).
    sdk_path: PathBuf,
    /// Consecutive crash count (reset on successful ready handshake).
    crash_count: u32,
    /// When the daemon last crashed (for backoff timing).
    last_crash: Option<Instant>,
}

impl PluginDaemonManager {
    /// Create a new manager. Does not spawn the daemon yet — call
    /// [`ensure_running`] to start it when plugins are detected.
    pub fn new(socket_path: PathBuf, plugin_dirs: Vec<PathBuf>, sdk_path: PathBuf) -> Self {
        let has_plugins = !plugin_dirs.is_empty();
        Self {
            socket_path: socket_path.clone(),
            has_plugins_flag: AtomicBool::new(has_plugins),
            inner: Mutex::new(PluginDaemonInner {
                process: None,
                plugin_dirs,
                sdk_path,
                crash_count: 0,
                last_crash: None,
            }),
        }
    }

    /// Returns the socket path the Python daemon listens on.
    /// Used by Phase 2.4 (bidirectional event dispatch) to connect to the daemon.
    #[allow(dead_code)]
    pub fn socket_path(&self) -> PathBuf {
        self.socket_path.clone()
    }

    /// Returns true if plugin directories are configured (non-empty).
    /// Sync-safe: reads an atomic flag, no mutex needed.
    pub fn has_plugins(&self) -> bool {
        self.has_plugins_flag.load(Ordering::Relaxed)
    }

    /// Ensure the daemon is running. If it's not running and we're past
    /// the backoff period, spawn a new one. No-op if no plugin dirs.
    ///
    /// Returns `true` if the daemon is now running, `false` otherwise.
    pub async fn ensure_running(&self) -> bool {
        let mut inner = self.inner.lock().await;

        if inner.plugin_dirs.is_empty() {
            return false;
        }

        if inner.process.is_some() {
            return true;
        }

        // Check backoff before respawning.
        if let Some(last_crash) = inner.last_crash {
            let backoff = backoff_duration(inner.crash_count);
            let elapsed = last_crash.elapsed();
            if elapsed < backoff {
                debug!(
                    remaining_ms = (backoff - elapsed).as_millis(),
                    "Plugin daemon in backoff"
                );
                return false;
            }
        }

        match spawn_plugin_daemon(&self.socket_path, &inner.plugin_dirs, &inner.sdk_path).await {
            Ok(proc) => {
                inner.process = Some(proc);
                inner.crash_count = 0;
                info!("Plugin daemon spawned and ready");
                true
            }
            Err(e) => {
                warn!("Failed to spawn plugin daemon: {}", e);
                inner.crash_count += 1;
                inner.last_crash = Some(Instant::now());
                false
            }
        }
    }

    /// Check if the daemon process has exited. If so, record the crash
    /// for backoff. Called periodically from the event loop.
    pub async fn check_health(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(ref mut proc) = inner.process {
            match proc.child.try_wait() {
                Ok(Some(status)) => {
                    info!(
                        exit_status = ?status,
                        "Plugin daemon exited"
                    );
                    inner.crash_count += 1;
                    inner.last_crash = Some(Instant::now());
                    inner.process = None;
                }
                Ok(None) => {} // Still running
                Err(e) => {
                    warn!("Failed to check plugin daemon status: {}", e);
                }
            }
        }
    }

    /// Shut down the daemon process (called on Rust daemon exit).
    pub async fn shutdown(&self) {
        let mut inner = self.inner.lock().await;
        if let Some(mut proc) = inner.process.take() {
            info!("Shutting down plugin daemon");
            // Send SIGTERM for graceful shutdown, then wait up to 3s.
            if let Some(pid) = proc.child.id() {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .stderr(std::process::Stdio::null())
                    .status();
                match tokio::time::timeout(Duration::from_secs(3), proc.child.wait()).await {
                    Ok(_) => {
                        info!("Plugin daemon exited gracefully after SIGTERM");
                    }
                    Err(_) => {
                        warn!("Plugin daemon did not exit after SIGTERM, sending SIGKILL");
                        let _ = proc.child.kill().await;
                    }
                }
            } else {
                // No PID available (already exited), just clean up.
                let _ = proc.child.kill().await;
            }
        }
        // Clean up the socket file.
        let _ = std::fs::remove_file(&self.socket_path);
    }

    /// Update the plugin directories. If they changed, restart the daemon.
    /// Used by Phase 4.1 (hot-reload) to respond to `.midtown/` file changes.
    #[allow(dead_code)]
    pub async fn update_plugin_dirs(&self, new_dirs: Vec<PathBuf>) {
        let mut inner = self.inner.lock().await;
        if inner.plugin_dirs == new_dirs {
            return;
        }
        info!(
            old = ?inner.plugin_dirs,
            new = ?new_dirs,
            "Plugin directories changed"
        );
        self.has_plugins_flag
            .store(!new_dirs.is_empty(), Ordering::Relaxed);
        inner.plugin_dirs = new_dirs;
        // Kill the current daemon so it restarts with new dirs.
        if let Some(mut proc) = inner.process.take() {
            let _ = proc.child.kill().await;
        }
        inner.crash_count = 0;
        inner.last_crash = None;
    }

    /// Check whether the daemon is currently running.
    /// Used by Phase 2.4 (bidirectional dispatch) and tests.
    pub async fn is_running(&self) -> bool {
        self.inner.lock().await.process.is_some()
    }

    /// Send a workflow event to the Python plugin daemon and await the response.
    ///
    /// Connects to the plugin daemon's Unix socket, sends the event as
    /// newline-delimited JSON, and reads the response. Returns `None` if the
    /// daemon is not running or the connection/dispatch fails (caller should
    /// fall back to compiled-in behavior).
    ///
    /// The request format matches what the Python `WorkflowDaemon._process_request`
    /// expects: `{"type": "pr.opened", "event": {...}, ...}`.
    pub async fn send_event(&self, event_json: &str) -> Option<PluginDispatchResult> {
        if !self.is_running().await {
            return None;
        }

        let socket_path = self.socket_path.clone();
        match tokio::time::timeout(
            DISPATCH_TIMEOUT,
            send_event_to_socket(&socket_path, event_json),
        )
        .await
        {
            Ok(Ok(result)) => Some(result),
            Ok(Err(e)) => {
                warn!("Plugin daemon dispatch error: {}", e);
                None
            }
            Err(_timeout) => {
                warn!(
                    "Plugin daemon dispatch timed out after {}s",
                    DISPATCH_TIMEOUT.as_secs()
                );
                None
            }
        }
    }
}

/// Send an event to the plugin daemon over the Unix socket and parse the response.
///
/// Opens a new connection per event (matching the Python server's one-request-per-connection
/// model in `_handle_connection`).
async fn send_event_to_socket(
    socket_path: &Path,
    event_json: &str,
) -> Result<PluginDispatchResult, io::Error> {
    let stream = tokio::net::UnixStream::connect(socket_path).await?;
    let (reader, mut writer) = stream.into_split();

    // Send the event as a single line of JSON.
    writer.write_all(event_json.as_bytes()).await?;
    if !event_json.ends_with('\n') {
        writer.write_all(b"\n").await?;
    }
    writer.shutdown().await?;

    // Read the response line.
    let mut buf_reader = BufReader::new(reader);
    let mut response_line = String::new();
    buf_reader.read_line(&mut response_line).await?;

    if response_line.is_empty() {
        return Err(io::Error::other(
            "plugin daemon closed connection without responding",
        ));
    }

    serde_json::from_str::<PluginDispatchResult>(&response_line)
        .map_err(|e| io::Error::other(format!("invalid plugin daemon response: {e}")))
}

/// Spawn the Python plugin daemon process.
///
/// Runs `uv run python -m midtown --socket-path <path> --plugin-dirs <dirs>`
/// and waits for `{"ready":true}` on stdout.
async fn spawn_plugin_daemon(
    socket_path: &Path,
    plugin_dirs: &[PathBuf],
    sdk_path: &Path,
) -> Result<DaemonProcess, io::Error> {
    // Clean up stale socket before spawning.
    let _ = std::fs::remove_file(socket_path);

    let dirs_arg = plugin_dirs
        .iter()
        .map(|d| d.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(",");

    let mut child = Command::new("uv")
        .args([
            "run",
            "--quiet",
            "--project",
            sdk_path.to_str().unwrap_or_default(),
            "python",
            "-m",
            "midtown",
            "--socket-path",
            socket_path.to_str().unwrap_or_default(),
            "--plugin-dirs",
            &dirs_arg,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| io::Error::other(format!("failed to spawn plugin daemon: {e}")))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("no stderr"))?;

    // Drain stderr in background.
    let stderr_drain = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    debug!(stderr = line.trim(), "Plugin daemon stderr");
                }
                Err(_) => break,
            }
        }
    });

    // Wait for ready signal on stdout.
    let mut reader = BufReader::new(stdout);
    let mut ready_line = String::new();
    let ready_result = tokio::time::timeout(READY_TIMEOUT, reader.read_line(&mut ready_line)).await;

    match ready_result {
        Ok(Ok(0)) => {
            // Process exited before sending ready.
            let _ = child.kill().await;
            Err(io::Error::other(
                "plugin daemon exited before sending ready signal",
            ))
        }
        Ok(Ok(_)) => {
            let trimmed = ready_line.trim();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
                && val.get("ready") == Some(&serde_json::Value::Bool(true))
            {
                // Keep draining stdout so the Python process doesn't get
                // BrokenPipeError if it (or a dependency) writes to stdout.
                let stdout_drain = tokio::spawn(async move {
                    let mut reader = reader;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line).await {
                            Ok(0) => break,
                            Ok(_) => {
                                debug!(stdout = line.trim(), "Plugin daemon stdout");
                            }
                            Err(_) => break,
                        }
                    }
                });
                return Ok(DaemonProcess {
                    child,
                    _stdout_drain: stdout_drain,
                    _stderr_drain: stderr_drain,
                });
            }
            let _ = child.kill().await;
            Err(io::Error::other(format!(
                "plugin daemon sent unexpected ready signal: {trimmed}"
            )))
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            Err(io::Error::other(format!(
                "error reading plugin daemon stdout: {e}"
            )))
        }
        Err(_timeout) => {
            let _ = child.kill().await;
            Err(io::Error::other(format!(
                "plugin daemon did not send ready signal within {}s",
                READY_TIMEOUT.as_secs()
            )))
        }
    }
}

/// Calculate exponential backoff for restart attempts.
fn backoff_duration(crash_count: u32) -> Duration {
    let base_ms = BASE_BACKOFF.as_millis() as u64;
    let max_ms = MAX_BACKOFF.as_millis() as u64;
    let multiplier = 1u64.checked_shl(crash_count.min(10)).unwrap_or(u64::MAX);
    let backoff_ms = base_ms.saturating_mul(multiplier);
    Duration::from_millis(backoff_ms.min(max_ms))
}

#[path = "plugin_daemon_tests.rs"]
#[cfg(test)]
mod tests;
