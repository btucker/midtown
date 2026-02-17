use super::*;

#[test]
fn test_usage_data_serialization() {
    let data = UsageData {
        session_util: 43.2,
        session_resets: Some(
            DateTime::parse_from_rfc3339("2026-02-05T22:59:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
        week_util: 52.1,
        week_resets: Some(
            DateTime::parse_from_rfc3339("2026-02-11T15:59:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
        account_email: Some("test@example.com".to_string()),
        provider: AuthProvider::Claude,
        profile_name: "test-profile".to_string(),
        cache_age_seconds: None,
        cache_stale: false,
    };

    let json = serde_json::to_string(&data).unwrap();
    assert!(json.contains("43.2"));
    assert!(json.contains("52.1"));
    assert!(json.contains("test@example.com"));
    assert!(json.contains("claude"));
    assert!(json.contains("test-profile"));
}

#[test]
fn test_usage_response_with_null_resets_at() {
    let json = r#"{
        "five_hour": {
            "utilization": 0.0,
            "resets_at": null
        },
        "seven_day": {
            "utilization": 12.5,
            "resets_at": "2026-02-11T15:59:00Z"
        }
    }"#;

    let data: UsageResponse = serde_json::from_str(json).unwrap();
    assert_eq!(data.five_hour.as_ref().unwrap().utilization, 0.0);
    assert!(data.five_hour.as_ref().unwrap().resets_at.is_none());
    assert_eq!(data.seven_day.as_ref().unwrap().utilization, 12.5);
}

#[test]
fn test_usage_response_both_null_resets() {
    let json = r#"{
        "five_hour": {
            "utilization": 0.0,
            "resets_at": null
        },
        "seven_day": {
            "utilization": 0.0,
            "resets_at": null
        }
    }"#;

    let data: UsageResponse = serde_json::from_str(json).unwrap();
    assert!(data.five_hour.as_ref().unwrap().resets_at.is_none());
    assert!(data.seven_day.as_ref().unwrap().resets_at.is_none());
}

#[test]
fn test_usage_response_missing_windows() {
    let json = r#"{
        "five_hour": null,
        "seven_day": null
    }"#;

    let data: UsageResponse = serde_json::from_str(json).unwrap();
    assert!(data.five_hour.is_none());
    assert!(data.seven_day.is_none());
}

#[test]
fn test_usage_data_serialization_with_none_resets() {
    let data = UsageData {
        session_util: 0.0,
        session_resets: None,
        week_util: 12.5,
        week_resets: Some(
            DateTime::parse_from_rfc3339("2026-02-11T15:59:00Z")
                .unwrap()
                .with_timezone(&Utc),
        ),
        account_email: Some("test@example.com".to_string()),
        provider: AuthProvider::Claude,
        profile_name: "test-profile".to_string(),
        cache_age_seconds: None,
        cache_stale: false,
    };

    let json = serde_json::to_string(&data).unwrap();
    let roundtrip: UsageData = serde_json::from_str(&json).unwrap();

    assert_eq!(roundtrip.session_util, 0.0);
    assert!(roundtrip.session_resets.is_none());
    assert_eq!(roundtrip.week_util, 12.5);
    assert!(roundtrip.week_resets.is_some());
    assert_eq!(roundtrip.provider, AuthProvider::Claude);
    assert_eq!(roundtrip.profile_name, "test-profile");
}

#[test]
fn test_fetch_multi_usage_mixed_providers_no_panic() {
    let profiles = vec![
        (AuthProvider::Claude, "claude-profile".to_string()),
        (AuthProvider::Codex, "codex-profile".to_string()),
        (AuthProvider::Zai, "zai-profile".to_string()),
    ];
    let _ = fetch_multi_usage(&profiles);
}

#[test]
fn test_fetch_multi_usage_empty_input() {
    let result = fetch_multi_usage(&[]);
    assert!(result.is_empty());
}

#[test]
fn test_codex_snapshot_to_usage_prefers_5h_and_7d_windows() {
    let snapshot = CodexRateLimitSnapshot {
        primary: Some(CodexRateLimitWindow {
            used_percent: 11.0,
            window_minutes: Some(300),
            resets_at: Some(1_700_000_000),
        }),
        secondary: Some(CodexRateLimitWindow {
            used_percent: 44.0,
            window_minutes: Some(10080),
            resets_at: Some(1_700_500_000),
        }),
    };

    let usage = snapshot_to_usage(snapshot, "work@example.com").unwrap();
    assert_eq!(usage.session_util, 11.0);
    assert_eq!(usage.week_util, 44.0);
}

#[test]
fn test_parse_status_text_fallback_with_two_percents() {
    let text = "Provider status: 12% used now, 34% weekly.";
    let usage = parse_status_text_to_usage(text, AuthProvider::Zai, "zai-profile").unwrap();
    assert_eq!(usage.session_util, 12.0);
    assert_eq!(usage.week_util, 34.0);
}

#[test]
fn test_zai_monitor_usage_urls_from_anthropic_base() {
    let urls = zai_monitor_usage_urls("https://api.z.ai/api/anthropic");
    assert!(
        urls.iter()
            .any(|u| u == "https://api.z.ai/api/monitor/usage/quota/limit")
    );
}

#[test]
fn test_parse_zai_monitor_usage_response_labeled_windows() {
    let body = r#"{
        "data": {
            "session_limit": { "usedPercent": 12.5, "resetAt": "2026-02-17T20:00:00Z" },
            "week_limit": { "usedPercent": 44.0, "resetAt": "2026-02-21T20:00:00Z" }
        }
    }"#;

    let usage = parse_zai_monitor_usage_response(body, "zai-profile").unwrap();
    assert_eq!(usage.session_util, 12.5);
    assert_eq!(usage.week_util, 44.0);
    assert!(usage.session_resets.is_some());
    assert!(usage.week_resets.is_some());
}

#[test]
fn test_parse_zai_monitor_usage_response_used_limit_pair() {
    let body = r#"{
        "data": {
            "limits": [
                { "window_minutes": 300, "used": 25, "limit": 100 },
                { "window_minutes": 10080, "used": 150, "limit": 600 }
            ]
        }
    }"#;

    let usage = parse_zai_monitor_usage_response(body, "zai-profile").unwrap();
    assert_eq!(usage.session_util, 25.0);
    assert_eq!(usage.week_util, 25.0);
}
