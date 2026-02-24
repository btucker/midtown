use super::*;
use crate::daemon::state::ProfileState;
use std::collections::HashMap;

fn make_pool(profiles: &[&str]) -> Vec<String> {
    profiles.iter().map(|s| s.to_string()).collect()
}

fn make_state(profiles: &[(&str, bool, Option<&str>)]) -> HashMap<String, ProfileState> {
    profiles
        .iter()
        .map(|(email, limited, last_used)| {
            let state = ProfileState {
                is_usage_limited: *limited,
                usage_limit_reset_at: None,
                last_used_at: last_used.map(|s| s.parse().unwrap()),
            };
            (email.to_string(), state)
        })
        .collect()
}

#[test]
fn selects_only_available_profile() {
    let pool = make_pool(&["alice@example.com"]);
    let state = make_state(&[("alice@example.com", false, None)]);
    assert_eq!(
        select_profile(&pool, &state),
        Some("alice@example.com".to_string())
    );
}

#[test]
fn skips_usage_limited_profiles() {
    let pool = make_pool(&["alice@example.com", "bob@example.com"]);
    let state = make_state(&[
        ("alice@example.com", true, None), // limited
        ("bob@example.com", false, None),  // available
    ]);
    assert_eq!(
        select_profile(&pool, &state),
        Some("bob@example.com".to_string())
    );
}

#[test]
fn all_profiles_limited_returns_none() {
    let pool = make_pool(&["alice@example.com", "bob@example.com"]);
    let state = make_state(&[
        ("alice@example.com", true, None),
        ("bob@example.com", true, None),
    ]);
    assert_eq!(select_profile(&pool, &state), None);
}

#[test]
fn selects_lru_among_available() {
    let pool = make_pool(&[
        "alice@example.com",
        "bob@example.com",
        "charlie@example.com",
    ]);
    let state = make_state(&[
        ("alice@example.com", false, Some("2026-01-01T10:00:00Z")), // oldest
        ("bob@example.com", false, Some("2026-01-01T11:00:00Z")),   // middle
        ("charlie@example.com", false, Some("2026-01-01T12:00:00Z")), // newest
    ]);
    // Should pick alice (LRU = oldest last_used_at)
    assert_eq!(
        select_profile(&pool, &state),
        Some("alice@example.com".to_string())
    );
}

#[test]
fn prefers_never_used_over_lru() {
    let pool = make_pool(&["alice@example.com", "bob@example.com"]);
    let state = make_state(&[
        ("alice@example.com", false, Some("2026-01-01T10:00:00Z")),
        // bob has no last_used_at (never used)
    ]);
    assert_eq!(
        select_profile(&pool, &state),
        Some("bob@example.com".to_string())
    );
}

#[test]
fn empty_pool_returns_none() {
    let pool = make_pool(&[]);
    let state = make_state(&[]);
    assert_eq!(select_profile(&pool, &state), None);
}

#[test]
fn single_profile_pool_behaves_like_no_pool() {
    let pool = make_pool(&["alice@example.com"]);
    let state = make_state(&[]);
    assert_eq!(
        select_profile(&pool, &state),
        Some("alice@example.com".to_string())
    );
}

#[test]
fn pool_with_unknown_profiles_treats_them_as_available() {
    // Profiles not in state (never used, never limited) should be treated as available.
    let pool = make_pool(&["new@example.com"]);
    let state = HashMap::new(); // empty — no known profiles
    assert_eq!(
        select_profile(&pool, &state),
        Some("new@example.com".to_string())
    );
}

#[test]
fn reset_at_expired_profile_still_limited_until_explicitly_cleared() {
    // Usage limit clearing is explicit (via ClearProfileLimit effect).
    // A profile with is_usage_limited=true but past reset_at is still limited
    // until the effect fires — this is correct reactive behavior.
    use chrono::Utc;
    let pool = make_pool(&["alice@example.com"]);
    let mut state = HashMap::new();
    state.insert(
        "alice@example.com".to_string(),
        ProfileState {
            is_usage_limited: true,
            usage_limit_reset_at: Some(Utc::now() - chrono::Duration::hours(1)), // past
            last_used_at: None,
        },
    );
    // Still limited — daemon hasn't fired the clear effect yet.
    assert_eq!(select_profile(&pool, &state), None);
}
