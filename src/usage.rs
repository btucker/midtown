//! Usage data fetching for Claude API rate limits.
//!
//! Fetches session (5-hour) and weekly (7-day) utilization data from
//! the Anthropic API using the OAuth token from macOS Keychain.
//! Used by both the TUI and the web UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Usage data from the Anthropic API.
#[derive(Debug, Clone, Serialize)]
pub struct UsageData {
    /// Session (5-hour window) utilization percentage (0-100)
    pub session_util: f64,
    /// When the session window resets
    pub session_resets: DateTime<Utc>,
    /// Weekly (7-day window) utilization percentage (0-100)
    pub week_util: f64,
    /// When the weekly window resets
    pub week_resets: DateTime<Utc>,
    /// Account email from OAuth credentials (if available)
    pub account_email: Option<String>,
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

/// OAuth credentials extracted from macOS Keychain.
pub struct OAuthCredentials {
    /// The OAuth access token
    pub token: String,
    /// The account email (if present in the credential)
    pub email: Option<String>,
}

/// Fetch OAuth credentials from macOS Keychain for a specific auth profile.
///
/// The keychain entry name is `Claude Code-credentials-{hash}` where `{hash}` is the
/// first 8 hex characters of SHA256(CLAUDE_CONFIG_DIR path).
#[cfg(target_os = "macos")]
pub fn get_oauth_credentials_for_profile(profile: &str) -> Option<OAuthCredentials> {
    use sha2::{Digest, Sha256};

    let config_dir = crate::auth::profile_dir(profile);
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

    let oauth = cred.get("claudeAiOauth")?;

    let token = oauth
        .get("accessToken")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())?;

    let email = oauth
        .get("email")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string());

    Some(OAuthCredentials { token, email })
}

/// Fetch OAuth credentials from macOS Keychain for the current midtown auth profile.
///
/// The keychain entry name is `Claude Code-credentials-{hash}` where `{hash}` is the
/// first 8 hex characters of SHA256(CLAUDE_CONFIG_DIR path).
#[cfg(target_os = "macos")]
pub fn get_oauth_credentials() -> Option<OAuthCredentials> {
    let current_profile = crate::auth::current_profile();
    get_oauth_credentials_for_profile(&current_profile)
}

#[cfg(not(target_os = "macos"))]
pub fn get_oauth_credentials_for_profile(_profile: &str) -> Option<OAuthCredentials> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn get_oauth_credentials() -> Option<OAuthCredentials> {
    None
}

/// Fetch usage data from the Anthropic API.
pub fn fetch_usage(token: &str, account_email: Option<String>) -> Option<UsageData> {
    // Use blocking reqwest since this runs on a blocking thread pool
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
        account_email,
    })
}

/// Fetch usage data for a specific profile using credentials from the macOS Keychain.
///
/// Combines credential retrieval and API fetch into a single call.
/// Returns `None` if credentials are unavailable or the API call fails.
pub fn fetch_usage_for_profile(profile: &str) -> Option<UsageData> {
    let creds = get_oauth_credentials_for_profile(profile)?;
    fetch_usage(&creds.token, creds.email)
}

/// Fetch usage data using credentials from the macOS Keychain.
///
/// Combines credential retrieval and API fetch into a single call.
/// Returns `None` if credentials are unavailable or the API call fails.
pub fn fetch_usage_with_credentials() -> Option<UsageData> {
    let creds = get_oauth_credentials()?;
    fetch_usage(&creds.token, creds.email)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_data_serialization() {
        let data = UsageData {
            session_util: 43.2,
            session_resets: DateTime::parse_from_rfc3339("2026-02-05T22:59:00Z")
                .unwrap()
                .with_timezone(&Utc),
            week_util: 52.1,
            week_resets: DateTime::parse_from_rfc3339("2026-02-11T15:59:00Z")
                .unwrap()
                .with_timezone(&Utc),
            account_email: Some("test@example.com".to_string()),
        };

        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("43.2"));
        assert!(json.contains("52.1"));
        assert!(json.contains("test@example.com"));
    }
}
