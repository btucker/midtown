//! Auth profile management for midtown.
//!
//! Manages multiple authentication profiles across providers (Claude, Codex), allowing
//! different accounts to be used for different purposes (e.g., separate accounts for E2E
//! testing, development, production).
//!
//! ## Storage Structure
//!
//! ```text
//! ~/.midtown/
//! ├── config.toml                    # [providers.claude].auth_profile = "user@example.com"
//! └── auth/
//!     ├── <profile>/                 # Claude profile directories (CLAUDE_CONFIG_DIR)
//!     │   └── .claude.json           # Claude auth tokens (managed by claude CLI)
//!     └── providers/
//!         └── codex/
//!             └── profiles/
//!                 └── <profile>/     # Codex profile directories (CODEX_HOME)
//! ```
//!
//! ## Environment Variables
//!
//! When spawning sessions, set the appropriate environment variable:
//! - Claude: `CLAUDE_CONFIG_DIR` to the profile directory
//! - Codex: `CODEX_HOME` to the profile directory

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::paths::midtown_base_dir;

/// Default profile name used when none is specified.
pub const DEFAULT_PROFILE: &str = "default";

/// Auth providers supported by Midtown.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum AuthProvider {
    #[default]
    Claude,
    Codex,
    Zai,
}

impl AuthProvider {
    /// Providers supported by this build, in display order.
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Zai];

    /// Iterate all supported providers.
    pub const fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Stable lower-case provider name used in config and paths.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Zai => "zai",
        }
    }

    /// Environment variable used by this provider to resolve auth/config home.
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
            Self::Zai => "", // z.ai uses multiple env vars (ANTHROPIC_AUTH_TOKEN, ANTHROPIC_BASE_URL)
        }
    }

    /// CLI executable name for interactive login.
    pub const fn cli_command(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Zai => "claude", // z.ai uses the claude CLI
        }
    }
}

impl std::fmt::Display for AuthProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for AuthProvider {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "zai" => Ok(Self::Zai),
            other => Err(format!(
                "Unsupported provider '{}'. Use one of: claude, codex, zai.",
                other
            )),
        }
    }
}

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

    // Reject leading/trailing dots (hidden dirs on Unix, special on Windows)
    if name.starts_with('.') || name.ends_with('.') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Profile name '{}' cannot start or end with a dot.", name),
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

/// Root directory for provider-scoped auth data.
///
/// For Claude this returns the legacy root (`~/.midtown/auth`) to preserve the
/// existing storage layout and avoid migration breakage.
fn provider_root(provider: AuthProvider) -> PathBuf {
    match provider {
        AuthProvider::Claude => auth_base_dir(),
        AuthProvider::Codex | AuthProvider::Zai => {
            auth_base_dir().join("providers").join(provider.as_str())
        }
    }
}

/// Returns the directory containing provider profiles.
fn provider_profiles_dir(provider: AuthProvider) -> PathBuf {
    match provider {
        AuthProvider::Claude => auth_base_dir(),
        AuthProvider::Codex | AuthProvider::Zai => provider_root(provider).join("profiles"),
    }
}

/// Get the profile directory for a specific profile.
///
/// Returns `~/.midtown/auth/<profile>/`.
pub fn profile_dir(name: &str) -> PathBuf {
    profile_dir_for(AuthProvider::Claude, name)
}

/// Get the profile directory for a specific provider/profile pair.
pub fn profile_dir_for(provider: AuthProvider, name: &str) -> PathBuf {
    provider_profiles_dir(provider).join(name)
}

/// Get the path to the current profile marker file for a provider.
fn current_profile_file_for(provider: AuthProvider) -> PathBuf {
    provider_root(provider).join("current")
}

/// Get the currently active profile name.
///
/// Resolution order:
/// 1. `[providers.claude].auth_profile` in global config.toml
/// 2. Legacy `~/.midtown/auth/current` file (migrated to config on first read)
/// 3. `DEFAULT_PROFILE`
pub fn current_profile() -> String {
    current_profile_for(AuthProvider::Claude)
}

/// Get the currently active profile name for a provider.
///
/// Resolution order (Claude only):
/// 1. `[providers.claude].auth_profile` in global config.toml
/// 2. Legacy `~/.midtown/auth/current` file (migrated to config on first read)
/// 3. `DEFAULT_PROFILE`
///
/// For other providers: Uses file-based marker at `~/.midtown/auth/providers/<provider>/current`.
pub fn current_profile_for(provider: AuthProvider) -> String {
    match provider {
        AuthProvider::Claude => {
            // Primary: read from global config
            let config = crate::config::GlobalConfig::load();
            if let Some(ref profile) = config.providers.claude.auth_profile
                && !profile.is_empty()
            {
                return profile.clone();
            }

            // Migration: check legacy file, migrate if found
            let legacy_file = current_profile_file_for(provider);
            if let Ok(contents) = std::fs::read_to_string(&legacy_file) {
                let trimmed = contents.trim().to_string();
                if !trimmed.is_empty() {
                    // Migrate to global config and clean up old file
                    if set_current_profile_in_config(&trimmed).is_ok() {
                        let _ = std::fs::remove_file(&legacy_file);
                    }
                    return trimmed;
                }
            }

            DEFAULT_PROFILE.to_string()
        }
        AuthProvider::Codex | AuthProvider::Zai => {
            // Codex and z.ai use file-based storage (no config.toml integration yet)
            std::fs::read_to_string(current_profile_file_for(provider))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| DEFAULT_PROFILE.to_string())
        }
    }
}

/// Get the config directory path for the current profile.
///
/// This is what should be set as `CLAUDE_CONFIG_DIR` when spawning Claude.
pub fn current_profile_dir() -> PathBuf {
    current_profile_dir_for(AuthProvider::Claude)
}

/// Get the config directory path for the current profile for a provider.
pub fn current_profile_dir_for(provider: AuthProvider) -> PathBuf {
    profile_dir_for(provider, &current_profile_for(provider))
}

/// Get the active auth profile for a specific project.
///
/// Resolution order:
/// 1. Project config's `auth_profile` field
/// 2. Global `[providers.claude].auth_profile` in config.toml
/// 3. `DEFAULT_PROFILE`
pub fn active_profile_for_project(project: &str) -> String {
    active_profile_for_project_with_provider(project, AuthProvider::Claude)
}

/// Get the active auth profile for a specific project and provider.
///
/// Resolution order:
/// 1. Project config provider-specific profile mapping
/// 2. Legacy `project.auth_profile` (Claude only)
/// 3. Provider current profile marker
/// 4. `DEFAULT_PROFILE`
pub fn active_profile_for_project_with_provider(project: &str, provider: AuthProvider) -> String {
    if let Some(config) = crate::config::FullProjectConfig::load(project)
        && let Some(profile) = project_profile_override(&config.project, provider)
    {
        return profile.to_string();
    }
    current_profile_for(provider)
}

/// Get the config directory path for the active profile of a specific project.
///
/// This is what should be set as `CLAUDE_CONFIG_DIR` when spawning Claude
/// for a specific project.
pub fn active_profile_dir_for_project(project: &str) -> PathBuf {
    active_profile_dir_for_project_with_provider(project, AuthProvider::Claude)
}

/// Get the config directory path for the active profile for a specific project/provider.
pub fn active_profile_dir_for_project_with_provider(
    project: &str,
    provider: AuthProvider,
) -> PathBuf {
    profile_dir_for(
        provider,
        &active_profile_for_project_with_provider(project, provider),
    )
}

/// Set the active profile.
///
/// Writes the profile name to `[providers.claude].auth_profile` in global config.toml.
/// Returns an error if the profile name is invalid or doesn't exist.
pub fn set_current_profile(name: &str) -> std::io::Result<()> {
    set_current_profile_for(AuthProvider::Claude, name)
}

/// Set the active profile for a provider.
///
/// For Claude: writes to `[providers.claude].auth_profile` in global config.toml.
/// For other providers: writes to file-based marker.
pub fn set_current_profile_for(provider: AuthProvider, name: &str) -> std::io::Result<()> {
    validate_profile_name(name)?;

    let dir = profile_dir_for(provider, name);
    if !dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Profile '{}' does not exist for {}. Create it first with: midtown auth --provider {} login {}",
                name, provider, provider, name
            ),
        ));
    }

    match provider {
        AuthProvider::Claude => {
            set_current_profile_in_config(name)?;

            // Clean up legacy file if it exists
            let legacy_file = current_profile_file_for(provider);
            if legacy_file.exists() {
                let _ = std::fs::remove_file(&legacy_file);
            }

            Ok(())
        }
        AuthProvider::Codex | AuthProvider::Zai => {
            // Codex and z.ai use file-based storage
            let current_file = current_profile_file_for(provider);
            std::fs::create_dir_all(current_file.parent().unwrap())?;
            std::fs::write(current_file, format!("{}\n", name))
        }
    }
}

/// Write the auth profile to global config.toml without validation.
///
/// Used internally by both `set_current_profile()` and the migration path
/// in `current_profile()`.
fn set_current_profile_in_config(name: &str) -> std::io::Result<()> {
    let mut config = crate::config::GlobalConfig::load();
    config.providers.claude.auth_profile = Some(name.to_string());
    config.save()
}

/// List all available profiles.
///
/// Returns a list of profile names (directory names under `~/.midtown/auth/`).
pub fn list_profiles() -> std::io::Result<Vec<String>> {
    list_profiles_for(AuthProvider::Claude)
}

/// List all available profiles for a provider.
pub fn list_profiles_for(provider: AuthProvider) -> std::io::Result<Vec<String>> {
    let profiles_dir = provider_profiles_dir(provider);
    if !profiles_dir.exists() {
        return Ok(vec![]);
    }

    let mut profiles = Vec::new();
    for entry in std::fs::read_dir(profiles_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir()
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            if provider == AuthProvider::Claude && name == "providers" {
                continue;
            }
            profiles.push(name.to_string());
        }
    }
    profiles.sort();
    Ok(profiles)
}

/// Check if a profile exists.
pub fn profile_exists(name: &str) -> bool {
    profile_exists_for(AuthProvider::Claude, name)
}

/// Check if a provider-specific profile exists.
pub fn profile_exists_for(provider: AuthProvider, name: &str) -> bool {
    profile_dir_for(provider, name).exists()
}

/// Remove a profile.
///
/// Deletes the profile directory and its contents.
/// Returns an error if attempting to remove the currently active profile.
pub fn remove_profile(name: &str) -> std::io::Result<()> {
    remove_profile_for(AuthProvider::Claude, name)
}

/// Remove a provider-specific profile.
pub fn remove_profile_for(provider: AuthProvider, name: &str) -> std::io::Result<()> {
    let current = current_profile_for(provider);
    if name == current {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "Cannot remove the active {} profile '{}'. Switch to a different profile first.",
                provider, name
            ),
        ));
    }

    let dir = profile_dir_for(provider, name);
    if !dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Profile '{}' does not exist for {}", name, provider),
        ));
    }

    std::fs::remove_dir_all(dir)
}

/// Create a profile directory if it doesn't exist.
///
/// This is called before launching `claude` for login so the config dir exists.
/// Returns an error if the profile name is invalid.
pub fn ensure_profile_dir(name: &str) -> std::io::Result<PathBuf> {
    ensure_profile_dir_for(AuthProvider::Claude, name)
}

/// Create a provider-specific profile directory if it doesn't exist.
pub fn ensure_profile_dir_for(provider: AuthProvider, name: &str) -> std::io::Result<PathBuf> {
    validate_profile_name(name)?;

    let dir = profile_dir_for(provider, name);
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
    profile_status_for(AuthProvider::Claude, name)
}

fn has_credentials_for(provider: AuthProvider, dir: &Path) -> bool {
    match provider {
        AuthProvider::Claude => dir.join(".claude.json").exists(),
        // Codex stores config/auth state under CODEX_HOME. We treat either
        // config or auth artifacts as "credentials present" for UX purposes.
        AuthProvider::Codex => {
            dir.join("auth.json").exists()
                || dir.join("credentials.json").exists()
                || dir.join("config.toml").exists()
        }
        // z.ai stores credentials in api_key.txt
        AuthProvider::Zai => dir.join("api_key.txt").exists(),
    }
}

/// Get detailed status for a provider-specific profile.
pub fn profile_status_for(provider: AuthProvider, name: &str) -> Option<ProfileStatus> {
    let dir = profile_dir_for(provider, name);
    if !dir.exists() {
        return None;
    }

    Some(ProfileStatus {
        name: name.to_string(),
        path: dir.clone(),
        is_current: current_profile_for(provider) == name,
        has_credentials: has_credentials_for(provider, &dir),
    })
}

/// Resolve the per-project profile override for a provider, if configured.
pub fn project_profile_override(
    project: &crate::config::ProjectMetadata,
    provider: AuthProvider,
) -> Option<&str> {
    if let Some(map) = project.auth_profiles.as_ref()
        && let Some(profile) = map.get(provider.as_str())
    {
        return Some(profile);
    }
    if provider == AuthProvider::Claude {
        return project.auth_profile.as_deref();
    }
    None
}

/// Set a per-project profile override for a provider.
pub fn set_project_profile_override(
    project: &mut crate::config::ProjectMetadata,
    provider: AuthProvider,
    profile: String,
) {
    let map: &mut HashMap<String, String> = project.auth_profiles.get_or_insert_with(HashMap::new);
    map.insert(provider.as_str().to_string(), profile.clone());
    if provider == AuthProvider::Claude {
        project.auth_profile = Some(profile);
    }
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
    fn test_auth_provider_all_contains_expected_providers() {
        let providers = AuthProvider::all();
        assert_eq!(
            providers,
            &[AuthProvider::Claude, AuthProvider::Codex, AuthProvider::Zai]
        );
    }

    #[test]
    fn test_codex_profile_dir_is_provider_scoped() {
        let dir = profile_dir_for(AuthProvider::Codex, "myprofile");
        let s = dir.to_string_lossy();
        assert!(s.contains(".midtown"));
        assert!(s.contains("auth"));
        assert!(s.contains("providers/codex/profiles/myprofile"));
    }

    #[test]
    fn test_zai_profile_dir_is_provider_scoped() {
        let dir = profile_dir_for(AuthProvider::Zai, "test@z.ai");
        let s = dir.to_string_lossy();
        assert!(s.contains(".midtown"));
        assert!(s.contains("auth"));
        assert!(s.contains("providers/zai/profiles/test@z.ai"));
    }

    #[test]
    fn test_zai_provider_from_str() {
        assert_eq!(AuthProvider::from_str("zai").unwrap(), AuthProvider::Zai);
        assert_eq!(AuthProvider::from_str("ZAI").unwrap(), AuthProvider::Zai);
        assert_eq!(AuthProvider::from_str(" zai ").unwrap(), AuthProvider::Zai);
    }

    #[test]
    fn test_zai_provider_as_str() {
        assert_eq!(AuthProvider::Zai.as_str(), "zai");
    }

    #[test]
    fn test_zai_provider_env_var() {
        // z.ai doesn't use a single env var for config dir
        assert_eq!(AuthProvider::Zai.env_var(), "");
    }

    #[test]
    fn test_zai_provider_cli_command() {
        assert_eq!(AuthProvider::Zai.cli_command(), "claude");
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
