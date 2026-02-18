//! CLI handlers for `midtown config` subcommands.
//!
//! Provides `get`, `set`, and `list` for both global (`~/.midtown/config.toml`)
//! and per-project (`~/.midtown/projects/<repo>/config.toml`) configuration.

use clap::Subcommand;
use std::path::{Path, PathBuf};

use super::Response;

/// Subcommands for `midtown config`.
#[derive(Subcommand, Debug, Clone)]
pub enum ConfigCommand {
    /// Get a config value
    Get {
        /// Dotted key path (e.g., default.max_coworkers, daemon.webhook_port)
        key: String,
        /// Use global config (~/.midtown/config.toml) instead of project config
        #[arg(long)]
        global: bool,
    },
    /// Set a config value and persist it
    Set {
        /// Dotted key path (e.g., default.max_coworkers, daemon.webhook_port)
        key: String,
        /// Value to set (parsed as appropriate type for the key)
        value: String,
        /// Use global config (~/.midtown/config.toml) instead of project config
        #[arg(long)]
        global: bool,
    },
    /// List all current config values
    List {
        /// Use global config (~/.midtown/config.toml) instead of project config
        #[arg(long)]
        global: bool,
    },
}

/// All supported config key paths.
const VALID_KEYS: &[&str] = &[
    "default.personality",
    "default.max_coworkers",
    "default.chat_layout",
    "default.chat_min_width",
    "default.zellij_swap_layout",
    "default.zellij_chat_pane_size",
    "default.user_display_name",
    "default.bin_command",
    "daemon.webhook_port",
    "daemon.webhook_secret",
    "daemon.pr_poll_interval_secs",
    "daemon.chat_monitor_enabled",
    "daemon.github_user",
    "daemon.webhook_restart_interval_secs",
    "daemon.worktree_cleanup_retention_hours",
];

/// Dispatch a `ConfigCommand` to the appropriate handler.
pub fn handle(cmd: &ConfigCommand) -> Result<Response, String> {
    match cmd {
        ConfigCommand::Get { key, global } => {
            if *global {
                let path = midtown::config::global_config_path();
                let value = get_global_key(key, &path)?;
                Ok(Response::Message { message: value })
            } else {
                let path = resolve_project_config_path()?;
                let value = get_project_key(key, &path)?;
                Ok(Response::Message { message: value })
            }
        }
        ConfigCommand::Set { key, value, global } => {
            if *global {
                let path = midtown::config::global_config_path();
                set_global_key(key, value, &path)?;
                Ok(Response::Message {
                    message: format!("Set {} = {} (global)", key, value),
                })
            } else {
                let path = resolve_project_config_path()?;
                set_project_key(key, value, &path)?;
                Ok(Response::Message {
                    message: format!("Set {} = {}", key, value),
                })
            }
        }
        ConfigCommand::List { global } => {
            if *global {
                let path = midtown::config::global_config_path();
                let output = list_global_config(&path)?;
                Ok(Response::Message { message: output })
            } else {
                let path = resolve_project_config_path()?;
                let output = list_project_config(&path)?;
                Ok(Response::Message { message: output })
            }
        }
    }
}

/// Resolve the project config path from the current working directory.
///
/// Returns an error if no git repo is detected (suggesting --global instead).
fn resolve_project_config_path() -> Result<PathBuf, String> {
    let project = midtown::paths::detect_repo_name().ok_or_else(|| {
        "Not in a git repository. Use --global to manage global config instead.".to_string()
    })?;
    Ok(midtown::config::project_config_path(&project))
}

/// Validate that `key` is in the supported key list.
///
/// Returns `Err` with a helpful message listing valid keys if unknown.
fn validate_key(key: &str) -> Result<(), String> {
    if VALID_KEYS.contains(&key) {
        Ok(())
    } else {
        Err(format!(
            "Unknown key '{}'. Valid keys:\n  {}",
            key,
            VALID_KEYS.join("\n  ")
        ))
    }
}

/// Get a value from the global config at `config_path`.
///
/// Returns `"(not set)"` when the field is `None` or the file doesn't exist.
pub fn get_global_key(key: &str, config_path: &Path) -> Result<String, String> {
    validate_key(key)?;

    let config = load_global_config(config_path)?;
    Ok(global_field_value(&config, key))
}

/// Set a value in the global config at `config_path`.
///
/// Parses `value` as the correct type for `key`, then loads, updates, and saves the config.
pub fn set_global_key(key: &str, value: &str, config_path: &Path) -> Result<(), String> {
    validate_key(key)?;

    let mut config = load_global_config(config_path)?;
    apply_global_key(&mut config, key, value)?;
    config
        .save_to(config_path)
        .map_err(|e| format!("Failed to save global config: {}", e))
}

/// Get a value from the project config at `config_path`.
///
/// Returns `"(not set)"` when the field is `None` or the file doesn't exist.
pub fn get_project_key(key: &str, config_path: &Path) -> Result<String, String> {
    validate_key(key)?;

    let config = load_project_config(config_path)?;
    Ok(project_field_value(&config, key))
}

/// Set a value in the project config at `config_path`.
///
/// Parses `value` as the correct type for `key`, then loads, updates, and saves the config.
pub fn set_project_key(key: &str, value: &str, config_path: &Path) -> Result<(), String> {
    validate_key(key)?;

    let mut config = load_project_config(config_path)?;
    apply_project_key(&mut config, key, value)?;
    config
        .save_to(config_path)
        .map_err(|e| format!("Failed to save project config: {}", e))
}

/// List all config values from the global config.
///
/// Output includes the config file path and all supported keys with their current values.
pub fn list_global_config(config_path: &Path) -> Result<String, String> {
    let config = load_global_config(config_path)?;
    let mut lines = Vec::new();
    lines.push(format!("Config: {}", config_path.display()));
    lines.push(String::new());
    for key in VALID_KEYS {
        let value = global_field_value(&config, key);
        lines.push(format!("  {:<45} {}", key, value));
    }
    Ok(lines.join("\n"))
}

/// List all config values from the project config.
///
/// Output includes the config file path and all supported keys with their current values.
pub fn list_project_config(config_path: &Path) -> Result<String, String> {
    let config = load_project_config(config_path)?;
    let mut lines = Vec::new();
    lines.push(format!("Config: {}", config_path.display()));
    lines.push(String::new());
    for key in VALID_KEYS {
        let value = project_field_value(&config, key);
        lines.push(format!("  {:<45} {}", key, value));
    }
    Ok(lines.join("\n"))
}

// ──────────────────────────────────────────────────────────────────────────────
// Internal helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Load GlobalConfig from an arbitrary path (for testing with temp files).
///
/// Returns an error if the file exists but cannot be read or parsed, to prevent
/// a subsequent `save_to` call from silently overwriting the user's config with defaults.
fn load_global_config(path: &Path) -> Result<midtown::config::GlobalConfig, String> {
    if !path.exists() {
        return Ok(midtown::config::GlobalConfig::default());
    }
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read config file {}: {}", path.display(), e))?;
    toml::from_str(&contents).map_err(|e| {
        format!(
            "Config file {} has a syntax error: {}\nFix the file manually before using 'midtown config set'.",
            path.display(),
            e
        )
    })
}

/// Load FullProjectConfig from an arbitrary path.
///
/// Returns an error if the file exists but cannot be read or parsed, to prevent
/// a subsequent `save_to` call from silently overwriting the user's config with defaults,
/// and so that `get`/`list` surface corruption rather than showing silent defaults.
fn load_project_config(path: &Path) -> Result<midtown::config::FullProjectConfig, String> {
    if !path.exists() {
        return Ok(midtown::config::FullProjectConfig::default());
    }
    let contents = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read config file {}: {}", path.display(), e))?;
    toml::from_str(&contents).map_err(|e| {
        format!(
            "Config file {} has a syntax error: {}\nFix the file manually before using 'midtown config set'.",
            path.display(),
            e
        )
    })
}

/// Format an `Option<T>` for display: value or `"(not set)"`.
fn fmt_opt<T: std::fmt::Display>(opt: Option<T>) -> String {
    match opt {
        Some(v) => v.to_string(),
        None => "(not set)".to_string(),
    }
}

/// Format an optional secret string: `****` when set, `(not set)` when absent.
fn fmt_secret(opt: Option<&str>) -> String {
    match opt {
        Some(_) => "****".to_string(),
        None => "(not set)".to_string(),
    }
}

/// Read a field from `GlobalConfig` by dotted key name.
fn global_field_value(config: &midtown::config::GlobalConfig, key: &str) -> String {
    match key {
        "default.personality" => fmt_opt(config.default.personality.map(|p| p.as_str())),
        "default.max_coworkers" => fmt_opt(config.default.max_coworkers),
        "default.chat_layout" => fmt_opt(config.default.chat_layout.map(chat_layout_str)),
        "default.chat_min_width" => fmt_opt(config.default.chat_min_width),
        "default.zellij_swap_layout" => fmt_opt(config.default.zellij_swap_layout),
        "default.zellij_chat_pane_size" => fmt_opt(config.default.zellij_chat_pane_size),
        "default.user_display_name" => fmt_opt(config.default.user_display_name.as_deref()),
        "default.bin_command" => fmt_opt(config.default.bin_command.as_deref()),
        "daemon.webhook_port" => fmt_opt(config.daemon.webhook_port),
        "daemon.webhook_secret" => fmt_secret(config.daemon.webhook_secret.as_deref()),
        "daemon.pr_poll_interval_secs" => fmt_opt(config.daemon.pr_poll_interval_secs),
        "daemon.chat_monitor_enabled" => fmt_opt(config.daemon.chat_monitor_enabled),
        "daemon.github_user" => fmt_opt(config.daemon.github_user.as_deref()),
        "daemon.webhook_restart_interval_secs" => {
            fmt_opt(config.daemon.webhook_restart_interval_secs)
        }
        "daemon.worktree_cleanup_retention_hours" => {
            fmt_opt(config.daemon.worktree_cleanup_retention_hours)
        }
        // validate_key() is called before every read, so this arm is unreachable.
        // If it fires, a key was added to VALID_KEYS without adding a read arm here.
        _ => unreachable!(
            "key '{}' passed validate_key but has no read arm in global_field_value",
            key
        ),
    }
}

/// Read a field from `FullProjectConfig` by dotted key name.
fn project_field_value(config: &midtown::config::FullProjectConfig, key: &str) -> String {
    match key {
        "default.personality" => fmt_opt(config.default.personality.map(|p| p.as_str())),
        "default.max_coworkers" => fmt_opt(config.default.max_coworkers),
        "default.chat_layout" => fmt_opt(config.default.chat_layout.map(chat_layout_str)),
        "default.chat_min_width" => fmt_opt(config.default.chat_min_width),
        "default.zellij_swap_layout" => fmt_opt(config.default.zellij_swap_layout),
        "default.zellij_chat_pane_size" => fmt_opt(config.default.zellij_chat_pane_size),
        "default.user_display_name" => fmt_opt(config.default.user_display_name.as_deref()),
        "default.bin_command" => fmt_opt(config.default.bin_command.as_deref()),
        "daemon.webhook_port" => fmt_opt(config.daemon.webhook_port),
        "daemon.webhook_secret" => fmt_secret(config.daemon.webhook_secret.as_deref()),
        "daemon.pr_poll_interval_secs" => fmt_opt(config.daemon.pr_poll_interval_secs),
        "daemon.chat_monitor_enabled" => fmt_opt(config.daemon.chat_monitor_enabled),
        "daemon.github_user" => fmt_opt(config.daemon.github_user.as_deref()),
        "daemon.webhook_restart_interval_secs" => {
            fmt_opt(config.daemon.webhook_restart_interval_secs)
        }
        "daemon.worktree_cleanup_retention_hours" => {
            fmt_opt(config.daemon.worktree_cleanup_retention_hours)
        }
        // validate_key() is called before every read, so this arm is unreachable.
        // If it fires, a key was added to VALID_KEYS without adding a read arm here.
        _ => unreachable!(
            "key '{}' passed validate_key but has no read arm in project_field_value",
            key
        ),
    }
}

/// Apply a parsed value to the matching field of `GlobalConfig`.
fn apply_global_key(
    config: &mut midtown::config::GlobalConfig,
    key: &str,
    value: &str,
) -> Result<(), String> {
    match key {
        "default.personality" => {
            config.default.personality = Some(parse_personality(value)?);
        }
        "default.max_coworkers" => {
            config.default.max_coworkers = Some(parse_usize(key, value)?);
        }
        "default.chat_layout" => {
            config.default.chat_layout = Some(parse_chat_layout(value)?);
        }
        "default.chat_min_width" => {
            config.default.chat_min_width = Some(parse_u16(key, value)?);
        }
        "default.zellij_swap_layout" => {
            config.default.zellij_swap_layout = Some(parse_bool(key, value)?);
        }
        "default.zellij_chat_pane_size" => {
            config.default.zellij_chat_pane_size = Some(parse_pane_size(value)?);
        }
        "default.user_display_name" => {
            config.default.user_display_name = Some(value.to_string());
        }
        "default.bin_command" => {
            config.default.bin_command = Some(value.to_string());
        }
        "daemon.webhook_port" => {
            config.daemon.webhook_port = Some(parse_u16(key, value)?);
        }
        "daemon.webhook_secret" => {
            config.daemon.webhook_secret = Some(value.to_string());
        }
        "daemon.pr_poll_interval_secs" => {
            config.daemon.pr_poll_interval_secs = Some(parse_u64(key, value)?);
        }
        "daemon.chat_monitor_enabled" => {
            config.daemon.chat_monitor_enabled = Some(parse_bool(key, value)?);
        }
        "daemon.github_user" => {
            config.daemon.github_user = Some(value.to_string());
        }
        "daemon.webhook_restart_interval_secs" => {
            config.daemon.webhook_restart_interval_secs = Some(parse_u64(key, value)?);
        }
        "daemon.worktree_cleanup_retention_hours" => {
            config.daemon.worktree_cleanup_retention_hours = Some(parse_u64(key, value)?);
        }
        // validate_key() is called before every write, so this arm is unreachable.
        // If it fires, a key was added to VALID_KEYS without adding a write arm here.
        _ => unreachable!(
            "key '{}' passed validate_key but has no write arm in apply_global_key",
            key
        ),
    }
    Ok(())
}

/// Apply a parsed value to the matching field of `FullProjectConfig`.
fn apply_project_key(
    config: &mut midtown::config::FullProjectConfig,
    key: &str,
    value: &str,
) -> Result<(), String> {
    match key {
        "default.personality" => {
            config.default.personality = Some(parse_personality(value)?);
        }
        "default.max_coworkers" => {
            config.default.max_coworkers = Some(parse_usize(key, value)?);
        }
        "default.chat_layout" => {
            config.default.chat_layout = Some(parse_chat_layout(value)?);
        }
        "default.chat_min_width" => {
            config.default.chat_min_width = Some(parse_u16(key, value)?);
        }
        "default.zellij_swap_layout" => {
            config.default.zellij_swap_layout = Some(parse_bool(key, value)?);
        }
        "default.zellij_chat_pane_size" => {
            config.default.zellij_chat_pane_size = Some(parse_pane_size(value)?);
        }
        "default.user_display_name" => {
            config.default.user_display_name = Some(value.to_string());
        }
        "default.bin_command" => {
            config.default.bin_command = Some(value.to_string());
        }
        "daemon.webhook_port" => {
            config.daemon.webhook_port = Some(parse_u16(key, value)?);
        }
        "daemon.webhook_secret" => {
            config.daemon.webhook_secret = Some(value.to_string());
        }
        "daemon.pr_poll_interval_secs" => {
            config.daemon.pr_poll_interval_secs = Some(parse_u64(key, value)?);
        }
        "daemon.chat_monitor_enabled" => {
            config.daemon.chat_monitor_enabled = Some(parse_bool(key, value)?);
        }
        "daemon.github_user" => {
            config.daemon.github_user = Some(value.to_string());
        }
        "daemon.webhook_restart_interval_secs" => {
            config.daemon.webhook_restart_interval_secs = Some(parse_u64(key, value)?);
        }
        "daemon.worktree_cleanup_retention_hours" => {
            config.daemon.worktree_cleanup_retention_hours = Some(parse_u64(key, value)?);
        }
        // validate_key() is called before every write, so this arm is unreachable.
        // If it fires, a key was added to VALID_KEYS without adding a write arm here.
        _ => unreachable!(
            "key '{}' passed validate_key but has no write arm in apply_project_key",
            key
        ),
    }
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────────────
// Type parsers
// ──────────────────────────────────────────────────────────────────────────────

fn parse_personality(value: &str) -> Result<midtown::config::Personality, String> {
    match value {
        "normal" => Ok(midtown::config::Personality::Normal),
        "fun" => Ok(midtown::config::Personality::Fun),
        "wild" => Ok(midtown::config::Personality::Wild),
        _ => Err(format!(
            "Invalid personality '{}'. Valid values: normal, fun, wild",
            value
        )),
    }
}

fn parse_chat_layout(value: &str) -> Result<midtown::config::ChatLayout, String> {
    match value {
        "auto" => Ok(midtown::config::ChatLayout::Auto),
        "split" => Ok(midtown::config::ChatLayout::Split),
        "window" => Ok(midtown::config::ChatLayout::Window),
        _ => Err(format!(
            "Invalid chat_layout '{}'. Valid values: auto, split, window",
            value
        )),
    }
}

fn parse_bool(key: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!(
            "Invalid boolean value '{}' for '{}'. Use true or false.",
            value, key
        )),
    }
}

fn parse_usize(key: &str, value: &str) -> Result<usize, String> {
    value.parse::<usize>().map_err(|_| {
        format!(
            "Invalid integer value '{}' for '{}'. Expected a non-negative integer.",
            value, key
        )
    })
}

/// Parse a Zellij chat pane size: must be 10-90.
fn parse_pane_size(value: &str) -> Result<u8, String> {
    let n = value.parse::<u8>().map_err(|_| {
        format!(
            "Invalid value '{}' for 'default.zellij_chat_pane_size'. Expected an integer 10-90.",
            value
        )
    })?;
    if !(10..=90).contains(&n) {
        return Err(format!(
            "Invalid value '{}' for 'default.zellij_chat_pane_size'. Must be between 10 and 90.",
            value
        ));
    }
    Ok(n)
}

fn parse_u16(key: &str, value: &str) -> Result<u16, String> {
    value.parse::<u16>().map_err(|_| {
        format!(
            "Invalid integer value '{}' for '{}'. Expected an integer 0-65535.",
            value, key
        )
    })
}

fn parse_u64(key: &str, value: &str) -> Result<u64, String> {
    value.parse::<u64>().map_err(|_| {
        format!(
            "Invalid integer value '{}' for '{}'. Expected a non-negative integer.",
            value, key
        )
    })
}

/// Convert `ChatLayout` to its canonical string representation.
fn chat_layout_str(layout: midtown::config::ChatLayout) -> &'static str {
    match layout {
        midtown::config::ChatLayout::Auto => "auto",
        midtown::config::ChatLayout::Split => "split",
        midtown::config::ChatLayout::Window => "window",
    }
}

#[path = "config_tests.rs"]
#[cfg(test)]
mod tests;
