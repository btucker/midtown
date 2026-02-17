//! Usage data fetching for provider-specific API rate limits.
//!
//! Midtown tracks provider/profile usage for Claude, Codex, and z.ai.
//! Each fetch uses a 5-minute profile-local cache. If a refresh fails and a
//! stale cache exists, stale values are returned with cache age metadata.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::auth::AuthProvider;

const CODEX_APP_SERVER_RPC_TIMEOUT: Duration = Duration::from_secs(3);
const CODEX_EXEC_JSON_TIMEOUT: Duration = Duration::from_secs(4);
const CODEX_STATUS_FALLBACK_TIMEOUT: Duration = Duration::from_secs(3);
const PROVIDER_STATUS_TIMEOUT: Duration = Duration::from_secs(8);

/// Usage data from provider rate-limit endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageData {
    /// Session (5-hour window) utilization percentage (0-100)
    pub session_util: f64,
    /// When the session window resets (None if unavailable)
    pub session_resets: Option<DateTime<Utc>>,
    /// Weekly (7-day window) utilization percentage (0-100)
    pub week_util: f64,
    /// When the weekly window resets (None if unavailable)
    pub week_resets: Option<DateTime<Utc>>,
    /// Account email from credentials/provider, if available
    pub account_email: Option<String>,
    /// Auth provider for this usage data
    #[serde(default)]
    pub provider: AuthProvider,
    /// Profile name for this usage data
    #[serde(default = "default_profile_name")]
    pub profile_name: String,
    /// Cache age in seconds when data is stale (None for fresh data)
    #[serde(default)]
    pub cache_age_seconds: Option<u64>,
    /// True when returned from stale cache after refresh failure
    #[serde(default)]
    pub cache_stale: bool,
}

fn default_profile_name() -> String {
    crate::auth::DEFAULT_PROFILE.to_string()
}

/// Raw Anthropic-compatible usage response shape.
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
    /// The OAuth access token.
    pub token: String,
    /// The account email (if present in the credential).
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexRateLimitWindow {
    #[serde(alias = "usedPercent")]
    used_percent: f64,
    #[serde(alias = "windowDurationMins")]
    window_minutes: Option<i64>,
    #[serde(alias = "resetsAt")]
    resets_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexRateLimitSnapshot {
    primary: Option<CodexRateLimitWindow>,
    secondary: Option<CodexRateLimitWindow>,
}

#[derive(Debug, Clone, Deserialize)]
struct CodexRateLimitsResponse {
    #[serde(default, alias = "rateLimits")]
    rate_limits: Option<CodexRateLimitSnapshot>,
    #[serde(default, alias = "rateLimitsByLimitId")]
    rate_limits_by_limit_id: Option<HashMap<String, CodexRateLimitSnapshot>>,
}

/// Fetch OAuth credentials from macOS Keychain for a specific profile.
///
/// The keychain entry name is `Claude Code-credentials-{hash}` where `{hash}`
/// is the first 8 hex chars of SHA256(CLAUDE_CONFIG_DIR path).
#[cfg(target_os = "macos")]
pub fn get_oauth_credentials_for_profile(profile: &str) -> Option<OAuthCredentials> {
    use sha2::{Digest, Sha256};

    let config_dir = crate::auth::profile_dir(profile);
    let config_dir_str = config_dir.to_string_lossy();

    let mut hasher = Sha256::new();
    hasher.update(config_dir_str.as_bytes());
    let hash = hasher.finalize();
    let hash_prefix = format!(
        "{:02x}{:02x}{:02x}{:02x}",
        hash[0], hash[1], hash[2], hash[3]
    );

    let service_name = format!("Claude Code-credentials-{}", hash_prefix);
    let output = Command::new("security")
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
        .map(ToString::to_string)?;
    let email = oauth
        .get("email")
        .and_then(|e| e.as_str())
        .map(ToString::to_string);

    Some(OAuthCredentials { token, email })
}

/// Fetch OAuth credentials from macOS Keychain for current Claude profile.
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

/// Fetch usage data from an Anthropic-compatible usage endpoint.
fn fetch_usage_from_url(
    url: &str,
    token: &str,
    account_email: Option<String>,
    provider: AuthProvider,
    profile_name: String,
) -> Option<UsageData> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("anthropic-beta", "oauth-2025-04-20")
        .timeout(Duration::from_secs(10))
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
        cache_age_seconds: None,
        cache_stale: false,
    })
}

/// Fetch usage data from Anthropic OAuth usage endpoint.
pub fn fetch_usage(
    token: &str,
    account_email: Option<String>,
    provider: AuthProvider,
    profile_name: String,
) -> Option<UsageData> {
    fetch_usage_from_url(
        "https://api.anthropic.com/api/oauth/usage",
        token,
        account_email,
        provider,
        profile_name,
    )
}

/// How long cached usage data is considered fresh (5 minutes).
const USAGE_CACHE_TTL: Duration = Duration::from_secs(300);

/// Path to usage cache for a provider/profile.
fn usage_cache_path(provider: AuthProvider, profile: &str) -> std::path::PathBuf {
    crate::auth::profile_dir_for(provider, profile).join("usage_cache.json")
}

/// Read cached usage data and cache age (fresh or stale).
fn read_usage_cache(provider: AuthProvider, profile: &str) -> Option<(UsageData, Duration)> {
    let path = usage_cache_path(provider, profile);
    let metadata = std::fs::metadata(&path).ok()?;
    let age = metadata.modified().ok()?.elapsed().ok()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    let mut data: UsageData = serde_json::from_str(&contents).ok()?;

    // Normalize cache metadata fields when reading from disk.
    data.cache_stale = false;
    data.cache_age_seconds = None;
    Some((data, age))
}

/// Write usage data to cache, dropping ephemeral stale markers.
fn write_usage_cache(provider: AuthProvider, profile: &str, data: &UsageData) {
    let path = usage_cache_path(provider, profile);
    let mut cached = data.clone();
    cached.cache_stale = false;
    cached.cache_age_seconds = None;
    if let Ok(json) = serde_json::to_string(&cached) {
        let _ = std::fs::write(path, json);
    }
}

fn mark_stale(mut data: UsageData, age: Duration) -> UsageData {
    data.cache_stale = true;
    data.cache_age_seconds = Some(age.as_secs());
    data
}

fn normalize_usage(mut data: UsageData, provider: AuthProvider, profile: &str) -> UsageData {
    data.provider = provider;
    data.profile_name = profile.to_string();
    data.cache_stale = false;
    data.cache_age_seconds = None;
    data
}

fn fetch_live_usage_for_profile(profile: &str, provider: AuthProvider) -> Option<UsageData> {
    match provider {
        AuthProvider::Claude => {
            let creds = get_oauth_credentials_for_profile(profile)?;
            fetch_usage(&creds.token, creds.email, provider, profile.to_string())
        }
        AuthProvider::Codex => fetch_codex_usage_for_profile(profile),
        AuthProvider::Zai => fetch_zai_usage_for_profile(profile),
    }
}

/// Fetch usage data for a provider/profile with stale-cache fallback.
pub fn fetch_usage_for_profile(profile: &str, provider: AuthProvider) -> Option<UsageData> {
    let cached = read_usage_cache(provider, profile);
    if let Some((cached_data, age)) = cached.as_ref()
        && *age <= USAGE_CACHE_TTL
    {
        return Some(normalize_usage(cached_data.clone(), provider, profile));
    }

    if let Some(fresh) = fetch_live_usage_for_profile(profile, provider) {
        let normalized = normalize_usage(fresh, provider, profile);
        write_usage_cache(provider, profile, &normalized);
        return Some(normalized);
    }

    cached
        .map(|(cached_data, age)| mark_stale(normalize_usage(cached_data, provider, profile), age))
}

/// Fetch usage data for current Claude profile from keychain credentials.
pub fn fetch_usage_with_credentials() -> Option<UsageData> {
    let creds = get_oauth_credentials()?;
    let current = crate::auth::current_profile();
    fetch_usage(&creds.token, creds.email, AuthProvider::Claude, current)
}

/// Fetch usage data for multiple provider/profile combinations.
pub fn fetch_multi_usage(profiles: &[(AuthProvider, String)]) -> Vec<UsageData> {
    profiles
        .iter()
        .filter_map(|(provider, profile)| fetch_usage_for_profile(profile, *provider))
        .collect()
}

fn unix_to_datetime(secs: Option<i64>) -> Option<DateTime<Utc>> {
    secs.and_then(|s| DateTime::from_timestamp(s, 0))
}

fn choose_windows(
    primary: Option<CodexRateLimitWindow>,
    secondary: Option<CodexRateLimitWindow>,
) -> Option<(CodexRateLimitWindow, CodexRateLimitWindow)> {
    let mut windows = Vec::new();
    if let Some(w) = primary {
        windows.push(w);
    }
    if let Some(w) = secondary {
        windows.push(w);
    }

    match windows.len() {
        0 => None,
        1 => {
            let only = windows[0].clone();
            Some((only.clone(), only))
        }
        _ => {
            let closest_idx = |target_minutes: i64| -> usize {
                windows
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, w)| {
                        w.window_minutes
                            .unwrap_or(target_minutes)
                            .abs_diff(target_minutes)
                    })
                    .map(|(idx, _)| idx)
                    .unwrap_or(0)
            };

            let session_idx = closest_idx(300);
            let mut week_idx = closest_idx(10080);
            if week_idx == session_idx {
                week_idx = if session_idx == 0 { 1 } else { 0 };
            }

            Some((windows[session_idx].clone(), windows[week_idx].clone()))
        }
    }
}

fn snapshot_to_usage(snapshot: CodexRateLimitSnapshot, profile: &str) -> Option<UsageData> {
    let (session, week) = choose_windows(snapshot.primary, snapshot.secondary)?;
    Some(UsageData {
        session_util: session.used_percent,
        session_resets: unix_to_datetime(session.resets_at),
        week_util: week.used_percent,
        week_resets: unix_to_datetime(week.resets_at),
        account_email: None,
        provider: AuthProvider::Codex,
        profile_name: profile.to_string(),
        cache_age_seconds: None,
        cache_stale: false,
    })
}

fn select_codex_snapshot(resp: CodexRateLimitsResponse) -> Option<CodexRateLimitSnapshot> {
    if let Some(snapshot) = resp.rate_limits {
        return Some(snapshot);
    }
    if let Some(mut by_limit) = resp.rate_limits_by_limit_id {
        if let Some(snapshot) = by_limit.remove("codex") {
            return Some(snapshot);
        }
        return by_limit.into_values().next();
    }
    None
}

fn fetch_codex_rate_limits_via_app_server(profile: &str) -> Option<CodexRateLimitSnapshot> {
    let profile_dir = crate::auth::profile_dir_for(AuthProvider::Codex, profile);
    let mut child = Command::new("codex")
        .args(["app-server", "--listen", "stdio://"])
        .env("CODEX_HOME", &profile_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });

    let init = serde_json::json!({
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "midtown", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": null
        }
    });
    let initialized = serde_json::json!({ "method": "initialized" });
    let read_limits = serde_json::json!({
        "id": 2,
        "method": "account/rateLimits/read",
        "params": null
    });

    let _ = writeln!(stdin, "{}", init);
    let _ = writeln!(stdin, "{}", initialized);
    let _ = writeln!(stdin, "{}", read_limits);
    let _ = stdin.flush();
    drop(stdin);

    let mut snapshot = None;
    let deadline = Instant::now() + CODEX_APP_SERVER_RPC_TIMEOUT;
    while Instant::now() < deadline {
        let timeout = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = rx.recv_timeout(timeout) else {
            break;
        };

        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        let is_target_id = value
            .get("id")
            .and_then(|id| id.as_i64())
            .is_some_and(|id| id == 2)
            || value
                .get("id")
                .and_then(|id| id.as_str())
                .is_some_and(|id| id == "2");
        if !is_target_id {
            continue;
        }

        if let Some(result) = value.get("result").cloned()
            && let Ok(parsed) = serde_json::from_value::<CodexRateLimitsResponse>(result)
        {
            snapshot = select_codex_snapshot(parsed);
        }
        break;
    }

    let _ = child.kill();
    let _ = child.wait();
    snapshot
}

fn run_command_with_timeout(
    mut command: Command,
    timeout: Duration,
) -> Option<std::process::Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().ok()?;

    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;

    let stdout_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut reader = BufReader::new(stdout);
        let _ = reader.read_to_end(&mut buf);
        buf
    });
    let stderr_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut reader = BufReader::new(stderr);
        let _ = reader.read_to_end(&mut buf);
        buf
    });

    let start = Instant::now();
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break Some(status);
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            // Intentionally do not join reader threads on timeout.
            // Some provider CLIs can leave descendant processes holding the
            // captured pipes open, which would block joins and defeat timeout.
            let _ = child.try_wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }?;

    let stdout = stdout_handle.join().ok()?;
    let stderr = stderr_handle.join().ok()?;

    Some(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn parse_codex_rate_limits_from_jsonl(text: &str) -> Option<CodexRateLimitSnapshot> {
    let mut snapshot = None;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };

        let Some(rate_limits) = value
            .get("rate_limits")
            .or_else(|| value.get("rateLimits"))
            .cloned()
        else {
            continue;
        };

        if let Ok(parsed) = serde_json::from_value::<CodexRateLimitSnapshot>(rate_limits) {
            snapshot = Some(parsed);
        }
    }

    snapshot
}

fn fetch_codex_rate_limits_via_exec_json(profile: &str) -> Option<CodexRateLimitSnapshot> {
    let profile_dir = crate::auth::profile_dir_for(AuthProvider::Codex, profile);
    let mut command = Command::new("codex");
    command
        .args(["exec", "--json", "--skip-git-repo-check", "/status"])
        .env("CODEX_HOME", &profile_dir);
    let output = run_command_with_timeout(command, CODEX_EXEC_JSON_TIMEOUT)?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Some(snapshot) = parse_codex_rate_limits_from_jsonl(&stdout) {
        return Some(snapshot);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_codex_rate_limits_from_jsonl(&stderr)
}

fn extract_percent_for_labels(text: &str, labels: &[&str]) -> Option<f64> {
    let Ok(re) = regex::Regex::new(r"([0-9]{1,3}(?:\.[0-9]+)?)\s*%") else {
        return None;
    };
    let lower = text.to_lowercase();
    for line in lower.lines() {
        if !labels.iter().any(|label| line.contains(label)) {
            continue;
        }
        if let Some(caps) = re.captures_iter(line).last()
            && let Some(raw) = caps.get(1).map(|m| m.as_str())
            && let Ok(v) = raw.parse::<f64>()
        {
            return Some(v);
        }
    }
    None
}

fn extract_first_two_percents(text: &str) -> (Option<f64>, Option<f64>) {
    let Ok(re) = regex::Regex::new(r"([0-9]{1,3}(?:\.[0-9]+)?)\s*%") else {
        return (None, None);
    };
    let mut vals = re
        .captures_iter(text)
        .filter_map(|caps| caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok()));
    (vals.next(), vals.next())
}

fn extract_reset_rfc3339_for_labels(text: &str, labels: &[&str]) -> Option<DateTime<Utc>> {
    let Ok(re) = regex::Regex::new(
        r"([12][0-9]{3}-[01][0-9]-[0-3][0-9]T[0-2][0-9]:[0-5][0-9](?::[0-5][0-9])?(?:\.[0-9]+)?(?:Z|[+\-][0-2][0-9]:[0-5][0-9]))",
    ) else {
        return None;
    };

    let lower = text.to_lowercase();
    for line in lower.lines() {
        if !labels.iter().any(|label| line.contains(label)) {
            continue;
        }
        if let Some(caps) = re.captures(line)
            && let Some(raw) = caps.get(1).map(|m| m.as_str())
            && let Ok(ts) = DateTime::parse_from_rfc3339(raw)
        {
            return Some(ts.with_timezone(&Utc));
        }
    }
    None
}

fn parse_status_text_to_usage(
    text: &str,
    provider: AuthProvider,
    profile: &str,
) -> Option<UsageData> {
    let session_labels = ["session", "5h", "5-hour", "five hour", "primary"];
    let week_labels = ["week", "7d", "7-day", "seven day", "secondary"];

    let mut session_util = extract_percent_for_labels(text, &session_labels);
    let mut week_util = extract_percent_for_labels(text, &week_labels);

    if session_util.is_none() || week_util.is_none() {
        let (first, second) = extract_first_two_percents(text);
        if session_util.is_none() {
            session_util = first;
        }
        if week_util.is_none() {
            week_util = second.or(session_util);
        }
    }

    let session_util = session_util?;
    let week_util = week_util.unwrap_or(session_util);

    Some(UsageData {
        session_util,
        session_resets: extract_reset_rfc3339_for_labels(text, &session_labels),
        week_util,
        week_resets: extract_reset_rfc3339_for_labels(text, &week_labels),
        account_email: None,
        provider,
        profile_name: profile.to_string(),
        cache_age_seconds: None,
        cache_stale: false,
    })
}

fn fetch_codex_usage_for_profile(profile: &str) -> Option<UsageData> {
    if let Some(snapshot) = fetch_codex_rate_limits_via_app_server(profile) {
        return snapshot_to_usage(snapshot, profile);
    }

    if let Some(snapshot) = fetch_codex_rate_limits_via_exec_json(profile) {
        return snapshot_to_usage(snapshot, profile);
    }

    let profile_dir = crate::auth::profile_dir_for(AuthProvider::Codex, profile);
    let mut command = Command::new("codex");
    command
        .args(["exec", "--skip-git-repo-check", "/status"])
        .env("CODEX_HOME", &profile_dir);
    let output = run_command_with_timeout(command, CODEX_STATUS_FALLBACK_TIMEOUT)?;

    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_status_text_to_usage(&text, AuthProvider::Codex, profile)
}

fn push_unique_url(urls: &mut Vec<String>, url: String) {
    if !urls.contains(&url) {
        urls.push(url);
    }
}

fn zai_monitor_usage_urls(base_url: &str) -> Vec<String> {
    let base = base_url.trim_end_matches('/');
    let mut urls = Vec::new();

    if let Some(prefix) = base.strip_suffix("/api/anthropic") {
        push_unique_url(
            &mut urls,
            format!("{}/api/monitor/usage/quota/limit", prefix),
        );
    }

    if let Ok(parsed) = reqwest::Url::parse(base)
        && let Some(host) = parsed.host_str()
    {
        let origin = if let Some(port) = parsed.port() {
            format!("{}://{}:{}", parsed.scheme(), host, port)
        } else {
            format!("{}://{}", parsed.scheme(), host)
        };
        push_unique_url(
            &mut urls,
            format!("{}/api/monitor/usage/quota/limit", origin),
        );
    }

    push_unique_url(&mut urls, format!("{}/api/monitor/usage/quota/limit", base));
    urls
}

fn zai_usage_urls(base_url: &str) -> Vec<String> {
    let base = base_url.trim_end_matches('/');
    let mut urls = Vec::new();
    if let Some(prefix) = base.strip_suffix("/api/anthropic") {
        urls.push(format!("{}/api/oauth/usage", prefix));
    }
    urls.push(format!("{}/api/oauth/usage", base));
    urls.push(format!("{}/oauth/usage", base));
    urls
}

fn fetch_zai_usage_via_http(api_key: &str, base_url: &str, profile: &str) -> Option<UsageData> {
    for url in zai_usage_urls(base_url) {
        if let Some(data) =
            fetch_usage_from_url(&url, api_key, None, AuthProvider::Zai, profile.to_string())
        {
            return Some(data);
        }
    }
    None
}

#[derive(Clone)]
struct JsonUsageCandidate {
    util: f64,
    reset: Option<DateTime<Utc>>,
    duration_minutes: Option<i64>,
    path: String,
}

fn normalize_percent(value: f64) -> Option<f64> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    if value <= 1.0 {
        return Some(value * 100.0);
    }
    if value <= 100.0 {
        return Some(value);
    }
    None
}

fn numeric_from_json(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.parse::<f64>().ok(),
        _ => None,
    }
}

fn extract_number_from_map(
    map: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<f64> {
    keys.iter()
        .filter_map(|key| map.get(*key))
        .find_map(numeric_from_json)
}

fn extract_percent_from_map(map: &serde_json::Map<String, serde_json::Value>) -> Option<f64> {
    let percent_keys = [
        "utilization",
        "used_percent",
        "usedPercent",
        "percent",
        "pct",
        "usage_percent",
        "usagePercent",
        "ratio",
        "rate",
    ];
    if let Some(raw) = extract_number_from_map(map, &percent_keys)
        && let Some(normalized) = normalize_percent(raw)
    {
        return Some(normalized);
    }

    let used_keys = [
        "used",
        "usage",
        "consumed",
        "current",
        "quota_used",
        "quotaUsed",
        "spent",
    ];
    let limit_keys = [
        "limit",
        "quota",
        "total",
        "max",
        "capacity",
        "quota_limit",
        "quotaLimit",
    ];
    let used = extract_number_from_map(map, &used_keys);
    let limit = extract_number_from_map(map, &limit_keys);
    if let (Some(used), Some(limit)) = (used, limit)
        && limit > 0.0
    {
        return normalize_percent((used / limit) * 100.0);
    }

    None
}

fn parse_datetime_json(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    match value {
        serde_json::Value::String(s) => DateTime::parse_from_rfc3339(s)
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        serde_json::Value::Number(n) => {
            let raw = n.as_i64()?;
            if raw > 1_000_000_000_000 {
                DateTime::from_timestamp_millis(raw)
            } else {
                DateTime::from_timestamp(raw, 0)
            }
        }
        _ => None,
    }
}

fn extract_reset_from_map(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Option<DateTime<Utc>> {
    let keys = [
        "resets_at",
        "reset_at",
        "resetsAt",
        "resetAt",
        "next_reset_at",
        "nextResetAt",
        "expires_at",
        "expiresAt",
    ];
    keys.iter()
        .filter_map(|key| map.get(*key))
        .find_map(parse_datetime_json)
}

fn extract_duration_minutes_from_map(
    map: &serde_json::Map<String, serde_json::Value>,
) -> Option<i64> {
    let minute_keys = [
        "window_minutes",
        "windowMinutes",
        "window_mins",
        "windowMins",
        "windowDurationMins",
        "duration_minutes",
    ];
    if let Some(v) = extract_number_from_map(map, &minute_keys)
        && v.is_finite()
    {
        return Some(v.round() as i64);
    }

    let second_keys = ["window_seconds", "windowSeconds", "duration_seconds"];
    if let Some(v) = extract_number_from_map(map, &second_keys)
        && v.is_finite()
    {
        return Some((v / 60.0).round() as i64);
    }

    None
}

fn collect_json_usage_candidates(
    value: &serde_json::Value,
    path: &str,
    out: &mut Vec<JsonUsageCandidate>,
) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(util) = extract_percent_from_map(map) {
                out.push(JsonUsageCandidate {
                    util,
                    reset: extract_reset_from_map(map),
                    duration_minutes: extract_duration_minutes_from_map(map),
                    path: path.to_ascii_lowercase(),
                });
            }

            for (key, child) in map {
                let child_path = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{}.{}", path, key)
                };
                collect_json_usage_candidates(child, &child_path, out);
            }
        }
        serde_json::Value::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                let child_path = if path.is_empty() {
                    idx.to_string()
                } else {
                    format!("{}.{}", path, idx)
                };
                collect_json_usage_candidates(child, &child_path, out);
            }
        }
        _ => {}
    }
}

fn find_candidate_by_labels(
    candidates: &[JsonUsageCandidate],
    labels: &[&str],
    exclude: Option<usize>,
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .find(|(idx, candidate)| {
            Some(*idx) != exclude && labels.iter().any(|label| candidate.path.contains(label))
        })
        .map(|(idx, _)| idx)
}

fn closest_duration_candidate(
    candidates: &[JsonUsageCandidate],
    target_minutes: i64,
    exclude: Option<usize>,
) -> Option<usize> {
    candidates
        .iter()
        .enumerate()
        .filter(|(idx, candidate)| Some(*idx) != exclude && candidate.duration_minutes.is_some())
        .min_by_key(|(_, candidate)| {
            candidate
                .duration_minutes
                .unwrap_or(target_minutes)
                .abs_diff(target_minutes)
        })
        .map(|(idx, _)| idx)
}

fn parse_zai_monitor_usage_response(body: &str, profile: &str) -> Option<UsageData> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut candidates = Vec::new();
    collect_json_usage_candidates(&parsed, "", &mut candidates);
    if candidates.is_empty() {
        return None;
    }

    let session_labels = ["session", "5h", "five", "hour", "primary", "short"];
    let week_labels = ["week", "7d", "seven", "day", "secondary", "long"];

    let session_idx = find_candidate_by_labels(&candidates, &session_labels, None)
        .or_else(|| closest_duration_candidate(&candidates, 300, None))
        .unwrap_or(0);

    let week_idx = find_candidate_by_labels(&candidates, &week_labels, Some(session_idx))
        .or_else(|| closest_duration_candidate(&candidates, 10080, Some(session_idx)))
        .unwrap_or(session_idx);

    let session = &candidates[session_idx];
    let week = &candidates[week_idx];
    Some(UsageData {
        session_util: session.util,
        session_resets: session.reset,
        week_util: week.util,
        week_resets: week.reset,
        account_email: None,
        provider: AuthProvider::Zai,
        profile_name: profile.to_string(),
        cache_age_seconds: None,
        cache_stale: false,
    })
}

fn fetch_zai_usage_via_monitor_endpoint(
    api_key: &str,
    base_url: &str,
    profile: &str,
) -> Option<UsageData> {
    let client = reqwest::blocking::Client::new();
    for url in zai_monitor_usage_urls(base_url) {
        let Ok(resp) = client
            .get(&url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("x-api-key", api_key)
            .header("api-key", api_key)
            .header("anthropic-api-key", api_key)
            .timeout(Duration::from_secs(10))
            .send()
        else {
            continue;
        };

        if !resp.status().is_success() {
            continue;
        }

        let Ok(body) = resp.text() else {
            continue;
        };
        if let Some(data) = parse_zai_monitor_usage_response(&body, profile) {
            return Some(data);
        }
        if let Some(data) = parse_status_text_to_usage(&body, AuthProvider::Zai, profile) {
            return Some(data);
        }
    }
    None
}

fn run_zai_status_command(api_key: &str, base_url: &str) -> Option<String> {
    let mut command = Command::new("claude");
    command
        .args(["-p", "--output-format", "json", "/status"])
        .env("ANTHROPIC_AUTH_TOKEN", api_key)
        .env("ANTHROPIC_BASE_URL", base_url);
    let output = run_command_with_timeout(command, PROVIDER_STATUS_TIMEOUT)?;

    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if combined.trim().is_empty() {
        None
    } else {
        Some(combined)
    }
}

fn fetch_zai_usage_for_profile(profile: &str) -> Option<UsageData> {
    let profile_dir = crate::auth::profile_dir_for(AuthProvider::Zai, profile);
    let Ok((api_key, base_url)) = crate::launch::zai_env_vars(&profile_dir) else {
        return None;
    };

    if let Some(data) = fetch_zai_usage_via_monitor_endpoint(&api_key, &base_url, profile) {
        return Some(data);
    }

    if let Some(data) = fetch_zai_usage_via_http(&api_key, &base_url, profile) {
        return Some(data);
    }

    let status = run_zai_status_command(&api_key, &base_url)?;
    parse_status_text_to_usage(&status, AuthProvider::Zai, profile)
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
}
