//! Project-level configuration for midtown.
//!
//! Reads from `~/.midtown/config.toml` with per-project sections:
//!
//! ```toml
//! [default]
//! bin_command = "midtown"
//!
//! [midtown]  # project name from repo
//! bin_command = "cargo run --release --"
//! ```

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for a single project.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectConfig {
    /// Command to invoke midtown (e.g., "midtown" or "cargo run --release --")
    #[serde(default = "default_bin_command")]
    pub bin_command: String,
}

fn default_bin_command() -> String {
    "midtown".to_string()
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            bin_command: default_bin_command(),
        }
    }
}

/// Root configuration containing default and per-project settings.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    /// Default settings for all projects
    #[serde(default)]
    pub default: ProjectConfig,

    /// Per-project settings (key is project/repo name)
    #[serde(flatten)]
    pub projects: HashMap<String, ProjectConfig>,
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
    /// Falls back to default if no project-specific config exists.
    pub fn for_project(&self, project_name: &str) -> &ProjectConfig {
        self.projects.get(project_name).unwrap_or(&self.default)
    }
}

/// Get the path to the config file.
pub fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".midtown")
        .join("config.toml")
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
        config.for_project(&project_name).bin_command.clone()
    }
}

/// Get the current project name from git repo root directory.
fn get_project_name() -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout);
    let path = PathBuf::from(path.trim());

    path.file_name().map(|s| s.to_string_lossy().to_string())
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
    fn test_parse_config() {
        let toml = r#"
[default]
bin_command = "midtown"

[my-project]
bin_command = "cargo run --release --"
"#;
        let config: Config = toml::from_str(toml).unwrap();

        assert_eq!(config.default.bin_command, "midtown");
        assert_eq!(
            config.for_project("my-project").bin_command,
            "cargo run --release --"
        );
        // Unknown project falls back to default
        assert_eq!(config.for_project("unknown").bin_command, "midtown");
    }

    #[test]
    fn test_config_path() {
        let path = config_path();
        assert!(path.to_string_lossy().contains(".midtown"));
        assert!(path.to_string_lossy().ends_with("config.toml"));
    }
}
