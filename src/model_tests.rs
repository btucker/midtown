use super::*;

#[test]
fn default_model_for_provider_role_uses_codex_model_for_all_roles() {
    let provider = crate::auth::AuthProvider::Codex;
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-project-lead"),
        "gpt-5.4"
    );
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-code-author"),
        "gpt-5.4"
    );
}

#[test]
fn default_model_for_provider_role_uses_claude_tiers() {
    let provider = crate::auth::AuthProvider::Claude;
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-project-lead"),
        "opus"
    );
    assert_eq!(
        default_model_for_provider_role(provider, "midtown-code-author"),
        "sonnet"
    );
}

#[test]
fn normalize_model_for_provider_role_rewrites_claude_alias_for_codex() {
    let normalized = normalize_model_for_provider_role(
        "sonnet",
        crate::auth::AuthProvider::Codex,
        "midtown-channel-lead",
    );
    assert_eq!(normalized, "gpt-5.4");
}

#[test]
fn normalize_model_for_provider_role_keeps_codex_alias_for_codex() {
    let normalized = normalize_model_for_provider_role(
        "gpt-5.4",
        crate::auth::AuthProvider::Codex,
        "midtown-code-author",
    );
    assert_eq!(normalized, "gpt-5.4");
}

#[test]
fn normalize_model_for_provider_role_rewrites_codex_alias_for_claude() {
    let normalized = normalize_model_for_provider_role(
        "gpt-5.4",
        crate::auth::AuthProvider::Claude,
        "midtown-project-lead",
    );
    assert_eq!(normalized, "opus");
}

#[test]
fn normalize_model_for_provider_role_maps_size_aliases_for_claude() {
    let small = normalize_model_for_provider_role(
        "small",
        crate::auth::AuthProvider::Claude,
        "midtown-code-author",
    );
    let medium = normalize_model_for_provider_role(
        "medium",
        crate::auth::AuthProvider::Claude,
        "midtown-project-lead",
    );
    assert_eq!(small, "haiku");
    assert_eq!(medium, "sonnet");
}

#[test]
fn normalize_model_for_provider_role_maps_size_aliases_for_codex() {
    let small = normalize_model_for_provider_role(
        "small",
        crate::auth::AuthProvider::Codex,
        "midtown-code-author",
    );
    let medium = normalize_model_for_provider_role(
        "medium",
        crate::auth::AuthProvider::Codex,
        "midtown-project-lead",
    );
    let large = normalize_model_for_provider_role(
        "large",
        crate::auth::AuthProvider::Codex,
        "midtown-code-reviewer",
    );
    assert_eq!(small, "gpt-5.1-codex-mini");
    assert_eq!(medium, "gpt-5.3-codex-spark");
    assert_eq!(large, "gpt-5.4");
}
