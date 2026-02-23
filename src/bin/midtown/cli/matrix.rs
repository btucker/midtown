use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::cli::Response;
use fs2::FileExt;
use midtown::matrix_bridge::{self, MatrixBridgeConfig};

fn matrix_runtime_dir() -> PathBuf {
    midtown::paths::midtown_base_dir().join("matrix")
}

fn matrix_pid_file() -> PathBuf {
    matrix_runtime_dir().join("bridge.pid")
}

/// Determine whether the matrix bridge process is running.
///
/// This checks the PID file lock held by the running process.
pub fn matrix_bridge_is_running() -> bool {
    let pid_file = matrix_pid_file();
    if !pid_file.exists() {
        return false;
    }
    is_process_running(&pid_file)
}

/// Spawn the matrix bridge subprocess if it is not already running.
pub fn launch_matrix_bridge() -> Result<(), String> {
    if matrix_bridge_is_running() {
        return Err("Matrix bridge already running".to_string());
    }

    let exe =
        std::env::current_exe().map_err(|e| format!("Failed to get current executable: {}", e))?;
    let mut cmd = Command::new(&exe);
    cmd.args(["matrix", "run"]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    cmd.spawn()
        .map_err(|e| format!("Failed to spawn matrix bridge: {}", e))?;

    let started = wait_for_matrix_bridge_running(Duration::from_secs(2));
    if started {
        Ok(())
    } else {
        Err("Matrix bridge failed to start".to_string())
    }
}

/// Stop the matrix bridge process if running.
pub fn stop_matrix_bridge() -> Result<bool, String> {
    let pid_file = matrix_pid_file();
    if !pid_file.exists() {
        return Ok(false);
    }

    if !matrix_bridge_is_running() {
        let _ = std::fs::remove_file(&pid_file);
        return Ok(false);
    }

    let pid_str = std::fs::read_to_string(&pid_file)
        .map_err(|e| format!("Failed to read matrix bridge PID file: {}", e))?;
    let pid: u32 = pid_str
        .trim()
        .parse()
        .map_err(|e| format!("Invalid matrix bridge PID: {}", e))?;

    let _ = Command::new("kill")
        .arg(pid.to_string())
        .stderr(Stdio::null())
        .status();

    let poll_interval = Duration::from_millis(50);
    let timeout = Duration::from_secs(2);
    let start = Instant::now();
    while matrix_bridge_is_running() && start.elapsed() < timeout {
        thread::sleep(poll_interval);
    }

    if matrix_bridge_is_running() {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .stderr(Stdio::null())
            .status();

        let kill_start = Instant::now();
        while matrix_bridge_is_running() && kill_start.elapsed() < Duration::from_secs(1) {
            thread::sleep(poll_interval);
        }
    }

    let _ = std::fs::remove_file(&pid_file);
    Ok(true)
}

/// Handle `midtown matrix run`.
///
/// This process currently owns only PID/liveness tracking and a placeholder
/// long-running loop for early-phase bridge lifecycle testing.
pub fn handle_matrix_run() -> Result<(), String> {
    if matrix_bridge_is_running() {
        return Err("Matrix bridge already running".to_string());
    }

    std::fs::create_dir_all(matrix_runtime_dir())
        .map_err(|e| format!("Failed to create matrix runtime dir: {}", e))?;

    let pid_file = matrix_pid_file();
    if pid_file.exists() {
        let _ = std::fs::remove_file(&pid_file);
    }

    let pid = std::process::id();
    let mut pid_file_handle = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&pid_file)
        .map_err(|e| format!("Failed to open matrix PID file: {}", e))?;

    pid_file_handle
        .try_lock_exclusive()
        .map_err(|e| format!("Failed to lock matrix PID file: {}", e))?;

    writeln!(&mut pid_file_handle, "{}", pid)
        .map_err(|e| format!("Failed to write matrix PID file: {}", e))?;

    let result = matrix_bridge::run(MatrixBridgeConfig::new(matrix_runtime_dir()));

    // Keep the PID lock handle alive while bridge is running.
    drop(pid_file_handle);
    let _ = std::fs::remove_file(&pid_file);
    result
}

/// Handle `midtown matrix stop`.
pub fn handle_matrix_stop() -> Result<Response, String> {
    if matrix_bridge_is_running() {
        match stop_matrix_bridge() {
            Ok(true) => Ok(Response::message("Stopped matrix bridge")),
            Ok(false) => Ok(Response::message("Matrix bridge was not running")),
            Err(e) => Err(format!("Failed to stop matrix bridge: {}", e)),
        }
    } else {
        Ok(Response::message("Matrix bridge was not running"))
    }
}

fn is_process_running(pid_file: &Path) -> bool {
    let file = match std::fs::OpenOptions::new().read(true).open(pid_file) {
        Ok(f) => f,
        Err(_) => return false,
    };

    match file.try_lock_exclusive() {
        Ok(_) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
    }
}

fn wait_for_matrix_bridge_running(timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if matrix_bridge_is_running() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}
