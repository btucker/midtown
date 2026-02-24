//! Auth profile management for midtown.
//!
//! Manages multiple authentication profiles across providers (Claude, Codex, z.ai), allowing
//! different accounts to be used for different purposes (e.g., separate accounts for E2E
//! testing, development, production).
//!
//! ## Storage Structure
//!
//! ```text
//! ~/.midtown/
//! ├── config.toml                    # [providers.claude].auth_profile = "user@example.com"
//! ├── auth/
//! │   ├── <profile>/                 # Claude profile containers
//! │   │   └── claude/                # CLAUDE_CONFIG_DIR (set per-session)
//! │   │       ├── .claude.json       # Auth tokens (per-profile, never shared)
//! │   │       ├── projects -> ~/.midtown/platforms/claude/projects  # symlink
//! │   │       ├── tasks    -> ~/.midtown/platforms/claude/tasks     # symlink
//! │   │       └── ...                # other profile-local entries (not symlinked)
//! │   └── providers/
//! │       ├── codex/
//! │       │   └── profiles/
//! │       │       └── <profile>/     # Codex profile directories (CODEX_HOME)
//! │       └── zai/
//! │           └── profiles/
//! │               └── <profile>/     # z.ai profile directories
//! │                   ├── api_key.txt      # API key (chmod 600)
//! │                   └── base_url.txt     # Optional base URL override
//! └── platforms/
//!     └── claude/                    # Shared Claude state (explicit symlink targets only)
//!         ├── plans/
//!         ├── plugins/
//!         ├── projects/
//!         ├── settings.json
//!         ├── tasks/
//!         └── teams/
//! ```
//!
//! ## Environment Variables
//!
//! When spawning sessions, set the appropriate environment variable:
//! - Claude: `CLAUDE_CONFIG_DIR` to the profile directory
//! - Codex: `CODEX_HOME` to the profile directory
//! - z.ai: `ANTHROPIC_AUTH_TOKEN` (API key) and `ANTHROPIC_BASE_URL`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use tracing::warn;

use crate::paths::midtown_base_dir;

/// Default profile name used when none is specified.
pub const DEFAULT_PROFILE: &str = "default";

/// Claude entries that are shared via symlink across profiles.
const CLAUDE_SHARED_SYMLINK_ENTRIES: &[&str] = &[
    "plans",
    "plugins",
    "projects",
    "settings.json",
    "tasks",
    "teams",
];

fn is_claude_shared_symlink_entry(name: &str) -> bool {
    CLAUDE_SHARED_SYMLINK_ENTRIES.contains(&name)
}

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
/// existing storage layout.
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
/// Returns `~/.midtown/auth/<profile>/claude/`
/// (the directory used as CLAUDE_CONFIG_DIR).
pub fn profile_dir(name: &str) -> PathBuf {
    profile_dir_for(AuthProvider::Claude, name)
}

/// Return the Claude profile directory for a known profile email.
///
/// Used by pool-based spawn to resolve a selected email to the directory
/// that gets set as `CLAUDE_CONFIG_DIR` for the spawned process.
///
/// Returns `~/.midtown/auth/<email>/claude/`
pub fn profile_dir_for_email(email: &str) -> PathBuf {
    profile_dir_for(AuthProvider::Claude, email)
}

/// Get the profile directory for a specific provider/profile pair.
///
/// For Claude, this returns `~/.midtown/auth/<profile>/claude/`
/// (the directory that gets set as CLAUDE_CONFIG_DIR, containing .claude.json
/// plus symlinks to shared state).
/// For other providers, returns the provider-scoped profile directory as before.
pub fn profile_dir_for(provider: AuthProvider, name: &str) -> PathBuf {
    let base = provider_profiles_dir(provider).join(name);
    match provider {
        AuthProvider::Claude => base.join("claude"),
        AuthProvider::Codex | AuthProvider::Zai => base,
    }
}

/// Get the shared provider storage directory.
///
/// For Claude, returns `~/.midtown/platforms/claude/` where shared state (tasks, projects,
/// settings, etc.) lives across all auth profiles.
/// For other providers, this isn't used (they don't share state).
fn shared_provider_storage_dir(provider: AuthProvider) -> Option<PathBuf> {
    match provider {
        AuthProvider::Claude => Some(midtown_base_dir().join("platforms").join("claude")),
        AuthProvider::Codex | AuthProvider::Zai => None,
    }
}

/// Legacy Claude profile root from the pre-provider split layout.
///
/// Historical paths:
/// - `~/.midtown/auth/<profile>/` (oldest)
/// - `~/.midtown/auth/<profile>/claude/` (intermediate)
fn legacy_claude_profile_container(profile_name: &str) -> PathBuf {
    auth_base_dir().join(profile_name)
}

/// Whether a Claude profile exists in a legacy on-disk layout.
fn has_legacy_claude_profile(profile_name: &str) -> bool {
    let container = legacy_claude_profile_container(profile_name);
    container.join("claude").exists() || container.exists()
}

/// Migrate a legacy Claude profile directory to the new structure.
///
/// If a profile exists at legacy paths (`~/.midtown/auth/<profile>/` or
/// `~/.midtown/auth/<profile>/claude/`), this migrates it to:
/// `~/.midtown/auth/<profile>/claude/`.
///
/// Migration behavior:
/// 1. Move `.claude.json` to `~/.midtown/auth/<profile>/claude/.claude.json`
/// 2. Move shared symlink entries to `~/.midtown/platforms/claude/`
/// 3. Keep all other entries profile-local in `~/.midtown/auth/<profile>/claude/`
///
/// Returns `true` if migration was performed, `false` if already migrated.
fn migrate_legacy_claude_profile(profile_name: &str) -> std::io::Result<bool> {
    let new_profile_dir = profile_dir_for(AuthProvider::Claude, profile_name);
    let legacy_container = legacy_claude_profile_container(profile_name);
    let legacy_nested = legacy_container.join("claude");

    let old_profile_dir = if legacy_nested.exists() {
        legacy_nested
    } else {
        legacy_container.clone()
    };

    // If the new structure already exists, no migration needed
    if new_profile_dir.exists() {
        return Ok(false);
    }

    // If the old directory doesn't exist, nothing to migrate
    if !old_profile_dir.exists() {
        return Ok(false);
    }

    let shared_dir = shared_provider_storage_dir(AuthProvider::Claude)
        .expect("Claude provider should have shared storage");

    // Create target directories
    std::fs::create_dir_all(&new_profile_dir).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "Failed to create new profile dir {}: {}",
                new_profile_dir.display(),
                e
            ),
        )
    })?;
    std::fs::create_dir_all(&shared_dir).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "Failed to create shared dir {}: {}",
                shared_dir.display(),
                e
            ),
        )
    })?;

    // Scan the old profile directory
    for entry in std::fs::read_dir(&old_profile_dir).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!(
                "Failed to read old profile dir {}: {}",
                old_profile_dir.display(),
                e
            ),
        )
    })? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip metadata/container subdirs that are not profile data.
        if name_str == "providers"
            || name_str == "platforms"
            || name_str == "anthropic"
            || name_str == "claude"
        {
            continue;
        }

        let old_path = entry.path();

        let destination = if name_str == ".claude.json" {
            new_profile_dir.join(&name)
        } else if is_claude_shared_symlink_entry(name_str.as_ref()) {
            // Explicitly-shared Claude entries go to provider shared storage.
            shared_dir.join(&name)
        } else {
            // Everything else stays profile-local.
            new_profile_dir.join(&name)
        };

        if name_str == ".claude.json" {
            // Verify the new profile dir exists
            let dir_meta = std::fs::metadata(&new_profile_dir);
            if !new_profile_dir.exists() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "New profile dir doesn't exist: {} (metadata: {:?})",
                        new_profile_dir.display(),
                        dir_meta
                    ),
                ));
            }
            // Use copy + remove instead of rename since the destination dir already exists
            std::fs::copy(&old_path, &destination).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to copy {} to {} (dir exists: {}): {}",
                        old_path.display(),
                        destination.display(),
                        new_profile_dir.exists(),
                        e
                    ),
                )
            })?;
            std::fs::remove_file(&old_path)?;
        } else if !destination.exists() {
            if old_path.is_dir() {
                // For directories, use recursive copy + remove since rename might cross filesystems
                copy_dir_recursive(&old_path, &destination)?;
                std::fs::remove_dir_all(&old_path)?;
            } else {
                std::fs::rename(&old_path, &destination).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!(
                            "Failed to rename {} to {}: {}",
                            old_path.display(),
                            destination.display(),
                            e
                        ),
                    )
                })?;
            }
        } else {
            // Destination already exists — merge directories (missing entries
            // only) and otherwise keep destination as source of truth.
            if old_path.is_dir() && destination.is_dir() {
                merge_dir_recursive_missing(&old_path, &destination)?;
                std::fs::remove_dir_all(&old_path)?;
            } else if old_path.is_dir() {
                std::fs::remove_dir_all(&old_path)?;
            } else {
                std::fs::remove_file(&old_path)?;
            }
        }
    }

    // Try to remove the old directory if it's now empty
    let _ = std::fs::remove_dir(&old_profile_dir);
    if old_profile_dir != legacy_container {
        let _ = std::fs::remove_dir(&legacy_container);
    }

    Ok(true)
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("Failed to create dst dir {}: {}", dst.display(), e),
        )
    })?;
    for entry in std::fs::read_dir(src).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("Failed to read src dir {}: {}", src.display(), e),
        )
    })? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!(
                        "Failed to copy {} to {}: {}",
                        src_path.display(),
                        dst_path.display(),
                        e
                    ),
                )
            })?;
        }
    }
    Ok(())
}

/// Merge a directory into an existing destination directory without overwriting
/// destination files.
fn merge_dir_recursive_missing(
    src: &std::path::Path,
    dst: &std::path::Path,
) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            if dst_path.is_dir() {
                merge_dir_recursive_missing(&src_path, &dst_path)?;
            } else if !dst_path.exists() {
                copy_dir_recursive(&src_path, &dst_path)?;
            }
        } else if !dst_path.exists() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Set up a Claude profile directory with symlinks to shared storage.
///
/// This ensures:
/// 1. The profile directory exists at `~/.midtown/auth/<profile>/claude/`
/// 2. `.claude.json` in that directory is a real file (never symlinked)
/// 3. Only explicit shared entries are symlinked to `~/.midtown/platforms/claude/`
/// 4. The shared storage directory exists
///
/// This is called both at profile creation and at launch time to pick up new
/// shared files that may have appeared.
fn setup_claude_profile_symlinks(profile_name: &str) -> std::io::Result<()> {
    let profile_dir = profile_dir_for(AuthProvider::Claude, profile_name);
    let shared_dir = shared_provider_storage_dir(AuthProvider::Claude)
        .expect("Claude provider should have shared storage");

    // Ensure both directories exist
    std::fs::create_dir_all(&profile_dir)?;
    std::fs::create_dir_all(&shared_dir)?;

    // First pass: normalize profile entries.
    // - Explicit shared entries are promoted to shared storage.
    // - Non-shared entries remain local (and any legacy non-shared symlinks are removed).
    for entry in std::fs::read_dir(&profile_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".claude.json" {
            continue;
        }

        let profile_path = entry.path();
        let metadata = profile_path.symlink_metadata()?;

        if !is_claude_shared_symlink_entry(name_str.as_ref()) {
            // Legacy behavior used to symlink "everything except .claude.json".
            // Remove non-allowlisted symlinks so only explicit entries remain shared.
            if metadata.file_type().is_symlink() {
                let _ = std::fs::remove_file(&profile_path);
            }
            continue;
        }

        let shared_path = shared_dir.join(&name);

        if metadata.file_type().is_symlink() {
            // Keep valid symlinks; stale/wrong symlinks will be recreated below.
            if let Ok(existing_target) = std::fs::read_link(&profile_path)
                && existing_target == shared_path
                && shared_path.exists()
            {
                continue;
            }
            let _ = std::fs::remove_file(&profile_path);
            continue;
        }

        if !shared_path.exists() {
            if std::fs::rename(&profile_path, &shared_path).is_err() {
                if profile_path.is_dir() {
                    copy_dir_recursive(&profile_path, &shared_path)?;
                    std::fs::remove_dir_all(&profile_path)?;
                } else {
                    std::fs::copy(&profile_path, &shared_path)?;
                    std::fs::remove_file(&profile_path)?;
                }
            }
            continue;
        }

        // Shared path already exists; merge directories (missing entries only),
        // otherwise keep shared as source of truth and drop duplicate local entry.
        if profile_path.is_dir() && shared_path.is_dir() {
            merge_dir_recursive_missing(&profile_path, &shared_path)?;
            std::fs::remove_dir_all(&profile_path)?;
        } else if profile_path.is_dir() {
            std::fs::remove_dir_all(&profile_path)?;
        } else {
            std::fs::remove_file(&profile_path)?;
        }
    }

    // Second pass: ensure the explicit shared entry list is symlinked.
    for name_str in CLAUDE_SHARED_SYMLINK_ENTRIES {
        let link_path = profile_dir.join(name_str);
        let target = shared_dir.join(name_str);

        if !target.exists() {
            // Remove stale symlinks for allowlisted entries if target is absent.
            if link_path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                let _ = std::fs::remove_file(&link_path);
            }
            continue;
        }

        // If the link already exists and points to the right place, skip it.
        if (link_path.exists() || link_path.symlink_metadata().is_ok())
            && let Ok(existing_target) = std::fs::read_link(&link_path)
            && existing_target == target
        {
            continue;
        }

        // Create the symlink (Unix only for now; Windows would need different handling).
        #[cfg(unix)]
        {
            // Remove stale entry if present (could be a symlink, file, or directory).
            if link_path.symlink_metadata().is_ok() {
                if link_path.is_dir()
                    && !link_path
                        .symlink_metadata()
                        .is_ok_and(|m| m.file_type().is_symlink())
                {
                    // Real directory (not a symlink to a directory) — remove recursively.
                    let _ = std::fs::remove_dir_all(&link_path);
                } else {
                    // Symlink or regular file.
                    let _ = std::fs::remove_file(&link_path);
                }
            }
            std::os::unix::fs::symlink(&target, &link_path)?;
        }

        #[cfg(not(unix))]
        {
            eprintln!(
                "Warning: Symlink creation not supported on this platform. Skipping: {}",
                link_path.display()
            );
        }
    }

    Ok(())
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
/// For Claude profiles, this ensures the profile is set up with symlinks before returning.
pub fn current_profile_dir() -> PathBuf {
    current_profile_dir_for(AuthProvider::Claude)
}

/// Get the config directory path for the current profile for a provider.
///
/// For Claude profiles, this ensures the profile is set up with symlinks before returning.
pub fn current_profile_dir_for(provider: AuthProvider) -> PathBuf {
    let profile_name = current_profile_for(provider);

    // For Claude, ensure the profile is set up before use
    if provider == AuthProvider::Claude
        && let Err(e) = ensure_profile_dir_for(provider, &profile_name)
    {
        warn!(
            "Failed to set up {} profile '{}': {}. Profile directory may be misconfigured.",
            provider, profile_name, e
        );
    }

    profile_dir_for(provider, &profile_name)
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
/// For Claude profiles, this ensures the profile is set up with symlinks before returning.
pub fn active_profile_dir_for_project(project: &str) -> PathBuf {
    active_profile_dir_for_project_with_provider(project, AuthProvider::Claude)
}

/// Get the config directory path for the active profile for a specific project/provider.
///
/// For Claude profiles, this ensures the profile is set up with symlinks before returning.
pub fn active_profile_dir_for_project_with_provider(
    project: &str,
    provider: AuthProvider,
) -> PathBuf {
    let profile_name = active_profile_for_project_with_provider(project, provider);

    // For Claude, ensure the profile is set up before use
    if provider == AuthProvider::Claude
        && let Err(e) = ensure_profile_dir_for(provider, &profile_name)
    {
        warn!(
            "Failed to set up {} profile '{}' for project '{}': {}. Profile directory may be misconfigured.",
            provider, profile_name, project, e
        );
    }

    profile_dir_for(provider, &profile_name)
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
    if provider == AuthProvider::Claude && !dir.exists() && has_legacy_claude_profile(name) {
        // Auto-migrate existing legacy profile layouts before validating existence.
        let _ = ensure_profile_dir_for(provider, name)?;
    }

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
    let mut profiles: std::collections::HashSet<String> = std::collections::HashSet::new();

    let profiles_dir = provider_profiles_dir(provider);
    if profiles_dir.exists() {
        for entry in std::fs::read_dir(profiles_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
            {
                if provider == AuthProvider::Claude && (name == "providers" || name == "platforms")
                {
                    continue;
                }
                profiles.insert(name.to_string());
            }
        }
    }

    let mut profiles: Vec<String> = profiles.into_iter().collect();
    profiles.sort();
    Ok(profiles)
}

/// Check if a profile exists.
pub fn profile_exists(name: &str) -> bool {
    profile_exists_for(AuthProvider::Claude, name)
}

/// Check if a provider-specific profile exists.
pub fn profile_exists_for(provider: AuthProvider, name: &str) -> bool {
    if profile_dir_for(provider, name).exists() {
        return true;
    }
    provider == AuthProvider::Claude && has_legacy_claude_profile(name)
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
///
/// For Claude profiles, this also:
/// 1. Migrates legacy profile directories if needed
/// 2. Sets up symlinks to shared storage
pub fn ensure_profile_dir_for(provider: AuthProvider, name: &str) -> std::io::Result<PathBuf> {
    validate_profile_name(name)?;

    // For Claude, handle migration and symlink setup
    if provider == AuthProvider::Claude {
        // Migrate legacy structure if needed
        migrate_legacy_claude_profile(name)?;

        // Set up symlinks to shared storage
        setup_claude_profile_symlinks(name)?;
    } else {
        // For other providers, just create the directory
        let dir = profile_dir_for(provider, name);
        std::fs::create_dir_all(&dir)?;
    }

    Ok(profile_dir_for(provider, name))
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

#[path = "auth_tests.rs"]
#[cfg(test)]
mod tests;
