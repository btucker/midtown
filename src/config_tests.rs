use super::*;

// ──────────────────────────────────────────────────────────────────────────────
// ModelSize enum
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn model_size_serde_roundtrip() {
    let toml = r#"
[execution]
default_model = "medium"
coworker_model = "small"
reviewer_model = "large"
"#;
    let config: FullProjectConfig = toml::from_str(toml).unwrap();
    assert_eq!(config.execution.default_model, Some(ModelSize::Medium));
    assert_eq!(config.execution.coworker_model, Some(ModelSize::Small));
    assert_eq!(config.execution.reviewer_model, Some(ModelSize::Large));
}

#[test]
fn model_size_display_and_from_str() {
    assert_eq!(ModelSize::Small.to_string(), "small");
    assert_eq!(ModelSize::Medium.to_string(), "medium");
    assert_eq!(ModelSize::Large.to_string(), "large");

    assert_eq!("small".parse::<ModelSize>().unwrap(), ModelSize::Small);
    assert_eq!("MEDIUM".parse::<ModelSize>().unwrap(), ModelSize::Medium);
    assert_eq!("Large".parse::<ModelSize>().unwrap(), ModelSize::Large);
    assert!("xlarge".parse::<ModelSize>().is_err());
}

#[test]
fn model_size_as_model_str() {
    assert_eq!(ModelSize::Small.as_model_str(), "small");
    assert_eq!(ModelSize::Medium.as_model_str(), "medium");
    assert_eq!(ModelSize::Large.as_model_str(), "large");
}

// ──────────────────────────────────────────────────────────────────────────────
// ExecutionSection model fields merge
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn model_fields_merge_correctly() {
    let base = ExecutionSection {
        default_model: Some(ModelSize::Medium),
        coworker_model: Some(ModelSize::Small),
        ..ExecutionSection::default()
    };
    let overrides = ExecutionSection {
        coworker_model: Some(ModelSize::Large),
        reviewer_model: Some(ModelSize::Large),
        ..ExecutionSection::default()
    };
    let merged = base.merge(&overrides);
    assert_eq!(merged.default_model, Some(ModelSize::Medium)); // from base
    assert_eq!(merged.coworker_model, Some(ModelSize::Large)); // overridden
    assert_eq!(merged.reviewer_model, Some(ModelSize::Large)); // from override
    assert_eq!(merged.lead_model, None); // neither set
}

#[test]
fn default_provider_merges_correctly() {
    let base = ExecutionSection {
        default_provider: Some(crate::auth::AuthProvider::Claude),
        ..ExecutionSection::default()
    };
    let overrides = ExecutionSection {
        default_provider: Some(crate::auth::AuthProvider::Codex),
        ..ExecutionSection::default()
    };
    let merged = base.merge(&overrides);
    assert_eq!(
        merged.default_provider,
        Some(crate::auth::AuthProvider::Codex)
    );
}

// ──────────────────────────────────────────────────────────────────────────────
// resolve_execution_provider with default_provider fallback
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn resolve_provider_falls_back_to_default_provider() {
    let exec = ExecutionSection {
        default_provider: Some(crate::auth::AuthProvider::Codex),
        ..ExecutionSection::default()
    };
    // Coworker has no role-specific provider — should fall back to default_provider
    let provider = resolve_execution_provider(&exec, ExecutionRole::Coworker);
    assert_eq!(provider, crate::auth::AuthProvider::Codex);
}

#[test]
fn resolve_provider_role_specific_overrides_default_provider() {
    let exec = ExecutionSection {
        default_provider: Some(crate::auth::AuthProvider::Codex),
        coworker_provider: Some(crate::auth::AuthProvider::Claude),
        ..ExecutionSection::default()
    };
    let provider = resolve_execution_provider(&exec, ExecutionRole::Coworker);
    assert_eq!(provider, crate::auth::AuthProvider::Claude);
}

// ──────────────────────────────────────────────────────────────────────────────
// ChannelLeadsConfig::model_for_channel_with_fallback
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn channel_leads_config_uses_execution_fallback() {
    let config = ChannelLeadsConfig::default();
    // No per-channel override, no default_model → execution fallback kicks in
    let model = config.model_for_channel_with_fallback("web", Some(ModelSize::Large));
    assert_eq!(model, "large");
}

#[test]
fn channel_leads_config_default_model_overrides_execution_fallback() {
    let config = ChannelLeadsConfig {
        default_model: Some("sonnet".to_string()),
        ..ChannelLeadsConfig::default()
    };
    // default_model takes priority over execution fallback
    let model = config.model_for_channel_with_fallback("web", Some(ModelSize::Large));
    assert_eq!(model, "sonnet");
}

// ──────────────────────────────────────────────────────────────────────────────
// Profile pool tests (existing)
// ──────────────────────────────────────────────────────────────────────────────

#[test]
fn coworker_profiles_pool_parses() {
    let toml = r#"
[execution]
coworker_profiles = ["alice@example.com", "bob@example.com"]
"#;
    let config: FullProjectConfig = toml::from_str(toml).unwrap();
    assert_eq!(
        config.execution.coworker_profiles,
        Some(vec![
            "alice@example.com".to_string(),
            "bob@example.com".to_string()
        ])
    );
}

#[test]
fn coworker_profiles_empty_is_none() {
    let toml = r#"
[execution]
"#;
    let config: FullProjectConfig = toml::from_str(toml).unwrap();
    assert!(config.execution.coworker_profiles.is_none());
}

#[test]
fn reviewer_and_channel_lead_profiles_parse() {
    let toml = r#"
[execution]
reviewer_profiles = ["reviewer@example.com"]
channel_lead_profiles = ["lead@example.com", "lead2@example.com"]
"#;
    let config: FullProjectConfig = toml::from_str(toml).unwrap();
    assert_eq!(
        config.execution.reviewer_profiles,
        Some(vec!["reviewer@example.com".to_string()])
    );
    assert_eq!(
        config.execution.channel_lead_profiles,
        Some(vec![
            "lead@example.com".to_string(),
            "lead2@example.com".to_string()
        ])
    );
}

#[test]
fn profile_pool_fields_merge_correctly() {
    let base = ExecutionSection {
        coworker_profiles: Some(vec!["base@example.com".to_string()]),
        ..ExecutionSection::default()
    };
    let override_section = ExecutionSection {
        coworker_profiles: Some(vec!["override@example.com".to_string()]),
        ..ExecutionSection::default()
    };
    let merged = base.merge(&override_section);
    assert_eq!(
        merged.coworker_profiles,
        Some(vec!["override@example.com".to_string()])
    );
}
