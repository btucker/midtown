//! Platform-specific pre-launch hooks.
//!
//! These hooks run immediately before launching provider CLIs (headed or
//! headless), so setup logic is centralized and consistent across paths.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::auth::AuthProvider;

const OFFICIAL_MARKETPLACE: &str = "anthropics/claude-plugins-official";
const OFFICIAL_MARKETPLACE_NAME: &str = "claude-plugins-official";

static CLAUDE_PLUGIN_SYNC_OK: AtomicBool = AtomicBool::new(false);

/// Run platform-specific pre-launch hooks.
///
/// Providers are normalized to launch platforms first (`zai` uses the Claude platform).
pub fn run_platform_prelaunch_hook(provider: AuthProvider) -> Result<(), String> {
    match platform_for_provider(provider) {
        AuthProvider::Claude => ensure_claude_plugins_installed_once(),
        AuthProvider::Codex => Ok(()),
        AuthProvider::Zai => Ok(()),
    }
}

fn platform_for_provider(provider: AuthProvider) -> AuthProvider {
    match provider {
        AuthProvider::Zai => AuthProvider::Claude,
        other => other,
    }
}

fn ensure_claude_plugins_installed_once() -> Result<(), String> {
    if CLAUDE_PLUGIN_SYNC_OK.load(Ordering::SeqCst) {
        return Ok(());
    }
    ensure_claude_plugins_installed()?;
    CLAUDE_PLUGIN_SYNC_OK.store(true, Ordering::SeqCst);
    Ok(())
}

fn required_claude_plugins() -> Vec<String> {
    let configured = crate::config::get_required_plugins();
    if configured.is_empty() {
        crate::daemon::REQUIRED_PLUGINS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        configured
    }
}

fn ensure_claude_plugins_installed() -> Result<(), String> {
    let required = required_claude_plugins();
    if required.is_empty() {
        return Ok(());
    }

    // Best effort: don't fail launch if marketplace setup cannot be changed.
    if let Err(e) = ensure_marketplace_configured() {
        eprintln!(
            "Warning: Could not configure Claude plugin marketplace: {}",
            e
        );
    }

    let installed = get_installed_plugins()?;
    let missing: Vec<&str> = required
        .iter()
        .map(String::as_str)
        .filter(|plugin| !installed.contains(*plugin))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    eprintln!("Installing {} required Claude plugin(s)...", missing.len());
    for plugin in missing {
        eprint!("  Installing {}... ", plugin);
        match install_plugin(plugin) {
            Ok(()) => eprintln!("done"),
            Err(e) => eprintln!("failed: {}", e),
        }
    }

    Ok(())
}

fn ensure_marketplace_configured() -> Result<(), String> {
    let output = std::process::Command::new("claude")
        .args(["plugin", "marketplace", "list"])
        .output()
        .map_err(|e| format!("Failed to run claude plugin marketplace list: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.contains(OFFICIAL_MARKETPLACE_NAME) {
        return Ok(());
    }

    let output = std::process::Command::new("claude")
        .args(["plugin", "marketplace", "add", OFFICIAL_MARKETPLACE])
        .output()
        .map_err(|e| format!("Failed to run claude plugin marketplace add: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(())
}

fn get_installed_plugins() -> Result<std::collections::HashSet<String>, String> {
    let output = std::process::Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
        .map_err(|e| format!("Failed to run claude plugin list: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "claude plugin list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(std::collections::HashSet::new());
    }

    let plugins: Vec<serde_json::Value> = serde_json::from_str(trimmed)
        .map_err(|e| format!("Failed to parse plugin list JSON: {}", e))?;

    Ok(plugins
        .iter()
        .filter_map(|p| p.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect())
}

fn install_plugin(name: &str) -> Result<(), String> {
    let output = std::process::Command::new("claude")
        .args(["plugin", "install", name])
        .output()
        .map_err(|e| format!("Failed to run claude plugin install: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}
