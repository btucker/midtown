//! Project-level configuration for midtown.
//!
//! Configuration is loaded from two sources:
//!
//! 1. **Global config** at `~/.midtown/config.toml`:
//!    ```toml
//!    [default]
//!    bin_command = "midtown"
//!    chat_layout = "auto"
//!    zellij_swap_layout = false
//!    zellij_chat_pane_size = 35
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
//!    [sandbox]
//!    allowed_paths = ["~/.cargo", "~/.rustup"]
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
//!    zellij_swap_layout = true
//!
//!    [daemon]
//!    webhook_port = 47023
//!
//!    [sandbox]
//!    allowed_paths = ["/opt/custom-toolchain"]
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

use ratatui_themes::ThemeName;
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
    /// Project name (e.g., "midtown"). Used for session names, paths, etc.
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

    /// Provider-specific auth profile overrides.
    /// Keys are provider names (e.g., "claude", "codex"), values are profile names.
    #[serde(default)]
    pub auth_profiles: Option<std::collections::HashMap<String, String>>,
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

    /// Project-specific execution provider overrides.
    #[serde(default)]
    pub execution: ExecutionSection,

    /// Channel configuration (seed channels)
    #[serde(default)]
    pub channels: ChannelsSection,

    /// Sandbox configuration (additional writable paths)
    #[serde(default)]
    pub sandbox: SandboxSection,

    /// Channel lead configuration (per-channel model selection)
    #[serde(default)]
    pub channel_leads: ChannelLeadsConfig,
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
                auth_profiles: None,
            },
            default: ProjectConfig {
                max_coworkers: Some(8),
                ..ProjectConfig::default()
            },
            daemon: DaemonSection::default(),
            execution: ExecutionSection::default(),
            channels: ChannelsSection::default(),
            sandbox: SandboxSection::default(),
            channel_leads: ChannelLeadsConfig::default(),
        }
    }

    /// Write this config to the given path.
    ///
    /// Loads the existing file first (to preserve comments/structure from manual edits),
    /// then overlays the current struct values and writes back.
    /// Creates parent directories if they don't exist.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
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
            // merge_tables() now handles generic None-removal for all Option<T> fields
            merge_tables(doc.as_table_mut(), &new_table);

            doc.to_string()
        } else {
            // No existing file, just serialize normally
            toml::to_string_pretty(self).map_err(std::io::Error::other)?
        };

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

    /// Whether to swap Zellij pane order (Lead left, chat right) (default: false)
    #[serde(default)]
    pub zellij_swap_layout: Option<bool>,

    /// Zellij chat pane width percentage (10-90, default: 35)
    #[serde(default)]
    pub zellij_chat_pane_size: Option<u8>,

    /// Maximum number of concurrent coworkers (default: 8)
    #[serde(default)]
    pub max_coworkers: Option<usize>,

    /// User display name shown in chat and @mentions (default: "user")
    #[serde(default)]
    pub user_display_name: Option<String>,

    /// TUI color theme (e.g. "dracula", "nord", "catppuccin-mocha", "gruvbox-dark",
    /// "tokyo-night", "one-dark-pro"). Defaults to "catppuccin-mocha".
    /// Full list: dracula, nord, catppuccin-mocha, catppuccin-latte, gruvbox-dark,
    /// gruvbox-light, tokyo-night, one-dark-pro, solarized-dark, solarized-light,
    /// monokai-pro, rose-pine, kanagawa, everforest, cyberpunk.
    #[serde(default)]
    pub theme: Option<ThemeName>,
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
            zellij_swap_layout: other.zellij_swap_layout.or(self.zellij_swap_layout),
            zellij_chat_pane_size: other.zellij_chat_pane_size.or(self.zellij_chat_pane_size),
            max_coworkers: other.max_coworkers.or(self.max_coworkers),
            user_display_name: other
                .user_display_name
                .clone()
                .or_else(|| self.user_display_name.clone()),
            theme: other.theme.or(self.theme),
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

    /// Get whether Zellij layout should be swapped with fallback to default.
    pub fn zellij_swap_layout(&self) -> bool {
        self.zellij_swap_layout.unwrap_or(false)
    }

    /// Get Zellij chat pane width percentage with fallback to default.
    ///
    /// Values outside 10-90 are treated as invalid and replaced with 35.
    pub fn zellij_chat_pane_size(&self) -> u8 {
        self.zellij_chat_pane_size
            .filter(|size| (10..=90).contains(size))
            .unwrap_or(35)
    }

    /// Get max_coworkers, or None if not configured (falls back to daemon default).
    pub fn max_coworkers(&self) -> Option<usize> {
        self.max_coworkers
    }

    /// Get user display name, or None if not configured (falls back to "user").
    pub fn user_display_name(&self) -> Option<&str> {
        self.user_display_name.as_deref()
    }

    /// Get the TUI theme name, defaulting to Catppuccin Mocha.
    pub fn theme(&self) -> ThemeName {
        self.theme.unwrap_or(ThemeName::CatppuccinMocha)
    }
}

/// Configuration for Claude Code plugins.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PluginsConfig {
    /// List of required plugin names (e.g., "superpowers@claude-plugins-official")
    #[serde(default)]
    pub required: Vec<String>,
}

/// Channels configuration section.
///
/// Pre-populated seed channels that should exist from daemon startup.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChannelsSection {
    /// Seed channels to create at daemon startup if they don't exist.
    /// Example: ["tui", "web-interface", "daemon", "auth", "docs"]
    #[serde(default)]
    pub seed: Vec<String>,
}

/// Channel lead configuration section.
///
/// Configures per-channel model selection for channel leads.
///
/// Example:
/// ```toml
/// [channel_leads]
/// default_model = "sonnet"
///
/// [channel_leads.overrides]
/// "daemon-architecture" = "opus"
/// "web-interface" = "sonnet"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ChannelLeadsConfig {
    /// Default model for all channel leads. When not set, falls back to per-channel
    /// defaults ("haiku" for "ops", "sonnet" for all others).
    #[serde(default)]
    pub default_model: Option<String>,

    /// Per-channel model overrides. Keys are channel names, values are model names.
    #[serde(default)]
    pub overrides: std::collections::HashMap<String, String>,
}

impl ChannelLeadsConfig {
    /// Get the model to use for a given channel.
    ///
    /// Priority: per-channel override → default_model → ops-specific default ("haiku") → "sonnet".
    pub fn model_for_channel(&self, channel_name: &str) -> String {
        self.overrides
            .get(channel_name)
            .cloned()
            .or_else(|| self.default_model.clone())
            .unwrap_or_else(|| {
                if channel_name == "ops" {
                    "haiku".to_string()
                } else {
                    "sonnet".to_string()
                }
            })
    }
}

/// Sandbox configuration section.
///
/// Controls additional writable paths for coworker sandboxes.
/// Project-level paths extend (not replace) global paths.
///
/// Example:
/// ```toml
/// [sandbox]
/// allowed_paths = ["~/.cargo", "~/.rustup"]
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct SandboxSection {
    /// Additional writable directories for coworker sandboxes.
    /// Paths are expanded (~ → home dir) and canonicalized.
    /// Project-level paths are merged with global paths (deduplicated).
    #[serde(default)]
    pub allowed_paths: Vec<String>,
}

impl SandboxSection {
    /// Merge project-level sandbox config with global config.
    ///
    /// Project paths extend (not replace) global paths. Duplicates are removed.
    pub fn merge(&self, other: &SandboxSection) -> SandboxSection {
        let mut merged = self.allowed_paths.clone();
        merged.extend(other.allowed_paths.clone());
        merged.sort();
        merged.dedup();
        SandboxSection {
            allowed_paths: merged,
        }
    }
}

/// Daemon configuration section.
///
/// These settings can be overridden by environment variables:
/// - `MIDTOWN_WEBHOOK_PORT` (set to 0 to disable)
/// - `MIDTOWN_WEBHOOK_SECRET`
/// - `MIDTOWN_WEBHOOK_RESTART_INTERVAL`
/// - `MIDTOWN_PR_POLL_INTERVAL`
/// - `MIDTOWN_CHAT_MONITOR` (set to 0 to disable)
/// - `MIDTOWN_LEAD_SESSION_REFRESH_INTERVAL` (set to 0 to disable)
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

    /// Hours to retain completed worktrees before cleanup (default: 24).
    /// Set to 0 to disable time-based cleanup (only PR-merge cleanup will run).
    #[serde(default)]
    pub worktree_cleanup_retention_hours: Option<u64>,

    /// Interval in seconds for periodic lead session refresh to prevent context drift (default: 5400 = 90 min).
    /// Set to 0 to disable periodic refresh.
    #[serde(default)]
    pub lead_session_refresh_interval_secs: Option<u64>,
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
            worktree_cleanup_retention_hours: other
                .worktree_cleanup_retention_hours
                .or(self.worktree_cleanup_retention_hours),
            lead_session_refresh_interval_secs: other
                .lead_session_refresh_interval_secs
                .or(self.lead_session_refresh_interval_secs),
        }
    }
}

/// Role-specific execution provider settings.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ExecutionSection {
    /// Default provider used for all lead roles (project lead and channel leads).
    #[serde(default)]
    pub lead_provider: Option<crate::auth::AuthProvider>,
    /// Provider override used only for the main project lead session.
    #[serde(default)]
    pub project_lead_provider: Option<crate::auth::AuthProvider>,
    /// Provider used for general developer coworkers.
    #[serde(default)]
    pub coworker_provider: Option<crate::auth::AuthProvider>,
    /// Provider used for reviewer sessions.
    #[serde(default)]
    pub reviewer_provider: Option<crate::auth::AuthProvider>,
    /// Review execution mode:
    /// - local: spawn local reviewer coworkers
    /// - github_app: rely on GitHub App/formal reviews only (no local reviewer spawns)
    /// - both: allow both local reviewers and GitHub App/formal reviews
    #[serde(default)]
    pub review_mode: Option<ReviewMode>,
    /// Provider used for channel lead sessions.
    #[serde(default)]
    pub channel_lead_provider: Option<crate::auth::AuthProvider>,
    /// Provider used for specialized workers (headless.execute default).
    #[serde(default)]
    pub specialized_provider: Option<crate::auth::AuthProvider>,
    /// Provider override for ad-hoc `headless.execute` RPC.
    #[serde(default)]
    pub headless_execute_provider: Option<crate::auth::AuthProvider>,
    /// Pool of auth profile emails for coworker spawning.
    /// Takes precedence over `coworker_provider` when set.
    /// Example: ["alice@example.com", "bob@example.com"]
    #[serde(default)]
    pub coworker_profiles: Option<Vec<String>>,
    /// Pool of auth profile emails for reviewer spawning.
    /// Takes precedence over `reviewer_provider` when set.
    #[serde(default)]
    pub reviewer_profiles: Option<Vec<String>>,
    /// Pool of auth profile emails for channel lead spawning.
    /// Takes precedence over `channel_lead_provider` when set.
    #[serde(default)]
    pub channel_lead_profiles: Option<Vec<String>>,
}

impl ExecutionSection {
    /// Merge another execution section into this one, with `other` taking precedence.
    pub fn merge(&self, other: &ExecutionSection) -> ExecutionSection {
        ExecutionSection {
            lead_provider: other.lead_provider.or(self.lead_provider),
            project_lead_provider: other.project_lead_provider.or(self.project_lead_provider),
            coworker_provider: other.coworker_provider.or(self.coworker_provider),
            reviewer_provider: other.reviewer_provider.or(self.reviewer_provider),
            review_mode: other.review_mode.or(self.review_mode),
            channel_lead_provider: other.channel_lead_provider.or(self.channel_lead_provider),
            specialized_provider: other.specialized_provider.or(self.specialized_provider),
            headless_execute_provider: other
                .headless_execute_provider
                .or(self.headless_execute_provider),
            coworker_profiles: other
                .coworker_profiles
                .clone()
                .or_else(|| self.coworker_profiles.clone()),
            reviewer_profiles: other
                .reviewer_profiles
                .clone()
                .or_else(|| self.reviewer_profiles.clone()),
            channel_lead_profiles: other
                .channel_lead_profiles
                .clone()
                .or_else(|| self.channel_lead_profiles.clone()),
        }
    }
}

/// How PR reviews are performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    /// Spawn local reviewer coworkers.
    #[default]
    Local,
    /// Use GitHub App/formal reviews only.
    GithubApp,
    /// Allow both local reviewer coworkers and GitHub App/formal reviews.
    Both,
}

/// Runtime role used to resolve the effective execution provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionRole {
    Lead,
    Coworker,
    Reviewer,
    ChannelLead,
    Specialized,
    HeadlessExecute,
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
///
/// This function performs two passes:
/// 1. **Overlay**: Add or update keys from new_table into target
/// 2. **Removal**: Remove keys from target that don't exist in new_table
///
/// The removal pass is critical for Option<T> fields. When an Option is set to None,
/// serde omits it from serialization. Without removal, the old value persists in the file.
///
/// Comment-only sections (tables with no key-value pairs) are preserved during removal.
///
/// **Note**: This function assumes all struct fields are serialized (no `skip_serializing_if`).
/// If a field is omitted from serialization, Phase 2 will remove it from the file.
fn merge_tables(target: &mut Table, new_table: &Table) {
    // Phase 1: Overlay new values onto target (add or update)
    for (key, new_value) in new_table.iter() {
        match (target.get_mut(key), new_value) {
            // Both are tables - recurse
            (Some(Item::Table(target_table)), Item::Table(new_table)) => {
                merge_tables(target_table, new_table);
            }
            // Target exists but isn't a table, or new value isn't a table - replace
            (Some(existing), new_item) => {
                *existing = new_item.clone();
            }
            // Key doesn't exist in target - add it
            (None, new_item) => {
                target.insert(key, new_item.clone());
            }
        }
    }

    // Phase 2: Remove keys from target that don't exist in new_table
    // This handles Option<T> fields set to None (serde omits them, so we must remove the old value)
    let keys_to_remove: Vec<String> = target
        .iter()
        .filter_map(|(key, item)| {
            // Keep the key if it exists in new_table
            if new_table.contains_key(key) {
                return None;
            }

            // Keep comment-only tables (tables with no key-value pairs, just comments)
            if let Item::Table(table) = item
                && table.is_empty()
            {
                return None;
            }

            // Remove this key
            Some(key.to_string())
        })
        .collect();

    for key in keys_to_remove {
        target.remove(&key);
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

    /// Default execution provider settings.
    #[serde(default)]
    pub execution: ExecutionSection,

    /// Sandbox configuration (additional writable paths)
    #[serde(default)]
    pub sandbox: SandboxSection,

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

# Swap Zellij pane order (Lead left, chat right)
# zellij_swap_layout = false

# Zellij chat pane width percentage (10-90)
# zellij_chat_pane_size = 35

# Maximum concurrent coworkers
# max_coworkers = 8

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

[execution]
# Default provider for all lead sessions (project lead + channel leads): "claude", "codex", or "zai"
# lead_provider = "claude"
# Optional override for the main project lead only
# project_lead_provider = "claude"
# Default provider for developer coworkers
# coworker_provider = "claude"
# Default provider for reviewers
# reviewer_provider = "claude"
# Review execution mode: "local", "github_app", or "both"
# review_mode = "local"
# Optional override for channel leads (defaults to lead_provider when unset)
# channel_lead_provider = "claude"
# Default provider for specialized workers (headless.execute)
# specialized_provider = "claude"
# Optional override provider for headless.execute RPC
# headless_execute_provider = "claude"

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
            || full.default.zellij_swap_layout.is_some()
            || full.default.zellij_chat_pane_size.is_some()
            || full.default.max_coworkers.is_some()
            || full.project.name.is_some()
            || full.project.auth_profile.is_some()
            || full
                .project
                .auth_profiles
                .as_ref()
                .is_some_and(|m| !m.is_empty())
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

/// Get the project-specific execution provider configuration, merged with global.
///
/// Priority: project execution section > global execution section.
pub fn get_project_execution_config(project_name: &str) -> ExecutionSection {
    let global = GlobalConfig::load();
    let project = FullProjectConfig::load(project_name);

    match project {
        Some(proj) => global.execution.merge(&proj.execution),
        None => global.execution,
    }
}

/// Get the effective review mode for a project.
///
/// Defaults to `ReviewMode::Local` when not configured.
pub fn get_review_mode_for_repo(project_name: &str) -> ReviewMode {
    get_project_execution_config(project_name)
        .review_mode
        .unwrap_or(ReviewMode::Local)
}

/// Resolve the effective execution provider for a role in a project.
///
/// If no role-specific provider is configured, defaults to Claude.
pub fn get_execution_provider_for_role(
    project_name: &str,
    role: ExecutionRole,
) -> crate::auth::AuthProvider {
    let execution = get_project_execution_config(project_name);
    resolve_execution_provider(&execution, role)
}

fn resolve_execution_provider(
    execution: &ExecutionSection,
    role: ExecutionRole,
) -> crate::auth::AuthProvider {
    let direct = match role {
        ExecutionRole::Lead => execution.project_lead_provider.or(execution.lead_provider),
        ExecutionRole::Coworker => execution.coworker_provider,
        ExecutionRole::Reviewer => execution.reviewer_provider,
        ExecutionRole::ChannelLead => execution.channel_lead_provider.or(execution.lead_provider),
        ExecutionRole::Specialized => execution.specialized_provider,
        ExecutionRole::HeadlessExecute => execution.headless_execute_provider,
    };
    let configured = match role {
        ExecutionRole::HeadlessExecute => direct.or(execution.specialized_provider),
        _ => direct,
    };
    configured.unwrap_or(crate::auth::AuthProvider::Claude)
}

/// Get the project-specific sandbox configuration, merged with global.
///
/// Project-level paths extend (not replace) global paths.
pub fn get_project_sandbox_config(project_name: &str) -> SandboxSection {
    let global = GlobalConfig::load();
    let project = FullProjectConfig::load(project_name);

    match project {
        Some(proj) => global.sandbox.merge(&proj.sandbox),
        None => global.sandbox,
    }
}

/// Get the channel lead configuration for a project.
///
/// Returns the `[channel_leads]` section from the project config, or a default
/// (empty) config if not set. Channel lead config is project-specific only —
/// there is no global default for per-channel model overrides.
pub fn get_channel_leads_config(project_name: &str) -> ChannelLeadsConfig {
    FullProjectConfig::load(project_name)
        .map(|full| full.channel_leads)
        .unwrap_or_default()
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

/// Clear per-project auth overrides for a specific provider across all projects.
///
/// Returns the number of project configs updated.
pub fn clear_all_project_auth_profiles_for(provider: crate::auth::AuthProvider) -> usize {
    let projects_dir = crate::paths::midtown_base_dir().join("projects");
    clear_project_auth_overrides_in_dir(&projects_dir, provider)
}

fn clear_project_auth_overrides_in_dir(
    projects_dir: &Path,
    provider: crate::auth::AuthProvider,
) -> usize {
    let mut updated = 0usize;

    let entries = match std::fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(_) => return 0,
    };

    for entry in entries.flatten() {
        let config_path = entry.path().join("config.toml");
        if let Some(mut config) = FullProjectConfig::load_from(&config_path) {
            let mut changed = false;

            if provider == crate::auth::AuthProvider::Claude
                && config.project.auth_profile.is_some()
            {
                config.project.auth_profile = None;
                changed = true;
            }

            if let Some(map) = config.project.auth_profiles.as_mut()
                && map.remove(provider.as_str()).is_some()
            {
                if map.is_empty() {
                    config.project.auth_profiles = None;
                }
                changed = true;
            }

            if changed && config.save_to(&config_path).is_ok() {
                updated += 1;
            }
        }
    }

    updated
}

/// Clear per-project auth overrides for all providers.
///
/// Returns the number of project configs updated.
pub fn clear_all_project_auth_profiles() -> usize {
    let projects_dir = crate::paths::midtown_base_dir().join("projects");
    let mut updated = 0usize;
    for provider in crate::auth::AuthProvider::all() {
        updated += clear_project_auth_overrides_in_dir(&projects_dir, *provider);
    }
    updated
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

/// Get the TUI theme name for the current project. Defaults to Catppuccin Mocha.
pub fn get_theme() -> ThemeName {
    let project_name = get_project_name().unwrap_or_default();
    let config = if project_name.is_empty() {
        GlobalConfig::load().default
    } else {
        get_project_config(&project_name)
    };
    config.theme()
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
            zellij_swap_layout: Some(false),
            zellij_chat_pane_size: Some(35),
            max_coworkers: Some(8),
            user_display_name: None,
            theme: None,
        };

        let project = ProjectConfig {
            bin_command: Some("custom".to_string()),
            chat_layout: None, // Not overridden
            chat_min_width: Some(200),
            zellij_swap_layout: None,
            zellij_chat_pane_size: None,
            max_coworkers: None, // Not overridden
            user_display_name: None,
            theme: None,
        };

        let merged = global.merge(&project);

        assert_eq!(merged.bin_command(), "custom"); // Overridden
        assert_eq!(merged.chat_layout(), ChatLayout::Auto); // From global
        assert_eq!(merged.chat_min_width(), 200); // Overridden
        assert!(!merged.zellij_swap_layout()); // From global
        assert_eq!(merged.zellij_chat_pane_size(), 35); // From global
        assert_eq!(merged.max_coworkers(), Some(8)); // From global
    }

    #[test]
    fn test_merge_configs_max_coworkers_override() {
        let global = ProjectConfig {
            bin_command: Some("midtown".to_string()),
            chat_layout: None,
            chat_min_width: None,
            zellij_swap_layout: None,
            zellij_chat_pane_size: None,
            max_coworkers: Some(8),
            user_display_name: None,
            theme: None,
        };

        let project = ProjectConfig {
            bin_command: None,
            chat_layout: None,
            chat_min_width: None,
            zellij_swap_layout: None,
            zellij_chat_pane_size: None,
            max_coworkers: Some(4),
            user_display_name: None,
            theme: None,
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
    fn test_zellij_layout_defaults() {
        let config = ProjectConfig::default();
        assert!(!config.zellij_swap_layout());
        assert_eq!(config.zellij_chat_pane_size(), 35);
    }

    #[test]
    fn test_zellij_layout_parse() {
        let toml = r#"
zellij_swap_layout = true
zellij_chat_pane_size = 42
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();
        assert!(config.zellij_swap_layout());
        assert_eq!(config.zellij_chat_pane_size(), 42);
    }

    #[test]
    fn test_zellij_chat_pane_size_invalid_falls_back() {
        let config = ProjectConfig {
            zellij_chat_pane_size: Some(99),
            ..ProjectConfig::default()
        };
        assert_eq!(config.zellij_chat_pane_size(), 35);
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
            worktree_cleanup_retention_hours: None,
            lead_session_refresh_interval_secs: None,
        };

        let project = DaemonSection {
            webhook_port: Some(47023),
            webhook_secret: None,
            webhook_restart_interval_secs: None,
            pr_poll_interval_secs: Some(120),
            chat_monitor_enabled: None,
            github_user: None,
            worktree_cleanup_retention_hours: None,
            lead_session_refresh_interval_secs: None,
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
            worktree_cleanup_retention_hours: None,
            lead_session_refresh_interval_secs: None,
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
        assert!(config.plugins.required.is_empty());
        assert!(config.daemon.webhook_port.is_none());
        assert!(config.execution.lead_provider.is_none());
        assert!(config.execution.coworker_provider.is_none());
    }

    #[test]
    fn test_default_template_contains_all_sections() {
        let template = GlobalConfig::default_template();
        assert!(template.contains("[default]"));
        assert!(template.contains("[plugins]"));
        assert!(template.contains("[daemon]"));
        assert!(template.contains("[execution]"));
        assert!(template.contains("max_coworkers"));
        assert!(template.contains("webhook_port"));
        assert!(template.contains("chat_layout"));
        assert!(template.contains("zellij_swap_layout"));
        assert!(template.contains("zellij_chat_pane_size"));
        assert!(template.contains("github_user"));
        assert!(template.contains("lead_provider"));
        assert!(template.contains("project_lead_provider"));
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
        config_a.project.auth_profiles = Some(std::collections::HashMap::from([
            ("claude".to_string(), "old@example.com".to_string()),
            ("codex".to_string(), "codex@example.com".to_string()),
        ]));
        config_a.save_to(&proj_a_dir.join("config.toml")).unwrap();

        let proj_b_dir = projects_dir.join("proj-b");
        std::fs::create_dir_all(&proj_b_dir).unwrap();
        let config_b = FullProjectConfig::minimal("proj-b", "/tmp/repo-b");
        config_b.save_to(&proj_b_dir.join("config.toml")).unwrap();

        let updated =
            clear_project_auth_overrides_in_dir(&projects_dir, crate::auth::AuthProvider::Claude);
        assert_eq!(updated, 1);

        // Verify: proj-a's auth_profile should be cleared
        let loaded_a = FullProjectConfig::load_from(&proj_a_dir.join("config.toml")).unwrap();
        assert_eq!(
            loaded_a.project.auth_profile, None,
            "auth_profile should be cleared for proj-a after global switch"
        );
        assert_eq!(
            loaded_a
                .project
                .auth_profiles
                .as_ref()
                .and_then(|m| m.get("claude"))
                .cloned(),
            None,
            "provider-specific claude override should be cleared"
        );
        assert_eq!(
            loaded_a
                .project
                .auth_profiles
                .as_ref()
                .and_then(|m| m.get("codex"))
                .map(String::as_str),
            Some("codex@example.com"),
            "other provider override should remain"
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
    fn test_full_project_config_save_preserves_comments() {
        // Bug: FullProjectConfig::save_to() uses toml::to_string_pretty() which destroys comments.
        // This is the same bug fixed for GlobalConfig in PR #933.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        // Write initial config with user comments
        let initial_toml = r#"# Project configuration for my awesome project
# This comment explains the max_coworkers setting
[project]
name = "testproj"
repos = ["/tmp/testproj"]
primary_repo = "/tmp/testproj"

[default]
# Set to 4 for my machine's capacity
max_coworkers = 4

[daemon]
# Custom webhook port
webhook_port = 47024
"#;
        std::fs::write(&path, initial_toml).unwrap();

        // Load, modify, and save
        let mut config = FullProjectConfig::load_from(&path).unwrap();
        config.default.user_display_name = Some("Alice".to_string()); // Add a new field
        config.save_to(&path).unwrap();

        // Verify comments are preserved
        let saved_contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            saved_contents.contains("# Project configuration for my awesome project"),
            "Top-level comment should be preserved"
        );
        assert!(
            saved_contents.contains("# This comment explains the max_coworkers setting"),
            "Section comment should be preserved"
        );
        assert!(
            saved_contents.contains("# Set to 4 for my machine's capacity"),
            "Inline comment should be preserved"
        );
        assert!(
            saved_contents.contains("# Custom webhook port"),
            "Daemon section comment should be preserved"
        );

        // Verify the new field was added
        assert!(
            saved_contents.contains("user_display_name"),
            "New field should be added"
        );

        // Verify existing values are preserved
        let reloaded = FullProjectConfig::load_from(&path).unwrap();
        assert_eq!(reloaded.project.name(), Some("testproj"));
        assert_eq!(reloaded.default.max_coworkers(), Some(4));
        assert_eq!(reloaded.default.user_display_name(), Some("Alice"));
        assert_eq!(reloaded.daemon.webhook_port, Some(47024));
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
    fn test_execution_section_merge() {
        let global = ExecutionSection {
            lead_provider: Some(crate::auth::AuthProvider::Claude),
            project_lead_provider: None,
            coworker_provider: Some(crate::auth::AuthProvider::Claude),
            reviewer_provider: None,
            review_mode: Some(ReviewMode::Local),
            channel_lead_provider: None,
            specialized_provider: None,
            headless_execute_provider: None,
            ..ExecutionSection::default()
        };
        let project = ExecutionSection {
            lead_provider: Some(crate::auth::AuthProvider::Codex),
            project_lead_provider: Some(crate::auth::AuthProvider::Zai),
            coworker_provider: None,
            reviewer_provider: Some(crate::auth::AuthProvider::Codex),
            review_mode: Some(ReviewMode::GithubApp),
            channel_lead_provider: None,
            specialized_provider: None,
            headless_execute_provider: None,
            ..ExecutionSection::default()
        };

        let merged = global.merge(&project);
        assert_eq!(merged.lead_provider, Some(crate::auth::AuthProvider::Codex));
        assert_eq!(
            merged.project_lead_provider,
            Some(crate::auth::AuthProvider::Zai)
        );
        assert_eq!(
            merged.coworker_provider,
            Some(crate::auth::AuthProvider::Claude)
        );
        assert_eq!(
            merged.reviewer_provider,
            Some(crate::auth::AuthProvider::Codex)
        );
        assert_eq!(merged.review_mode, Some(ReviewMode::GithubApp));
    }

    #[test]
    fn test_parse_execution_section() {
        let toml = r#"
[execution]
lead_provider = "codex"
project_lead_provider = "zai"
coworker_provider = "zai"
reviewer_provider = "claude"
review_mode = "both"
specialized_provider = "codex"
"#;
        let config: GlobalConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.execution.lead_provider,
            Some(crate::auth::AuthProvider::Codex)
        );
        assert_eq!(
            config.execution.project_lead_provider,
            Some(crate::auth::AuthProvider::Zai)
        );
        assert_eq!(
            config.execution.coworker_provider,
            Some(crate::auth::AuthProvider::Zai)
        );
        assert_eq!(
            config.execution.reviewer_provider,
            Some(crate::auth::AuthProvider::Claude)
        );
        assert_eq!(config.execution.review_mode, Some(ReviewMode::Both));
        assert_eq!(
            config.execution.specialized_provider,
            Some(crate::auth::AuthProvider::Codex)
        );
    }

    #[test]
    fn test_headless_execute_provider_precedence() {
        let specialized_only = ExecutionSection {
            specialized_provider: Some(crate::auth::AuthProvider::Codex),
            ..ExecutionSection::default()
        };
        assert_eq!(
            resolve_execution_provider(&specialized_only, ExecutionRole::HeadlessExecute),
            crate::auth::AuthProvider::Codex
        );

        let headless_override = ExecutionSection {
            specialized_provider: Some(crate::auth::AuthProvider::Codex),
            headless_execute_provider: Some(crate::auth::AuthProvider::Zai),
            ..ExecutionSection::default()
        };
        assert_eq!(
            resolve_execution_provider(&headless_override, ExecutionRole::HeadlessExecute),
            crate::auth::AuthProvider::Zai
        );

        let default_only = ExecutionSection::default();
        assert_eq!(
            resolve_execution_provider(&default_only, ExecutionRole::HeadlessExecute),
            crate::auth::AuthProvider::Claude
        );
    }

    #[test]
    fn test_lead_provider_is_default_for_all_leads() {
        let execution = ExecutionSection {
            lead_provider: Some(crate::auth::AuthProvider::Codex),
            ..ExecutionSection::default()
        };

        assert_eq!(
            resolve_execution_provider(&execution, ExecutionRole::Lead),
            crate::auth::AuthProvider::Codex
        );
        assert_eq!(
            resolve_execution_provider(&execution, ExecutionRole::ChannelLead),
            crate::auth::AuthProvider::Codex
        );
    }

    #[test]
    fn test_project_lead_provider_only_overrides_project_lead() {
        let execution = ExecutionSection {
            lead_provider: Some(crate::auth::AuthProvider::Claude),
            project_lead_provider: Some(crate::auth::AuthProvider::Zai),
            ..ExecutionSection::default()
        };

        assert_eq!(
            resolve_execution_provider(&execution, ExecutionRole::Lead),
            crate::auth::AuthProvider::Zai
        );
        assert_eq!(
            resolve_execution_provider(&execution, ExecutionRole::ChannelLead),
            crate::auth::AuthProvider::Claude
        );
    }

    #[test]
    fn test_channel_lead_provider_override_still_wins_for_channel_leads() {
        let execution = ExecutionSection {
            lead_provider: Some(crate::auth::AuthProvider::Claude),
            channel_lead_provider: Some(crate::auth::AuthProvider::Zai),
            ..ExecutionSection::default()
        };

        assert_eq!(
            resolve_execution_provider(&execution, ExecutionRole::Lead),
            crate::auth::AuthProvider::Claude
        );
        assert_eq!(
            resolve_execution_provider(&execution, ExecutionRole::ChannelLead),
            crate::auth::AuthProvider::Zai
        );
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

    #[test]
    fn test_merge_tables_removes_none_fields_full_project_config() {
        // Bug: merge_tables() only handles auth_profile explicitly, but all Option<T> fields
        // have the same latent defect. When a field is set to None, the serializer omits it,
        // but merge_tables() doesn't remove the old value from the file.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        // Write initial config with all Option fields set
        let initial_toml = r#"[project]
name = "testproj"
repos = ["/tmp/testproj"]
primary_repo = "/tmp/testproj"
auth_profile = "old@example.com"

[default]
bin_command = "cargo run --"
chat_layout = "split"
max_coworkers = 8
user_display_name = "OldUser"

[daemon]
webhook_port = 9000
webhook_secret = "old-secret"
"#;
        std::fs::write(&path, initial_toml).unwrap();

        // Load, set all Option fields to None, and save
        let mut config = FullProjectConfig::load_from(&path).unwrap();
        config.project.auth_profile = None;
        config.default.bin_command = None;
        config.default.chat_layout = None;
        config.default.max_coworkers = None;
        config.default.user_display_name = None;
        config.daemon.webhook_port = None;
        config.daemon.webhook_secret = None;
        config.save_to(&path).unwrap();

        // Verify: stale values should be removed from the file
        let saved_contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            !saved_contents.contains("auth_profile"),
            "auth_profile should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("bin_command"),
            "bin_command should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("chat_layout"),
            "chat_layout should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("max_coworkers"),
            "max_coworkers should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("user_display_name"),
            "user_display_name should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("webhook_port"),
            "webhook_port should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("webhook_secret"),
            "webhook_secret should be removed when set to None"
        );

        // Verify: reloading the config confirms None values (not stale values)
        let reloaded = FullProjectConfig::load_from(&path).unwrap();
        assert_eq!(reloaded.project.auth_profile, None);
        assert_eq!(reloaded.default.bin_command, None);
        assert_eq!(reloaded.default.chat_layout, None);
        assert_eq!(reloaded.default.max_coworkers, None);
        assert_eq!(reloaded.default.user_display_name, None);
        assert_eq!(reloaded.daemon.webhook_port, None);
        assert_eq!(reloaded.daemon.webhook_secret, None);
    }

    #[test]
    fn test_merge_tables_removes_none_fields_global_config() {
        // Same bug applies to GlobalConfig::save()
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        // Write initial config with all Option fields set
        let initial_toml = r#"[default]
bin_command = "midtown"
chat_layout = "split"
max_coworkers = 8
user_display_name = "OldUser"

[daemon]
webhook_port = 9000
webhook_secret = "old-secret"
github_user = "old-user"

[providers.claude]
auth_profile = "old@example.com"
"#;
        std::fs::write(&path, initial_toml).unwrap();

        // Load, set all Option fields to None, and save
        let mut config: GlobalConfig =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        config.default.bin_command = None;
        config.default.chat_layout = None;
        config.default.max_coworkers = None;
        config.default.user_display_name = None;
        config.daemon.webhook_port = None;
        config.daemon.webhook_secret = None;
        config.daemon.github_user = None;
        config.providers.claude.auth_profile = None;
        config.save_to(&path).unwrap();

        // Verify: stale values should be removed from the file
        let saved_contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            !saved_contents.contains("bin_command"),
            "bin_command should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("chat_layout"),
            "chat_layout should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("max_coworkers"),
            "max_coworkers should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("user_display_name"),
            "user_display_name should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("webhook_port"),
            "webhook_port should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("webhook_secret"),
            "webhook_secret should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("github_user"),
            "github_user should be removed when set to None"
        );
        assert!(
            !saved_contents.contains("auth_profile"),
            "auth_profile should be removed when set to None"
        );

        // Verify: reloading the config confirms None values (not stale values)
        let reloaded: GlobalConfig =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(reloaded.default.bin_command, None);
        assert_eq!(reloaded.default.chat_layout, None);
        assert_eq!(reloaded.default.max_coworkers, None);
        assert_eq!(reloaded.default.user_display_name, None);
        assert_eq!(reloaded.daemon.webhook_port, None);
        assert_eq!(reloaded.daemon.webhook_secret, None);
        assert_eq!(reloaded.daemon.github_user, None);
        assert_eq!(reloaded.providers.claude.auth_profile, None);
    }

    #[test]
    fn test_global_auth_switch_clears_project_overrides() {
        // Integration test: Reproduce the bug where per-project auth_profile
        // overrides persist after a global switch, causing active_profile_for_project()
        // to return the stale project override instead of the new global profile.
        //
        // This test simulates the handle_auth_switch RPC flow:
        // 1. Set a per-project auth_profile override
        // 2. Perform a global auth switch (set_current_profile_for + clear_all_project_auth_profiles)
        // 3. Verify the override is cleared and the global profile is active
        let dir = tempfile::tempdir().unwrap();
        let projects_dir = dir.path().join("projects");
        let proj_dir = projects_dir.join("test-repo");
        std::fs::create_dir_all(&proj_dir).unwrap();

        // Create a project with a per-project auth_profile override
        let mut project_config = FullProjectConfig::minimal("test-repo", "/tmp/test-repo");
        project_config.project.auth_profile = Some("project@example.com".to_string());
        project_config.project.auth_profiles = Some(std::collections::HashMap::from([
            ("claude".to_string(), "project@example.com".to_string()),
            ("codex".to_string(), "codex@example.com".to_string()),
        ]));
        project_config
            .save_to(&proj_dir.join("config.toml"))
            .unwrap();

        // Verify the override is set
        let loaded_before = FullProjectConfig::load_from(&proj_dir.join("config.toml")).unwrap();
        assert_eq!(
            loaded_before.project.auth_profile.as_deref(),
            Some("project@example.com"),
            "Per-project override should be set before global switch"
        );

        // Simulate a global auth switch for Claude: clear Claude project overrides.
        clear_project_auth_overrides_in_dir(&projects_dir, crate::auth::AuthProvider::Claude);

        // Verify: the project override should now be cleared
        let loaded_after = FullProjectConfig::load_from(&proj_dir.join("config.toml")).unwrap();
        assert_eq!(
            loaded_after.project.auth_profile, None,
            "Per-project override should be cleared after global switch"
        );
        assert_eq!(
            loaded_after
                .project
                .auth_profiles
                .as_ref()
                .and_then(|m| m.get("claude")),
            None,
            "provider-specific Claude override should be cleared"
        );
        assert_eq!(
            loaded_after
                .project
                .auth_profiles
                .as_ref()
                .and_then(|m| m.get("codex"))
                .map(String::as_str),
            Some("codex@example.com"),
            "provider-specific non-Claude override should remain"
        );

        // Verify: other config fields survived
        assert_eq!(loaded_after.project.name.as_deref(), Some("test-repo"));
    }

    #[test]
    fn test_channels_section_parse() {
        let toml = r#"
[channels]
seed = ["tui", "web-interface", "daemon", "docs"]
"#;
        let config: FullProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.channels.seed,
            vec!["tui", "web-interface", "daemon", "docs"]
        );
    }

    #[test]
    fn test_channels_section_default() {
        let config = FullProjectConfig::default();
        assert!(config.channels.seed.is_empty());
    }

    #[test]
    fn test_channels_section_roundtrip() {
        let toml = r#"
[project]
name = "testproj"

[channels]
seed = ["daemon", "tui", "web"]
"#;
        let config: FullProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.channels.seed, vec!["daemon", "tui", "web"]);

        let serialized = toml::to_string_pretty(&config).unwrap();
        let reparsed: FullProjectConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(reparsed.channels.seed, vec!["daemon", "tui", "web"]);
    }

    #[test]
    fn test_channels_section_save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = FullProjectConfig::minimal("testproj", "/tmp/testproj");
        config.channels.seed = vec!["daemon".to_string(), "tui".to_string(), "web".to_string()];
        config.save_to(&path).unwrap();

        let loaded = FullProjectConfig::load_from(&path).unwrap();
        assert_eq!(loaded.channels.seed, vec!["daemon", "tui", "web"]);
    }

    #[test]
    fn test_parse_sandbox_config() {
        let toml = r#"
allowed_paths = ["~/.cargo", "~/.rustup", "/opt/toolchain"]
"#;
        let config: SandboxSection = toml::from_str(toml).unwrap();
        assert_eq!(config.allowed_paths.len(), 3);
        assert_eq!(config.allowed_paths[0], "~/.cargo");
        assert_eq!(config.allowed_paths[1], "~/.rustup");
        assert_eq!(config.allowed_paths[2], "/opt/toolchain");
    }

    #[test]
    fn test_sandbox_section_merge_deduplicates() {
        let global = SandboxSection {
            allowed_paths: vec!["~/.cargo".to_string(), "~/.rustup".to_string()],
        };
        let project = SandboxSection {
            allowed_paths: vec!["~/.cargo".to_string(), "/opt/toolchain".to_string()],
        };
        let merged = global.merge(&project);
        assert_eq!(merged.allowed_paths.len(), 3);
        assert!(merged.allowed_paths.contains(&"~/.cargo".to_string()));
        assert!(merged.allowed_paths.contains(&"~/.rustup".to_string()));
        assert!(merged.allowed_paths.contains(&"/opt/toolchain".to_string()));
    }

    #[test]
    fn test_sandbox_section_merge_empty_global() {
        let global = SandboxSection::default();
        let project = SandboxSection {
            allowed_paths: vec!["/opt/toolchain".to_string()],
        };
        let merged = global.merge(&project);
        assert_eq!(merged.allowed_paths, vec!["/opt/toolchain"]);
    }

    #[test]
    fn test_sandbox_section_merge_empty_project() {
        let global = SandboxSection {
            allowed_paths: vec!["~/.cargo".to_string()],
        };
        let project = SandboxSection::default();
        let merged = global.merge(&project);
        assert_eq!(merged.allowed_paths, vec!["~/.cargo"]);
    }

    #[test]
    fn test_get_project_sandbox_config_defaults_to_global() {
        // This test assumes "nonexistent-project" doesn't have a config file
        let config = get_project_sandbox_config("nonexistent-project-12345");
        // Should return global config (which defaults to empty)
        assert_eq!(config.allowed_paths.len(), 0);
    }

    /// Integration test: verify that sandbox config flows through to writable_dirs()
    #[test]
    fn test_sandbox_config_integration() {
        use std::path::Path;

        // Simulate merged config with some paths
        let sandbox_config = SandboxSection {
            allowed_paths: vec!["~/.cargo".to_string(), "/opt/toolchain".to_string()],
        };

        // Call writable_dirs with the configured paths
        let dirs = crate::sandbox::writable_dirs(
            Path::new("/home/user/project"),
            &[],
            &sandbox_config.allowed_paths,
        );

        // Verify the configured paths are included and expanded
        let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/root"));
        let cargo_path = home.join(".cargo").to_string_lossy().to_string();

        assert!(
            dirs.contains(&cargo_path),
            "Should include ~/.cargo expanded to {}",
            cargo_path
        );
        assert!(
            dirs.contains(&"/opt/toolchain".to_string()),
            "Should include /opt/toolchain"
        );

        // Verify standard paths are also included
        assert!(
            dirs.iter().any(|d| d.ends_with(".midtown")),
            "Should include ~/.midtown"
        );
        assert!(
            dirs.iter().any(|d| d.ends_with(".claude")),
            "Should include ~/.claude"
        );
    }

    #[test]
    fn test_channel_leads_default_model() {
        let config = ChannelLeadsConfig::default();
        assert_eq!(config.model_for_channel("any-channel"), "sonnet");
    }

    #[test]
    fn test_channel_leads_ops_defaults_to_haiku() {
        let config = ChannelLeadsConfig::default();
        assert_eq!(config.model_for_channel("ops"), "haiku");
    }

    #[test]
    fn test_channel_leads_ops_override_takes_precedence() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("ops".to_string(), "sonnet".to_string());
        let config = ChannelLeadsConfig {
            default_model: None,
            overrides,
        };
        assert_eq!(config.model_for_channel("ops"), "sonnet");
    }

    #[test]
    fn test_channel_leads_ops_default_model_takes_precedence() {
        let config = ChannelLeadsConfig {
            default_model: Some("opus".to_string()),
            overrides: std::collections::HashMap::new(),
        };
        assert_eq!(config.model_for_channel("ops"), "opus");
    }

    #[test]
    fn test_channel_leads_configured_default() {
        let config = ChannelLeadsConfig {
            default_model: Some("opus".to_string()),
            overrides: std::collections::HashMap::new(),
        };
        assert_eq!(config.model_for_channel("any-channel"), "opus");
    }

    #[test]
    fn test_channel_leads_per_channel_override() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("daemon-architecture".to_string(), "opus".to_string());
        let config = ChannelLeadsConfig {
            default_model: Some("sonnet".to_string()),
            overrides,
        };
        assert_eq!(config.model_for_channel("daemon-architecture"), "opus");
        assert_eq!(config.model_for_channel("web-interface"), "sonnet");
    }

    #[test]
    fn test_channel_leads_override_takes_precedence_over_default() {
        let mut overrides = std::collections::HashMap::new();
        overrides.insert("tui".to_string(), "haiku".to_string());
        let config = ChannelLeadsConfig {
            default_model: Some("opus".to_string()),
            overrides,
        };
        assert_eq!(config.model_for_channel("tui"), "haiku");
    }

    #[test]
    fn test_channel_leads_toml_parsing() {
        let toml = r#"
[channel_leads]
default_model = "sonnet"

[channel_leads.overrides]
"daemon-architecture" = "opus"
"web-interface" = "sonnet"
"#;
        let config: FullProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.channel_leads.default_model.as_deref(),
            Some("sonnet")
        );
        assert_eq!(
            config
                .channel_leads
                .model_for_channel("daemon-architecture"),
            "opus"
        );
        assert_eq!(
            config.channel_leads.model_for_channel("web-interface"),
            "sonnet"
        );
        assert_eq!(
            config.channel_leads.model_for_channel("unknown-channel"),
            "sonnet"
        );
    }

    #[test]
    fn test_channel_leads_toml_minimal() {
        // No channel_leads section — all defaults
        let toml = r#"
[project]
name = "test"
"#;
        let config: FullProjectConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.channel_leads.model_for_channel("any-channel"),
            "sonnet"
        );
    }
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
