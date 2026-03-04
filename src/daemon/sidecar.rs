//! Persistent workflow sidecar process management.
//!
//! Instead of spawning a new `uv run workflow.py` subprocess per event
//! (~300-800ms overhead), the daemon maintains a long-lived Python sidecar
//! process per workflow script. Events are sent as newline-delimited JSON
//! on stdin, and the sidecar responds with `{"ok":true}` on stdout.
//!
//! The sidecar lifecycle:
//! - Spawned on first event (lazy) or when the script file changes (hot-reload).
//! - Restarted with exponential backoff on crash.
//! - Killed on daemon shutdown.
//!
//! Backwards compatibility: if the script doesn't use `run_loop()` (it uses
//! `run()` for single-shot mode), the sidecar will fail to start (no `{"ready":true}`
//! on stdout) and the daemon falls back to subprocess-per-event automatically.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// How long to wait for the sidecar to send `{"ready":true}` after spawn.
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// How long to wait for a per-event response from the sidecar.
const EVENT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum backoff between restart attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// Base backoff for the first restart attempt.
const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// A running sidecar process for a single workflow script.
struct SidecarProcess {
    child: Child,
    /// Buffered reader for stdout (reads `{"ok":true}` ack lines).
    stdout: BufReader<tokio::process::ChildStdout>,
    /// Stdin pipe for sending event envelopes.
    stdin: tokio::process::ChildStdin,
    /// Handle for the background stderr drain task.
    _stderr_drain: tokio::task::JoinHandle<()>,
}

/// Per-script sidecar state (running process + restart bookkeeping).
struct SidecarEntry {
    /// The running sidecar process, if any.
    process: Option<SidecarProcess>,
    /// Path to the workflow script this sidecar runs (for diagnostics).
    #[allow(dead_code)]
    script_path: PathBuf,
    /// Consecutive crash count (reset on successful event delivery).
    crash_count: u32,
    /// When the sidecar last crashed (for backoff timing).
    last_crash: Option<Instant>,
    /// Whether we've determined this script doesn't support sidecar mode.
    /// Once set, all events fall back to subprocess-per-event.
    single_shot_only: bool,
    /// Modification time of the script when the sidecar was spawned.
    /// Used to detect hot-reload needs.
    script_mtime: Option<std::time::SystemTime>,
}

/// Manages persistent workflow sidecar processes.
///
/// Keyed by a composite of (channel, script_path) since different channels
/// can have different workflow scripts via the 4-level resolution order.
pub(crate) struct WorkflowSidecarManager {
    /// Sidecar entries keyed by script path (canonical).
    sidecars: Mutex<HashMap<PathBuf, SidecarEntry>>,
    /// Socket path for the daemon (passed to sidecar in event envelopes).
    socket_path: PathBuf,
}

impl WorkflowSidecarManager {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            sidecars: Mutex::new(HashMap::new()),
            socket_path,
        }
    }

    /// Send an event to the appropriate sidecar, spawning it if needed.
    ///
    /// Returns `Ok(true)` if the event was delivered via sidecar,
    /// `Ok(false)` if the script doesn't support sidecar mode (caller
    /// should fall back to subprocess), and `Err` on I/O failure.
    pub async fn send_event(
        &self,
        script_path: &Path,
        event_json: &str,
        state_file: &Path,
    ) -> Result<bool, String> {
        let canonical = script_path
            .canonicalize()
            .unwrap_or_else(|_| script_path.to_path_buf());

        let mut sidecars = self.sidecars.lock().await;

        // Check for hot-reload: if the script's mtime changed, kill the old sidecar
        // and clear the single_shot_only flag (the user may have upgraded the script).
        let current_mtime = std::fs::metadata(script_path)
            .and_then(|m| m.modified())
            .ok();
        if let Some(entry) = sidecars.get_mut(&canonical)
            && let (Some(cached), Some(current)) = (entry.script_mtime, current_mtime)
            && cached != current
        {
            info!(
                script = %script_path.display(),
                "Workflow script modified — restarting sidecar"
            );
            if let Some(mut proc) = entry.process.take() {
                let _ = proc.child.kill().await;
            }
            entry.crash_count = 0;
            entry.last_crash = None;
            entry.script_mtime = None;
            entry.single_shot_only = false;
        }

        // Check if this script is known to be single-shot only.
        if sidecars
            .get(&canonical)
            .is_some_and(|entry| entry.single_shot_only)
        {
            return Ok(false);
        }

        // Ensure sidecar is running (spawn if needed).
        let entry = sidecars
            .entry(canonical.clone())
            .or_insert_with(|| SidecarEntry {
                process: None,
                script_path: script_path.to_path_buf(),
                crash_count: 0,
                last_crash: None,
                single_shot_only: false,
                script_mtime: None,
            });

        if entry.process.is_none() {
            // Check backoff before respawning.
            if let Some(last_crash) = entry.last_crash {
                let backoff = backoff_duration(entry.crash_count);
                let elapsed = last_crash.elapsed();
                if elapsed < backoff {
                    debug!(
                        script = %script_path.display(),
                        remaining_ms = (backoff - elapsed).as_millis(),
                        "Sidecar in backoff — falling back to subprocess"
                    );
                    return Ok(false);
                }
            }

            match spawn_sidecar(script_path).await {
                Ok(proc) => {
                    entry.process = Some(proc);
                    entry.script_mtime = current_mtime;
                    info!(
                        script = %script_path.display(),
                        "Sidecar spawned and ready"
                    );
                }
                Err(SidecarSpawnError::NotSupported) => {
                    debug!(
                        script = %script_path.display(),
                        "Script does not support sidecar mode — using subprocess fallback"
                    );
                    entry.single_shot_only = true;
                    entry.script_mtime = current_mtime;
                    return Ok(false);
                }
                Err(SidecarSpawnError::Io(e)) => {
                    warn!(
                        script = %script_path.display(),
                        "Failed to spawn sidecar: {} — falling back to subprocess",
                        e
                    );
                    entry.crash_count += 1;
                    entry.last_crash = Some(Instant::now());
                    return Ok(false);
                }
            }
        }

        // Send the event envelope.
        let proc = entry.process.as_mut().unwrap();
        let envelope = serde_json::json!({
            "event": serde_json::from_str::<serde_json::Value>(event_json)
                .unwrap_or(serde_json::Value::Null),
            "state_file": state_file.to_string_lossy(),
            "socket": self.socket_path.to_string_lossy(),
        });
        let line = serde_json::to_string(&envelope).unwrap_or_default() + "\n";

        // Write to stdin.
        if let Err(e) = proc.stdin.write_all(line.as_bytes()).await {
            warn!(
                script = %script_path.display(),
                "Sidecar stdin write failed: {} — killing and falling back",
                e
            );
            let _ = proc.child.kill().await;
            entry.process = None;
            entry.crash_count += 1;
            entry.last_crash = Some(Instant::now());
            return Ok(false);
        }

        // Read the ack response with a timeout.
        let mut response_line = String::new();
        let read_result =
            tokio::time::timeout(EVENT_TIMEOUT, proc.stdout.read_line(&mut response_line)).await;

        match read_result {
            Ok(Ok(0)) => {
                // EOF — sidecar exited.
                warn!(
                    script = %script_path.display(),
                    "Sidecar exited (EOF on stdout)"
                );
                entry.process = None;
                entry.crash_count += 1;
                entry.last_crash = Some(Instant::now());
                Ok(false)
            }
            Ok(Ok(_)) => {
                // Parse the response.
                let response_line = response_line.trim();
                if let Ok(resp) = serde_json::from_str::<serde_json::Value>(response_line) {
                    if resp.get("ok") == Some(&serde_json::Value::Bool(true)) {
                        // Success — reset crash counter.
                        entry.crash_count = 0;
                        return Ok(true);
                    }
                    let error = resp
                        .get("error")
                        .and_then(|e| e.as_str())
                        .unwrap_or("unknown");
                    return Err(format!("sidecar handler error: {error}"));
                }
                // Unparseable response — keep sidecar alive but report error.
                Err(format!(
                    "sidecar returned unparseable response: {response_line}"
                ))
            }
            Ok(Err(e)) => {
                warn!(
                    script = %script_path.display(),
                    "Sidecar stdout read error: {}",
                    e
                );
                let _ = proc.child.kill().await;
                entry.process = None;
                entry.crash_count += 1;
                entry.last_crash = Some(Instant::now());
                Ok(false)
            }
            Err(_timeout) => {
                warn!(
                    script = %script_path.display(),
                    "Sidecar event timed out after {}s — killing",
                    EVENT_TIMEOUT.as_secs()
                );
                let _ = proc.child.kill().await;
                entry.process = None;
                entry.crash_count += 1;
                entry.last_crash = Some(Instant::now());
                Err("sidecar event timed out".to_string())
            }
        }
    }

    /// Shut down all running sidecars (called on daemon shutdown).
    pub async fn shutdown_all(&self) {
        let mut sidecars = self.sidecars.lock().await;
        for (path, entry) in sidecars.iter_mut() {
            if let Some(mut proc) = entry.process.take() {
                debug!(script = %path.display(), "Shutting down sidecar");
                // Close stdin to signal EOF, then kill if it doesn't exit.
                drop(proc.stdin);
                let kill_result =
                    tokio::time::timeout(Duration::from_secs(3), proc.child.wait()).await;
                if kill_result.is_err() {
                    let _ = proc.child.kill().await;
                }
            }
        }
        sidecars.clear();
    }

    /// Check if any sidecars have died and need cleanup.
    /// Called periodically from the daemon event loop.
    pub async fn check_health(&self) {
        let mut sidecars = self.sidecars.lock().await;
        let mut dead_keys = Vec::new();

        for (path, entry) in sidecars.iter_mut() {
            if let Some(ref mut proc) = entry.process {
                // Non-blocking check if the process has exited.
                match proc.child.try_wait() {
                    Ok(Some(status)) => {
                        info!(
                            script = %path.display(),
                            exit_status = ?status,
                            "Sidecar process exited"
                        );
                        entry.crash_count += 1;
                        entry.last_crash = Some(Instant::now());
                        dead_keys.push(path.clone());
                    }
                    Ok(None) => {} // Still running
                    Err(e) => {
                        warn!(
                            script = %path.display(),
                            "Failed to check sidecar process status: {}",
                            e
                        );
                    }
                }
            }
        }

        for key in dead_keys {
            if let Some(entry) = sidecars.get_mut(&key) {
                entry.process = None;
            }
        }
    }

    /// Clear the "single_shot_only" flag for a script, forcing re-detection.
    /// Used when a script file is modified (it might have been upgraded to sidecar mode).
    #[cfg(test)]
    async fn clear_single_shot_flag(&self, script_path: &Path) {
        let canonical = script_path
            .canonicalize()
            .unwrap_or_else(|_| script_path.to_path_buf());
        let mut sidecars = self.sidecars.lock().await;
        if let Some(entry) = sidecars.get_mut(&canonical) {
            entry.single_shot_only = false;
        }
    }
}

enum SidecarSpawnError {
    /// The script doesn't support sidecar mode (no `{"ready":true}` within timeout).
    NotSupported,
    /// I/O error spawning or communicating with the process.
    Io(io::Error),
}

/// Spawn a sidecar process and wait for it to signal readiness.
async fn spawn_sidecar(script_path: &Path) -> Result<SidecarProcess, SidecarSpawnError> {
    let mut child = Command::new("uv")
        .args([
            "run",
            "--quiet",
            script_path.to_str().unwrap_or_default(),
            "--sidecar",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(SidecarSpawnError::Io)?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| SidecarSpawnError::Io(io::Error::other("no stdout")))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| SidecarSpawnError::Io(io::Error::other("no stdin")))?;

    // Drain stderr in a background task to prevent the OS pipe buffer from
    // filling up and blocking the sidecar process.
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| SidecarSpawnError::Io(io::Error::other("no stderr")))?;
    let script_display = script_path.display().to_string();
    let stderr_drain = tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    debug!(
                        script = %script_display,
                        stderr = line.trim(),
                        "Sidecar stderr"
                    );
                }
                Err(_) => break,
            }
        }
    });

    let mut reader = BufReader::new(stdout);

    // Wait for the ready signal.
    let mut ready_line = String::new();
    let ready_result = tokio::time::timeout(READY_TIMEOUT, reader.read_line(&mut ready_line)).await;

    match ready_result {
        Ok(Ok(0)) => {
            // Process exited before sending ready.
            Err(SidecarSpawnError::NotSupported)
        }
        Ok(Ok(_)) => {
            let trimmed = ready_line.trim();
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
                && val.get("ready") == Some(&serde_json::Value::Bool(true))
            {
                return Ok(SidecarProcess {
                    child,
                    stdout: reader,
                    stdin,
                    _stderr_drain: stderr_drain,
                });
            }
            // Got output but not the ready signal — not a sidecar-mode script.
            let _ = child.kill().await;
            Err(SidecarSpawnError::NotSupported)
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            Err(SidecarSpawnError::Io(e))
        }
        Err(_timeout) => {
            // No ready signal within timeout — assume single-shot script.
            let _ = child.kill().await;
            Err(SidecarSpawnError::NotSupported)
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

#[path = "sidecar_tests.rs"]
#[cfg(test)]
mod tests;
