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

/// Validate a profile name.
///
/// Returns an error if the name contains path traversal characters or is invalid.
/// Valid names contain alphanumeric characters, hyphens, underscores, `@`, and `.`
/// (to support email addresses as profile names).
pub fn validate_profile_name(name: &str) -> std::io::Result<()> {
    if name.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Profile name cannot be empty",
        ));
    }

    // Reject path traversal and dangerous characters
    if name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.contains(' ')
        || name.contains('\'')
        || name.contains('"')
        || name.contains('$')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Profile name '{}' contains invalid characters (/, \\, .., space, quotes, $).",
                name
            ),
        ));
    }

    // Only allow safe characters: alphanumeric, hyphen, underscore, @, .
    if !name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '@' || c == '.')
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Profile name '{}' contains invalid characters. Use alphanumeric characters, hyphens, underscores, @, or dots.",
                name
            ),
        ));
    }

    Ok(())
}

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

/// Get the active auth profile for a specific project.
///
/// Resolution order:
/// 1. Project config's `auth_profile` field
/// 2. Global `~/.midtown/auth/current` file
/// 3. `DEFAULT_PROFILE`
pub fn active_profile_for_project(project: &str) -> String {
    if let Some(config) = crate::config::FullProjectConfig::load(project)
        && let Some(ref profile) = config.project.auth_profile
    {
        return profile.clone();
    }
    current_profile()
}

/// Get the config directory path for the active profile of a specific project.
///
/// This is what should be set as `CLAUDE_CONFIG_DIR` when spawning Claude
/// for a specific project.
pub fn active_profile_dir_for_project(project: &str) -> PathBuf {
    profile_dir(&active_profile_for_project(project))
}

/// Set the active profile.
///
/// Writes the profile name to `~/.midtown/auth/current`.
/// Returns an error if the profile name is invalid or doesn't exist.
pub fn set_current_profile(name: &str) -> std::io::Result<()> {
    validate_profile_name(name)?;

    let dir = profile_dir(name);
    if !dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Profile '{}' does not exist. Create it first with: midtown auth login {}",
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
/// Returns an error if the profile name is invalid.
pub fn ensure_profile_dir(name: &str) -> std::io::Result<PathBuf> {
    validate_profile_name(name)?;

    let dir = profile_dir(name);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
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

    #[test]
    fn test_validate_profile_name_valid() {
        assert!(validate_profile_name("default").is_ok());
        assert!(validate_profile_name("e2e").is_ok());
        assert!(validate_profile_name("my-profile").is_ok());
        assert!(validate_profile_name("my_profile").is_ok());
        assert!(validate_profile_name("Profile123").is_ok());
    }

    #[test]
    fn test_validate_profile_name_email_addresses() {
        // Email addresses should be valid profile names
        assert!(validate_profile_name("user@example.com").is_ok());
        assert!(validate_profile_name("ben.tucker@company.io").is_ok());
        assert!(validate_profile_name("test@test.co").is_ok());
    }

    #[test]
    fn test_validate_profile_name_empty() {
        assert!(validate_profile_name("").is_err());
    }

    #[test]
    fn test_validate_profile_name_path_traversal() {
        // Reject path traversal attempts
        assert!(validate_profile_name("..").is_err());
        assert!(validate_profile_name("../etc").is_err());
        assert!(validate_profile_name("foo/bar").is_err());
        assert!(validate_profile_name("/tmp/evil").is_err());
        assert!(validate_profile_name("foo\\bar").is_err());
        // Double dots in email-like strings should also be rejected
        assert!(validate_profile_name("user@evil..com").is_err());
    }

    #[test]
    fn test_validate_profile_name_special_chars() {
        // Reject special characters that could cause issues
        assert!(validate_profile_name("foo'bar").is_err());
        assert!(validate_profile_name("foo\"bar").is_err());
        assert!(validate_profile_name("foo bar").is_err());
        assert!(validate_profile_name("foo$bar").is_err());
    }
}
