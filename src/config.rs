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
//!    webhook_port = 47022
//!    webhook_secret = "your-secret"
//!    webhook_restart_interval_secs = 300
//!    pr_poll_interval_secs = 60
//!    chat_monitor_enabled = true
//!    ```
//!
//! 2. **Project config** at `~/.midtown/projects/<project>/config.toml`:
//!    ```toml
//!    # All fields are optional - only override what you need
//!    bin_command = "cargo run --release --"
//!    chat_layout = "split"
//!    max_coworkers = 4
//!    ```
//!
//! Project config takes precedence over global defaults.
//!
//! Daemon settings can also be overridden via environment variables:
//! - `MIDTOWN_WEBHOOK_PORT` (set to 0 to disable)
//! - `MIDTOWN_WEBHOOK_SECRET`
//! - `MIDTOWN_WEBHOOK_RESTART_INTERVAL`
//! - `MIDTOWN_PR_POLL_INTERVAL`
//! - `MIDTOWN_CHAT_MONITOR` (set to 0 to disable)

use serde::Deserialize;
use std::path::PathBuf;

/// Chat layout mode for the Lead session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
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

/// Configuration for a single project.
///
/// Used both as global defaults and project-specific overrides.
#[derive(Debug, Clone, Deserialize, Default)]
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

    /// Maximum number of concurrent coworkers (default: 16)
    #[serde(default)]
    pub max_coworkers: Option<usize>,
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
}

/// Configuration for Claude Code plugins.
#[derive(Debug, Clone, Deserialize, Default)]
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
#[derive(Debug, Clone, Deserialize, Default)]
pub struct DaemonSection {
    /// Port for the webhook server (default: 47022, set to 0 to disable)
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
}

/// Global configuration from `~/.midtown/config.toml`.
#[derive(Debug, Clone, Deserialize, Default)]
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
}

impl GlobalConfig {
    /// Load global configuration from `~/.midtown/config.toml`.
    ///
    /// Returns default config if file doesn't exist or can't be parsed.
    pub fn load() -> Self {
        let path = global_config_path();

        if !path.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Get the list of required plugins.
    pub fn required_plugins(&self) -> &[String] {
        &self.plugins.required
    }
}

/// Load project-specific configuration from `~/.midtown/projects/<project>/config.toml`.
///
/// Returns None if the file doesn't exist.
fn load_project_config(project_name: &str) -> Option<ProjectConfig> {
    let path = project_config_path(project_name);

    if !path.exists() {
        return None;
    }

    std::fs::read_to_string(&path)
        .ok()
        .and_then(|contents| toml::from_str(&contents).ok())
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
        };

        let project = ProjectConfig {
            bin_command: Some("custom".to_string()),
            chat_layout: None, // Not overridden
            chat_min_width: Some(200),
            max_coworkers: None, // Not overridden
        };

        let merged = global.merge(&project);

        assert_eq!(merged.bin_command(), "custom"); // Overridden
        assert_eq!(merged.chat_layout(), ChatLayout::Auto); // From global
        assert_eq!(merged.chat_min_width(), 200); // Overridden
        assert_eq!(merged.max_coworkers(), Some(8)); // From global
    }

    #[test]
    fn test_merge_configs_max_coworkers_override() {
        let global = ProjectConfig {
            bin_command: Some("midtown".to_string()),
            chat_layout: None,
            chat_min_width: None,
            max_coworkers: Some(16),
        };

        let project = ProjectConfig {
            bin_command: None,
            chat_layout: None,
            chat_min_width: None,
            max_coworkers: Some(4),
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
}
