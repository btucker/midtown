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

/// Default interval for polling PRs (30 seconds)
pub const DEFAULT_PR_POLL_INTERVAL_SECS: u64 = 30;

/// Minimum time between nudging the same PR issue (10 minutes)
pub const PR_NUDGE_COOLDOWN_SECS: u64 = 600;

/// Minimum age in seconds before a PR is eligible for auto-review (60 seconds)
pub const PR_REVIEW_DELAY_SECS: u64 = 60;

/// Maximum number of concurrent PR reviews the daemon will run.
pub const MAX_CONCURRENT_REVIEWS: usize = 4;

/// How long a review assignment is valid before it can be reassigned (10 minutes).
/// Re-exported from github_state for use by the in-memory tracker.
pub use crate::github_state::PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS;

// ---------------------------------------------------------------------------
// Internal timing constants
// ---------------------------------------------------------------------------

/// How long a coworker must be idle before being sent on a break (30 seconds)
pub(super) const IDLE_BREAK_DURATION: Duration = Duration::from_secs(30);

/// How often to check for idle coworkers (30 seconds)
pub(super) const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(30);

/// How often to check lead pane activity for typing indicator (3 seconds)
pub(super) const LEAD_TYPING_CHECK_INTERVAL: Duration = Duration::from_secs(3);

/// How often to check if channel rotation is needed (1 hour)
pub(super) const CHANNEL_ROTATION_CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// Maximum age of the oldest message before rotation triggers (24 hours)
pub(super) const CHANNEL_ROTATION_MAX_AGE_HOURS: u64 = 24;

/// How many minutes of recent messages to retain after rotation (60 minutes)
pub(super) const CHANNEL_ROTATION_RETAIN_MINUTES: i64 = 60;

/// Interval for checking orphaned tasks (5 seconds)
pub(super) const ORPHAN_CHECK_INTERVAL_SECS: u64 = 5;

/// Minimum time a coworker must be alive before being sent on a break (5 minutes)
/// This prevents spawn storms where coworkers are rapidly sent on breaks.
pub(super) const MINIMUM_COWORKER_LIFETIME: Duration = Duration::from_secs(300);

/// How long a coworker must be interrupted before nudging them to continue (60 seconds)
pub(super) const INTERRUPTED_NUDGE_DURATION: Duration = Duration::from_secs(60);

/// Cooldown between orphan recovery spawns (5 seconds)
/// Only spawn one coworker per tick, with a minimum gap between spawns.
pub(super) const ORPHAN_SPAWN_COOLDOWN: Duration = Duration::from_secs(5);

/// Extra buffer added to usage limit expiry times before nudging (30 seconds).
/// Gives the API a moment to actually reset before we ask coworkers to retry.
pub(super) const USAGE_LIMIT_NUDGE_BUFFER: Duration = Duration::from_secs(30);

/// Number of coworker slots reserved for reviewers.
pub(super) const REVIEW_HEADROOM: usize = 2;

// ---------------------------------------------------------------------------
// Name / sender lists
// ---------------------------------------------------------------------------

/// Senders whose messages are skipped by the chat monitor (not routed for @mentions).
pub(super) const SKIP_SENDERS: &[&str] = &["midtown", "system", "github", "user"];

/// Senders that are considered "system" (not coworkers) for channel post handling.
pub(super) const SYSTEM_SENDERS: &[&str] = &["Lead", "lead", "github", "system", "GitHub"];

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
