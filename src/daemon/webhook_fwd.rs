//! Webhook forwarder — manages the `gh webhook forward` process.
//!
//! Runs a watchdog loop that starts, monitors, and periodically restarts the
//! GitHub CLI webhook forwarder. Handles stale hook cleanup when the forwarder
//! encounters "Hook already exists" errors.
//!
//! Also detects and cleans up orphaned `gh webhook forward` processes from
//! previous daemon runs that weren't properly shut down.

use tokio::sync::watch;
use tracing::{debug, info, warn};

/// Watchdog task that manages the gh webhook forward process with periodic restarts.
///
/// The `gh webhook forward` command can sometimes stop delivering events without
/// terminating. This watchdog ensures reliability by:
/// 1. Starting the forwarder process
/// 2. Restarting it every `restart_interval_secs` seconds
/// 3. Cleaning up on shutdown signal
pub(super) async fn webhook_forwarder_watchdog(
    port: u16,
    restart_interval_secs: u64,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    // Get the GitHub repo name (owner/repo) for webhook forwarding
    let gh_repo = match get_github_repo_name() {
        Some(repo) => repo,
        None => {
            warn!(
                "Could not determine GitHub repo (gh repo view failed). Webhook forwarding disabled."
            );
            warn!("Webhooks will still work if configured manually in GitHub settings.");
            return;
        }
    };

    // Ensure gh-webhook extension is installed
    if !ensure_gh_webhook_extension() {
        warn!("gh-webhook extension not available, webhook forwarding disabled");
        return;
    }

    let url = format!("http://localhost:{}/webhook", port);
    info!(
        "Starting webhook forwarder watchdog (restart every {}s)",
        restart_interval_secs
    );

    // Clean up any orphaned gh webhook forward processes from previous runs
    cleanup_orphaned_webhook_forwarders(&gh_repo);

    let mut current_process: Option<std::process::Child> = None;

    loop {
        // Kill any existing process before starting a new one
        if let Some(mut child) = current_process.take() {
            debug!("Stopping previous webhook forwarder process");
            let _ = child.kill();
            let _ = child.wait();
        }

        // Start new forwarder process
        match start_gh_webhook_forward(&gh_repo, &url) {
            Ok(mut child) => {
                info!("Started gh webhook forward for {} to {}", gh_repo, url);

                // Check if the process exits quickly (indicating an error)
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                match child.try_wait() {
                    Ok(Some(status)) if !status.success() => {
                        // Process exited quickly with an error — check stderr
                        let stderr = child
                            .stderr
                            .take()
                            .and_then(|mut s| {
                                let mut buf = String::new();
                                std::io::Read::read_to_string(&mut s, &mut buf).ok()?;
                                Some(buf)
                            })
                            .unwrap_or_default();

                        if stderr.contains("Hook already exists")
                            || stderr.contains("already_exists")
                            || stderr.contains("422")
                        {
                            warn!(
                                "Webhook forwarder failed with stale hook error: {}",
                                stderr.trim()
                            );
                            if delete_stale_github_webhooks(&gh_repo) {
                                info!("Cleaned up stale webhook(s), will retry on next cycle");
                            } else {
                                warn!("Failed to clean up stale webhooks");
                            }
                        } else {
                            warn!(
                                "Webhook forwarder exited early with status {}: {}",
                                status,
                                stderr.trim()
                            );
                        }
                        // Don't store — process already exited
                    }
                    _ => {
                        // Process still running after 3s — healthy start
                        current_process = Some(child);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to start gh webhook forward: {}", e);
            }
        }

        // Wait for restart interval or shutdown signal
        let restart_delay =
            tokio::time::sleep(std::time::Duration::from_secs(restart_interval_secs));

        tokio::select! {
            _ = restart_delay => {
                debug!("Webhook forwarder restart interval elapsed, restarting...");
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Webhook forwarder watchdog received shutdown signal");
                    break;
                }
            }
        }
    }

    // Clean up on exit
    if let Some(mut child) = current_process {
        info!("Stopping gh webhook forward...");
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Get the GitHub repo name (owner/repo) from the current directory.
fn get_github_repo_name() -> Option<String> {
    std::process::Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Ensure the gh-webhook extension is installed.
fn ensure_gh_webhook_extension() -> bool {
    let extension_check = std::process::Command::new("gh")
        .args(["extension", "list"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("webhook"))
        .unwrap_or(false);

    if extension_check {
        return true;
    }

    info!("Installing gh-webhook extension...");
    match std::process::Command::new("gh")
        .args(["extension", "install", "cli/gh-webhook"])
        .status()
    {
        Ok(status) => status.success(),
        Err(e) => {
            warn!("Failed to install gh-webhook extension: {}", e);
            false
        }
    }
}

/// Start the gh webhook forward process.
fn start_gh_webhook_forward(repo: &str, url: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new("gh")
        .args([
            "webhook",
            "forward",
            "--events=pull_request,pull_request_review,check_run,status,issue_comment,pull_request_review_comment",
            &format!("--repo={}", repo),
            &format!("--url={}", url),
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
}

/// Delete stale GitHub CLI webhooks that cause "Hook already exists" errors.
///
/// Lists all webhooks on the repo and deletes any with `name: "cli"` that point
/// to `webhook-forwarder.github.com` — these are leftover from previous
/// `gh webhook forward` sessions that weren't cleaned up on exit.
fn delete_stale_github_webhooks(repo: &str) -> bool {
    let output = match std::process::Command::new("gh")
        .args(["api", &format!("repos/{}/hooks", repo), "--paginate"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        Ok(o) => {
            warn!(
                "Failed to list webhooks: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            );
            return false;
        }
        Err(e) => {
            warn!("Failed to run gh api: {}", e);
            return false;
        }
    };

    let body = String::from_utf8_lossy(&output.stdout);
    let hooks: Vec<serde_json::Value> = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse webhooks response: {}", e);
            return false;
        }
    };

    let mut deleted = false;
    for hook in &hooks {
        let name = hook["name"].as_str().unwrap_or_default();
        let hook_url = hook["config"]["url"].as_str().unwrap_or_default();
        let id = hook["id"].as_u64().unwrap_or_default();

        if name == "cli" && hook_url.contains("webhook-forwarder.github.com") && id != 0 {
            info!("Deleting stale CLI webhook {} (url: {})", id, hook_url);
            match std::process::Command::new("gh")
                .args([
                    "api",
                    "--method",
                    "DELETE",
                    &format!("repos/{}/hooks/{}", repo, id),
                ])
                .output()
            {
                Ok(o) if o.status.success() => {
                    info!("Successfully deleted stale webhook {}", id);
                    deleted = true;
                }
                Ok(o) => {
                    warn!(
                        "Failed to delete webhook {}: {}",
                        id,
                        String::from_utf8_lossy(&o.stderr).trim()
                    );
                }
                Err(e) => {
                    warn!("Failed to run gh api DELETE for webhook {}: {}", id, e);
                }
            }
        }
    }

    if !deleted {
        warn!("No stale CLI webhooks found to delete");
    }
    deleted
}

/// Find and kill orphaned `gh webhook forward` processes for this repo.
///
/// When the daemon crashes or is killed without proper cleanup, the `gh webhook
/// forward` subprocess may keep running. This causes "Hook already exists" errors
/// when trying to start a new forwarder. We detect these orphans by their command
/// line arguments and kill them before starting a fresh forwarder.
fn cleanup_orphaned_webhook_forwarders(repo: &str) {
    let pids = find_gh_webhook_forward_pids(repo);

    if pids.is_empty() {
        debug!(
            "No orphaned gh webhook forward processes found for {}",
            repo
        );
        return;
    }

    for pid in pids {
        info!(
            "Found orphaned gh webhook forward process (PID {}) for {}, killing it",
            pid, repo
        );
        // Send SIGTERM first for graceful shutdown
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();

        // Give it a moment to exit
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Check if still running and force kill if needed
        if is_process_running(pid) {
            debug!("Process {} still running, sending SIGKILL", pid);
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .status();
        }
    }

    info!(
        "Cleaned up orphaned webhook forwarder(s) for {}, proceeding with fresh start",
        repo
    );
}

/// Find PIDs of `gh webhook forward` processes for a specific repo.
///
/// Uses `pgrep` with full command line matching on macOS/Linux.
fn find_gh_webhook_forward_pids(repo: &str) -> Vec<u32> {
    // pgrep -f matches against the full command line
    // We look for processes matching "gh webhook forward" with our repo
    let output = std::process::Command::new("pgrep")
        .args(["-f", &format!("gh webhook forward.*--repo={}", repo)])
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|line| line.trim().parse::<u32>().ok())
            .collect(),
        Ok(_) => {
            // pgrep returns non-zero when no matches found — that's fine
            Vec::new()
        }
        Err(e) => {
            // pgrep not available, try ps-based fallback
            debug!("pgrep failed ({}), trying ps fallback", e);
            find_gh_webhook_forward_pids_via_ps(repo)
        }
    }
}

/// Fallback: find PIDs using `ps aux` when pgrep isn't available.
fn find_gh_webhook_forward_pids_via_ps(repo: &str) -> Vec<u32> {
    let output = std::process::Command::new("ps").args(["aux"]).output().ok();

    let Some(output) = output else {
        return Vec::new();
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let search_pattern = "gh webhook forward";
    let repo_pattern = format!("--repo={}", repo);

    stdout
        .lines()
        .filter(|line| line.contains(search_pattern) && line.contains(&repo_pattern))
        .filter_map(|line| {
            // ps aux format: USER PID %CPU %MEM VSZ RSS TTY STAT START TIME COMMAND
            line.split_whitespace().nth(1)?.parse::<u32>().ok()
        })
        .collect()
}

/// Check if a process is still running.
fn is_process_running(pid: u32) -> bool {
    // kill -0 checks if process exists without sending a signal
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
