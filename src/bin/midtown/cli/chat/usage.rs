//! Usage data fetching for Claude API rate limits.
//!
//! Fetches session (5-hour) and weekly (7-day) utilization data from
//! the Anthropic API using the OAuth token from macOS Keychain.

use chrono::{DateTime, Utc};
use serde::Deserialize;

/// Usage data from the Anthropic API.
#[derive(Debug, Clone)]
pub struct UsageData {
    /// Session (5-hour window) utilization percentage (0-100)
    pub session_util: f64,
    /// When the session window resets
    pub session_resets: DateTime<Utc>,
    /// Weekly (7-day window) utilization percentage (0-100)
    pub week_util: f64,
    /// When the weekly window resets
    pub week_resets: DateTime<Utc>,
}

/// Raw API response shape.
#[derive(Deserialize)]
struct UsageResponse {
    five_hour: Option<UsageWindow>,
    seven_day: Option<UsageWindow>,
}

#[derive(Deserialize)]
struct UsageWindow {
    utilization: f64,
    resets_at: String,
}

/// Fetch the OAuth access token from macOS Keychain for the current midtown auth profile.
///
/// The keychain entry name is `Claude Code-credentials-{hash}` where `{hash}` is the
/// first 8 hex characters of SHA256(CLAUDE_CONFIG_DIR path).
#[cfg(target_os = "macos")]
pub fn get_oauth_token() -> Option<String> {
    use sha2::{Digest, Sha256};

    let config_dir = midtown::auth::current_profile_dir();
    let config_dir_str = config_dir.to_string_lossy();

    // Derive the keychain service name hash
    let mut hasher = Sha256::new();
    hasher.update(config_dir_str.as_bytes());
    let hash = hasher.finalize();
    let hash_prefix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3]
    );

    let service_name = format!("Claude Code-credentials-{}", hash_prefix);

    // Use macOS `security` CLI to retrieve the credential
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", &service_name, "-w"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let cred_json = String::from_utf8(output.stdout).ok()?;
    let cred: serde_json::Value = serde_json::from_str(cred_json.trim()).ok()?;

    cred.get("claudeAiOauth")
        .and_then(|oauth| oauth.get("accessToken"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn get_oauth_token() -> Option<String> {
    None
}

/// Fetch usage data from the Anthropic API.
pub fn fetch_usage(token: &str) -> Option<UsageData> {
    // Use blocking reqwest since we run this on a background thread
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .ok()?;

    if !resp.status().is_success() {
        return None;
    }

    let data: UsageResponse = resp.json().ok()?;

    let five_hour = data.five_hour?;
    let seven_day = data.seven_day?;

    let session_resets = DateTime::parse_from_rfc3339(&five_hour.resets_at)
        .ok()?
        .with_timezone(&Utc);
    let week_resets = DateTime::parse_from_rfc3339(&seven_day.resets_at)
        .ok()?
        .with_timezone(&Utc);

    Some(UsageData {
        session_util: five_hour.utilization,
        session_resets,
        week_util: seven_day.utilization,
        week_resets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_response_parsing() {
        let json = r#"{
            "five_hour": {
                "utilization": 43.2,
                "resets_at": "2026-02-05T22:59:00Z"
            },
            "seven_day": {
                "utilization": 52.1,
                "resets_at": "2026-02-11T15:59:00Z"
            },
            "seven_day_oauth_apps": null,
            "seven_day_opus": null,
            "extra_usage": {
                "is_enabled": false,
                "monthly_limit": null,
                "used_credits": null,
                "utilization": null
            }
        }"#;

        let resp: UsageResponse = serde_json::from_str(json).unwrap();
        let five = resp.five_hour.unwrap();
        assert!((five.utilization - 43.2).abs() < f64::EPSILON);
        assert_eq!(five.resets_at, "2026-02-05T22:59:00Z");

        let seven = resp.seven_day.unwrap();
        assert!((seven.utilization - 52.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usage_response_missing_fields() {
        let json = r#"{
            "five_hour": null,
            "seven_day": null
        }"#;

        let resp: UsageResponse = serde_json::from_str(json).unwrap();
        assert!(resp.five_hour.is_none());
        assert!(resp.seven_day.is_none());
    }
}
