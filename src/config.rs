//! Project-level configuration for midtown.
//!
//! Configuration is loaded from two sources:
//!
//! 1. **Global config** at `~/.midtown/config.toml`:
//!    ```toml
//!    [default]
//!    bin_command = "midtown"
//!    chat_layout = "auto"
//!    max_coworkers = 8
//!
//!    [plugins]
//!    required = [
//!        "superpowers@claude-plugins-official",
//!    ]
//!
//!    [daemon]
//!    webhook_port = 47023
//!    webhook_secret = "your-secret"
//!    webhook_restart_interval_secs = 300
//!    pr_poll_interval_secs = 60
//!    chat_monitor_enabled = true
//!
//!    [providers.claude]
//!    auth_profile = "user@example.com"
//!    ```
//!
//! 2. **Project config** at `~/.midtown/projects/<project>/config.toml`:
//!    ```toml
//!    [project]
//!    name = "midtown"
//!    repos = ["/path/to/repo"]
//!    primary_repo = "/path/to/repo"
//!
//!    [default]
//!    max_coworkers = 4
//!    chat_layout = "split"
//!    personality = "fun"  # "normal" (default), "fun", or "wild"
//!
//!    [daemon]
//!    webhook_port = 47023
//!    ```
//!
//! Project config takes precedence over global defaults.
//! Single-repo projects work with minimal config (just name, repo inferred from workdir).
//!
//! Daemon settings can also be overridden via environment variables:
//! - `MIDTOWN_WEBHOOK_PORT` (set to 0 to disable)
//! - `MIDTOWN_WEBHOOK_SECRET`
//! - `MIDTOWN_WEBHOOK_RESTART_INTERVAL`
//! - `MIDTOWN_PR_POLL_INTERVAL`
//! - `MIDTOWN_CHAT_MONITOR` (set to 0 to disable)
//! - `MIDTOWN_GITHUB_USER`

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{Item, Table};

/// Chat layout mode for the Lead session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ChatLayout {
    /// Automatically choose based on terminal width
    #[default]
    Auto,
    /// Always use split pane (side-by-side)
    Split,
    /// Always use separate window
    Window,
}

/// Project identity and repo metadata.
///
/// This is the `[project]` section of a per-project `config.toml`:
/// ```toml
/// [project]
/// name = "midtown"
/// repos = ["/path/to/repo"]
/// primary_repo = "/path/to/repo"
/// ```
///
/// For single-repo projects, only `name` is required.
/// `repos` defaults to `[primary_repo]` and `primary_repo` is inferred from workdir.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProjectMetadata {
    /// Project name (e.g., "midtown"). Used for tmux session names, paths, etc.
    #[serde(default)]
    pub name: Option<String>,

    /// List of repository paths belonging to this project.
    /// For single-repo projects, this contains just one entry.
    #[serde(default)]
    pub repos: Vec<String>,

    /// Primary repository path. This is the repo used for the daemon socket,
    /// channel, and other singleton resources.
    #[serde(default)]
    pub primary_repo: Option<String>,

    /// Auth profile to use for this project (email address).
    /// When set, overrides the global `[providers.claude].auth_profile` setting.
    #[serde(default)]
    pub auth_profile: Option<String>,
}

impl ProjectMetadata {
    /// Get the project name, falling back to the primary repo directory name.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Get the primary repo path. Falls back to the first entry in `repos`.
    pub fn primary_repo(&self) -> Option<&str> {
        self.primary_repo
            .as_deref()
            .or_else(|| self.repos.first().map(|s| s.as_str()))
    }

    /// Get the list of repos. If empty, returns the primary_repo as a single-element vec.
    pub fn repos(&self) -> Vec<&str> {
        if self.repos.is_empty() {
            self.primary_repo.as_deref().into_iter().collect()
        } else {
            self.repos.iter().map(|s| s.as_str()).collect()
        }
    }
}

/// Personality variant for agent channel/GitHub voice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Personality {
    /// Professional and concise
    #[default]
    Normal,
    /// Playful and expressive
    Fun,
    /// Over-the-top creative
    Wild,
}

impl Personality {
    /// Return the variant name as used in personalities.md headers.
    pub fn as_str(&self) -> &'static str {
        match self {
            Personality::Normal => "normal",
            Personality::Fun => "fun",
            Personality::Wild => "wild",
        }
    }
}

/// Full per-project configuration file.
///
/// This is the top-level structure for `~/.midtown/projects/<project>/config.toml`:
/// ```toml
/// [project]
/// name = "midtown"
/// repos = ["/path/to/repo"]
///
/// [default]
/// max_coworkers = 4
///
/// [daemon]
/// webhook_port = 47023
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct FullProjectConfig {
    /// Project identity and repo metadata
    #[serde(default)]
    pub project: ProjectMetadata,

    /// Project-specific overrides for default settings
    #[serde(default)]
    pub default: ProjectConfig,

    /// Project-specific daemon configuration overrides
    #[serde(default)]
    pub daemon: DaemonSection,
}

impl FullProjectConfig {
    /// Load a full project config from the given path.
    ///
    /// Returns None if the file doesn't exist.
    /// Returns default if the file can't be parsed.
    pub fn load_from(path: &Path) -> Option<Self> {
        if !path.exists() {
            return None;
        }

        std::fs::read_to_string(path)
            .ok()
            .and_then(|contents| toml::from_str(&contents).ok())
    }

    /// Load the full project config for a named project.
    ///
    /// Looks in `~/.midtown/projects/<project_name>/config.toml`.
    pub fn load(project_name: &str) -> Option<Self> {
        Self::load_from(&project_config_path(project_name))
    }

    /// Create a minimal config for a single-repo project.
    ///
    /// This is used when auto-creating config on daemon startup.
    pub fn minimal(name: &str, repo_path: &str) -> Self {
        Self {
            project: ProjectMetadata {
                name: Some(name.to_string()),
                repos: vec![repo_path.to_string()],
                primary_repo: Some(repo_path.to_string()),
                auth_profile: None,
            },
            default: ProjectConfig {
                max_coworkers: Some(8),
                ..ProjectConfig::default()
            },
            daemon: DaemonSection::default(),
        }
    }

    /// Write this config to the given path.
    ///
    /// Creates parent directories if they don't exist.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, contents)
    }

    /// Save this config for the named project.
    pub fn save(&self, project_name: &str) -> std::io::Result<()> {
        self.save_to(&project_config_path(project_name))
    }
}

/// Configuration for a single project.
///
/// Used both as global defaults and project-specific overrides.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProjectConfig {
    /// Command to invoke midtown (e.g., "midtown" or "cargo run --release --")
    #[serde(default)]
    pub bin_command: Option<String>,

    /// Chat pane layout mode (auto, split, or window)
    #[serde(default)]
    pub chat_layout: Option<ChatLayout>,

    /// Minimum terminal width for split layout in auto mode (default: 160)
    #[serde(default)]
    pub chat_min_width: Option<u16>,

    /// Maximum number of concurrent coworkers (default: 8)
    #[serde(default)]
    pub max_coworkers: Option<usize>,

    /// Personality variant for agent voice in channel/GitHub (normal, fun, wild)
    #[serde(default)]
    pub personality: Option<Personality>,

    /// User display name shown in chat and @mentions (default: "user")
    #[serde(default)]
    pub user_display_name: Option<String>,
}

impl ProjectConfig {
    /// Merge another config into this one, with `other` taking precedence.
    fn merge(&self, other: &ProjectConfig) -> ProjectConfig {
        ProjectConfig {
            bin_command: other
                .bin_command
                .clone()
                .or_else(|| self.bin_command.clone()),
            chat_layout: other.chat_layout.or(self.chat_layout),
            chat_min_width: other.chat_min_width.or(self.chat_min_width),
            max_coworkers: other.max_coworkers.or(self.max_coworkers),
            personality: other.personality.or(self.personality),
            user_display_name: other
                .user_display_name
                .clone()
                .or_else(|| self.user_display_name.clone()),
        }
    }

    /// Get bin_command with fallback to default.
    pub fn bin_command(&self) -> String {
        self.bin_command
            .clone()
            .unwrap_or_else(|| "midtown".to_string())
    }

    /// Get chat_layout with fallback to default.
    pub fn chat_layout(&self) -> ChatLayout {
        self.chat_layout.unwrap_or_default()
    }

    /// Get chat_min_width with fallback to default.
    pub fn chat_min_width(&self) -> u16 {
        self.chat_min_width.unwrap_or(160)
    }

    /// Get max_coworkers, or None if not configured (falls back to daemon default).
    pub fn max_coworkers(&self) -> Option<usize> {
        self.max_coworkers
    }

    /// Get personality with fallback to Normal.
    pub fn personality(&self) -> Personality {
        self.personality.unwrap_or_default()
    }

    /// Get user display name, or None if not configured (falls back to "user").
    pub fn user_display_name(&self) -> Option<&str> {
        self.user_display_name.as_deref()
    }
}

/// Configuration for Claude Code plugins.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PluginsConfig {
    /// List of required plugin names (e.g., "superpowers@claude-plugins-official")
    #[serde(default)]
    pub required: Vec<String>,
}

/// Daemon configuration section.
///
/// These settings can be overridden by environment variables:
/// - `MIDTOWN_WEBHOOK_PORT` (set to 0 to disable)
/// - `MIDTOWN_WEBHOOK_SECRET`
/// - `MIDTOWN_WEBHOOK_RESTART_INTERVAL`
/// - `MIDTOWN_PR_POLL_INTERVAL`
/// - `MIDTOWN_CHAT_MONITOR` (set to 0 to disable)
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct DaemonSection {
    /// Port for the webhook server (default: 47023, set to 0 to disable)
    #[serde(default)]
    pub webhook_port: Option<u16>,

    /// GitHub webhook secret for signature verification
    #[serde(default)]
    pub webhook_secret: Option<String>,

    /// Interval in seconds to restart webhook forwarder (default: 300)
    #[serde(default)]
    pub webhook_restart_interval_secs: Option<u64>,

    /// Interval in seconds to poll PRs for actionable issues (default: 60)
    #[serde(default)]
    pub pr_poll_interval_secs: Option<u64>,

    /// Enable chat monitor for @mention routing (default: true)
    #[serde(default)]
    pub chat_monitor_enabled: Option<bool>,

    /// GitHub username for `gh` CLI authentication.
    /// When set, fetches the user's token and sets GH_TOKEN env var at daemon startup.
    /// This is faster and more reliable than `gh auth switch` (no global state races).
    #[serde(default)]
    pub github_user: Option<String>,
}

impl DaemonSection {
    /// Merge another daemon section into this one, with `other` taking precedence.
    pub fn merge(&self, other: &DaemonSection) -> DaemonSection {
        DaemonSection {
            webhook_port: other.webhook_port.or(self.webhook_port),
            webhook_secret: other
                .webhook_secret
                .clone()
                .or_else(|| self.webhook_secret.clone()),
            webhook_restart_interval_secs: other
                .webhook_restart_interval_secs
                .or(self.webhook_restart_interval_secs),
            pr_poll_interval_secs: other.pr_poll_interval_secs.or(self.pr_poll_interval_secs),
            chat_monitor_enabled: other.chat_monitor_enabled.or(self.chat_monitor_enabled),
            github_user: other
                .github_user
                .clone()
                .or_else(|| self.github_user.clone()),
        }
    }
}

/// Claude provider configuration within `[providers.claude]`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ClaudeProviderConfig {
    /// Auth profile (email address) to use globally.
    /// Equivalent to the old `~/.midtown/auth/current` file.
    #[serde(default)]
    pub auth_profile: Option<String>,
}

/// Provider configuration section (`[providers]`).
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProvidersConfig {
    /// Claude Code provider settings
    #[serde(default)]
    pub claude: ClaudeProviderConfig,
}

/// Recursively merge new_table values into target, preserving comments and formatting.
fn merge_tables(target: &mut Table, new_table: &Table) {
    for (key, new_value) in new_table.iter() {
        match (target.get_mut(key), new_value) {
            // Both are tables - recurse
            (Some(Item::Table(target_table)), Item::Table(new_table)) => {
                merge_tables(target_table, new_table);
            }
            // Target exists but isn't a table, or new value isn't a table - replace value
            (Some(existing), new_item) => {
                if let Some(new_val) = new_item.as_value() {
                    *existing = Item::Value(new_val.clone());
                }
            }
            // Key doesn't exist in target - add it
            (None, new_item) => {
                target.insert(key, new_item.clone());
            }
        }
    }
}

/// Global configuration from `~/.midtown/config.toml`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct GlobalConfig {
    /// Default settings for all projects
    #[serde(default)]
    pub default: ProjectConfig,

    /// Plugin configuration
    #[serde(default)]
    pub plugins: PluginsConfig,

    /// Daemon configuration
    #[serde(default)]
    pub daemon: DaemonSection,

    /// Provider configuration (auth profiles, etc.)
    #[serde(default)]
    pub providers: ProvidersConfig,
}

impl GlobalConfig {
    /// Load global configuration from `~/.midtown/config.toml`.
    ///
    /// If the file doesn't exist, generates a template with all options
    /// commented out so users can discover and enable them.
    /// Returns default config if file doesn't exist or can't be parsed.
    pub fn load() -> Self {
        let path = global_config_path();

        if !path.exists() {
            // Generate template config so users can discover available options
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(&path, Self::default_template());
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save this global config back to `~/.midtown/config.toml`.
    ///
    /// Loads the existing file first (to preserve comments/structure from manual edits),
    /// then overlays the current struct values and writes back.
    /// Creates parent directories if needed.
    pub fn save(&self) -> std::io::Result<()> {
        self.save_to(&global_config_path())
    }

    /// Save this global config to a specific path (used for testing).
    ///
    /// Loads the existing file first (to preserve comments/structure from manual edits),
    /// then overlays the current struct values and writes back.
    /// Creates parent directories if needed.
    pub fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // If file exists, load and update it to preserve comments
        let contents = if path.exists() {
            let existing = std::fs::read_to_string(path)?;
            let mut doc = existing
                .parse::<toml_edit::DocumentMut>()
                .map_err(std::io::Error::other)?;

            // Serialize current struct to get new values
            let new_values = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
            let new_table: toml_edit::Table = new_values
                .parse::<toml_edit::DocumentMut>()
                .map_err(std::io::Error::other)?
                .as_table()
                .clone();

            // Update document with new values while preserving comments/formatting
            merge_tables(doc.as_table_mut(), &new_table);

            doc.to_string()
        } else {
            // No existing file, just serialize normally
            toml::to_string_pretty(self).map_err(std::io::Error::other)?
        };

        std::fs::write(path, contents)
    }

    /// Generate a commented-out template with all available global config options.
    ///
    /// NOTE: Update this template when adding new fields to GlobalConfig,
    /// ProjectConfig, PluginsConfig, or DaemonSection.
    pub fn default_template() -> String {
        r#"# Midtown global configuration
# Options here apply to all projects unless overridden by project config.
# Uncomment and modify options as needed.

[default]
# Command to run the midtown binary (useful if not on PATH)
# bin_command = "midtown"

# Chat pane layout: "auto", "split", or "window"
# chat_layout = "auto"

# Minimum terminal width (columns) before auto layout switches to bottom
# chat_min_width = 160

# Maximum concurrent coworkers
# max_coworkers = 8

# Personality for coworker messages: "normal", "fun", or "wild"
# personality = "normal"

# Your display name shown in chat and @mentions (default: "user")
# user_display_name = "Ben"

[plugins]
# Required plugins to install for all projects
# required = ["superpowers@claude-plugins-official"]

[daemon]
# Port for the webhook server (set to 0 to disable)
# webhook_port = 47023

# GitHub webhook secret for signature verification
# webhook_secret = ""

# Interval in seconds to restart webhook forwarder
# webhook_restart_interval_secs = 300

# Interval in seconds to poll PRs for actionable issues
# pr_poll_interval_secs = 30

# Enable chat monitor for @mention routing
# chat_monitor_enabled = true

# GitHub username for gh CLI authentication
# When set, fetches token and sets GH_TOKEN env var at daemon startup
# github_user = ""

[providers.claude]
# Auth profile (email address) to use for Claude Code sessions
# This replaces the old ~/.midtown/auth/current file
# auth_profile = "user@example.com"
"#
        .to_string()
    }

    /// Get the list of required plugins.
    pub fn required_plugins(&self) -> &[String] {
        &self.plugins.required
    }
}

/// Load project-specific configuration from `~/.midtown/projects/<project>/config.toml`.
///
/// Returns None if the file doesn't exist.
/// Supports both the new structured format (with `[project]`, `[default]`, `[daemon]` sections)
/// and the legacy flat format (top-level keys like `bin_command`, `max_coworkers`).
fn load_project_config(project_name: &str) -> Option<ProjectConfig> {
    let path = project_config_path(project_name);

    if !path.exists() {
        return None;
    }

    let contents = std::fs::read_to_string(&path).ok()?;

    // Try the new structured format first (with [project], [default], [daemon] sections)
    if let Ok(full) = toml::from_str::<FullProjectConfig>(&contents) {
        // If the [default] section has any values, use the structured format
        if full.default.bin_command.is_some()
            || full.default.chat_layout.is_some()
            || full.default.chat_min_width.is_some()
            || full.default.max_coworkers.is_some()
            || full.default.personality.is_some()
            || full.project.name.is_some()
            || full.project.auth_profile.is_some()
            || full.daemon.github_user.is_some()
        {
            return Some(full.default);
        }
    }

    // Fall back to legacy flat format
    toml::from_str(&contents).ok()
}

/// Load the full project config (including [project] and [daemon] sections).
///
/// Returns None if the file doesn't exist.
pub fn load_full_project_config(project_name: &str) -> Option<FullProjectConfig> {
    FullProjectConfig::load(project_name)
}

/// Get the project-specific daemon configuration, merged with global.
///
/// Priority: project daemon section > global daemon section.
pub fn get_project_daemon_config(project_name: &str) -> DaemonSection {
    let global = GlobalConfig::load();
    let project = FullProjectConfig::load(project_name);

    match project {
        Some(proj) => global.daemon.merge(&proj.daemon),
        None => global.daemon,
    }
}

/// Get the effective configuration for a project.
///
/// Merges global defaults with project-specific overrides.
pub fn get_project_config(project_name: &str) -> ProjectConfig {
    let global = GlobalConfig::load();

    match load_project_config(project_name) {
        Some(project) => global.default.merge(&project),
        None => global.default,
    }
}

/// Ensure a project config.toml exists, creating a minimal one if needed.
///
/// Called on daemon startup to ensure every project has a config file.
/// If the file already exists, it is not modified.
/// If it doesn't exist, a minimal config is created with the project name
/// and repo path inferred from the working directory.
pub fn ensure_project_config(project_name: &str, workdir: &Path) -> std::io::Result<()> {
    // Reject coworker names to prevent worktree directories from being
    // registered as projects (e.g., "broadway" instead of "midtown").
    if crate::coworker::is_coworker_name(project_name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Refusing to create project config for '{}': this is a coworker name, not a project",
                project_name
            ),
        ));
    }

    let path = project_config_path(project_name);
    if path.exists() {
        return Ok(());
    }

    let repo_path = workdir.to_string_lossy().to_string();
    let config = FullProjectConfig::minimal(project_name, &repo_path);
    config.save_to(&path)
}

/// Get the path to the global config file.
pub fn global_config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("config.toml")
}

/// Get the path to a project-specific config file.
pub fn project_config_path(project_name: &str) -> PathBuf {
    crate::paths::projects_dir_for_repo(project_name).join("config.toml")
}

/// Clear the `auth_profile` override from all project configs.
///
/// When switching auth globally (`--all`), per-project overrides must be
/// cleared so every project falls through to the global
/// `[providers.claude].auth_profile` setting. Without this, projects that had a
/// per-project `auth_profile` set would ignore the global switch because
/// `active_profile_for_project()` checks the project config first.
pub fn clear_all_project_auth_profiles() {
    let projects_dir = crate::paths::midtown_base_dir().join("projects");

    let entries = match std::fs::read_dir(&projects_dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let config_path = entry.path().join("config.toml");
        if let Some(mut config) = FullProjectConfig::load_from(&config_path)
            && config.project.auth_profile.is_some()
        {
            config.project.auth_profile = None;
            let _ = config.save_to(&config_path);
        }
    }
}

/// Starting port for auto-assigned per-project webhook ports.
/// Port 47022 is reserved for the shared multi-project webserver.
const AUTO_PORT_START: u16 = 47023;

/// Scan all project configs and collect the webhook ports that are in use.
///
/// Returns a sorted list of ports found in `[daemon].webhook_port` across
/// all project config files in `~/.midtown/projects/*/config.toml`.
pub fn collect_used_webhook_ports() -> Vec<u16> {
    let projects_dir = crate::paths::midtown_base_dir().join("projects");
    let mut ports = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&projects_dir) {
        for entry in entries.flatten() {
            let config_path = entry.path().join("config.toml");
            if let Some(full) = FullProjectConfig::load_from(&config_path)
                && let Some(port) = full.daemon.webhook_port
                && port > 0
            {
                ports.push(port);
            }
        }
    }

    ports.sort();
    ports
}

/// Auto-assign a webhook port for a project.
///
/// Scans all existing project configs for used ports and picks the next
/// available one starting from `AUTO_PORT_START` (47023).
/// Port 47022 is reserved for the shared webserver.
///
/// The assigned port is written back to the project's config.toml so it
/// remains stable across restarts.
pub fn assign_webhook_port(project_name: &str) -> u16 {
    let used_ports = collect_used_webhook_ports();

    // Find next available port starting from AUTO_PORT_START
    let mut port = AUTO_PORT_START;
    for used in &used_ports {
        if *used == port {
            port += 1;
        } else if *used > port {
            break;
        }
    }

    // Write the assigned port back to config.toml
    let path = project_config_path(project_name);
    let mut config = FullProjectConfig::load_from(&path).unwrap_or_default();
    config.daemon.webhook_port = Some(port);
    let _ = config.save_to(&path);

    port
}

/// Get the bin_command for the current project.
///
/// Determines project name from the current git repo and looks up config.
pub fn get_bin_command() -> String {
    let project_name = get_project_name().unwrap_or_default();

    if project_name.is_empty() {
        GlobalConfig::load().default.bin_command()
    } else {
        get_project_config(&project_name).bin_command()
    }
}

/// Get the chat layout configuration for the current project.
pub fn get_chat_layout() -> (ChatLayout, u16) {
    let project_name = get_project_name().unwrap_or_default();

    let config = if project_name.is_empty() {
        GlobalConfig::load().default
    } else {
        get_project_config(&project_name)
    };

    (config.chat_layout(), config.chat_min_width())
}

/// Get the list of required plugins from config.
pub fn get_required_plugins() -> Vec<String> {
    GlobalConfig::load().plugins.required.clone()
}

/// Get the personality setting for the current project.
pub fn get_personality() -> Personality {
    let project_name = get_project_name().unwrap_or_default();

    let config = if project_name.is_empty() {
        GlobalConfig::load().default
    } else {
        get_project_config(&project_name)
    };

    config.personality()
}

/// Get the user display name for the current project, if configured.
pub fn get_user_display_name() -> Option<String> {
    let project_name = get_project_name().unwrap_or_default();

    let config = if project_name.is_empty() {
        GlobalConfig::load().default
    } else {
        get_project_config(&project_name)
    };

    config.user_display_name().map(|s| s.to_string())
}

/// Get the user display name for a specific project, if configured.
pub fn get_user_display_name_for_project(project_name: &str) -> Option<String> {
    get_project_config(project_name)
        .user_display_name()
        .map(|s| s.to_string())
}

/// Get the current project name from git repo root directory.
fn get_project_name() -> Option<String> {
    crate::paths::detect_repo_name()
}

// Legacy alias for backwards compatibility
#[deprecated(note = "Use global_config_path() instead")]
pub fn config_path() -> PathBuf {
    global_config_path()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GlobalConfig::default();
        assert_eq!(config.default.bin_command(), "midtown");
    }

    #[test]
    fn test_parse_global_config() {
        let toml = r#"
[default]
bin_command = "midtown"
chat_layout = "split"

[plugins]
required = ["test-plugin"]
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.default.bin_command(), "midtown");
        assert_eq!(config.default.chat_layout(), ChatLayout::Split);
        assert_eq!(config.required_plugins(), &["test-plugin"]);
    }

    #[test]
    fn test_parse_project_config() {
        let toml = r#"
bin_command = "cargo run --release --"
chat_layout = "window"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.bin_command(), "cargo run --release --");
        assert_eq!(config.chat_layout(), ChatLayout::Window);
    }

    #[test]
    fn test_project_config_partial() {
        // Project config can specify only some fields
        let toml = r#"
bin_command = "custom-command"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.bin_command(), "custom-command");
        // Uses defaults for unspecified fields
        assert_eq!(config.chat_layout(), ChatLayout::Auto);
        assert_eq!(config.chat_min_width(), 160);
    }

    #[test]
    fn test_merge_configs() {
        let global = ProjectConfig {
            bin_command: Some("midtown".to_string()),
            chat_layout: Some(ChatLayout::Auto),
            chat_min_width: Some(160),
            max_coworkers: Some(8),
            personality: Some(Personality::Fun),
            user_display_name: None,
        };

        let project = ProjectConfig {
            bin_command: Some("custom".to_string()),
            chat_layout: None, // Not overridden
            chat_min_width: Some(200),
            max_coworkers: None, // Not overridden
            personality: None,   // Not overridden
            user_display_name: None,
        };

        let merged = global.merge(&project);

        assert_eq!(merged.bin_command(), "custom"); // Overridden
        assert_eq!(merged.chat_layout(), ChatLayout::Auto); // From global
        assert_eq!(merged.chat_min_width(), 200); // Overridden
        assert_eq!(merged.max_coworkers(), Some(8)); // From global
        assert_eq!(merged.personality(), Personality::Fun); // From global
    }

    #[test]
    fn test_merge_configs_max_coworkers_override() {
        let global = ProjectConfig {
            bin_command: Some("midtown".to_string()),
            chat_layout: None,
            chat_min_width: None,
            max_coworkers: Some(8),
            personality: None,
            user_display_name: None,
        };

        let project = ProjectConfig {
            bin_command: None,
            chat_layout: None,
            chat_min_width: None,
            max_coworkers: Some(4),
            personality: None,
            user_display_name: None,
        };

        let merged = global.merge(&project);
        assert_eq!(merged.max_coworkers(), Some(4)); // Project overrides global
    }

    #[test]
    fn test_global_config_path() {
        let path = global_config_path();
        assert!(path.to_string_lossy().contains(".midtown"));
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }

    #[test]
    fn test_project_config_path() {
        let path = project_config_path("myproject");
        assert!(path.to_string_lossy().contains(".midtown"));
        assert!(path.to_string_lossy().contains("projects"));
        assert!(path.to_string_lossy().contains("myproject"));
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }

    #[test]
    fn test_chat_layout_default() {
        let config = ProjectConfig::default();
        assert_eq!(config.chat_layout(), ChatLayout::Auto);
        assert_eq!(config.chat_min_width(), 160);
    }

    #[test]
    fn test_chat_layout_parse() {
        let toml = r#"
[default]
chat_layout = "split"
chat_min_width = 200
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.default.chat_layout(), ChatLayout::Split);
        assert_eq!(config.default.chat_min_width(), 200);
    }

    #[test]
    fn test_chat_layout_auto() {
        let toml = r#"
[default]
chat_layout = "auto"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.default.chat_layout(), ChatLayout::Auto);
    }

    #[test]
    fn test_plugins_config_default() {
        let config = GlobalConfig::default();
        assert!(config.plugins.required.is_empty());
        assert!(config.required_plugins().is_empty());
    }

    #[test]
    fn test_plugins_config_parse() {
        let toml = r#"
[plugins]
required = [
    "superpowers@claude-plugins-official",
    "code-review@claude-plugins-official",
    "commit-commands@claude-plugins-official",
]

[default]
bin_command = "midtown"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.plugins.required.len(), 3);
        assert_eq!(
            config.plugins.required[0],
            "superpowers@claude-plugins-official"
        );
        assert_eq!(
            config.required_plugins(),
            &[
                "superpowers@claude-plugins-official",
                "code-review@claude-plugins-official",
                "commit-commands@claude-plugins-official",
            ]
        );
    }

    #[test]
    fn test_plugins_config_empty() {
        let toml = r#"
[plugins]

[default]
bin_command = "midtown"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert!(config.plugins.required.is_empty());
    }

    #[test]
    fn test_max_coworkers_parse() {
        let toml = r#"
[default]
max_coworkers = 8
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.default.max_coworkers(), Some(8));
    }

    #[test]
    fn test_max_coworkers_project_config() {
        let toml = r#"
bin_command = "midtown"
max_coworkers = 4
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.max_coworkers(), Some(4));
        assert_eq!(config.bin_command(), "midtown");
    }

    #[test]
    fn test_max_coworkers_default_none() {
        let config = ProjectConfig::default();
        assert_eq!(config.max_coworkers(), None);
    }

    #[test]
    fn test_daemon_section_default() {
        let config = GlobalConfig::default();
        assert!(config.daemon.webhook_port.is_none());
        assert!(config.daemon.webhook_secret.is_none());
        assert!(config.daemon.webhook_restart_interval_secs.is_none());
        assert!(config.daemon.pr_poll_interval_secs.is_none());
        assert!(config.daemon.chat_monitor_enabled.is_none());
        assert!(config.daemon.github_user.is_none());
    }

    #[test]
    fn test_daemon_section_parse() {
        let toml = r#"
[daemon]
webhook_port = 8080
webhook_secret = "my-secret"
webhook_restart_interval_secs = 600
pr_poll_interval_secs = 120
chat_monitor_enabled = false
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.daemon.webhook_port, Some(8080));
        assert_eq!(config.daemon.webhook_secret, Some("my-secret".to_string()));
        assert_eq!(config.daemon.webhook_restart_interval_secs, Some(600));
        assert_eq!(config.daemon.pr_poll_interval_secs, Some(120));
        assert_eq!(config.daemon.chat_monitor_enabled, Some(false));
    }

    #[test]
    fn test_daemon_section_partial() {
        let toml = r#"
[daemon]
webhook_port = 9000
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.daemon.webhook_port, Some(9000));
        // Other fields default to None
        assert!(config.daemon.webhook_secret.is_none());
        assert!(config.daemon.webhook_restart_interval_secs.is_none());
        assert!(config.daemon.pr_poll_interval_secs.is_none());
        assert!(config.daemon.chat_monitor_enabled.is_none());
    }

    #[test]
    fn test_daemon_section_disable_webhook() {
        let toml = r#"
[daemon]
webhook_port = 0
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.daemon.webhook_port, Some(0));
    }

    #[test]
    fn test_daemon_github_user_parse() {
        let toml = r#"
[daemon]
github_user = "midtown-sh"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.daemon.github_user, Some("midtown-sh".to_string()));
    }

    #[test]
    fn test_daemon_github_user_merge_override() {
        let global = DaemonSection {
            github_user: Some("global-user".to_string()),
            ..DaemonSection::default()
        };
        let project = DaemonSection {
            github_user: Some("project-user".to_string()),
            ..DaemonSection::default()
        };
        let merged = global.merge(&project);
        assert_eq!(merged.github_user, Some("project-user".to_string()));
    }

    #[test]
    fn test_daemon_github_user_merge_fallback() {
        let global = DaemonSection {
            github_user: Some("global-user".to_string()),
            ..DaemonSection::default()
        };
        let project = DaemonSection::default();
        let merged = global.merge(&project);
        assert_eq!(merged.github_user, Some("global-user".to_string()));
    }

    #[test]
    fn test_personality_default() {
        let config = ProjectConfig::default();
        assert_eq!(config.personality(), Personality::Normal);
    }

    #[test]
    fn test_personality_parse() {
        let toml = r#"
[default]
personality = "fun"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.default.personality(), Personality::Fun);
    }

    #[test]
    fn test_personality_wild() {
        let toml = r#"
personality = "wild"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.personality(), Personality::Wild);
    }

    #[test]
    fn test_personality_normal_explicit() {
        let toml = r#"
personality = "normal"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.personality(), Personality::Normal);
    }

    #[test]
    fn test_personality_merge_override() {
        let global = ProjectConfig {
            bin_command: None,
            chat_layout: None,
            chat_min_width: None,
            max_coworkers: None,
            personality: Some(Personality::Normal),
            user_display_name: None,
        };
        let project = ProjectConfig {
            bin_command: None,
            chat_layout: None,
            chat_min_width: None,
            max_coworkers: None,
            personality: Some(Personality::Wild),
            user_display_name: None,
        };
        let merged = global.merge(&project);
        assert_eq!(merged.personality(), Personality::Wild);
    }

    #[test]
    fn test_user_display_name_merge() {
        // Global sets display name, project doesn't override
        let global = ProjectConfig {
            user_display_name: Some("Ben".to_string()),
            ..ProjectConfig::default()
        };
        let project = ProjectConfig::default();
        let merged = global.merge(&project);
        assert_eq!(merged.user_display_name(), Some("Ben"));

        // Project overrides global
        let project = ProjectConfig {
            user_display_name: Some("Alice".to_string()),
            ..ProjectConfig::default()
        };
        let merged = global.merge(&project);
        assert_eq!(merged.user_display_name(), Some("Alice"));

        // Neither sets it
        let global = ProjectConfig::default();
        let project = ProjectConfig::default();
        let merged = global.merge(&project);
        assert_eq!(merged.user_display_name(), None);
    }

    #[test]
    fn test_user_display_name_deserialization() {
        let toml_str = r#"
            user_display_name = "Ben"
        "#;
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.user_display_name(), Some("Ben"));

        // Missing field should be None
        let toml_str = "";
        let config: ProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.user_display_name(), None);
    }

    #[test]
    fn test_personality_as_str() {
        assert_eq!(Personality::Normal.as_str(), "normal");
        assert_eq!(Personality::Fun.as_str(), "fun");
        assert_eq!(Personality::Wild.as_str(), "wild");
    }

    #[test]
    fn test_project_metadata_default() {
        let meta = ProjectMetadata::default();
        assert!(meta.name().is_none());
        assert!(meta.primary_repo().is_none());
        assert!(meta.repos().is_empty());
    }

    #[test]
    fn test_project_metadata_name_only() {
        let toml_str = r#"
[project]
name = "myapp"
"#;
        let config: FullProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.project.name(), Some("myapp"));
        assert!(config.project.primary_repo().is_none());
        assert!(config.project.repos().is_empty());
    }

    #[test]
    fn test_project_metadata_single_repo() {
        let toml_str = r#"
[project]
name = "midtown"
repos = ["/home/user/midtown"]
primary_repo = "/home/user/midtown"
"#;
        let config: FullProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.project.name(), Some("midtown"));
        assert_eq!(config.project.primary_repo(), Some("/home/user/midtown"));
        assert_eq!(config.project.repos(), vec!["/home/user/midtown"]);
    }

    #[test]
    fn test_project_metadata_primary_repo_fallback() {
        // When repos is set but primary_repo is not, primary_repo falls back to first repo
        let toml_str = r#"
[project]
name = "multi"
repos = ["/path/a", "/path/b"]
"#;
        let config: FullProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.project.primary_repo(), Some("/path/a"));
    }

    #[test]
    fn test_project_metadata_repos_fallback() {
        // When repos is empty but primary_repo is set, repos() returns [primary_repo]
        let toml_str = r#"
[project]
name = "single"
primary_repo = "/path/to/repo"
"#;
        let config: FullProjectConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.project.repos(), vec!["/path/to/repo"]);
    }

    #[test]
    fn test_full_project_config_parse() {
        let toml_str = r#"
[project]
name = "midtown"
repos = ["/path/to/repo"]
primary_repo = "/path/to/repo"

[default]
max_coworkers = 4
chat_layout = "split"

[daemon]
webhook_port = 47023
"#;
        let config: FullProjectConfig = toml::from_str(toml_str).unwrap();

        assert_eq!(config.project.name(), Some("midtown"));
        assert_eq!(config.project.primary_repo(), Some("/path/to/repo"));
        assert_eq!(config.default.max_coworkers(), Some(4));
        assert_eq!(config.default.chat_layout(), ChatLayout::Split);
        assert_eq!(config.daemon.webhook_port, Some(47023));
    }

    #[test]
    fn test_full_project_config_minimal() {
        let config = FullProjectConfig::minimal("myapp", "/home/user/myapp");

        assert_eq!(config.project.name(), Some("myapp"));
        assert_eq!(config.project.primary_repo(), Some("/home/user/myapp"));
        assert_eq!(config.project.repos(), vec!["/home/user/myapp"]);
        assert_eq!(config.default.max_coworkers(), Some(8));
        assert!(config.daemon.webhook_port.is_none());
    }

    #[test]
    fn test_full_project_config_empty_sections() {
        // An empty file should parse successfully with all defaults
        let toml_str = "";
        let config: FullProjectConfig = toml::from_str(toml_str).unwrap();

        assert!(config.project.name().is_none());
        assert!(config.project.repos().is_empty());
        assert!(config.default.max_coworkers().is_none());
        assert!(config.daemon.webhook_port.is_none());
    }

    #[test]
    fn test_full_project_config_partial_sections() {
        // Only [project] section, no [default] or [daemon]
        let toml_str = r#"
[project]
name = "solo"
"#;
        let config: FullProjectConfig = toml::from_str(toml_str).unwrap();

        assert_eq!(config.project.name(), Some("solo"));
        assert!(config.default.bin_command.is_none());
        assert!(config.daemon.webhook_port.is_none());
    }

    #[test]
    fn test_full_project_config_roundtrip() {
        let config = FullProjectConfig::minimal("roundtrip", "/tmp/roundtrip");
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: FullProjectConfig = toml::from_str(&serialized).unwrap();

        assert_eq!(config.project.name(), deserialized.project.name());
        assert_eq!(
            config.project.primary_repo(),
            deserialized.project.primary_repo()
        );
    }

    #[test]
    fn test_full_project_config_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let config = FullProjectConfig::minimal("testproj", "/tmp/testproj");
        config.save_to(&path).unwrap();

        let loaded = FullProjectConfig::load_from(&path).unwrap();
        assert_eq!(loaded.project.name(), Some("testproj"));
        assert_eq!(loaded.project.primary_repo(), Some("/tmp/testproj"));
    }

    #[test]
    fn test_daemon_section_merge() {
        let global = DaemonSection {
            webhook_port: Some(47022),
            webhook_secret: Some("global-secret".to_string()),
            webhook_restart_interval_secs: Some(300),
            pr_poll_interval_secs: Some(60),
            chat_monitor_enabled: Some(true),
            github_user: Some("global-user".to_string()),
        };

        let project = DaemonSection {
            webhook_port: Some(47023),
            webhook_secret: None,
            webhook_restart_interval_secs: None,
            pr_poll_interval_secs: Some(120),
            chat_monitor_enabled: None,
            github_user: None,
        };

        let merged = global.merge(&project);
        assert_eq!(merged.webhook_port, Some(47023)); // Project overrides
        assert_eq!(merged.webhook_secret, Some("global-secret".to_string())); // Falls back to global
        assert_eq!(merged.webhook_restart_interval_secs, Some(300)); // Falls back to global
        assert_eq!(merged.pr_poll_interval_secs, Some(120)); // Project overrides
        assert_eq!(merged.chat_monitor_enabled, Some(true)); // Falls back to global
        assert_eq!(merged.github_user, Some("global-user".to_string())); // Falls back to global
    }

    #[test]
    fn test_daemon_section_merge_empty() {
        let global = DaemonSection {
            webhook_port: Some(47022),
            webhook_secret: None,
            webhook_restart_interval_secs: None,
            pr_poll_interval_secs: None,
            chat_monitor_enabled: None,
            github_user: None,
        };

        let empty = DaemonSection::default();
        let merged = global.merge(&empty);
        assert_eq!(merged.webhook_port, Some(47022)); // Global preserved
    }

    #[test]
    fn test_ensure_project_config_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Directly test save_to / load_from since ensure_project_config
        // uses hardcoded paths
        let config = FullProjectConfig::minimal("testproj", "/tmp/repo");
        config.save_to(&config_path).unwrap();

        assert!(config_path.exists());

        let loaded = FullProjectConfig::load_from(&config_path).unwrap();
        assert_eq!(loaded.project.name(), Some("testproj"));
        assert_eq!(loaded.project.primary_repo(), Some("/tmp/repo"));
    }

    #[test]
    fn test_ensure_project_config_rejects_coworker_names() {
        let workdir = Path::new("/tmp/fake-repo");

        // Coworker avenue names should be rejected
        let result = ensure_project_config("broadway", workdir);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);

        let result = ensure_project_config("amsterdam", workdir);
        assert!(result.is_err());

        // Overflow names should also be rejected
        let result = ensure_project_config("bleecker", workdir);
        assert!(result.is_err());
    }

    #[test]
    fn test_full_project_config_load_nonexistent() {
        let result = FullProjectConfig::load_from(Path::new("/nonexistent/config.toml"));
        assert!(result.is_none());
    }

    #[test]
    fn test_collect_used_webhook_ports_with_projects() {
        let dir = tempfile::tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        // Create some project configs with ports
        for (name, port) in &[("proj-a", 47023u16), ("proj-b", 47025), ("proj-c", 47024)] {
            let proj_dir = projects_dir.join(name);
            std::fs::create_dir_all(&proj_dir).unwrap();
            let mut config = FullProjectConfig::minimal(name, "/tmp/repo");
            config.daemon.webhook_port = Some(*port);
            config.save_to(&proj_dir.join("config.toml")).unwrap();
        }

        // Create a project with no port set
        let no_port_dir = projects_dir.join("proj-d");
        std::fs::create_dir_all(&no_port_dir).unwrap();
        FullProjectConfig::minimal("proj-d", "/tmp/repo")
            .save_to(&no_port_dir.join("config.toml"))
            .unwrap();

        // Scan the directory directly (since collect_used_webhook_ports uses hardcoded base dir)
        let mut ports = Vec::new();
        for entry in std::fs::read_dir(&projects_dir).unwrap().flatten() {
            let config_path = entry.path().join("config.toml");
            if let Some(full) = FullProjectConfig::load_from(&config_path)
                && let Some(port) = full.daemon.webhook_port
                && port > 0
            {
                ports.push(port);
            }
        }
        ports.sort();
        assert_eq!(ports, vec![47023, 47024, 47025]);
    }

    #[test]
    fn test_port_assignment_finds_gaps() {
        // Simulate port assignment logic: used ports [47023, 47024, 47026]
        // Should find 47025 as next available
        let used_ports = vec![47023u16, 47024, 47026];
        let auto_port_start = 47023u16;

        let mut port = auto_port_start;
        for used in &used_ports {
            if *used == port {
                port += 1;
            } else if *used > port {
                break;
            }
        }
        assert_eq!(port, 47025);
    }

    #[test]
    fn test_port_assignment_no_gaps() {
        // Simulate: used ports [47023, 47024, 47025]
        // Should assign 47026
        let used_ports = vec![47023u16, 47024, 47025];
        let auto_port_start = 47023u16;

        let mut port = auto_port_start;
        for used in &used_ports {
            if *used == port {
                port += 1;
            } else if *used > port {
                break;
            }
        }
        assert_eq!(port, 47026);
    }

    #[test]
    fn test_port_assignment_empty() {
        // No used ports: should assign AUTO_PORT_START (47023)
        let used_ports: Vec<u16> = vec![];
        let auto_port_start = 47023u16;

        let mut port = auto_port_start;
        for used in &used_ports {
            if *used == port {
                port += 1;
            } else if *used > port {
                break;
            }
        }
        assert_eq!(port, 47023);
    }

    #[test]
    fn test_assign_webhook_port_writes_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");

        // Create a config without webhook_port
        let config = FullProjectConfig::minimal("test-port", "/tmp/repo");
        assert!(config.daemon.webhook_port.is_none());
        config.save_to(&config_path).unwrap();

        // Simulate what assign_webhook_port does (writing port to config)
        let mut loaded = FullProjectConfig::load_from(&config_path).unwrap();
        loaded.daemon.webhook_port = Some(47023);
        loaded.save_to(&config_path).unwrap();

        // Verify it was persisted
        let reloaded = FullProjectConfig::load_from(&config_path).unwrap();
        assert_eq!(reloaded.daemon.webhook_port, Some(47023));
    }

    #[test]
    fn test_default_template_is_valid_toml() {
        // The template should parse as valid TOML (all options are commented out)
        let template = GlobalConfig::default_template();
        let config: GlobalConfig = toml::from_str(&template).unwrap();
        // All values should be defaults since everything is commented out
        assert!(config.default.bin_command.is_none());
        assert!(config.default.max_coworkers.is_none());
        assert!(config.default.personality.is_none());
        assert!(config.plugins.required.is_empty());
        assert!(config.daemon.webhook_port.is_none());
    }

    #[test]
    fn test_default_template_contains_all_sections() {
        let template = GlobalConfig::default_template();
        assert!(template.contains("[default]"));
        assert!(template.contains("[plugins]"));
        assert!(template.contains("[daemon]"));
        assert!(template.contains("max_coworkers"));
        assert!(template.contains("personality"));
        assert!(template.contains("webhook_port"));
        assert!(template.contains("chat_layout"));
        assert!(template.contains("github_user"));
    }

    #[test]
    fn test_auth_profile_roundtrip() {
        // Reproduce bug: auth_profile survives round-trip serialization
        let toml = r#"
[project]
name = "midtown"
repos = ["/path/to/repo"]
primary_repo = "/path/to/repo"
auth_profile = "ben@btucker.net"

[default]

[daemon]
webhook_port = 47024
"#;
        let config: FullProjectConfig = toml::from_str(toml).expect("Failed to parse config");

        // Verify auth_profile was parsed
        assert_eq!(
            config.project.auth_profile.as_deref(),
            Some("ben@btucker.net")
        );

        // Serialize back to TOML
        let serialized = toml::to_string(&config).expect("Failed to serialize config");

        // Parse again
        let reparsed: FullProjectConfig =
            toml::from_str(&serialized).expect("Failed to reparse config");

        // Verify auth_profile survived the round-trip
        assert_eq!(
            reparsed.project.auth_profile.as_deref(),
            Some("ben@btucker.net"),
            "auth_profile was lost during serialization round-trip"
        );
    }

    #[test]
    fn test_per_project_auth_profile_check_without_fallback() {
        // This test validates the fix for the per-project auth switch bug.
        //
        // Bug: The RPC handler used active_profile_for_project() which falls back
        // to the global profile when auth_profile is unset. This caused
        // "Already on profile" when the target matched the global default,
        // preventing the project config from being updated.
        //
        // Fix: Check config.project.auth_profile directly (no fallback).

        // Scenario 1: Config with no auth_profile set should NOT match any profile
        let config = FullProjectConfig::minimal("test-project", "/path/to/repo");
        assert_eq!(
            config.project.auth_profile.as_deref(),
            None,
            "minimal config should have no auth_profile"
        );
        // This is the check the fixed RPC handler uses — it should NOT short-circuit
        assert_ne!(
            config.project.auth_profile.as_deref(),
            Some("alice@example.com"),
            "config with no auth_profile should not match any profile"
        );

        // Scenario 2: After setting auth_profile, it should match
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        let mut config = FullProjectConfig::minimal("test-project", "/path/to/repo");
        config.project.auth_profile = Some("alice@example.com".to_string());
        config.save_to(&path).unwrap();

        let loaded = FullProjectConfig::load_from(&path).unwrap();
        assert_eq!(
            loaded.project.auth_profile.as_deref(),
            Some("alice@example.com"),
            "saved auth_profile should be loadable"
        );

        // Scenario 3: Different profile should not match
        assert_ne!(
            loaded.project.auth_profile.as_deref(),
            Some("bob@example.com"),
            "different profile should not match"
        );
    }

    #[test]
    fn test_clear_all_project_auth_profiles() {
        // Reproduce the bug: switching auth globally should clear per-project overrides.
        // Without clearing, projects with auth_profile set ignore the global switch
        // because active_profile_for_project() checks project config first.
        let dir = tempfile::tempdir().unwrap();
        let projects_dir = dir.path().join("projects");

        // Create two projects: one with auth_profile set, one without
        let proj_a_dir = projects_dir.join("proj-a");
        std::fs::create_dir_all(&proj_a_dir).unwrap();
        let mut config_a = FullProjectConfig::minimal("proj-a", "/tmp/repo-a");
        config_a.project.auth_profile = Some("old@example.com".to_string());
        config_a.save_to(&proj_a_dir.join("config.toml")).unwrap();

        let proj_b_dir = projects_dir.join("proj-b");
        std::fs::create_dir_all(&proj_b_dir).unwrap();
        let config_b = FullProjectConfig::minimal("proj-b", "/tmp/repo-b");
        config_b.save_to(&proj_b_dir.join("config.toml")).unwrap();

        // Simulate clear_all_project_auth_profiles logic on our temp dir
        // (can't use the real function since it uses the hardcoded base dir)
        for entry in std::fs::read_dir(&projects_dir).unwrap().flatten() {
            let config_path = entry.path().join("config.toml");
            if let Some(mut config) = FullProjectConfig::load_from(&config_path)
                && config.project.auth_profile.is_some()
            {
                config.project.auth_profile = None;
                config.save_to(&config_path).unwrap();
            }
        }

        // Verify: proj-a's auth_profile should be cleared
        let loaded_a = FullProjectConfig::load_from(&proj_a_dir.join("config.toml")).unwrap();
        assert_eq!(
            loaded_a.project.auth_profile, None,
            "auth_profile should be cleared for proj-a after global switch"
        );

        // Verify: proj-b should still have no auth_profile (unchanged)
        let loaded_b = FullProjectConfig::load_from(&proj_b_dir.join("config.toml")).unwrap();
        assert_eq!(
            loaded_b.project.auth_profile, None,
            "proj-b should still have no auth_profile"
        );

        // Verify: other config fields survived
        assert_eq!(loaded_a.project.name.as_deref(), Some("proj-a"));
        assert_eq!(loaded_b.project.name.as_deref(), Some("proj-b"));
    }

    #[test]
    fn test_providers_claude_config_parse() {
        let toml = r#"
[providers.claude]
auth_profile = "user@example.com"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.providers.claude.auth_profile.as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn test_providers_claude_config_default() {
        let config = GlobalConfig::default();
        assert!(config.providers.claude.auth_profile.is_none());
    }

    #[test]
    fn test_providers_claude_config_roundtrip() {
        let toml = r#"
[default]
bin_command = "midtown"

[providers.claude]
auth_profile = "ben@btucker.net"

[daemon]
webhook_port = 47023
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.providers.claude.auth_profile.as_deref(),
            Some("ben@btucker.net")
        );

        let serialized = toml::to_string_pretty(&config).unwrap();
        let reparsed: GlobalConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(
            reparsed.providers.claude.auth_profile.as_deref(),
            Some("ben@btucker.net")
        );
    }

    #[test]
    fn test_providers_config_missing_section() {
        // Config without [providers] section should parse with defaults
        let toml = r#"
[default]
bin_command = "midtown"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert!(config.providers.claude.auth_profile.is_none());
    }

    #[test]
    fn test_global_config_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = GlobalConfig::default();
        config.providers.claude.auth_profile = Some("test@example.com".to_string());
        config.default.bin_command = Some("midtown".to_string());

        let contents = toml::to_string_pretty(&config).unwrap();
        std::fs::write(&path, contents).unwrap();

        let loaded_contents = std::fs::read_to_string(&path).unwrap();
        let loaded: GlobalConfig = toml::from_str(&loaded_contents).unwrap();
        assert_eq!(
            loaded.providers.claude.auth_profile.as_deref(),
            Some("test@example.com")
        );
        assert_eq!(loaded.default.bin_command(), "midtown");
    }

    #[test]
    fn test_default_template_contains_providers_section() {
        let template = GlobalConfig::default_template();
        assert!(template.contains("[providers.claude]"));
        assert!(template.contains("auth_profile"));
    }

    #[test]
    fn test_global_config_save_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        // Create initial config with comments
        let initial_contents = r#"# User's custom comment about their setup
# This should be preserved

[default]
# Comment about bin_command
bin_command = "old-midtown"

[providers.claude]
# User's note about their auth profile
auth_profile = "user@example.com"
"#;
        std::fs::write(&path, initial_contents).unwrap();

        // Load, modify, and save using save_to
        let loaded_contents = std::fs::read_to_string(&path).unwrap();
        let mut config: GlobalConfig = toml::from_str(&loaded_contents).unwrap();
        config.default.bin_command = Some("new-midtown".to_string());
        config.save_to(&path).unwrap();

        // Read back and verify comments are preserved
        let saved_contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved_contents.contains("User's custom comment"),
            "Top-level comment should be preserved"
        );
        assert!(
            saved_contents.contains("Comment about bin_command"),
            "Field-level comment should be preserved"
        );
        assert!(
            saved_contents.contains("User's note about their auth profile"),
            "Provider section comment should be preserved"
        );

        // Verify the new value is saved
        assert!(saved_contents.contains("new-midtown"));
    }
}
