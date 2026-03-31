//! Model alias normalization for multi-provider support.
//!
//! Maps generic size aliases (small/medium/large) and provider-specific model
//! names to the correct model string for the active auth provider.

/// Default model alias for an agent type/provider pair.
///
/// Claude uses "sonnet" for coworker/channel-lead and "opus" for lead/reviewer.
/// Codex uses "gpt-5.4" for all roles.
pub fn default_model_for_provider_role(
    provider: crate::auth::AuthProvider,
    agent_type: &str,
) -> &'static str {
    match provider {
        crate::auth::AuthProvider::Codex => "gpt-5.4",
        crate::auth::AuthProvider::Claude => match agent_type {
            "midtown-project-lead" | "midtown-code-reviewer" => "opus",
            _ => "sonnet",
        },
    }
}

/// Normalize a launch model alias to a provider-compatible value for the agent type.
///
/// This guards against stale model aliases when the provider is switched (for example,
/// `sonnet` lingering after switching to Codex). Explicit provider-compatible models
/// are preserved.
pub fn normalize_model_for_provider_role(
    model: &str,
    provider: crate::auth::AuthProvider,
    agent_type: &str,
) -> String {
    let trimmed = model.trim();
    let default_model = default_model_for_provider_role(provider, agent_type);
    if trimmed.is_empty() {
        return default_model.to_string();
    }

    let lower = trimmed.to_ascii_lowercase();
    // Provider-level generic size aliases.
    // "small/medium/large" normalize before cross-provider compatibility checks.
    if let Some(size_alias) = normalize_size_alias_for_provider(&lower, provider) {
        return size_alias;
    }

    match provider {
        crate::auth::AuthProvider::Codex => {
            // Claude aliases are not valid in Codex.
            if lower.contains("sonnet") || lower.contains("opus") || lower.contains("haiku") {
                default_model.to_string()
            } else {
                trimmed.to_string()
            }
        }
        crate::auth::AuthProvider::Claude => {
            // OpenAI/Codex model aliases are not valid in Claude.
            if is_openai_model_alias(&lower) {
                default_model.to_string()
            } else {
                trimmed.to_string()
            }
        }
    }
}

fn normalize_size_alias_for_provider(
    lower_model: &str,
    provider: crate::auth::AuthProvider,
) -> Option<String> {
    let size = match lower_model {
        "small" => "small",
        "medium" => "medium",
        "large" => "large",
        _ => return None,
    };

    let normalized = match provider {
        crate::auth::AuthProvider::Claude => match size {
            "small" => "haiku",
            "medium" => "sonnet",
            "large" => "opus",
            _ => unreachable!(),
        },
        crate::auth::AuthProvider::Codex => match size {
            "small" => "gpt-5.1-codex-mini",
            "medium" => "gpt-5.3-codex-spark",
            "large" => "gpt-5.4",
            _ => unreachable!(),
        },
    };
    Some(normalized.to_string())
}

/// Infer the auth provider for a model alias.
///
/// Returns `Some(provider)` when the alias is unambiguously associated with a
/// single provider (e.g., "opus" → Claude, "gpt-5.4" → Codex).
/// Returns `None` for provider-agnostic size aliases ("small", "medium", "large")
/// or unknown model names, in which case the caller should keep the current provider.
pub fn provider_for_model_alias(model: &str) -> Option<crate::auth::AuthProvider> {
    let lower = model.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }

    // Provider-agnostic size aliases — keep current provider
    if matches!(lower.as_str(), "small" | "medium" | "large") {
        return None;
    }

    // Claude model aliases
    if lower.contains("sonnet") || lower.contains("opus") || lower.contains("haiku") {
        return Some(crate::auth::AuthProvider::Claude);
    }

    // Codex model aliases
    if is_openai_model_alias(&lower) {
        return Some(crate::auth::AuthProvider::Codex);
    }

    None
}

fn is_openai_model_alias(lower_model: &str) -> bool {
    lower_model.contains("codex")
        || lower_model.starts_with("gpt-")
        || lower_model
            .as_bytes()
            .get(0..2)
            .is_some_and(|prefix| prefix[0] == b'o' && prefix[1].is_ascii_digit())
}

/// Resolve the model for a spawn, respecting `execution.default_model` from config.
///
/// Resolution order:
/// 1. Role-specific `*_model` from config (e.g., `coworker_model = "large"`)
/// 2. `execution.default_model` from config (e.g., `default_model = "large"`)
/// 3. Hardcoded default via `default_model_for_provider_role()`
///
/// The returned string is already normalized for the provider (e.g., "large" → "opus" for Claude).
/// All spawn paths should use this instead of calling `default_model_for_provider_role()` directly.
pub fn resolve_model_for_role(
    repo_name: &str,
    provider: crate::auth::AuthProvider,
    agent_type: &str,
) -> String {
    let execution_role = crate::config::execution_role_for_agent_type(agent_type);
    crate::config::get_model_for_role(repo_name, execution_role)
        .map(|size| normalize_model_for_provider_role(size.as_model_str(), provider, agent_type))
        .unwrap_or_else(|| default_model_for_provider_role(provider, agent_type).to_string())
}

#[path = "model_tests.rs"]
#[cfg(test)]
mod tests;
