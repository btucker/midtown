//! Constants used throughout the daemon module.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Public API constants
// ---------------------------------------------------------------------------

/// Default maximum number of concurrent coworkers.
pub const DEFAULT_MAX_COWORKERS: usize = 8;

/// Default interval for restarting the webhook forwarder (5 minutes)
pub const DEFAULT_WEBHOOK_RESTART_INTERVAL_SECS: u64 = 300;

/// Default port for the per-project webhook server.
/// Port 47022 is reserved for the shared multi-project webserver.
/// Per-project daemons use 47023+.
pub const DEFAULT_WEBHOOK_PORT: u16 = 47023;

/// Default interval for polling PRs (30 seconds).
///
/// PR polling now runs in the main event loop (not a separate task) to prevent
/// spawn races with TaskDispatchTick. Adaptive intervals based on webhook health
/// were removed as part of this change for simplicity.
pub const DEFAULT_PR_POLL_INTERVAL_SECS: u64 = 30;

/// Minimum time between nudging the same PR issue (10 minutes)
pub const PR_NUDGE_COOLDOWN_SECS: u64 = 600;

/// Minimum age in seconds before a PR is eligible for auto-review (45 seconds).
/// Tradeoff: Faster reviewer spawn vs. sufficient time for CI startup and author context.
/// Reduced from 60s to 45s to improve throughput while maintaining buffer for CI checks
/// to start reporting and for PR author to add additional context after opening.
/// Hash bucket deduplication prevents duplicate reviewer assignments.
pub const PR_REVIEW_DELAY_SECS: u64 = 45;

/// How long a review assignment is valid before it can be reassigned (30 minutes).
/// Re-exported from github_state for use by the in-memory tracker.
pub use crate::github_state::PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS;

// ---------------------------------------------------------------------------
// Internal timing constants
// ---------------------------------------------------------------------------

/// How often to check for idle coworkers (30 seconds)
pub(super) const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// How often to check if channel rotation is needed (1 hour)
pub(super) const CHANNEL_ROTATION_CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// Maximum age of the oldest message before rotation triggers (24 hours)
pub(super) const CHANNEL_ROTATION_MAX_AGE_HOURS: u64 = 24;

/// How many minutes of recent messages to retain after rotation (60 minutes)
pub(super) const CHANNEL_ROTATION_RETAIN_MINUTES: i64 = 60;

/// Interval for task dispatch and orphaned worktree cleanup tick (5 seconds).
/// Tradeoff: Tasks get assigned faster and orphan recovery happens sooner vs. more frequent
/// snapshot collection (CPU impact). Worktree cleanup is still rate-limited to one worktree
/// per tick. Testing shows 5s interval has acceptable CPU overhead while significantly
/// improving task assignment latency. With 5s interval, 10 orphaned worktrees take ~50s.
pub(super) const ORPHAN_CHECK_INTERVAL_SECS: u64 = 5;

/// Minimum time a coworker must be alive before being sent on a break (60 seconds).
/// Reduced from 300s: the spawn storm concern that motivated the 5-min guard is less
/// relevant now that dual-dispatch is fixed and worktree collision guards are in place.
pub(super) const MINIMUM_COWORKER_LIFETIME: Duration = Duration::from_secs(60);

/// How long an attached session can persist without a detach before being auto-detached (10 min).
///
/// If the interactive session ends without a proper `midtown session detach` (terminal crash,
/// SSH disconnect, wrapper bug), the entry stays in `attached_coworkers` forever and the
/// lead can never respawn. Auto-detach clears stale entries so `ensure_lead_alive()` can
/// respawn the lead on the next tick.
pub(super) const ATTACH_TIMEOUT: Duration = Duration::from_secs(600);

/// Cooldown before the lead session is automatically respawned after stopping (5 minutes).
/// The lead may have stopped intentionally (auth error, manual detach/reattach). A long
/// cooldown prevents crash loops where a broken lead respawns repeatedly within seconds.
/// This is longer than MINIMUM_COWORKER_LIFETIME because a crash-looping lead is more
/// disruptive than a coworker cycling too fast.
pub(super) const LEAD_RESPAWN_COOLDOWN: Duration = Duration::from_secs(300);

/// Default interval for periodic lead session refresh to prevent context drift (90 minutes).
///
/// Long lead sessions accumulate context and the LLM can start forgetting system
/// prompt instructions. Periodic refresh restarts the session for a clean slate.
/// Set to 0 in config to disable.
pub const DEFAULT_LEAD_SESSION_REFRESH_INTERVAL_SECS: u64 = 90 * 60;

/// Cooldown between orphan recovery spawns (2 seconds).
/// Tradeoff: Multiple orphan recoveries happen faster vs. spawn storm risk. At 2s with
/// ORPHAN_CHECK_INTERVAL_SECS=5s, we can recover multiple tasks quickly without overwhelming
/// the system. Still enforces one-spawn-per-tick to prevent uncontrolled spawning.
pub(super) const ORPHAN_SPAWN_COOLDOWN: Duration = Duration::from_secs(2);

/// Cooldown between session-centric dispatch recovery spawns (2 seconds).
/// Same cadence as orphan recovery -- session dispatch is a parallel recovery path
/// that uses session records instead of orphan detection heuristics.
pub(super) const SESSION_DISPATCH_COOLDOWN: Duration = Duration::from_secs(2);

/// Grace period after a coworker stops before orphan recovery kicks in (40 seconds).
/// Tradeoff: Faster recovery of abandoned tasks vs. risk of recovering tasks that are
/// legitimately completing. Reduced from 60s to 40s (still conservative) to speed up
/// orphan recovery while maintaining safety margin for slow completion workflows (PR merge
/// + CI checks, network latency, delayed RPC when coworker is wrapping up).
pub(super) const ORPHAN_RECOVERY_GRACE_PERIOD: Duration = Duration::from_secs(40);

/// Cooldown after a coworker spawn failure before retrying (60 seconds).
/// Tradeoff: Failed spawns retry sooner vs. risk of rapid retry loops. Most spawn failures
/// are transient (system load, network hiccup), so 60s gives the system time to stabilize
/// while still retrying reasonably fast. The task resets to pending so other coworkers can
/// claim it during the cooldown, providing an alternative recovery path.
pub(super) const SPAWN_FAILURE_COOLDOWN: Duration = Duration::from_secs(60);

/// Cooldown between zombie respawn attempts for the same coworker (5 minutes).
/// Prevents respawn loops if the zombie condition keeps recurring.
pub(super) const ZOMBIE_RESPAWN_COOLDOWN: Duration = Duration::from_secs(300);

/// How long a coworker's process can go without events before considering it stuck (3 minutes).
/// If the headless session hasn't emitted any events for this duration, the coworker
/// is killed and restarted with its current task. The pending tool detection (has_pending_tool)
/// and running subagent detection (has_running_subagent) provide precise stuck detection,
/// allowing a shorter timeout without false positives.
pub(super) const COWORKER_STUCK_DURATION: Duration = Duration::from_secs(180);

/// How long a reviewer's process can go without events before considering it stuck (5 minutes).
/// Reviewers take longer than task coworkers (reading diffs, writing comments), so this
/// threshold is longer than COWORKER_STUCK_DURATION.
pub(super) const REVIEWER_STUCK_DURATION: Duration = Duration::from_secs(300);

/// Stuck duration for reviewers that have posted a "Review in progress" placeholder.
/// Shorter than REVIEWER_STUCK_DURATION because we know they started the review.
/// If they die or freeze after posting the placeholder, we recover faster.
pub(super) const REVIEWER_PLACEHOLDER_STUCK_DURATION: Duration = Duration::from_secs(120);

/// Maximum number of times a stuck reviewer can be restarted for the same PR.
/// After this many restarts (meaning max_restarts + 1 total attempts), the daemon
/// stops retrying and posts an escalation warning for the lead to handle.
pub(super) const MAX_REVIEWER_RESTARTS: u32 = 2;

/// Maximum number of times a stuck task coworker can be restarted.
/// After this many restarts, the daemon stops retrying and posts an escalation warning.
#[allow(dead_code)] // Will be used when task-based stuck restart backoff is implemented
pub(super) const MAX_TASK_RESTARTS: u32 = 3;

/// Extra buffer added to usage limit expiry times before nudging (30 seconds).
/// Gives the API a moment to actually reset before we ask coworkers to retry.
pub(super) const USAGE_LIMIT_NUDGE_BUFFER: Duration = Duration::from_secs(30);

/// Cooldown between API error retry nudges for the same coworker (90 seconds).
/// API errors are transient, so we periodically nudge to encourage retry.
/// Unlike usage limits (which have a known reset time), API errors may resolve
/// at any moment, so periodic nudging is more appropriate.
pub(super) const API_ERROR_NUDGE_COOLDOWN: Duration = Duration::from_secs(90);

/// Auth errors (expired OAuth tokens) require user intervention and won't resolve
/// with retries. This cooldown prevents repeatedly shutting down the same coworker
/// and spamming notifications. Set to 5 minutes to allow time for the user to re-auth.
pub(super) const AUTH_ERROR_SHUTDOWN_COOLDOWN: Duration = Duration::from_secs(300);

/// Number of coworker slots reserved for reviewers.
pub(super) const REVIEW_HEADROOM: usize = 2;

/// TTL for the negative review cache in `is_pr_reviewed` (2 minutes).
///
/// When a PR is confirmed to have no Claude review, we cache that result for this
/// duration to avoid repeated `gh pr view` GraphQL calls on every poll tick.
/// After 2 minutes, we re-check in case a review was posted in the meantime.
pub(super) const PR_REVIEW_NEGATIVE_CACHE_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// Stuck detection constants (nudge lead when things are stuck)
// ---------------------------------------------------------------------------

/// How long a PR can be open with no review before nudging lead (15 minutes)
pub(super) const STUCK_NO_REVIEW_DURATION: Duration = Duration::from_secs(15 * 60);

/// How long a PR can have unresolved feedback before nudging lead (30 minutes)
pub(super) const STUCK_UNRESOLVED_FEEDBACK_DURATION: Duration = Duration::from_secs(30 * 60);

/// How long a PR can be approved + green but not merged before nudging lead (10 minutes)
pub(super) const STUCK_MERGE_READY_DURATION: Duration = Duration::from_secs(10 * 60);

/// How long a coworker can be silent (no channel activity) before nudging lead (20 minutes)
pub(super) const STUCK_SILENT_COWORKER_DURATION: Duration = Duration::from_secs(20 * 60);

/// Cooldown between stuck-condition nudges for the same issue (30 minutes)
/// Longer than PR_NUDGE_COOLDOWN_SECS because these go to the lead, not coworkers.
pub const STUCK_NUDGE_COOLDOWN_SECS: u64 = 30 * 60;

/// Number of repeated nudges before escalating to a bug report.
/// With 30-minute cooldown, 2 nudges means the issue has persisted for 45+ minutes
/// (15 min initial + 30 min cooldown). At this point, repeated warnings become
/// an escalation suggesting the lead investigate as a potential daemon bug.
pub(super) const STUCK_ESCALATION_NUDGE_COUNT: u32 = 2;

/// Channel name for daemon operational messages.
///
/// Operational messages (spawns, shutdowns, health checks, stuck detection,
/// worktree cleanups, PR routing decisions) are routed here instead of the
/// main project channel. This keeps team communication clean and ops noise
/// in a dedicated view.
pub(super) const OPS_CHANNEL: &str = "ops";

// ---------------------------------------------------------------------------
// Name / sender lists
// ---------------------------------------------------------------------------

/// Senders whose messages are skipped by the chat monitor (not routed for @mentions).
/// Includes "architect" to prevent diagram messages from triggering mention routing.
pub(super) const SKIP_SENDERS: &[&str] = &["midtown", "system", "github", "user", "architect"];

/// Senders that are considered "system" (not coworkers) for channel post handling.
pub(super) const SYSTEM_SENDERS: &[&str] = &["github", "midtown", "system", "GitHub"];

/// All valid coworker names for @mention detection.
pub(super) const COWORKER_NAMES: &[&str] = &[
    "lexington",
    "park",
    "madison",
    "broadway",
    "amsterdam",
    "columbus",
    "central",
    "riverside",
    "york",
    "pleasant",
    "vernon",
    "bleecker",
    "houston",
    "canal",
    "spring",
    "prince",
    "mercer",
];
