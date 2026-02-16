//! Provider-specific adapters for headed (interactive terminal) agents.
//!
//! Headless remains the default execution model. This module only affects
//! headed message delivery paths (eg, lead nudges via headed intercom).

use crate::auth::AuthProvider;

/// Provider-agnostic interface for headed control messages.
///
/// Adapters can customize message shaping for provider-specific UIs while
/// keeping daemon logic generic.
pub trait HeadedAgentAdapter: Send + Sync {
    /// The provider this adapter targets.
    fn provider(&self) -> AuthProvider;

    /// Build the text payload to inject into the headed agent.
    fn format_system_message(&self, message: &str) -> String;
}

/// Claude headed adapter.
#[derive(Debug, Default)]
pub struct ClaudeHeadedAdapter;

impl HeadedAgentAdapter for ClaudeHeadedAdapter {
    fn provider(&self) -> AuthProvider {
        AuthProvider::Claude
    }

    fn format_system_message(&self, message: &str) -> String {
        // Keep current behavior unchanged while routing through the adapter API.
        message.to_string()
    }
}

/// Codex headed adapter.
#[derive(Debug, Default)]
pub struct CodexHeadedAdapter;

impl HeadedAgentAdapter for CodexHeadedAdapter {
    fn provider(&self) -> AuthProvider {
        AuthProvider::Codex
    }

    fn format_system_message(&self, message: &str) -> String {
        // Today Codex accepts plain text nudges similarly to Claude.
        // Keeping this separated allows provider-specific shaping later.
        message.to_string()
    }
}

/// z.ai headed adapter.
#[derive(Debug, Default)]
pub struct ZaiHeadedAdapter;

impl HeadedAgentAdapter for ZaiHeadedAdapter {
    fn provider(&self) -> AuthProvider {
        AuthProvider::Zai
    }

    fn format_system_message(&self, message: &str) -> String {
        message.to_string()
    }
}

/// Construct a compiled-in adapter for a provider.
pub fn adapter_for(provider: AuthProvider) -> Box<dyn HeadedAgentAdapter> {
    match provider {
        AuthProvider::Claude => Box::<ClaudeHeadedAdapter>::default(),
        AuthProvider::Codex => Box::<CodexHeadedAdapter>::default(),
        AuthProvider::Zai => Box::<ZaiHeadedAdapter>::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_factory_returns_matching_provider() {
        assert_eq!(
            adapter_for(AuthProvider::Claude).provider(),
            AuthProvider::Claude
        );
        assert_eq!(
            adapter_for(AuthProvider::Codex).provider(),
            AuthProvider::Codex
        );
        assert_eq!(adapter_for(AuthProvider::Zai).provider(), AuthProvider::Zai);
    }

    #[test]
    fn codex_and_claude_currently_preserve_message_text() {
        let msg = "system: please review this";
        assert_eq!(
            adapter_for(AuthProvider::Claude).format_system_message(msg),
            msg
        );
        assert_eq!(
            adapter_for(AuthProvider::Codex).format_system_message(msg),
            msg
        );
    }
}
