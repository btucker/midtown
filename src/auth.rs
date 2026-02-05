//! Auth profile management for midtown.
//!
//! Manages multiple Claude authentication profiles, allowing different accounts
//! to be used for different purposes (e.g., separate accounts for E2E testing,
//! development, production).
//!
//! ## Storage Structure
//!
//! ```text
//! ~/.midtown/auth/
//! ├── current              # Text file with current profile name
//! └── <profile>/           # Per-profile directories (Claude's CLAUDE_CONFIG_DIR)
//!     └── .claude.json     # Claude config with auth tokens (managed by claude CLI)
//! ```
//!
//! ## Environment Variable
//!
//! When spawning Claude Code sessions, set `CLAUDE_CONFIG_DIR` to the profile
//! directory to use that profile's authentication.

use std::path::PathBuf;

use crate::paths::midtown_base_dir;

/// Default profile name used when none is specified.
pub const DEFAULT_PROFILE: &str = "default";

/// Get the base auth directory.
///
/// Returns `~/.midtown/auth/`.
pub fn auth_base_dir() -> PathBuf {
    midtown_base_dir().join("auth")
}

/// Get the profile directory for a specific profile.
///
/// Returns `~/.midtown/auth/<profile>/`.
pub fn profile_dir(name: &str) -> PathBuf {
    auth_base_dir().join(name)
}

/// Get the path to the current profile marker file.
///
/// Returns `~/.midtown/auth/current`.
fn current_profile_file() -> PathBuf {
    auth_base_dir().join("current")
}

/// Get the currently active profile name.
///
/// Returns the profile name from `~/.midtown/auth/current`, or "default" if
/// the file doesn't exist or is empty.
pub fn current_profile() -> String {
    std::fs::read_to_string(current_profile_file())
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_PROFILE.to_string())
}

/// Get the config directory path for the current profile.
///
/// This is what should be set as `CLAUDE_CONFIG_DIR` when spawning Claude.
pub fn current_profile_dir() -> PathBuf {
    profile_dir(&current_profile())
}

/// Set the active profile.
///
/// Writes the profile name to `~/.midtown/auth/current`.
/// Returns an error if the profile doesn't exist.
pub fn set_current_profile(name: &str) -> std::io::Result<()> {
    let dir = profile_dir(name);
    if !dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Profile '{}' does not exist. Create it first with: midtown auth login --profile {}",
                name, name
            ),
        ));
    }

    let current_file = current_profile_file();
    std::fs::create_dir_all(current_file.parent().unwrap())?;
    std::fs::write(current_file, format!("{}\n", name))
}

/// List all available profiles.
///
/// Returns a list of profile names (directory names under `~/.midtown/auth/`).
pub fn list_profiles() -> std::io::Result<Vec<String>> {
    let auth_dir = auth_base_dir();
    if !auth_dir.exists() {
        return Ok(vec![]);
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(auth_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            profiles.push(name.to_string());
        }
    }
    profiles.sort();
    Ok(profiles)
}

/// Check if a profile exists.
pub fn profile_exists(name: &str) -> bool {
    profile_dir(name).exists()
}

/// Remove a profile.
///
/// Deletes the profile directory and its contents.
/// Returns an error if attempting to remove the currently active profile.
pub fn remove_profile(name: &str) -> std::io::Result<()> {
    let current = current_profile();
    if name == current {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Cannot remove the active profile '{}'. Switch to a different profile first.",
                name
            ),
        ));
    }

    let dir = profile_dir(name);
    if !dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Profile '{}' does not exist", name),
        ));
    }

    std::fs::remove_dir_all(dir)
}

/// Create a profile directory if it doesn't exist.
///
/// This is called before launching `claude` for login so the config dir exists.
pub fn ensure_profile_dir(name: &str) -> std::io::Result<PathBuf> {
    let dir = profile_dir(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Migrate legacy E2E auth directory to the new profile system.
///
/// Moves `~/.midtown/claude-auth/` to `~/.midtown/auth/e2e/` if:
/// - The legacy directory exists
/// - The new profile doesn't already exist
///
/// Returns Ok(true) if migration was performed, Ok(false) if not needed.
pub fn migrate_legacy_auth() -> std::io::Result<bool> {
    let legacy_dir = midtown_base_dir().join("claude-auth");
    let new_dir = profile_dir("e2e");

    if !legacy_dir.exists() {
        return Ok(false);
    }

    if new_dir.exists() {
        // New profile already exists, don't overwrite
        return Ok(false);
    }

    // Create parent directory
    std::fs::create_dir_all(auth_base_dir())?;

    // Move legacy to new location
    std::fs::rename(&legacy_dir, &new_dir)?;

    Ok(true)
}

/// Get profile status information for display.
pub struct ProfileStatus {
    pub name: String,
    pub path: PathBuf,
    pub is_current: bool,
    pub has_credentials: bool,
}

/// Get detailed status for a profile.
pub fn profile_status(name: &str) -> Option<ProfileStatus> {
    let dir = profile_dir(name);
    if !dir.exists() {
        return None;
    }

    // Claude stores its config in .claude.json
    let claude_config = dir.join(".claude.json");

    Some(ProfileStatus {
        name: name.to_string(),
        path: dir.clone(),
        is_current: current_profile() == name,
        has_credentials: claude_config.exists(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auth_base_dir() {
        let dir = auth_base_dir();
        assert!(dir.to_string_lossy().contains(".midtown"));
        assert!(dir.to_string_lossy().ends_with("auth"));
    }

    #[test]
    fn test_profile_dir() {
        let dir = profile_dir("myprofile");
        assert!(dir.to_string_lossy().contains(".midtown"));
        assert!(dir.to_string_lossy().contains("auth"));
        assert!(dir.to_string_lossy().ends_with("myprofile"));
    }

    #[test]
    fn test_default_profile_constant() {
        assert_eq!(DEFAULT_PROFILE, "default");
    }

    #[test]
    fn test_profile_status_nonexistent() {
        // Non-existent profile should return None
        let status = profile_status("nonexistent-test-profile-xyz123");
        assert!(status.is_none());
    }
}
