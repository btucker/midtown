//! Platform-specific pre-launch hooks.
//!
//! These hooks run immediately before launching provider CLIs (interactive or
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
/// Providers are normalized to launch platforms first (`zai` uses the Claude
/// platform). When `profile_dir` is provided, provider setup that writes into a
/// profile container targets that explicit directory instead of the ambient
/// local profile. Codex skill sync relies on this so attach/resume flows update
/// the launched session's `CODEX_HOME`, not whichever Codex profile is currently active.
pub fn run_platform_prelaunch_hook(
    provider: AuthProvider,
    profile_dir: Option<&Path>,
) -> Result<(), String> {
    match Platform::from_provider(provider) {
        Platform::Claude => ensure_claude_plugins_installed_once(),
        Platform::Codex => ensure_codex_skills_synced(profile_dir),
    }
}

fn codex_destination_skills_dir(profile_dir: Option<&Path>) -> std::path::PathBuf {
    profile_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| crate::auth::current_profile_dir_for(AuthProvider::Codex))
        .join("skills")
}

fn ensure_codex_skills_synced(profile_dir: Option<&Path>) -> Result<(), String> {
    let Some(home_dir) = dirs::home_dir() else {
        return Ok(());
    };

    let source_skills_dir = home_dir.join(".codex").join("skills");
    if !source_skills_dir.is_dir() {
        return Ok(());
    }

    let destination_skills_dir = codex_destination_skills_dir(profile_dir);
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

/// Hardcoded list of required Claude Code plugins.
///
/// These plugins are essential for midtown's agents to function properly.
/// The daemon will automatically install any missing plugins on startup.
pub const REQUIRED_PLUGINS: &[&str] = &[
    "superpowers@claude-plugins-official",
    "code-review@claude-plugins-official",
    "pr-review-toolkit@claude-plugins-official",
    "commit-commands@claude-plugins-official",
    "feature-dev@claude-plugins-official",
    "explanatory-output-style@claude-plugins-official",
    "code-simplifier@claude-plugins-official",
];

fn required_claude_plugins() -> Vec<String> {
    let configured = crate::config::get_required_plugins();
    if configured.is_empty() {
        REQUIRED_PLUGINS.iter().map(|s| (*s).to_string()).collect()
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
        // Uninstall first to clear stale/orphaned entries from installed_plugins.json,
        // then reinstall fresh. Ignoring uninstall errors (plugin may not be registered).
        let _ = uninstall_plugin(plugin);
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

/// Filter plugin list entries to only those with healthy, loadable installations.
///
/// Excludes plugins that are orphaned (`.orphaned_at` marker) or missing their
/// plugin manifest. This causes `ensure_claude_plugins_installed()` to reinstall
/// them instead of silently accepting broken installations.
fn filter_healthy_plugins(plugins: &[serde_json::Value]) -> HashSet<String> {
    plugins
        .iter()
        .filter_map(|p| {
            let id = p.get("id").and_then(|id| id.as_str())?;
            let install_path = p.get("installPath").and_then(|p| p.as_str())?;
            let path = Path::new(install_path);

            // Skip plugins whose installation is orphaned or missing content.
            // Claude Code's plugin auto-updater can mark cached versions as orphaned
            // (via .orphaned_at marker), stripping their content. Cross-profile
            // references in installed_plugins.json may point to these stale paths.
            if path.join(".orphaned_at").exists() {
                eprintln!(
                    "Warning: Plugin {} has orphaned installation at {}, will reinstall",
                    id, install_path
                );
                return None;
            }

            // Verify the plugin has actual content (plugin manifest exists)
            if !path.join(".claude-plugin").join("plugin.json").exists()
                && !path.join("plugin.json").exists()
            {
                eprintln!(
                    "Warning: Plugin {} is missing plugin manifest at {}, will reinstall",
                    id, install_path
                );
                return None;
            }

            Some(id.to_string())
        })
        .collect()
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

    Ok(filter_healthy_plugins(&plugins))
}

fn uninstall_plugin(name: &str) -> Result<(), String> {
    let output = std::process::Command::new("claude")
        .args(["plugin", "uninstall", name])
        .output()
        .map_err(|e| format!("Failed to run claude plugin uninstall: {}", e))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
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
