//! Platform-specific pre-launch hooks.
//!
//! These hooks run immediately before launching provider CLIs (headed or
//! headless), so setup logic is centralized and consistent across paths.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::auth::AuthProvider;
use crate::platform::Platform;

const OFFICIAL_MARKETPLACE: &str = "anthropics/claude-plugins-official";
const OFFICIAL_MARKETPLACE_NAME: &str = "claude-plugins-official";

static CLAUDE_PLUGIN_SYNC_OK: AtomicBool = AtomicBool::new(false);

/// Run platform-specific pre-launch hooks.
///
/// Providers are normalized to launch platforms first (`zai` uses the Claude platform).
pub fn run_platform_prelaunch_hook(provider: AuthProvider) -> Result<(), String> {
    match Platform::from_provider(provider) {
        Platform::Claude => ensure_claude_plugins_installed_once(),
        Platform::Codex => ensure_codex_skills_synced(),
    }
}

fn ensure_codex_skills_synced() -> Result<(), String> {
    let Some(home_dir) = dirs::home_dir() else {
        return Ok(());
    };

    let source_skills_dir = home_dir.join(".codex").join("skills");
    if !source_skills_dir.is_dir() {
        return Ok(());
    }

    let profile_dir = crate::auth::current_profile_dir_for(AuthProvider::Codex);
    let destination_skills_dir = profile_dir.join("skills");
    sync_directory_with_cleanup(&source_skills_dir, &destination_skills_dir)
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

fn sync_directory_with_cleanup(source: &Path, destination: &Path) -> Result<(), String> {
    if paths_match(source, destination) {
        return Ok(());
    }

    std::fs::create_dir_all(destination).map_err(|e| {
        format!(
            "Failed to create destination directory {}: {}",
            destination.display(),
            e
        )
    })?;

    let mut source_entries: HashSet<std::ffi::OsString> = HashSet::new();
    for entry in std::fs::read_dir(source).map_err(|e| {
        format!(
            "Failed to read source directory {}: {}",
            source.display(),
            e
        )
    })? {
        let entry = entry.map_err(|e| {
            format!(
                "Failed to read source directory entry in {}: {}",
                source.display(),
                e
            )
        })?;
        let name = entry.file_name();
        let source_path = entry.path();
        let destination_path = destination.join(&name);
        source_entries.insert(name);

        let source_type = entry.file_type().map_err(|e| {
            format!(
                "Failed to read file type for {}: {}",
                source_path.display(),
                e
            )
        })?;

        if source_type.is_dir() {
            if destination_path.exists() && !destination_path.is_dir() {
                remove_path(&destination_path)?;
            }
            sync_directory_with_cleanup(&source_path, &destination_path)?;
            continue;
        }

        if destination_path.is_dir() {
            remove_path(&destination_path)?;
        } else if destination_path.exists() {
            std::fs::remove_file(&destination_path).map_err(|e| {
                format!(
                    "Failed to remove destination file {}: {}",
                    destination_path.display(),
                    e
                )
            })?;
        }

        std::fs::copy(&source_path, &destination_path).map_err(|e| {
            format!(
                "Failed to copy {} to {}: {}",
                source_path.display(),
                destination_path.display(),
                e
            )
        })?;
    }

    for entry in std::fs::read_dir(destination).map_err(|e| {
        format!(
            "Failed to read destination directory {}: {}",
            destination.display(),
            e
        )
    })? {
        let entry = entry.map_err(|e| {
            format!(
                "Failed to read destination directory entry in {}: {}",
                destination.display(),
                e
            )
        })?;
        if !source_entries.contains(&entry.file_name()) {
            remove_path(&entry.path())?;
        }
    }

    Ok(())
}

fn remove_path(path: &Path) -> Result<(), String> {
    let metadata = path
        .symlink_metadata()
        .map_err(|e| format!("Failed to inspect {}: {}", path.display(), e))?;

    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path)
            .map_err(|e| format!("Failed to remove file {}: {}", path.display(), e))
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(path)
            .map_err(|e| format!("Failed to remove directory {}: {}", path.display(), e))
    } else {
        Err(format!("Unsupported filesystem entry: {}", path.display()))
    }
}

fn paths_match(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

#[path = "platform_launch_tests.rs"]
#[cfg(test)]
mod tests;
