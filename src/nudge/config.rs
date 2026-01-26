//! Nudge configuration

use std::time::Duration;

use super::{DEFAULT_NUDGE_INTERVAL_SECS, DEFAULT_NUDGE_TEMPLATE};

/// Configuration for the nudge system
#[derive(Debug, Clone)]
pub struct NudgeConfig {
    /// Interval between periodic nudges
    pub interval: Duration,
    /// Message template with {task} placeholder
    pub message_template: String,
    /// Whether nudging is enabled
    pub enabled: bool,
}

impl NudgeConfig {
    /// Create a new configuration with custom settings
    pub fn new(interval_secs: u64, template: impl Into<String>) -> Self {
        Self {
            interval: Duration::from_secs(interval_secs),
            message_template: template.into(),
            enabled: true,
        }
    }

    /// Create a configuration with nudging disabled
    pub fn disabled() -> Self {
        Self {
            interval: Duration::from_secs(DEFAULT_NUDGE_INTERVAL_SECS),
            message_template: DEFAULT_NUDGE_TEMPLATE.to_string(),
            enabled: false,
        }
    }

    /// Set the nudge interval in seconds
    pub fn with_interval_secs(mut self, secs: u64) -> Self {
        self.interval = Duration::from_secs(secs);
        self
    }

    /// Set the message template
    pub fn with_template(mut self, template: impl Into<String>) -> Self {
        self.message_template = template.into();
        self
    }

    /// Enable or disable nudging
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl Default for NudgeConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(DEFAULT_NUDGE_INTERVAL_SECS),
            message_template: DEFAULT_NUDGE_TEMPLATE.to_string(),
            enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = NudgeConfig::default();
        assert_eq!(config.interval, Duration::from_secs(300));
        assert!(config.message_template.contains("{task}"));
        assert!(config.enabled);
    }

    #[test]
    fn test_custom_config() {
        let config = NudgeConfig::new(60, "Custom: {task}");
        assert_eq!(config.interval, Duration::from_secs(60));
        assert_eq!(config.message_template, "Custom: {task}");
    }

    #[test]
    fn test_builder_pattern() {
        let config = NudgeConfig::default()
            .with_interval_secs(120)
            .with_template("Test: {task}")
            .with_enabled(false);

        assert_eq!(config.interval, Duration::from_secs(120));
        assert_eq!(config.message_template, "Test: {task}");
        assert!(!config.enabled);
    }

    #[test]
    fn test_disabled_config() {
        let config = NudgeConfig::disabled();
        assert!(!config.enabled);
        // Should still have valid defaults for other fields
        assert_eq!(config.interval, Duration::from_secs(300));
    }
}
