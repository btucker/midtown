//! Webhook management commands.
//!
//! Commands for managing GitHub webhook forwarding.

use std::path::PathBuf;
use std::process::Command;

use crate::cli::Response;

/// Default webhook server port
pub const WEBHOOK_PORT: u16 = 8787;

/// Get the config directory for midtown.
fn config_dir() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".config")
        });

    config_dir.join("midtown")
}

/// Get the configured repository, if any.
pub fn get_configured_repo() -> Option<String> {
    let config_file = config_dir().join("config.json");
    if !config_file.exists() {
        return None;
    }

    let content = std::fs::read_to_string(&config_file).ok()?;
    let config: serde_json::Value = serde_json::from_str(&content).ok()?;
    config
        .get("repo")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Save the repository to config.
pub fn save_repo_config(repo: &str) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create config dir: {}", e))?;

    let config_file = dir.join("config.json");

    // Read existing config or create new
    let mut config: serde_json::Value = if config_file.exists() {
        let content =
            std::fs::read_to_string(&config_file).map_err(|e| format!("Failed to read config: {}", e))?;
        serde_json::from_str(&content).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // Update repo
    config["repo"] = serde_json::json!(repo);

    // Write back
    let content =
        serde_json::to_string_pretty(&config).map_err(|e| format!("Failed to serialize config: {}", e))?;
    std::fs::write(&config_file, content).map_err(|e| format!("Failed to write config: {}", e))?;

    Ok(())
}

/// Check if gh webhook forward is running.
fn is_webhook_forward_running() -> bool {
    // Check for running gh webhook forward process
    let output = Command::new("pgrep")
        .args(["-f", "gh webhook forward"])
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Get the PID of the webhook forward process, if running.
fn get_webhook_forward_pid() -> Option<u32> {
    let output = Command::new("pgrep")
        .args(["-f", "gh webhook forward"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next()?.trim().parse().ok()
}

/// Handle `midtown webhooks status` command.
pub fn handle_status() -> Result<Response, String> {
    let repo = get_configured_repo();
    let is_running = is_webhook_forward_running();
    let pid = get_webhook_forward_pid();

    let status_str = if is_running {
        format!("active (PID {})", pid.unwrap_or(0))
    } else {
        "inactive".to_string()
    };

    let repo_str = repo.as_deref().unwrap_or("not configured");

    let message = format!(
        "Webhook forwarding: {}\nRepository: {}\nLocal endpoint: http://localhost:{}/webhook",
        status_str, repo_str, WEBHOOK_PORT
    );

    Ok(Response::WebhookStatus {
        active: is_running,
        repo,
        port: WEBHOOK_PORT,
        pid,
        message,
    })
}

/// Start webhook forwarding for a repository.
pub fn start_webhook_forward(repo: &str) -> Result<u32, String> {
    // Check if gh extension is installed
    let ext_check = Command::new("gh")
        .args(["extension", "list"])
        .output()
        .map_err(|e| format!("Failed to check gh extensions: {}", e))?;

    let ext_list = String::from_utf8_lossy(&ext_check.stdout);
    if !ext_list.contains("webhook") {
        return Err(
            "gh webhook extension not installed. Run: gh extension install cli/gh-webhook".to_string(),
        );
    }

    // Build the forward command
    let events = [
        "pull_request",
        "pull_request_review",
        "pull_request_review_comment",
        "issue_comment",
        "check_run",
        "check_suite",
        "status",
    ]
    .join(",");

    let url = format!("http://localhost:{}/webhook", WEBHOOK_PORT);

    // Spawn gh webhook forward in background
    let child = Command::new("gh")
        .args([
            "webhook",
            "forward",
            &format!("--repo={}", repo),
            &format!("--events={}", events),
            &format!("--url={}", url),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start webhook forwarding: {}", e))?;

    Ok(child.id())
}

/// Stop webhook forwarding.
pub fn stop_webhook_forward() -> Result<(), String> {
    if let Some(pid) = get_webhook_forward_pid() {
        Command::new("kill")
            .arg(pid.to_string())
            .status()
            .map_err(|e| format!("Failed to stop webhook forwarding: {}", e))?;
    }
    Ok(())
}
