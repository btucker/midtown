//! Persistent GitHub state for the midtown daemon.
//!
//! Stores PR reviewer assignments in a JSON file that survives daemon restarts.
//! This prevents duplicate reviewer assignments and enables the web UI to show
//! which coworker is reviewing each PR.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use tracing::{debug, warn};

/// How long a review assignment is valid before it expires (10 minutes).
/// Mirrors PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS from the in-memory tracker.
pub const PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS: u64 = 600;

/// Persistent state for GitHub-related data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubState {
    /// Map of PR number -> reviewer assignment
    #[serde(default)]
    pub pr_reviewers: HashMap<u64, PrReviewerAssignment>,

    /// Set of PR numbers that have a confirmed completed review.
    /// Review status is monotonic — once a PR has a review, it never loses it.
    /// This cache eliminates redundant `gh pr view` calls on every poll cycle.
    #[serde(default)]
    pub reviewed_prs: std::collections::HashSet<u64>,

    /// Pending reviewer spawns from webhook events, waiting for the delay to expire.
    /// Persisted so they survive daemon restarts (unlike the previous tokio::sleep approach).
    #[serde(default)]
    pub pending_review_spawns: Vec<PendingReviewSpawn>,

    /// Per-PR timestamp of the last webhook event that handled this PR.
    /// Polling checks this to defer to webhooks when they're healthy for a specific PR.
    #[serde(default)]
    pub pr_last_webhook_event: HashMap<u64, DateTime<Utc>>,

    /// Map of PR number -> author session info for PR handoff.
    ///
    /// When a coworker opens a PR, we store their Claude session ID so that
    /// any other coworker can resume work on that PR with full context.
    /// This enables PR continuity when the original author is unavailable.
    #[serde(default)]
    pub pr_author_sessions: HashMap<u64, PrAuthorSession>,

    /// GitHub API rate limit state (GraphQL and REST quotas).
    /// Fetched periodically to enable adaptive throttling when quotas run low.
    #[serde(default)]
    pub rate_limit: crate::github_rate_limit::GitHubRateLimit,

    /// Map of PR number -> review comment IDs.
    ///
    /// When a completed review is detected, the GitHub comment ID is stored here.
    /// The `pr.merge` RPC uses this to verify that all review feedback has been
    /// addressed (via `<!-- addresses-review: {id} -->` tags) before allowing merge.
    #[serde(default)]
    pub pr_review_comment_ids: HashMap<u64, Vec<u64>>,
}

/// A pending reviewer spawn triggered by a webhook event.
///
/// Instead of using `tokio::time::sleep` in a detached task (which is lost on restart),
/// pending spawns are persisted here and checked each tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingReviewSpawn {
    /// PR number that needs a reviewer.
    pub pr_number: u64,
    /// Wall-clock time after which the reviewer should be spawned.
    pub spawn_after: DateTime<Utc>,
}

/// How the reviewer assignment was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssignmentSource {
    /// Triggered by a GitHub webhook event (PR opened / ready_for_review).
    Webhook,
    /// Triggered by the periodic polling loop as a fallback.
    PollingFallback,
    /// Manually assigned (e.g., by the lead or via an RPC command).
    Manual,
}

impl fmt::Display for AssignmentSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssignmentSource::Webhook => write!(f, "webhook"),
            AssignmentSource::PollingFallback => write!(f, "polling"),
            AssignmentSource::Manual => write!(f, "manual"),
        }
    }
}

/// A PR reviewer assignment record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrReviewerAssignment {
    /// PR number
    pub pr_number: u64,
    /// Coworker name assigned to review (display/routing label).
    pub reviewer: String,
    /// Claude session ID for the reviewing session, if known.
    ///
    /// Used to uniquely identify the session when multiple sessions share
    /// a coworker name. `None` for assignments created before session tracking
    /// was added (backward-compatible with persisted state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer_session_id: Option<String>,
    /// When the assignment was made
    pub assigned_at: DateTime<Utc>,
    /// How this assignment was triggered (webhook, polling fallback, or manual).
    #[serde(default = "default_assignment_source")]
    pub source: AssignmentSource,
    /// Optional webhook delivery ID for debugging/telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_event_id: Option<String>,
    /// How many times this reviewer has been restarted for the same PR.
    /// Used by stuck reviewer detection to implement backoff — after
    /// `MAX_REVIEWER_RESTARTS`, no further restarts are attempted.
    #[serde(default)]
    pub restart_count: u32,
}

/// Tracks the Claude session associated with a PR author.
///
/// When a coworker opens a PR, we store their session ID so that any other
/// coworker can later resume work on that PR with full context preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrAuthorSession {
    /// The Claude session ID (UUID) used when the PR was created.
    pub session_id: String,
    /// The git branch name for this PR.
    pub branch: String,
    /// The coworker who originally authored the PR.
    pub original_author: String,
    /// When this session was recorded.
    pub stored_at: DateTime<Utc>,
    /// The task ID associated with this PR (extracted from [Midtown !XXX] in PR title).
    /// Used to prevent auto-completion of tasks until the PR merges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

fn default_assignment_source() -> AssignmentSource {
    AssignmentSource::PollingFallback
}

/// Extract task ID from a PR title in the format "[Midtown !XXX]".
///
/// Returns the task ID as a string (e.g., "42") if found, otherwise None.
fn extract_task_id_from_title(title: &str) -> Option<String> {
    // Look for pattern "[Midtown !NNN]" - case insensitive
    let lower = title.to_lowercase();
    if let Some(start) = lower.find("[midtown !") {
        let after_marker = &title[start + 10..]; // Skip "[midtown !"
        if let Some(end) = after_marker.find(']') {
            let num_str = after_marker[..end].trim();
            // Validate it's all digits
            if !num_str.is_empty() && num_str.chars().all(|c| c.is_ascii_digit()) {
                return Some(num_str.to_string());
            }
        }
    }
    None
}

impl GitHubState {
    /// Load state from a file, returning default if file doesn't exist.
    pub fn load(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(contents) => {
                let state: Self = serde_json::from_str(&contents).map_err(|e| {
                    warn!("Failed to parse github-state.json: {}", e);
                    io::Error::new(ErrorKind::InvalidData, e)
                })?;
                debug!(
                    "Loaded GitHub state with {} PR reviewers",
                    state.pr_reviewers.len()
                );
                Ok(state)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                debug!("github-state.json not found, using defaults");
                Ok(Self::default())
            }
            Err(e) => Err(e),
        }
    }

    /// Save state to a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(path, contents)?;
        debug!(
            "Saved GitHub state with {} PR reviewers",
            self.pr_reviewers.len()
        );
        Ok(())
    }

    /// Assign a reviewer to a PR with the given source.
    pub fn assign_reviewer(&mut self, pr_number: u64, reviewer: &str, source: AssignmentSource) {
        let assignment = PrReviewerAssignment {
            pr_number,
            reviewer: reviewer.to_string(),
            reviewer_session_id: None,
            assigned_at: Utc::now(),
            source,
            webhook_event_id: None,
            restart_count: 0,
        };
        self.pr_reviewers.insert(pr_number, assignment);
    }

    /// Assign a reviewer to a PR with a webhook event ID for tracing.
    pub fn assign_reviewer_with_event_id(
        &mut self,
        pr_number: u64,
        reviewer: &str,
        source: AssignmentSource,
        webhook_event_id: Option<String>,
    ) {
        let assignment = PrReviewerAssignment {
            pr_number,
            reviewer: reviewer.to_string(),
            reviewer_session_id: None,
            assigned_at: Utc::now(),
            source,
            webhook_event_id,
            restart_count: 0,
        };
        self.pr_reviewers.insert(pr_number, assignment);
    }

    /// Assign a reviewer to a PR with a specific restart count.
    ///
    /// Used by stuck reviewer detection to preserve the restart count across
    /// restarts, enabling backoff after repeated failures.
    pub fn assign_reviewer_with_restart_count(
        &mut self,
        pr_number: u64,
        reviewer: &str,
        source: AssignmentSource,
        restart_count: u32,
    ) {
        let assignment = PrReviewerAssignment {
            pr_number,
            reviewer: reviewer.to_string(),
            reviewer_session_id: None,
            assigned_at: Utc::now(),
            source,
            webhook_event_id: None,
            restart_count,
        };
        self.pr_reviewers.insert(pr_number, assignment);
    }

    /// Check if a PR has a reviewer assigned.
    pub fn get_reviewer(&self, pr_number: u64) -> Option<&str> {
        self.pr_reviewers
            .get(&pr_number)
            .map(|a| a.reviewer.as_str())
    }

    /// Check if a PR has been assigned for review and the assignment hasn't expired.
    pub fn is_assigned(&self, pr_number: u64) -> bool {
        match self.pr_reviewers.get(&pr_number) {
            Some(assignment) => {
                let elapsed = Utc::now().signed_duration_since(assignment.assigned_at);
                elapsed < chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64)
            }
            None => false,
        }
    }

    /// Remove a reviewer assignment (e.g., when PR is merged/closed).
    pub fn remove_assignment(&mut self, pr_number: u64) -> Option<PrReviewerAssignment> {
        self.pr_reviewers.remove(&pr_number)
    }

    /// Get all coworkers currently assigned to review PRs.
    pub fn assigned_reviewers(&self) -> impl Iterator<Item = &str> {
        self.pr_reviewers.values().map(|a| a.reviewer.as_str())
    }

    /// Get the PR number assigned to a specific reviewer.
    pub fn pr_for_reviewer(&self, reviewer: &str) -> Option<u64> {
        self.pr_reviewers
            .iter()
            .find(|(_, a)| a.reviewer == reviewer)
            .map(|(pr_number, _)| *pr_number)
    }

    /// Returns true if the reviewer has an assignment whose timestamp falls within
    /// `PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS + extra_secs`.
    ///
    /// Used by `compute_active_reviewers_with_health` to protect alive reviewers
    /// during the race window between `SessionMonitorTick` and `PrPollTick` without
    /// promoting truly stale assignments from sessions that ended long ago.
    pub fn reviewer_has_recent_assignment(&self, reviewer: &str, extra_secs: u64) -> bool {
        let limit =
            chrono::Duration::seconds((PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS + extra_secs) as i64);
        self.pr_reviewers.iter().any(|(_, a)| {
            a.reviewer == reviewer && Utc::now().signed_duration_since(a.assigned_at) < limit
        })
    }

    /// Remove a reviewer assignment by coworker name (e.g., when coworker session ends).
    ///
    /// Returns the removed assignment if found.
    pub fn remove_assignment_by_reviewer(
        &mut self,
        reviewer: &str,
    ) -> Option<PrReviewerAssignment> {
        if let Some(pr_number) = self.pr_for_reviewer(reviewer) {
            self.pr_reviewers.remove(&pr_number)
        } else {
            None
        }
    }

    /// Clean up assignments that have expired (older than timeout).
    pub fn cleanup_expired_assignments(&mut self) {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        let to_remove: Vec<_> = self
            .pr_reviewers
            .iter()
            .filter(|(_, a)| now.signed_duration_since(a.assigned_at) > timeout)
            .map(|(pr, _)| *pr)
            .collect();

        for pr in to_remove {
            debug!(
                "Cleaning up expired reviewer assignment for PR #{} (timed out)",
                pr
            );
            self.pr_reviewers.remove(&pr);
        }
    }

    /// Clean up expired assignments, but preserve those for active coworkers.
    ///
    /// Same as `cleanup_expired_assignments` but skips removal of assignments
    /// where the reviewer coworker is still running. This prevents losing track
    /// of a reviewer just because the review is taking longer than the timeout.
    /// Running coworkers' assignments are preserved regardless of timeout.
    ///
    /// **Optimistic assignment safety**: Assignments younger than `timeout` (600s)
    /// are never pruned, even if the reviewer doesn't appear in `running_coworkers`.
    /// This protects against the window between optimistic assignment (before spawn)
    /// and worktree creation (after spawn completes).
    ///
    /// When `running_session_ids` is provided, assignments with a known
    /// `reviewer_session_id` are matched by session ID instead of name. This
    /// enables correct behavior when multiple sessions share a coworker name.
    /// Assignments without a `reviewer_session_id` fall back to name matching.
    pub fn cleanup_expired_preserving(
        &mut self,
        running_coworkers: &std::collections::HashSet<String>,
        running_session_ids: Option<&std::collections::HashSet<String>>,
    ) {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);

        let is_reviewer_running = |a: &PrReviewerAssignment| -> bool {
            // If we have session IDs and the assignment has one, prefer session-based matching
            if let (Some(session_ids), Some(sid)) = (running_session_ids, &a.reviewer_session_id) {
                return session_ids.contains(sid);
            }
            // Fall back to name-based matching
            running_coworkers.contains(&a.reviewer)
        };

        let to_remove: Vec<_> = self
            .pr_reviewers
            .iter()
            .filter(|(_, a)| {
                now.signed_duration_since(a.assigned_at) > timeout && !is_reviewer_running(a)
            })
            .map(|(pr, _)| *pr)
            .collect();

        for pr in to_remove {
            debug!(
                "Cleaning up expired reviewer assignment for PR #{} (timed out, coworker not running)",
                pr
            );
            self.pr_reviewers.remove(&pr);
        }

        // Refresh timestamps for running coworkers whose assignments would have expired
        for assignment in self.pr_reviewers.values_mut() {
            if now.signed_duration_since(assignment.assigned_at) > timeout
                && is_reviewer_running(assignment)
            {
                assignment.assigned_at = now;
            }
        }
    }

    /// Backfill `reviewer_session_id` for assignments that were created before
    /// the session ID was known (optimistic assignment pattern).
    ///
    /// During reviewer spawn, the assignment is created BEFORE the spawn completes,
    /// so `reviewer_session_id` is initially `None`. Once the session starts and
    /// the `init` event provides the session ID, subsequent poll ticks can observe
    /// it in the snapshot's `running_coworkers`. This method matches assignments
    /// by reviewer name and fills in the missing session ID.
    pub fn backfill_reviewer_session_ids(
        &mut self,
        coworker_session_ids: &std::collections::HashMap<String, String>,
    ) {
        for assignment in self.pr_reviewers.values_mut() {
            if assignment.reviewer_session_id.is_none()
                && let Some(sid) = coworker_session_ids.get(&assignment.reviewer)
            {
                assignment.reviewer_session_id = Some(sid.clone());
                debug!(
                    "Backfilled reviewer_session_id for PR #{} (reviewer={}, session={})",
                    assignment.pr_number, assignment.reviewer, sid
                );
            }
        }
    }

    /// Record that a webhook event handled a specific PR.
    ///
    /// Polling will check this timestamp before acting on the same PR,
    /// deferring to the webhook path when it's been active recently.
    pub fn record_webhook_event(&mut self, pr_number: u64) {
        self.pr_last_webhook_event.insert(pr_number, Utc::now());
    }

    /// Check if a webhook recently handled this PR (within the given window).
    ///
    /// Returns `true` if a webhook event was recorded for this PR within
    /// `window_secs` seconds, meaning polling should defer.
    pub fn webhook_recently_handled(&self, pr_number: u64, window_secs: i64) -> bool {
        match self.pr_last_webhook_event.get(&pr_number) {
            Some(ts) => {
                let elapsed = Utc::now().signed_duration_since(*ts);
                elapsed < chrono::Duration::seconds(window_secs)
            }
            None => false,
        }
    }

    /// Clean up stale per-PR webhook event timestamps (older than 1 hour).
    pub fn cleanup_stale_webhook_events(&mut self) {
        let cutoff = Utc::now() - chrono::Duration::seconds(3600);
        self.pr_last_webhook_event.retain(|_, ts| *ts > cutoff);
    }

    /// Add a pending review spawn for a PR.
    pub fn add_pending_review_spawn(&mut self, pr_number: u64, spawn_after: DateTime<Utc>) {
        // Don't add duplicates for the same PR
        if !self
            .pending_review_spawns
            .iter()
            .any(|p| p.pr_number == pr_number)
        {
            self.pending_review_spawns.push(PendingReviewSpawn {
                pr_number,
                spawn_after,
            });
        }
    }

    /// Drain pending review spawns that are ready (spawn_after <= now).
    pub fn drain_ready_review_spawns(&mut self) -> Vec<u64> {
        let now = Utc::now();
        let (ready, remaining): (Vec<_>, Vec<_>) = self
            .pending_review_spawns
            .drain(..)
            .partition(|p| p.spawn_after <= now);
        self.pending_review_spawns = remaining;
        ready.into_iter().map(|p| p.pr_number).collect()
    }

    /// Check if a PR has a cached completed review result.
    pub fn has_cached_review(&self, pr_number: u64) -> bool {
        self.reviewed_prs.contains(&pr_number)
    }

    /// Mark a PR as having a completed review (cache it permanently).
    pub fn mark_reviewed_pr(&mut self, pr_number: u64) {
        self.reviewed_prs.insert(pr_number);
    }

    /// Count active (non-expired) review assignments.
    pub fn active_count(&self) -> usize {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        self.pr_reviewers
            .values()
            .filter(|a| now.signed_duration_since(a.assigned_at) < timeout)
            .count()
    }

    /// Get the set of coworker names with active (non-expired) review assignments.
    pub fn active_reviewers(&self) -> std::collections::HashSet<String> {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        self.pr_reviewers
            .values()
            .filter(|a| now.signed_duration_since(a.assigned_at) < timeout)
            .map(|a| a.reviewer.clone())
            .collect()
    }

    /// Get all active (non-expired) review assignments.
    pub fn active_assignments(&self) -> HashMap<u64, PrReviewerAssignment> {
        let now = Utc::now();
        let timeout = chrono::Duration::seconds(PR_REVIEW_ASSIGNMENT_TIMEOUT_SECS as i64);
        self.pr_reviewers
            .iter()
            .filter(|(_, a)| now.signed_duration_since(a.assigned_at) < timeout)
            .map(|(pr, a)| (*pr, a.clone()))
            .collect()
    }

    /// Clean up review cache entries for PRs that are no longer open.
    fn cleanup_closed_review_cache(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();
        self.reviewed_prs.retain(|pr| open_set.contains(pr));
    }

    /// Clean up assignments for PRs that are no longer open.
    ///
    /// Takes a list of open PR numbers and removes assignments for any PRs not in the list.
    pub fn cleanup_closed_prs(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();
        let to_remove: Vec<_> = self
            .pr_reviewers
            .keys()
            .filter(|pr| !open_set.contains(pr))
            .copied()
            .collect();

        for pr in to_remove {
            debug!("Cleaning up reviewer assignment for closed PR #{}", pr);
            self.pr_reviewers.remove(&pr);
        }

        // Also clean up review cache for closed PRs
        self.cleanup_closed_review_cache(open_pr_numbers);

        // Clean up per-PR webhook event timestamps for closed PRs
        self.pr_last_webhook_event
            .retain(|pr, _| open_set.contains(pr));

        // Clean up PR author sessions for closed PRs
        self.pr_author_sessions
            .retain(|pr, _| open_set.contains(pr));
    }

    /// Store the Claude session ID for a PR author.
    ///
    /// Called when a coworker opens a PR, so that any other coworker can later
    /// resume work on that PR with the original session context. Also extracts
    /// the task ID from the PR title if present (format: "[Midtown !XXX]").
    pub fn store_pr_author_session(
        &mut self,
        pr_number: u64,
        session_id: &str,
        branch: &str,
        author: &str,
        title: &str,
    ) {
        let task_id = extract_task_id_from_title(title);
        debug!(
            "Storing author session for PR #{}: session={}, branch={}, author={}, task_id={:?}",
            pr_number, session_id, branch, author, task_id
        );
        self.pr_author_sessions.insert(
            pr_number,
            PrAuthorSession {
                session_id: session_id.to_string(),
                branch: branch.to_string(),
                original_author: author.to_string(),
                stored_at: Utc::now(),
                task_id,
            },
        );
    }

    /// Get the stored author session for a PR.
    ///
    /// Returns the session ID and branch info if available, allowing another
    /// coworker to resume work on this PR with full context.
    pub fn get_pr_author_session(&self, pr_number: u64) -> Option<&PrAuthorSession> {
        self.pr_author_sessions.get(&pr_number)
    }

    /// Remove the stored author session for a PR (e.g., when PR is merged/closed).
    pub fn remove_pr_author_session(&mut self, pr_number: u64) -> Option<PrAuthorSession> {
        self.pr_author_sessions.remove(&pr_number)
    }

    /// Maps PR number → task ID for all author sessions that have a task ID.
    pub fn pr_to_task_map(&self) -> HashMap<u64, String> {
        self.pr_author_sessions
            .iter()
            .filter_map(|(pr_number, session)| {
                session
                    .task_id
                    .as_ref()
                    .map(|task_id| (*pr_number, task_id.clone()))
            })
            .collect()
    }

    /// Maps task ID → PR number for all author sessions that have a task ID.
    pub fn task_to_pr_map(&self) -> HashMap<String, u64> {
        self.pr_author_sessions
            .iter()
            .filter_map(|(pr_number, session)| {
                session
                    .task_id
                    .as_ref()
                    .map(|task_id| (task_id.clone(), *pr_number))
            })
            .collect()
    }

    /// Record a review comment ID for a PR.
    ///
    /// Called when a completed review is detected to track which comments
    /// need to be addressed before merge is allowed.
    pub fn add_review_comment_id(&mut self, pr_number: u64, comment_id: u64) {
        let ids = self.pr_review_comment_ids.entry(pr_number).or_default();
        if !ids.contains(&comment_id) {
            ids.push(comment_id);
        }
    }

    /// Get the review comment IDs for a PR.
    pub fn get_review_comment_ids(&self, pr_number: u64) -> &[u64] {
        self.pr_review_comment_ids
            .get(&pr_number)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

/// Load GitHub state for a specific repository (legacy file).
///
/// Used only by migration code in `daemon::state`. New code should use
/// `DaemonPersistentState::load_for_repo()` instead.
pub fn load_state_for_repo(repo: &str) -> io::Result<GitHubState> {
    let path = crate::paths::github_state_file_for_repo(repo);
    GitHubState::load(&path)
}

#[path = "github_state_tests.rs"]
#[cfg(test)]
mod tests;
