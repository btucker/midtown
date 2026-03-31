//! Auth profile management for midtown.
//!
//! Manages multiple authentication profiles across providers (Claude, Codex), allowing
//! different accounts to be used for different purposes (e.g., separate accounts for E2E
//! testing, development, production).
//!
//! ## Storage Structure
//!
//! ```text
//! ~/.midtown/platforms/
//! ├── claude/
//! │   ├── shared/          # settings, agents, plugins, projects, tasks, teams
//! │   ├── <profile>/       # .claude.json (token) + symlinks to ../shared/
//! │   └── current          # active profile name
//! └── codex/
//!     ├── shared/
//!     ├── <profile>/
//!     └── current
//! ```
//!
//! ## Environment Variables
//!
//! When spawning sessions, set the appropriate environment variable:
//! - Claude: `CLAUDE_CONFIG_DIR` to the profile directory
//! - Codex: `CODEX_HOME` to the profile directory

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;

use tracing::warn;

use crate::paths::midtown_base_dir;

/// Default profile name used when none is specified.
pub const DEFAULT_PROFILE: &str = "default";

/// Claude entries that are shared via symlink across profiles.
const CLAUDE_SHARED_SYMLINK_ENTRIES: &[&str] = &[
    "agents",
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
}

impl AuthProvider {
    /// Providers supported by this build, in display order.
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    /// Iterate all supported providers.
    pub const fn all() -> &'static [Self] {
        &Self::ALL
    }

    /// Stable lower-case provider name used in config and paths.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Environment variable used by this provider to resolve auth/config home.
    pub const fn env_var(self) -> &'static str {
        match self {
            Self::Claude => "CLAUDE_CONFIG_DIR",
            Self::Codex => "CODEX_HOME",
        }
    }

    /// CLI executable name for interactive login.
    pub const fn cli_command(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
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
            other => Err(format!(
                "Unsupported provider '{}'. Use one of: claude, codex.",
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

/// Get the legacy auth directory.
///
/// Returns `~/.midtown/auth/`. Used only for legacy paths (Codex profiles
/// not yet migrated to the platforms/ layout) and migration code.
pub fn auth_base_dir() -> PathBuf {
    midtown_base_dir().join("auth")
}

/// Root directory for provider-scoped auth data (legacy).
///
/// For Claude this returns the legacy root (`~/.midtown/auth`).
/// Codex still uses this path; Claude profiles have moved to
/// `~/.midtown/platforms/claude/`.
fn provider_root(provider: AuthProvider) -> PathBuf {
    match provider {
        AuthProvider::Claude => auth_base_dir(),
        AuthProvider::Codex => auth_base_dir().join("providers").join(provider.as_str()),
    }
}

/// Returns the directory containing provider profiles.
fn provider_profiles_dir(provider: AuthProvider) -> PathBuf {
    match provider {
        AuthProvider::Claude => midtown_base_dir().join("platforms").join("claude"),
        AuthProvider::Codex => provider_root(provider).join("profiles"),
    }
}

/// Get the profile directory for a specific profile.
///
/// Returns `~/.midtown/platforms/claude/<profile>/`
/// (the directory used as CLAUDE_CONFIG_DIR).
pub fn profile_dir(name: &str) -> PathBuf {
    profile_dir_for(AuthProvider::Claude, name)
}

/// Get the profile directory for a specific provider/profile pair.
///
/// For Claude, this returns `~/.midtown/platforms/claude/<profile>/`
/// (the directory that gets set as CLAUDE_CONFIG_DIR, containing .claude.json
/// plus symlinks to shared state).
/// For other providers, returns the provider-scoped profile directory as before.
pub fn profile_dir_for(provider: AuthProvider, name: &str) -> PathBuf {
    provider_profiles_dir(provider).join(name)
}

/// Get the shared provider storage directory.
///
/// For Claude, returns `~/.midtown/platforms/claude/shared/` where shared state (tasks, projects,
/// settings, etc.) lives across all auth profiles.
/// For other providers, this isn't used (they don't share state).
fn shared_provider_storage_dir(provider: AuthProvider) -> Option<PathBuf> {
    match provider {
        AuthProvider::Claude => Some(
            midtown_base_dir()
                .join("platforms")
                .join("claude")
                .join("shared"),
        ),
        AuthProvider::Codex => None,
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
/// Detects profiles at old locations:
/// - `~/.midtown/auth/<profile>/claude/` (intermediate layout)
/// - `~/.midtown/auth/<profile>/` (oldest layout)
///
/// Moves real files to `~/.midtown/platforms/claude/<profile>/`.
/// Symlinks are skipped (they pointed to the old shared location and will be
/// recreated by `setup_claude_profile_symlinks()` with correct relative targets).
///
/// Returns `true` if migration was performed, `false` if already migrated.
fn migrate_legacy_claude_profile(profile_name: &str) -> std::io::Result<bool> {
    let new_profile_dir = profile_dir_for(AuthProvider::Claude, profile_name);

    // If the new structure already exists, no migration needed
    if new_profile_dir.exists() {
        return Ok(false);
    }

    let legacy_container = legacy_claude_profile_container(profile_name);
    let legacy_nested = legacy_container.join("claude");

    let old_profile_dir = if legacy_nested.exists() {
        legacy_nested
    } else {
        legacy_container.clone()
    };

    // If the old directory doesn't exist, nothing to migrate
    if !old_profile_dir.exists() {
        return Ok(false);
    }

    // Create new profile directory
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

    // Move real files from old location to new; skip symlinks (they will be
    // recreated by setup_claude_profile_symlinks with correct relative targets).
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
        let metadata = old_path.symlink_metadata()?;

        // Skip symlinks — they pointed to the old shared location and will be
        // recreated correctly after migration.
        if metadata.file_type().is_symlink() {
            let _ = std::fs::remove_file(&old_path);
            continue;
        }

        let destination = new_profile_dir.join(&name);

        if !destination.exists() {
            if old_path.is_dir() {
                copy_dir_recursive(&old_path, &destination)?;
                std::fs::remove_dir_all(&old_path)?;
            } else {
                std::fs::copy(&old_path, &destination).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!(
                            "Failed to copy {} to {}: {}",
                            old_path.display(),
                            destination.display(),
                            e
                        ),
                    )
                })?;
                std::fs::remove_file(&old_path)?;
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
/// 1. The profile directory exists at `~/.midtown/platforms/claude/<profile>/`
/// 2. `.claude.json` in that directory is a real file (never symlinked)
/// 3. Only explicit shared entries are symlinked to `../shared/<entry>` (relative)
/// 4. The shared storage directory exists at `~/.midtown/platforms/claude/shared/`
///
/// Symlinks use relative targets (`../shared/<entry>`) so they remain valid even
/// Migrate shared entries from the top-level platform dir to the `shared/` subdirectory.
/// This is a one-time migration: if `~/.midtown/platforms/claude/agents` exists as a real
/// directory (not a symlink), move it to `~/.midtown/platforms/claude/shared/agents`.
fn migrate_shared_to_subdirectory() -> std::io::Result<()> {
    let platform_dir = crate::paths::midtown_base_dir()
        .join("platforms")
        .join("claude");
    let shared_dir = platform_dir.join("shared");

    // Check if migration is needed: shared/ doesn't exist but top-level entries do
    if shared_dir.exists() {
        return Ok(()); // Already migrated
    }

    let mut needs_migration = false;
    for entry_name in CLAUDE_SHARED_SYMLINK_ENTRIES {
        let top_level = platform_dir.join(entry_name);
        if top_level.exists() && !top_level.is_symlink() {
            needs_migration = true;
            break;
        }
    }

    if !needs_migration {
        return Ok(());
    }

    tracing::info!("migrating shared state to platforms/claude/shared/");
    std::fs::create_dir_all(&shared_dir)?;

    for entry_name in CLAUDE_SHARED_SYMLINK_ENTRIES {
        let top_level = platform_dir.join(entry_name);
        let dest = shared_dir.join(entry_name);
        if top_level.exists() && !top_level.is_symlink() && !dest.exists() {
            std::fs::rename(&top_level, &dest)?;
        }
    }

    Ok(())
}

/// if the base directory is relocated (e.g., in tests or alternate installs).
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
        let relative_target = PathBuf::from("../shared").join(name_str.as_ref());

        if metadata.file_type().is_symlink() {
            // Keep valid relative symlinks; remove stale/wrong/absolute symlinks
            // so they get recreated below.
            if let Ok(existing_target) = std::fs::read_link(&profile_path)
                && existing_target == relative_target
                && shared_path.exists()
            {
                continue;
            }
            let _ = std::fs::remove_file(&profile_path);
            continue;
        }

        // Real file/directory in profile for a shared entry — promote to shared storage.
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

    // Second pass: ensure the explicit shared entry list is symlinked using
    // relative targets (../shared/<entry>).
    for entry_name in CLAUDE_SHARED_SYMLINK_ENTRIES {
        let link_path = profile_dir.join(entry_name);
        let shared_path = shared_dir.join(entry_name);
        let relative_target = PathBuf::from("../shared").join(entry_name);

        if !shared_path.exists() {
            // Remove stale symlinks for allowlisted entries if target is absent.
            if link_path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                let _ = std::fs::remove_file(&link_path);
            }
            continue;
        }

        // If the link already exists and points to the right relative target, skip it.
        if (link_path.exists() || link_path.symlink_metadata().is_ok())
            && let Ok(existing_target) = std::fs::read_link(&link_path)
            && existing_target == relative_target
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
            std::os::unix::fs::symlink(&relative_target, &link_path)?;
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
///
/// For Claude, this is `~/.midtown/platforms/claude/current`.
/// For other providers, this is under their provider root.
fn current_profile_file_for(provider: AuthProvider) -> PathBuf {
    match provider {
        AuthProvider::Claude => midtown_base_dir()
            .join("platforms")
            .join("claude")
            .join("current"),
        AuthProvider::Codex => provider_root(provider).join("current"),
    }
}

/// Legacy location for the Claude current-profile marker file.
///
/// Returns `~/.midtown/auth/current` (the old location before the platform layout migration).
fn legacy_current_profile_file() -> PathBuf {
    auth_base_dir().join("current")
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

            // Migration: check new location first, then legacy file
            for file in [
                current_profile_file_for(provider),
                legacy_current_profile_file(),
            ] {
                if let Ok(contents) = std::fs::read_to_string(&file) {
                    let trimmed = contents.trim().to_string();
                    if !trimmed.is_empty() {
                        // Migrate to global config and clean up old file
                        if set_current_profile_in_config(&trimmed).is_ok() {
                            let _ = std::fs::remove_file(&file);
                        }
                        return trimmed;
                    }
                }
            }

            DEFAULT_PROFILE.to_string()
        }
        AuthProvider::Codex => {
            // Codex uses file-based storage (no config.toml integration yet)
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

            // Clean up file-based markers if they exist (both new and legacy locations)
            for file in [
                current_profile_file_for(provider),
                legacy_current_profile_file(),
            ] {
                if file.exists() {
                    let _ = std::fs::remove_file(&file);
                }
            }

            Ok(())
        }
        AuthProvider::Codex => {
            // Codex uses file-based storage
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
/// Returns a list of profile names (directory names under `~/.midtown/platforms/claude/`).
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
                if provider == AuthProvider::Claude
                    && (name == "providers" || name == "platforms" || name == "shared")
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

        // Migrate shared entries from top-level to shared/ subdirectory
        migrate_shared_to_subdirectory()?;

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

/// Start the OAuth login flow for a provider.
///
/// Spawns the provider's CLI which opens the default browser for OAuth
/// authentication. The CLI handles the full flow (browser open → user
/// authenticates → OAuth callback returns token).
///
/// When `inherit_stdio` is true, the child's stdin/stdout/stderr are inherited
/// so the user sees CLI output in their terminal (used by `midtown auth login`).
/// When false, stdio is suppressed for headless operation (used by the web API).
///
/// Returns the spawned child process. The caller can `.wait()` on it (CLI) or
/// drop it to let it run detached (web).
pub fn start_login(
    provider: AuthProvider,
    email: &str,
    inherit_stdio: bool,
) -> Result<std::process::Child, String> {
    let profile_dir = ensure_profile_dir_for(provider, email)
        .map_err(|e| format!("Failed to create profile directory: {}", e))?;

    let mut cmd = Command::new(provider.cli_command());

    match provider {
        AuthProvider::Claude => {
            cmd.args(["auth", "login", "--email", email])
                .env("CLAUDE_CONFIG_DIR", &profile_dir);
        }
        AuthProvider::Codex => {
            cmd.arg("login").env("CODEX_HOME", &profile_dir);
        }
    }

    if inherit_stdio {
        cmd.stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    } else {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
    }

    cmd.spawn().map_err(|e| {
        format!(
            "Failed to launch {}: {}. Is {} installed?",
            provider.cli_command(),
            e,
            provider.cli_command()
        )
    })
}

#[path = "auth_tests.rs"]
#[cfg(test)]
mod tests;
