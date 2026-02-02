//! Plugin management for ensuring required Claude Code plugins are installed.
//!
//! Extracted from mod.rs to keep the event loop module thin.

use std::collections::HashSet;

use tracing::{debug, info, warn};

/// Ensure required Claude Code plugins are installed.
///
/// Reads the required plugins list from config, checks which are already
/// installed, and installs any missing ones. Failures are logged as warnings
/// but don't block daemon startup.
pub(super) async fn ensure_plugins_installed() {
    let required = crate::config::get_required_plugins();
    if required.is_empty() {
        debug!("No required plugins configured");
        return;
    }

    info!("Checking {} required plugins", required.len());

    // Get list of installed plugins
    let installed = match get_installed_plugins().await {
        Ok(plugins) => plugins,
        Err(e) => {
            warn!("Failed to check installed plugins: {}", e);
            return;
        }
    };

    // Find missing plugins
    let missing: Vec<_> = required
        .iter()
        .filter(|p| !installed.contains(*p))
        .collect();

    if missing.is_empty() {
        info!("All required plugins are installed");
        return;
    }

    info!("Installing {} missing plugins", missing.len());

    // Install missing plugins
    for plugin in missing {
        match install_plugin(plugin).await {
            Ok(()) => info!("Installed plugin: {}", plugin),
            Err(e) => warn!("Failed to install plugin {}: {}", plugin, e),
        }
    }
}

/// Get list of installed plugin IDs.
async fn get_installed_plugins() -> Result<HashSet<String>, String> {
    let output = tokio::process::Command::new("claude")
        .args(["plugin", "list", "--json"])
        .output()
        .await
        .map_err(|e| format!("Failed to run claude plugin list: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("claude plugin list failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parse JSON output - it's an array of objects with "id" field
    let plugins: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .map_err(|e| format!("Failed to parse plugin list JSON: {}", e))?;

    let ids: HashSet<String> = plugins
        .iter()
        .filter_map(|p| p.get("id").and_then(|id| id.as_str()).map(String::from))
        .collect();

    Ok(ids)
}

/// Install a plugin by name.
async fn install_plugin(name: &str) -> Result<(), String> {
    let output = tokio::process::Command::new("claude")
        .args(["plugin", "add", name])
        .output()
        .await
        .map_err(|e| format!("Failed to run claude plugin add: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(stderr.to_string());
    }

    Ok(())
}
