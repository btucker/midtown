//! Usage data fetching for Claude API rate limits.
//!
//! Fetches session (5-hour) and weekly (7-day) utilization data from
//! the Anthropic API using the OAuth token from macOS Keychain.
//! Used by both the TUI and the web UI.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::auth::AuthProvider;

/// Usage data from the Anthropic API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageData {
    /// Session (5-hour window) utilization percentage (0-100)
    pub session_util: f64,
    /// When the session window resets (None if no active session)
    pub session_resets: Option<DateTime<Utc>>,
    /// Weekly (7-day window) utilization percentage (0-100)
    pub week_util: f64,
    /// When the weekly window resets (None if no active window)
    pub week_resets: Option<DateTime<Utc>>,
    /// Account email from OAuth credentials (if available)
    pub account_email: Option<String>,
    /// Auth provider for this usage data
    #[serde(default)]
    pub provider: AuthProvider,
    /// Profile name for this usage data
    #[serde(default = "default_profile_name")]
    pub profile_name: String,
}

fn default_profile_name() -> String {
    crate::auth::DEFAULT_PROFILE.to_string()
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
    resets_at: Option<String>,
}

/// OAuth credentials extracted from macOS Keychain.
pub struct OAuthCredentials {
    /// The OAuth access token
    pub token: String,
    /// The account email (if present in the credential)
    pub email: Option<String>,
}

/// Fetch OAuth credentials from macOS Keychain for a specific profile.
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
    let current = crate::auth::current_profile();
    get_oauth_credentials_for_profile(&current)
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
pub fn fetch_usage(
    token: &str,
    account_email: Option<String>,
    provider: AuthProvider,
    profile_name: String,
) -> Option<UsageData> {
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

    let (session_util, session_resets) = match data.five_hour {
        Some(w) => {
            let resets = w
                .resets_at
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            (w.utilization, resets)
        }
        None => (0.0, None),
    };

    let (week_util, week_resets) = match data.seven_day {
        Some(w) => {
            let resets = w
                .resets_at
                .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
                .map(|dt| dt.with_timezone(&Utc));
            (w.utilization, resets)
        }
        None => (0.0, None),
    };

    Some(UsageData {
        session_util,
        session_resets,
        week_util,
        week_resets,
        account_email,
        provider,
        profile_name,
    })
}

/// How long cached usage data is considered fresh (5 minutes).
const USAGE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

/// Path to the usage cache file for a profile.
fn usage_cache_path(profile: &str) -> std::path::PathBuf {
    crate::auth::profile_dir(profile).join("usage_cache.json")
}

/// Read cached usage data if it exists and is fresh.
fn read_usage_cache(profile: &str) -> Option<UsageData> {
    let path = usage_cache_path(profile);
    let metadata = std::fs::metadata(&path).ok()?;
    let age = metadata.modified().ok()?.elapsed().ok()?;
    if age > USAGE_CACHE_TTL {
        return None;
    }
    let contents = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&contents).ok()
}

/// Write usage data to the cache file.
fn write_usage_cache(profile: &str, data: &UsageData) {
    let path = usage_cache_path(profile);
    if let Ok(json) = serde_json::to_string(data) {
        let _ = std::fs::write(path, json);
    }
}

/// Fetch usage data for a profile, using a 60-second file cache.
///
/// Returns cached data if fresh, otherwise fetches from the Anthropic API
/// and updates the cache.
pub fn fetch_usage_for_profile(profile: &str, provider: AuthProvider) -> Option<UsageData> {
    if let Some(cached) = read_usage_cache(profile) {
        return Some(cached);
    }
    let creds = get_oauth_credentials_for_profile(profile)?;
    let data = fetch_usage(&creds.token, creds.email, provider, profile.to_string())?;
    write_usage_cache(profile, &data);
    Some(data)
}

/// Fetch usage data using credentials from the macOS Keychain.
///
/// Combines credential retrieval and API fetch into a single call.
/// Returns `None` if credentials are unavailable or the API call fails.
pub fn fetch_usage_with_credentials() -> Option<UsageData> {
    let creds = get_oauth_credentials()?;
    let current = crate::auth::current_profile();
    fetch_usage(&creds.token, creds.email, AuthProvider::Claude, current)
}

/// Fetch usage data for multiple provider/profile combinations.
///
/// Each entry in the input is (provider, profile_name). Returns a Vec of
/// UsageData, one per profile that successfully fetched. Profiles without
/// credentials or with API failures are skipped (no entry in the result).
///
/// For non-Claude providers (e.g., z.ai), this currently returns None since
/// they don't have usage APIs yet.
pub fn fetch_multi_usage(profiles: &[(AuthProvider, String)]) -> Vec<UsageData> {
    profiles
        .iter()
        .filter_map(|(provider, profile)| {
            // Only Claude supports usage API currently
            if *provider != AuthProvider::Claude {
                return None;
            }
            fetch_usage_for_profile(profile, *provider)
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
        };

        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("43.2"));
        assert!(json.contains("52.1"));
        assert!(json.contains("test@example.com"));
        assert!(json.contains("claude"));
        assert!(json.contains("test-profile"));
    }

    /// Test that fetch_usage correctly parses API responses where resets_at is null.
    ///
    /// The Anthropic usage API returns `resets_at: null` when utilization is 0%
    /// (no active session window). Previously, `UsageWindow.resets_at` was a
    /// non-optional `String`, causing the entire response deserialization to fail.
    #[test]
    fn test_usage_response_with_null_resets_at() {
        // This is the actual shape returned by the API when an account has
        // 0% session utilization — resets_at is null, not absent.
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
        assert_eq!(
            data.seven_day.as_ref().unwrap().resets_at.as_deref(),
            Some("2026-02-11T15:59:00Z")
        );
    }

    /// Test that both windows can have null resets_at.
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

    /// Test that missing windows (null at top level) are handled.
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

    /// Test UsageData round-trip serialization with None resets.
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
}
