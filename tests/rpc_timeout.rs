//! Tests for RPC timeout issues when gh CLI calls are slow.
//!
//! Reproduces the issue where `midtown status` and `midtown channel post` hit
//! 5-second read timeouts when the daemon's `handle_status` makes synchronous
//! gh CLI calls that are slowed by GitHub API rate limiting.

/// Test that handle_status uses cached PR data instead of calling gh CLI.
///
/// When PR data is available in WorldSnapshot (from the daemon's 30-second
/// polling), handle_status should serve that cached data rather than making
/// fresh gh CLI calls that can timeout under API rate limiting.
#[test]
fn test_handle_status_uses_cached_pr_data() {
    // This test will verify that handle_status doesn't make gh CLI calls
    // when PR data is available in the snapshot.
    //
    // TODO: Implement after adding pr_cache to WorldSnapshot
    //
    // Expected behavior:
    // 1. DaemonState has a WorldSnapshot with populated pr_cache
    // 2. handle_status is called
    // 3. Response contains PR data from the cache (not fresh gh calls)
    // 4. No "gh pr list" process is spawned during the RPC call
}

/// Test that CLI commands use longer timeout than hooks.
///
/// Hook-context calls need 5s timeouts (they block Claude Code), but
/// CLI commands like `midtown status` and `midtown channel post` should
/// tolerate 15-30s for slow GitHub operations.
#[test]
fn test_cli_commands_use_longer_timeout() {
    // This test will verify that DaemonClient can use different timeouts
    // for different contexts (hook vs CLI).
    //
    // TODO: Implement after adding timeout parameter to DaemonClient
    //
    // Expected behavior:
    // 1. Hook-context calls use 5s timeout
    // 2. CLI command calls use 15-30s timeout
    // 3. Both timeouts are configurable
}

/// Test that merged PR data is cached and not re-fetched on every status call.
#[test]
fn test_merged_prs_cached() {
    // Similar to test_handle_status_uses_cached_pr_data but for merged PRs.
    //
    // TODO: Implement after caching merged PR data
    //
    // Expected behavior:
    // 1. Merged PRs are fetched once per 5 minutes by the poller
    // 2. handle_status serves cached merged PR data
    // 3. No "gh pr list --state merged" is called during status RPC
}
