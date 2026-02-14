//! Settings and prompt file management for Claude Code sessions.
//!
//! Writes system prompts and settings files to the state directory for
//! both Lead and coworker sessions. These files are referenced by the
//! Claude CLI via `--system-prompt` and `--settings-file` flags.

use std::path::PathBuf;

use crate::Error;

/// Embedded common settings shared by both Lead and coworker Claude Code sessions.
const DEFAULT_COMMON_SETTINGS: &str = include_str!("../agents/common-settings.json");

/// Embedded settings specific to Lead Claude Code sessions (merged on top of common).
const DEFAULT_LEAD_SETTINGS: &str = include_str!("../agents/lead-settings.json");

/// Embedded settings specific to coworker Claude Code sessions (merged on top of common).
const DEFAULT_COWORKER_SETTINGS: &str = include_str!("../agents/coworker-settings.json");

/// Get the state directory for midtown.
fn state_dir() -> PathBuf {
    let state_dir = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local")
                .join("state")
        });
    state_dir.join("midtown")
}

/// Load settings by merging common with role-specific, then replacing `{bin}` placeholders.
///
/// Role-specific keys override common keys (shallow merge of top-level keys).
/// The `{bin}` placeholder in hook commands is replaced with the actual binary command.
fn load_settings(role_settings: &str) -> serde_json::Value {
    let bin_command = crate::config::get_bin_command();

    // Merge common + role JSON as raw strings, then replace {bin} before parsing.
    // This is simpler than walking the parsed JSON tree to find command strings.
    let common = DEFAULT_COMMON_SETTINGS.replace("{bin}", &bin_command);
    let role = role_settings.replace("{bin}", &bin_command);

    let mut settings: serde_json::Value =
        serde_json::from_str(&common).expect("invalid common-settings.json");
    let role: serde_json::Value = serde_json::from_str(&role).expect("invalid role settings JSON");

    if let (Some(base), Some(overrides)) = (settings.as_object_mut(), role.as_object()) {
        for (key, value) in overrides {
            base.insert(key.clone(), value.clone());
        }
    }

    settings
}

/// Read plugins from user's ~/.claude/settings.json
fn read_user_plugins() -> Option<serde_json::Value> {
    let home = std::env::var("HOME").ok()?;
    let settings_path = std::path::PathBuf::from(home)
        .join(".claude")
        .join("settings.json");
    let content = std::fs::read_to_string(settings_path).ok()?;
    let settings: serde_json::Value = serde_json::from_str(&content).ok()?;
    settings.get("enabledPlugins").cloned()
}

/// Build coworker settings from agents/common-settings.json + agents/coworker-settings.json.
///
/// Merges base settings, replaces `{bin}` placeholders, and adds user's enabled plugins.
fn coworker_settings_json() -> serde_json::Value {
    let mut settings = load_settings(DEFAULT_COWORKER_SETTINGS);

    // Add user's plugins from ~/.claude/settings.json
    let user_plugins = read_user_plugins().unwrap_or_default();
    settings["enabledPlugins"] = user_plugins;

    settings
}

/// Write coworker settings to a shared file and return the path.
/// All coworkers use the same settings file.
pub fn write_coworker_settings_file() -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("coworker-settings.json");
    let settings = coworker_settings_json();
    std::fs::write(&path, settings.to_string()).map_err(Error::Io)?;

    Ok(path)
}

/// Build lead settings from agents/common-settings.json + agents/lead-settings.json.
///
/// Merges base settings and replaces `{bin}` placeholders.
pub fn lead_settings_json() -> serde_json::Value {
    load_settings(DEFAULT_LEAD_SETTINGS)
}

/// Write Lead settings to a file and return the path.
pub fn write_lead_settings_file() -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("lead-settings.json");
    let settings = lead_settings_json();
    std::fs::write(&path, settings.to_string()).map_err(Error::Io)?;

    Ok(path)
}

/// Write the Lead system prompt to a file and return the path.
pub fn write_lead_prompt_file() -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join("lead-prompt.md");
    std::fs::write(&path, crate::agents::lead_system_prompt()).map_err(Error::Io)?;

    Ok(path)
}

/// Write a coworker's system prompt to a file and return the path.
pub fn write_coworker_prompt_file(name: &str, prompt: &str) -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join(format!("coworker-{}-prompt.md", name));
    std::fs::write(&path, prompt).map_err(Error::Io)?;

    Ok(path)
}

/// Write a coworker's initial prompt to a file and return the path.
///
/// This is the task/nudge message that the coworker should work on. It's
/// passed to claude via `-p "$(cat file)"` so it's available at startup
/// without needing to send keystrokes after the TUI initializes.
pub fn write_coworker_initial_prompt_file(name: &str, prompt: &str) -> crate::Result<PathBuf> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(Error::Io)?;

    let path = dir.join(format!("coworker-{}-initial-prompt.md", name));
    std::fs::write(&path, prompt).map_err(Error::Io)?;

    Ok(path)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coworker_settings_json_is_valid() {
        let settings = coworker_settings_json();

        // Verify common settings are merged in
        assert_eq!(settings["autoUpdates"], false);

        // CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS is blocklisted from settings.json by Claude Code;
        // it's now exported as a real shell env var in to_shell_command() instead.
        assert!(settings["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"].is_null());

        // Verify coworker-specific settings
        assert_eq!(settings["editorMode"], "normal");

        // Verify no Stop hook (removed - daemon handles work assignment)
        assert!(settings["hooks"]["Stop"].is_null());

        // Verify PostToolUse hooks for task operations, questions, and insights
        let post_tool_hooks = &settings["hooks"]["PostToolUse"];
        assert!(post_tool_hooks.is_array());
        assert_eq!(post_tool_hooks.as_array().unwrap().len(), 4);

        // TaskUpdate hook
        assert_eq!(post_tool_hooks[0]["matcher"], "TaskUpdate");
        assert_eq!(
            post_tool_hooks[0]["hooks"][0]["command"],
            "midtown hook task"
        );

        // TaskCreate hook
        assert_eq!(post_tool_hooks[1]["matcher"], "TaskCreate");
        assert_eq!(
            post_tool_hooks[1]["hooks"][0]["command"],
            "midtown hook task"
        );

        // AskUserQuestion hook
        assert_eq!(post_tool_hooks[2]["matcher"], "AskUserQuestion");
        assert_eq!(
            post_tool_hooks[2]["hooks"][0]["command"],
            "midtown hook ask"
        );

        // Insight hook (no matcher)
        assert!(post_tool_hooks[3]["matcher"].is_null());
        assert_eq!(
            post_tool_hooks[3]["hooks"][0]["command"],
            "midtown hook insight"
        );

        // Verify Notification hook for idle
        let notification_hooks = &settings["hooks"]["Notification"];
        assert!(notification_hooks.is_array());
        assert_eq!(notification_hooks[0]["matcher"], "idle_prompt");
        assert_eq!(
            notification_hooks[0]["hooks"][0]["command"],
            "midtown hook idle"
        );

        // Verify {bin} placeholders were replaced (no literal "{bin}" in output)
        let serialized = settings.to_string();
        assert!(
            !serialized.contains("{bin}"),
            "settings should not contain unreplaced {{bin}} placeholders"
        );
    }

    #[test]
    fn test_lead_settings_json_is_valid() {
        let settings = lead_settings_json();

        // Verify common settings are merged in
        assert_eq!(settings["autoUpdates"], false);

        // CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS is blocklisted from settings.json by Claude Code;
        // it's now exported as a real shell env var in to_shell_command() instead.
        assert!(settings["env"]["CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS"].is_null());

        // Verify lead-specific hooks
        let stop_hooks = &settings["hooks"]["Stop"];
        assert!(stop_hooks.is_array());
        assert!(
            stop_hooks[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook lead-stop")
        );

        let post_tool_hooks = &settings["hooks"]["PostToolUse"];
        assert!(post_tool_hooks.is_array());
        // Lead has only the catch-all insight hook (task hooks removed —
        // Lead now uses `midtown task` CLI instead of TaskCreate/TaskUpdate tools)
        assert!(
            post_tool_hooks[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook insight"),
            "PostToolUse hook should be the catch-all insight hook"
        );

        // Verify {bin} placeholders were replaced
        let serialized = settings.to_string();
        assert!(
            !serialized.contains("{bin}"),
            "settings should not contain unreplaced {{bin}} placeholders"
        );
    }
}
