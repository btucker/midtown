//! Project-level configuration for midtown.
//!
//! Configuration is loaded from two places:
//!
//! 1. Global defaults: `~/.midtown/config.toml`
//! ```toml
//! [default]
//! bin_command = "midtown"
//! chat_layout = "auto"
//!
//! [plugins]
//! required = [
//!     "superpowers@claude-plugins-official",
//!     "code-review@claude-plugins-official",
//! ]
//! ```
//!
//! 2. Per-project overrides: `~/.midtown/projects/<project>/config.toml`
//! ```toml
//! bin_command = "cargo run --release --"
//! chat_layout = "split"
//! ```
//!
//! Project-specific settings take precedence over global defaults.

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
/// Used both for global defaults and per-project overrides.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    /// Command to invoke midtown (e.g., "midtown" or "cargo run --release --")
    #[serde(default = "default_bin_command")]
    pub bin_command: String,

    /// Chat pane layout mode (auto, split, or window)
    #[serde(default)]
    pub chat_layout: ChatLayout,

    /// Minimum terminal width for split layout in auto mode (default: 160)
    #[serde(default = "default_chat_min_width")]
    pub chat_min_width: u16,
}

fn default_chat_min_width() -> u16 {
    160
}

fn default_bin_command() -> String {
    "midtown".to_string()
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            bin_command: default_bin_command(),
            chat_layout: ChatLayout::default(),
            chat_min_width: default_chat_min_width(),
        }
    }
}

impl ProjectConfig {
    /// Merge another config into this one, taking values from `other` where set.
    ///
    /// This is used to layer project-specific config on top of global defaults.
    fn merge_from(&mut self, other: &ProjectConfig) {
        // Only override if the other value is not the default
        if other.bin_command != default_bin_command() {
            self.bin_command = other.bin_command.clone();
        }
        if other.chat_layout != ChatLayout::default() {
            self.chat_layout = other.chat_layout;
        }
        if other.chat_min_width != default_chat_min_width() {
            self.chat_min_width = other.chat_min_width;
        }
    }
}

/// Configuration for Claude Code plugins.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct PluginsConfig {
    /// List of required plugin names (e.g., "superpowers@claude-plugins-official")
    #[serde(default)]
    pub required: Vec<String>,
}

/// Root configuration containing default settings and plugin config.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Default settings for all projects
    #[serde(default)]
    pub default: ProjectConfig,

    /// Plugin configuration
    #[serde(default)]
    pub plugins: PluginsConfig,
}

impl Config {
    /// Load configuration from `~/.midtown/config.toml`.
    ///
    /// Returns default config if file doesn't exist or can't be parsed.
    pub fn load() -> Self {
        let path = config_path();

        if !path.exists() {
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Get configuration for a specific project.
    ///
    /// Loads project-specific config from `~/.midtown/projects/<project>/config.toml`
    /// and merges it with global defaults. Project settings take precedence.
    pub fn for_project(&self, project_name: &str) -> ProjectConfig {
        let mut config = self.default.clone();

        // Try to load project-specific config
        if let Some(project_config) = load_project_config(project_name) {
            config.merge_from(&project_config);
        }

        config
    }

    /// Get the list of required plugins.
    pub fn required_plugins(&self) -> &[String] {
        &self.plugins.required
    }
}

/// Get the path to the global config file.
pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("config.toml")
}

/// Get the path to a project-specific config file.
pub fn project_config_path(project_name: &str) -> PathBuf {
    crate::paths::projects_dir_for_repo(project_name).join("config.toml")
}

/// Load project-specific configuration.
///
/// Returns None if the file doesn't exist or can't be parsed.
fn load_project_config(project_name: &str) -> Option<ProjectConfig> {
    let path = project_config_path(project_name);

    if !path.exists() {
        return None;
    }

    std::fs::read_to_string(&path)
        .ok()
        .and_then(|contents| toml::from_str(&contents).ok())
}

/// Get the bin_command for the current project.
///
/// Determines project name from the current git repo and looks up config.
pub fn get_bin_command() -> String {
    let config = Config::load();

    // Try to get project name from git repo
    let project_name = get_project_name().unwrap_or_default();

    if project_name.is_empty() {
        config.default.bin_command.clone()
    } else {
        config.for_project(&project_name).bin_command
    }
}

/// Get the chat layout configuration for the current project.
pub fn get_chat_layout() -> (ChatLayout, u16) {
    let config = Config::load();
    let project_name = get_project_name().unwrap_or_default();

    let project_config = if project_name.is_empty() {
        config.default.clone()
    } else {
        config.for_project(&project_name)
    };

    (project_config.chat_layout, project_config.chat_min_width)
}

/// Get the list of required plugins from config.
pub fn get_required_plugins() -> Vec<String> {
    let config = Config::load();
    config.plugins.required.clone()
}

/// Get the current project name from git repo root directory.
fn get_project_name() -> Option<String> {
    crate::paths::detect_repo_name()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.default.bin_command, "midtown");
    }

    #[test]
    fn test_parse_global_config() {
        let toml = r#"
[default]
bin_command = "midtown"
chat_layout = "split"

[plugins]
required = ["superpowers@claude-plugins-official"]
"#;
        let config: Config = toml::from_str(toml).unwrap();

        assert_eq!(config.default.bin_command, "midtown");
        assert_eq!(config.default.chat_layout, ChatLayout::Split);
        assert_eq!(config.plugins.required.len(), 1);
    }

    #[test]
    fn test_config_path() {
        let path = config_path();
        assert!(path.to_string_lossy().contains(".midtown"));
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }

    #[test]
    fn test_project_config_path() {
        let path = project_config_path("my-project");
        assert!(path.to_string_lossy().contains(".midtown"));
        assert!(path.to_string_lossy().contains("projects"));
        assert!(path.to_string_lossy().contains("my-project"));
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }

    #[test]
    fn test_chat_layout_default() {
        let config = Config::default();
        assert_eq!(config.default.chat_layout, ChatLayout::Auto);
        assert_eq!(config.default.chat_min_width, 160);
    }

    #[test]
    fn test_chat_layout_parse() {
        let toml = r#"
[default]
chat_layout = "split"
chat_min_width = 200
"#;
        let config: Config = toml::from_str(toml).unwrap();

        assert_eq!(config.default.chat_layout, ChatLayout::Split);
        assert_eq!(config.default.chat_min_width, 200);
    }

    #[test]
    fn test_chat_layout_auto() {
        let toml = r#"
[default]
chat_layout = "auto"
"#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.default.chat_layout, ChatLayout::Auto);
    }

    #[test]
    fn test_plugins_config_default() {
        let config = Config::default();
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
        let config: Config = toml::from_str(toml).unwrap();

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
        let config: Config = toml::from_str(toml).unwrap();
        assert!(config.plugins.required.is_empty());
    }

    #[test]
    fn test_project_config_merge() {
        let mut base = ProjectConfig::default();
        assert_eq!(base.bin_command, "midtown");
        assert_eq!(base.chat_layout, ChatLayout::Auto);

        // Create a project config with overrides
        let project = ProjectConfig {
            bin_command: "cargo run --release --".to_string(),
            chat_layout: ChatLayout::Window,
            chat_min_width: default_chat_min_width(), // Keep default
        };

        base.merge_from(&project);

        assert_eq!(base.bin_command, "cargo run --release --");
        assert_eq!(base.chat_layout, ChatLayout::Window);
        assert_eq!(base.chat_min_width, 160); // Should keep default
    }

    #[test]
    fn test_for_project_returns_defaults_when_no_project_config() {
        let config = Config::default();
        let project = config.for_project("nonexistent-project");

        assert_eq!(project.bin_command, "midtown");
        assert_eq!(project.chat_layout, ChatLayout::Auto);
    }

    #[test]
    fn test_parse_project_config_file() {
        // Test parsing a flat project config (no sections)
        let toml = r#"
bin_command = "cargo run --release --"
chat_layout = "split"
"#;
        let config: ProjectConfig = toml::from_str(toml).unwrap();

        assert_eq!(config.bin_command, "cargo run --release --");
        assert_eq!(config.chat_layout, ChatLayout::Split);
    }
}
