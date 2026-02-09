//! GitHub API rate limit tracking and adaptive throttling.
//!
//! Monitors both GraphQL and REST API quotas to prevent exhaustion.
//! The daemon uses this to adaptively reduce polling frequency when quotas run low.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::process::Command;
use tracing::{debug, warn};

/// GitHub API rate limit state for both GraphQL and REST quotas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitHubRateLimit {
    /// GraphQL API quota (5000/hr, separate from REST)
    pub graphql: QuotaState,
    /// REST API quota (5000/hr, separate from GraphQL)
    pub core: QuotaState,
    /// When this data was last fetched
    pub last_updated: DateTime<Utc>,
}

/// Rate limit state for a single quota (GraphQL or REST).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaState {
    /// Maximum requests allowed in the time window
    pub limit: u32,
    /// Number of requests already consumed
    pub used: u32,
    /// Number of requests remaining
    pub remaining: u32,
    /// When the quota resets (Unix timestamp)
    pub reset: i64,
}

impl QuotaState {
    /// Returns the percentage of quota remaining (0.0 to 1.0).
    pub fn remaining_pct(&self) -> f64 {
        if self.limit == 0 {
            return 1.0;
        }
        self.remaining as f64 / self.limit as f64
    }

    /// Returns when the quota resets as a DateTime.
    pub fn reset_time(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.reset, 0).unwrap_or_else(Utc::now)
    }

    /// Returns true if the quota is critically low (< 5%).
    pub fn is_critical(&self) -> bool {
        self.remaining_pct() < 0.05
    }

    /// Returns true if the quota is low (< 20%).
    pub fn is_low(&self) -> bool {
        self.remaining_pct() < 0.20
    }
}

impl Default for GitHubRateLimit {
    fn default() -> Self {
        Self {
            graphql: QuotaState {
                limit: 5000,
                used: 0,
                remaining: 5000,
                reset: 0,
            },
            core: QuotaState {
                limit: 5000,
                used: 0,
                remaining: 5000,
                reset: 0,
            },
            last_updated: Utc::now(),
        }
    }
}

/// API response from `gh api rate_limit`.
#[derive(Debug, Deserialize)]
struct RateLimitResponse {
    resources: Resources,
}

#[derive(Debug, Deserialize)]
struct Resources {
    graphql: QuotaResponse,
    core: QuotaResponse,
}

#[derive(Debug, Deserialize)]
struct QuotaResponse {
    limit: u32,
    used: u32,
    remaining: u32,
    reset: i64,
}

impl GitHubRateLimit {
    /// Fetch current rate limit state from the GitHub API.
    ///
    /// Returns `None` if the API call fails or cannot be parsed.
    pub fn fetch() -> Option<Self> {
        let output = Command::new("gh")
            .args(["api", "rate_limit"])
            .output();

        let output = match output {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                warn!(
                    "gh api rate_limit failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
                return None;
            }
            Err(e) => {
                warn!("Failed to execute gh api rate_limit: {}", e);
                return None;
            }
        };

        let body = String::from_utf8_lossy(&output.stdout);
        let response: RateLimitResponse = match serde_json::from_str(&body) {
            Ok(r) => r,
            Err(e) => {
                warn!("Failed to parse rate_limit response: {}", e);
                return None;
            }
        };

        let graphql = QuotaState {
            limit: response.resources.graphql.limit,
            used: response.resources.graphql.used,
            remaining: response.resources.graphql.remaining,
            reset: response.resources.graphql.reset,
        };

        let core = QuotaState {
            limit: response.resources.core.limit,
            used: response.resources.core.used,
            remaining: response.resources.core.remaining,
            reset: response.resources.core.reset,
        };

        debug!(
            "Fetched GitHub rate limits: GraphQL {}/{} ({}%), REST {}/{} ({}%)",
            graphql.remaining,
            graphql.limit,
            (graphql.remaining_pct() * 100.0) as u32,
            core.remaining,
            core.limit,
            (core.remaining_pct() * 100.0) as u32
        );

        Some(Self {
            graphql,
            core,
            last_updated: Utc::now(),
        })
    }

    /// Returns true if either quota is critically low (< 5%).
    pub fn is_critical(&self) -> bool {
        self.graphql.is_critical() || self.core.is_critical()
    }

    /// Returns true if either quota is low (< 20%).
    pub fn is_low(&self) -> bool {
        self.graphql.is_low() || self.core.is_low()
    }

    /// Returns a summary string for logging/display.
    pub fn summary(&self) -> String {
        format!(
            "GraphQL: {}/{} ({}%), REST: {}/{} ({}%)",
            self.graphql.remaining,
            self.graphql.limit,
            (self.graphql.remaining_pct() * 100.0) as u32,
            self.core.remaining,
            self.core.limit,
            (self.core.remaining_pct() * 100.0) as u32
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quota_state_remaining_pct() {
        let quota = QuotaState {
            limit: 5000,
            used: 4000,
            remaining: 1000,
            reset: 0,
        };
        assert_eq!(quota.remaining_pct(), 0.2);
    }

    #[test]
    fn test_quota_state_is_low() {
        let quota_low = QuotaState {
            limit: 5000,
            used: 4500,
            remaining: 500,
            reset: 0,
        };
        assert!(quota_low.is_low());

        let quota_ok = QuotaState {
            limit: 5000,
            used: 3000,
            remaining: 2000,
            reset: 0,
        };
        assert!(!quota_ok.is_low());
    }

    #[test]
    fn test_quota_state_is_critical() {
        let quota_critical = QuotaState {
            limit: 5000,
            used: 4900,
            remaining: 100,
            reset: 0,
        };
        assert!(quota_critical.is_critical());

        let quota_low_but_not_critical = QuotaState {
            limit: 5000,
            used: 4500,
            remaining: 500,
            reset: 0,
        };
        assert!(!quota_low_but_not_critical.is_critical());
    }

    #[test]
    fn test_rate_limit_summary() {
        let rate_limit = GitHubRateLimit {
            graphql: QuotaState {
                limit: 5000,
                used: 3000,
                remaining: 2000,
                reset: 0,
            },
            core: QuotaState {
                limit: 5000,
                used: 1000,
                remaining: 4000,
                reset: 0,
            },
            last_updated: Utc::now(),
        };
        let summary = rate_limit.summary();
        assert!(summary.contains("GraphQL: 2000/5000 (40%)"));
        assert!(summary.contains("REST: 4000/5000 (80%)"));
    }
}
