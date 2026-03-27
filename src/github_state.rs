//! Persistent GitHub state for the midtown daemon.
//!
//! Stores PR review cache, webhook event timestamps, rate limits, review comment IDs,
//! and external PR tracking in a JSON file that survives daemon restarts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use tracing::{debug, warn};

use crate::persistence::JsonPersistable;

/// Persistent state for GitHub-related data.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GitHubState {
    /// Set of PR numbers that have a confirmed completed review.
    /// Review status is monotonic — once a PR has a review, it never loses it.
    /// This cache eliminates redundant `gh pr view` calls on every poll cycle.
    #[serde(default)]
    pub reviewed_prs: HashSet<u64>,

    /// Per-PR timestamp of the last webhook event that handled this PR.
    /// Polling checks this to defer to webhooks when they're healthy for a specific PR.
    #[serde(default)]
    pub pr_last_webhook_event: HashMap<u64, DateTime<Utc>>,

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

    /// Map of PR number -> external PR info for fork/cross-repo PRs.
    ///
    /// Detected via webhook (`head.repo` differs from `base.repo`) or polling
    /// (`headRepositoryOwner` differs from the base repo owner). External PRs
    /// are blocked from all daemon automation until explicitly allowed by the user.
    #[serde(default)]
    pub external_prs: HashMap<u64, ExternalPrInfo>,

    /// Set of PR numbers explicitly allowed by the user for daemon processing.
    ///
    /// Once a PR is in this set, it's treated as a normal (non-external) PR.
    #[serde(default)]
    pub allowed_external_prs: HashSet<u64>,

    /// Set of repository full names (e.g., "user/repo") allowed for daemon processing.
    ///
    /// All PRs from repos in this set bypass the external PR block.
    #[serde(default)]
    pub allowed_external_repos: HashSet<String>,
}

/// Info about an external (fork/cross-repo) PR that is blocked from daemon processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalPrInfo {
    /// PR number
    pub pr_number: u64,
    /// The source repository full name (e.g., "external-user/repo-fork")
    pub source_repo: String,
    /// The PR title
    pub title: String,
    /// When the external PR was first detected
    pub detected_at: DateTime<Utc>,
    /// Whether a channel notification has already been posted for this PR
    #[serde(default)]
    pub notified: bool,
}

impl JsonPersistable for GitHubState {}

impl GitHubState {
    /// Load state from a file, returning default if file doesn't exist.
    pub fn load(path: &Path) -> io::Result<Self> {
        Self::load_json(path)
            .inspect(|_| debug!("Loaded GitHub state"))
            .inspect_err(|e| warn!("Failed to load github-state.json: {}", e))
    }

    /// Save state to a file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        self.save_json(path)?;
        debug!("Saved GitHub state");
        Ok(())
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

    /// Check if a PR has a cached completed review result.
    pub fn has_cached_review(&self, pr_number: u64) -> bool {
        self.reviewed_prs.contains(&pr_number)
    }

    /// Mark a PR as having a completed review (cache it permanently).
    pub fn mark_reviewed_pr(&mut self, pr_number: u64) {
        self.reviewed_prs.insert(pr_number);
    }

    /// Clean up review cache entries for PRs that are no longer open.
    fn cleanup_closed_review_cache(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();
        self.reviewed_prs.retain(|pr| open_set.contains(pr));
    }

    /// Clean up state for PRs that are no longer open.
    ///
    /// Takes a list of open PR numbers and removes state for any PRs not in the list.
    pub fn cleanup_closed_prs(&mut self, open_pr_numbers: &[u64]) {
        let open_set: std::collections::HashSet<_> = open_pr_numbers.iter().collect();

        // Clean up review cache for closed PRs
        self.cleanup_closed_review_cache(open_pr_numbers);

        // Clean up per-PR webhook event timestamps for closed PRs
        self.pr_last_webhook_event
            .retain(|pr, _| open_set.contains(pr));

        // NOTE: cleanup_closed_external_prs is NOT called here because
        // cleanup_closed_prs receives a filtered list (external PRs already removed).
        // External PR cleanup is called separately with the unfiltered PR list
        // in evaluate_open_prs.
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

    /// Check if a PR is from an external/fork repo and NOT yet allowed.
    ///
    /// Returns `true` if the PR is blocked (external and not allowlisted).
    pub fn is_blocked_external_pr(&self, pr_number: u64) -> bool {
        if self.allowed_external_prs.contains(&pr_number) {
            return false;
        }
        if let Some(info) = self.external_prs.get(&pr_number) {
            if self.allowed_external_repos.contains(&info.source_repo) {
                return false;
            }
            // For polling-detected PRs with placeholder name ("owner/fork"),
            // also check if any allowed repo shares the same owner.
            if info.source_repo.ends_with("/fork") {
                let owner = info.source_repo.trim_end_matches("/fork");
                if self
                    .allowed_external_repos
                    .iter()
                    .any(|r| r.starts_with(owner) && r.as_bytes().get(owner.len()) == Some(&b'/'))
                {
                    return false;
                }
            }
            return true;
        }
        false
    }

    /// Record an external PR. Returns true if this is newly detected (not previously known).
    ///
    /// If the PR was previously recorded with a placeholder repo name (e.g. `"owner/fork"`
    /// from the polling path), updates it with the real repo name when available from webhooks.
    pub fn record_external_pr(&mut self, pr_number: u64, source_repo: &str, title: &str) -> bool {
        use std::collections::hash_map::Entry;
        match self.external_prs.entry(pr_number) {
            Entry::Occupied(mut e) => {
                // Update placeholder repo name with real name from webhook
                if e.get().source_repo.ends_with("/fork") && !source_repo.ends_with("/fork") {
                    e.get_mut().source_repo = source_repo.to_string();
                }
                false
            }
            Entry::Vacant(e) => {
                e.insert(ExternalPrInfo {
                    pr_number,
                    source_repo: source_repo.to_string(),
                    title: title.to_string(),
                    detected_at: Utc::now(),
                    notified: false,
                });
                true
            }
        }
    }

    /// Mark an external PR as having been notified in the channel.
    pub fn mark_external_pr_notified(&mut self, pr_number: u64) {
        if let Some(info) = self.external_prs.get_mut(&pr_number) {
            info.notified = true;
        }
    }

    /// Allow a specific external PR for daemon processing.
    pub fn allow_external_pr(&mut self, pr_number: u64) {
        self.allowed_external_prs.insert(pr_number);
    }

    /// Allow all PRs from a specific repository.
    pub fn allow_external_repo(&mut self, repo: &str) {
        self.allowed_external_repos.insert(repo.to_string());
    }

    /// Clean up external PR entries for PRs that are no longer open.
    pub fn cleanup_closed_external_prs(&mut self, open_pr_numbers: &[u64]) {
        let open_set: HashSet<_> = open_pr_numbers.iter().collect();
        self.external_prs.retain(|pr, _| open_set.contains(pr));
        self.allowed_external_prs.retain(|pr| open_set.contains(pr));
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
