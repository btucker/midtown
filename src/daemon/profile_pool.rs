//! Profile pool selection for multi-account coworker spawning.
//!
//! This module is intentionally pure (no I/O, no async) so it can be tested
//! easily and called from the spawn path without coupling to DaemonState.

use crate::daemon::state::ProfileState;
use std::collections::HashMap;

/// Select a profile from the pool using LRU-among-available strategy.
///
/// Returns `None` if the pool is empty or all profiles are usage-limited.
///
/// Selection algorithm:
/// 1. Filter out profiles with `is_usage_limited: true`
/// 2. Among available profiles, pick the one with the oldest `last_used_at`
/// 3. Never-used profiles (`last_used_at: None`) are preferred over any timestamp
/// 4. Unknown profiles (not in `state`) are treated as available and never-used
pub fn select_profile(pool: &[String], state: &HashMap<String, ProfileState>) -> Option<String> {
    if pool.is_empty() {
        return None;
    }

    // Filter to profiles not currently usage-limited.
    let available: Vec<&String> = pool
        .iter()
        .filter(|email| {
            state
                .get(*email)
                .map(|s| !s.is_usage_limited)
                .unwrap_or(true) // unknown profile = available (never been limited)
        })
        .collect();

    if available.is_empty() {
        return None;
    }

    // Among available, pick LRU: never-used first, then oldest last_used_at.
    // None (never used) maps to i64::MIN so it sorts before any real timestamp.
    available
        .into_iter()
        .min_by_key(|email| {
            state
                .get(*email)
                .and_then(|s| s.last_used_at)
                .map(|t| t.timestamp())
                .unwrap_or(i64::MIN)
        })
        .cloned()
}

#[path = "profile_pool_tests.rs"]
#[cfg(test)]
mod tests;
