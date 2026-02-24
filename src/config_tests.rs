use super::*;

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
