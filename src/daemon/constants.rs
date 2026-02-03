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

/// Grace period before clearing the typing indicator after no pane changes (30 seconds).
/// The lead may pause briefly (reading code, thinking) without having finished work.
pub(super) const LEAD_TYPING_GRACE_PERIOD: Duration = Duration::from_secs(30);

/// How often to check if channel rotation is needed (1 hour)
pub(super) const CHANNEL_ROTATION_CHECK_INTERVAL: Duration = Duration::from_secs(3600);

/// Maximum age of the oldest message before rotation triggers (24 hours)
pub(super) const CHANNEL_ROTATION_MAX_AGE_HOURS: u64 = 24;

/// How many minutes of recent messages to retain after rotation (60 minutes)
pub(super) const CHANNEL_ROTATION_RETAIN_MINUTES: i64 = 60;

/// How often to check if the lead window is still alive (10 seconds)
pub(super) const LEAD_HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// Grace period after daemon startup before the lead health check activates (30 seconds).
/// Prevents races where a freshly started daemon (e.g., after `midtown restart`)
/// tries to respawn the lead window before the tmux session is fully settled.
pub(super) const LEAD_HEALTH_CHECK_STARTUP_GRACE: Duration = Duration::from_secs(30);

/// Interval for checking orphaned tasks (5 seconds)
pub(super) const ORPHAN_CHECK_INTERVAL_SECS: u64 = 5;

/// Minimum time a coworker must be alive before being sent on a break (5 minutes)
/// This prevents spawn storms where coworkers are rapidly sent on breaks.
pub(super) const MINIMUM_COWORKER_LIFETIME: Duration = Duration::from_secs(300);

/// Grace period for pane activity before a coworker can be sent on break (2 minutes).
/// If the coworker's tmux pane content changed within this window, they are considered
/// actively working and must not be sent on break, regardless of task ownership.
pub(super) const PANE_ACTIVITY_GRACE: Duration = Duration::from_secs(120);

/// Cooldown between orphan recovery spawns (5 seconds)
/// Only spawn one coworker per tick, with a minimum gap between spawns.
pub(super) const ORPHAN_SPAWN_COOLDOWN: Duration = Duration::from_secs(5);

/// Cooldown after a coworker spawn failure before retrying (2 minutes).
/// Prevents infinite respawn loops when a coworker's environment is broken
/// (missing worktree, bad session, etc.). The task is reset to pending so
/// other coworkers can pick it up.
pub(super) const SPAWN_FAILURE_COOLDOWN: Duration = Duration::from_secs(120);

/// Cooldown between zombie respawn attempts for the same coworker (5 minutes).
/// Prevents respawn loops if the zombie condition keeps recurring.
pub(super) const ZOMBIE_RESPAWN_COOLDOWN: Duration = Duration::from_secs(300);

/// Minimum age for a coworker before it can be flagged as a zombie (20 seconds).
/// Avoids false positives during normal startup when the TUI hasn't rendered yet.
pub(super) const ZOMBIE_MIN_AGE_SECS: i64 = 20;

/// Maximum zombie respawn attempts before giving up (3 attempts).
/// After this many failed respawns, the coworker is shut down and an alert
/// is posted to the channel. Prevents infinite respawn loops when the
/// underlying cause persists (e.g., broken worktree, bad prompt).
pub(super) const MAX_ZOMBIE_RESPAWN_ATTEMPTS: u32 = 3;

/// How long a coworker's pane can remain unchanged before considering it stuck (3 minutes).
/// If the tmux pane content hash hasn't changed for this duration AND the coworker shows
/// activity indicators (running subagent), it is killed and restarted with its current task.
/// A frozen pane without activity indicators means the coworker is idle/waiting, not stuck.
pub(super) const COWORKER_STUCK_DURATION: Duration = Duration::from_secs(180);

/// Minimum elapsed compaction time before we consider it stuck (5 minutes).
/// Compaction is a normal, useful operation. Only interrupt if it has been
/// running for an unusually long time with no progress. Better to leave a
/// truly-stuck coworker for an extra few minutes than to interrupt legitimate
/// compaction.
pub(super) const MIN_COMPACTION_STUCK_DURATION: Duration = Duration::from_secs(300);

/// Cooldown between compaction recovery attempts for the same coworker (3 minutes).
/// Prevents spamming Escape if the detection fires repeatedly.
pub(super) const COMPACTION_RECOVERY_COOLDOWN: Duration = Duration::from_secs(180);

/// Cooldown between queued-prompt recovery attempts for the same coworker (60 seconds).
/// Shorter than compaction because the fix (Escape) is lightweight and the state
/// can recur quickly after nudges are delivered.
pub(super) const QUEUED_PROMPT_RECOVERY_COOLDOWN: Duration = Duration::from_secs(60);

/// Minimum age in seconds before a coworker is eligible for queued nudge detection (60 seconds).
/// During startup, the TUI structure is still forming and `has_queued_nudges` can produce
/// false positives. This gives the TUI time to settle before we start detecting queued nudges.
pub(super) const QUEUED_NUDGE_MIN_AGE_SECS: i64 = 60;

/// Extra buffer added to usage limit expiry times before nudging (30 seconds).
/// Gives the API a moment to actually reset before we ask coworkers to retry.
pub(super) const USAGE_LIMIT_NUDGE_BUFFER: Duration = Duration::from_secs(30);

/// Number of coworker slots reserved for reviewers.
pub(super) const REVIEW_HEADROOM: usize = 2;

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
