//! Tests for LaunchConfig

use super::*;

#[test]
fn test_to_headless_config_sets_cargo_home() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
    let headless = config.to_headless_config();

    // CARGO_HOME should be set to ~/.midtown/cargo to avoid sandbox violations
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/root"));
    let expected_cargo_home = home.join(".midtown/cargo").to_string_lossy().to_string();

    assert_eq!(
        headless.env.get("CARGO_HOME"),
        Some(&expected_cargo_home),
        "CARGO_HOME should be set to ~/.midtown/cargo for sandboxed coworkers"
    );
}

#[test]
fn test_shell_command_sets_cargo_home() {
    let config = LaunchConfig::coworker("park", "myrepo", SessionMode::Fresh, None);
    let result = config.to_shell_command(
        std::path::Path::new("/tmp/settings.json"),
        std::path::Path::new("/tmp/prompt.md"),
        None,
        std::path::Path::new("/tmp/test-repo"),
    );

    // CARGO_HOME should be set in the environment
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/root"));
    let expected_cargo_home = home.join(".midtown/cargo").to_string_lossy().to_string();

    assert!(
        result.shell_command.contains(&format!("CARGO_HOME='{}'", expected_cargo_home)),
        "CARGO_HOME should be set in shell command environment: {}",
        result.shell_command
    );
}
