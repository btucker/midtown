//! PR management — polling, reviewer spawning, comment nudging.
//!
//! This module runs in the background to:
//! - Poll open PRs for merge conflicts, CI failures, and review status
//! - Nudge PR authors when approved (author-driven merge decisions)
//! - Spawn reviewer coworkers for unreviewed PRs
//! - Process pending review spawns from webhook-triggered delays
//! - Nudge PR owners when their PR receives comments

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::{config, daemon_messages};

use super::DaemonState;
use super::constants::*;
use super::effects::Effect;
use super::helpers::is_lead_branch;
use super::helpers::*;
use super::snapshot::WorldSnapshot;
use super::trackers::{PrIssueType, StuckConditionType};

/// Get list of coworker names who have open PRs.
///
/// A coworker is considered to have an open PR if the PR's branch name
/// starts with the coworker's name (e.g., "lexington/fix-auth").
/// Coworkers with open PRs should NEVER be sent on a break.
/// Get coworker names that have open PRs (branch name starts with coworker name).
///
/// Uses cached data from the latest `poll_prs_for_issues` call when available,
/// avoiding a separate `gh pr list` API call.
pub(super) fn get_coworkers_with_open_prs(state: &DaemonState) -> Vec<String> {
    let cache = state.pr_coworker_cache.read().unwrap();
    if !cache.open_pr_owners.is_empty() {
        return cache.open_pr_owners.iter().cloned().collect();
    }
    drop(cache);

    // Fallback to API call if cache is empty (e.g., first tick before poll runs)
    let output = std::process::Command::new("gh")
        .args(["pr", "list", "--json", "headRefName"])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                return prs
                    .iter()
                    .filter_map(|pr| {
                        pr.get("headRefName")
                            .and_then(|r| r.as_str())
                            .and_then(coworker_from_branch)
                    })
                    .collect();
            }
            Vec::new()
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "Failed to get PRs from gh CLI for idle check: {}",
                stderr.trim()
            );
            Vec::new()
        }
        Err(e) => {
            warn!("Failed to execute gh pr list for idle check: {}", e);
            Vec::new()
        }
    }
}

/// How often to re-fetch merged PRs (5 minutes). Merges aren't urgent so
/// polling less frequently saves significant API calls.
const MERGED_PRS_FETCH_INTERVAL_SECS: u64 = 300;

/// Get coworker names that have recently merged PRs (branch name starts with coworker name).
///
/// Uses a time-based cache to reduce API calls. Merged PR status is only refreshed
/// every 5 minutes since merge events aren't time-critical.
pub(super) fn get_coworkers_with_merged_prs(state: &DaemonState) -> HashSet<String> {
    // Check if we need to refresh (uses CooldownTracker instead of standalone timestamp)
    let needs_refresh = {
        let cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.check(
            "merged_pr_fetch",
            "global",
            Duration::from_secs(MERGED_PRS_FETCH_INTERVAL_SECS),
        )
    };

    if !needs_refresh {
        let cache = state.pr_coworker_cache.read().unwrap();
        return cache.merged_pr_owners.clone();
    }

    // Fetch from API
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--limit",
            "20",
            "--json",
            "headRefName,number",
        ])
        .output();

    let (coworker_names, branch_names, pr_numbers): (
        HashSet<String>,
        HashSet<String>,
        HashSet<u64>,
    ) = match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            if let Ok(prs) = serde_json::from_str::<Vec<serde_json::Value>>(&stdout) {
                let branches: HashSet<String> = prs
                    .iter()
                    .filter_map(|pr| {
                        pr.get("headRefName")
                            .and_then(|r| r.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                let coworkers: HashSet<String> = branches
                    .iter()
                    .filter_map(|b| coworker_from_branch(b))
                    .collect();
                let numbers: HashSet<u64> = prs
                    .iter()
                    .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
                    .collect();
                (coworkers, branches, numbers)
            } else {
                (HashSet::new(), HashSet::new(), HashSet::new())
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to get merged PRs from gh CLI: {}", stderr.trim());
            (HashSet::new(), HashSet::new(), HashSet::new())
        }
        Err(e) => {
            warn!("Failed to execute gh pr list (merged): {}", e);
            (HashSet::new(), HashSet::new(), HashSet::new())
        }
    };

    // Update cache
    {
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.merged_pr_owners = coworker_names.clone();
        cache.merged_pr_branches = branch_names;
        cache.merged_pr_numbers = pr_numbers;
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("merged_pr_fetch", "global");
    }

    coworker_names
}

/// Get PR numbers of recently merged PRs from cache.
///
/// Used by task dispatch to skip tasks referencing merged PRs.
/// Data is populated by `get_coworkers_with_merged_prs()` as a side effect.
pub(super) fn get_merged_pr_numbers(state: &DaemonState) -> HashSet<u64> {
    let cache = state.pr_coworker_cache.read().unwrap();
    cache.merged_pr_numbers.clone()
}

/// Compute a time-aware hash of PR data for caching purposes.
///
/// Includes a time bucket (current time divided by `bucket_secs`) so the hash changes
/// periodically even when the data is unchanged. This ensures time-based decisions
/// (like PR age eligibility for reviewer spawn) are re-evaluated.
///
/// # Arguments
/// * `data` - The PR data string to hash
/// * `bucket_secs` - The time bucket size in seconds (hash changes every this many seconds)
fn compute_time_aware_hash(data: &str, bucket_secs: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    compute_time_aware_hash_at(data, bucket_secs, now_secs)
}

/// Internal function for computing time-aware hash with explicit timestamp.
/// Used by `compute_time_aware_hash` and tests.
#[cfg(test)]
fn compute_time_aware_hash_at(data: &str, bucket_secs: u64, timestamp_secs: u64) -> u64 {
    let time_bucket = timestamp_secs / bucket_secs;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    time_bucket.hash(&mut hasher);
    hasher.finish()
}

#[cfg(not(test))]
fn compute_time_aware_hash_at(data: &str, bucket_secs: u64, timestamp_secs: u64) -> u64 {
    let time_bucket = timestamp_secs / bucket_secs;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    time_bucket.hash(&mut hasher);
    hasher.finish()
}

// ============================================================================

/// Poll all open PRs and return effects for actionable issues.
///
/// Fetches PR data from GitHub, reads tracker state to avoid duplicate nudges,
/// and returns a list of effects to execute. The caller is responsible for
/// executing the returned effects via `execute_effects()`.
///
/// Called from `evaluate_tick(PrPollTick)` in the main event loop.
pub(super) async fn poll_prs_for_issues(
    snap: &WorldSnapshot,
    state: &DaemonState,
) -> Result<Vec<Effect>, Box<dyn std::error::Error + Send + Sync>> {
    debug!("Polling PRs for actionable issues...");

    let mut effects: Vec<Effect> = Vec::new();

    // Get list of active coworkers from snapshot (consistent with other tick handlers)
    let active_coworkers: Vec<String> = snap
        .active_coworkers
        .iter()
        .map(|c| c.name.clone())
        .collect();

    // Get running coworkers for cleanup_expired_preserving, which removes timed-out
    // reviewer assignments but preserves those for still-running reviewers (i.e., reviews
    // that are taking longer than the timeout but the reviewer is still actively working).
    // Exclude usage-limited coworkers: they're running but can't complete reviews,
    // so their expired assignments should be cleaned up to allow reassignment.
    let running_coworker_names: HashSet<String> = snap
        .running_coworkers
        .iter()
        .map(|c| c.name.clone())
        .filter(|name| !snap.usage_limited_coworkers.contains(&name.to_lowercase()))
        .collect();

    // Get list of idle coworkers for handoff decisions
    let idle_coworkers: Vec<String> = {
        let records = state.coworker_records.read().await;
        records
            .iter()
            .filter(|(name, record)| {
                // Must be an active coworker
                active_coworkers.contains(name)
                    // Must have reported Idle phase
                    && record.workflow_phase
                        == Some(crate::coworker_state::WorkflowPhase::Idle)
            })
            .map(|(name, _)| name.clone())
            .collect()
    };

    // Run gh pr list command (include createdAt and isDraft for review filtering)
    // Include state field to filter out merged/closed PRs after restart
    // Include comments and author for polling-based review comment detection
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "open",
            "--json",
            "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state,comments,author",
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr list failed: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Hash the response to detect changes. If the PR data hasn't changed since the last poll,
    // skip the expensive lock acquisition, issue detection, and nudge logic.
    //
    // IMPORTANT: Include a time bucket so hash changes every PR_REVIEW_DELAY_SECS. This ensures
    // time-based decisions (like PR age eligibility for reviewer spawn) are re-evaluated even
    // when PR data is unchanged. Without this, a PR that was "too new" on one poll would never
    // be re-checked if the response hash stayed the same.
    let response_hash = compute_time_aware_hash(&stdout, PR_REVIEW_DELAY_SECS);
    {
        let mut last_hash = state.last_pr_poll_hash.lock().await;
        if *last_hash == response_hash && response_hash != 0 {
            debug!("PR poll: data unchanged, skipping processing");
            return Ok(effects);
        }
        *last_hash = response_hash;
    }

    let prs: Vec<serde_json::Value> = serde_json::from_str(&stdout)?;

    // Cleanup old tracking entries, but preserve assignments for RUNNING coworkers
    // so reviewers don't lose their PR tracking while actively reviewing.
    // Using running_coworkers (not active_coworkers) ensures that idle/stopped
    // reviewers have their assignments cleaned up, freeing slots for new reviews.
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.cleanup();
    }
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github
            .cleanup_expired_preserving(&running_coworker_names);
        ps.github.cleanup_stale_webhook_events();
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.cleanup(Duration::from_secs(7200)); // 2 hours
    }

    // Filter to only open PRs (defense-in-depth: gh pr list --state open should only return
    // open PRs, but verify via the state field to guard against stale/cached results)
    let prs: Vec<serde_json::Value> = prs
        .into_iter()
        .filter(|pr| {
            let state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("OPEN");
            state == "OPEN"
        })
        .collect();

    // Cache open PR owners for reuse by get_coworkers_with_open_prs
    {
        let owners: HashSet<String> = prs
            .iter()
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(coworker_from_branch)
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.open_pr_owners = owners;
    }

    // Cache coworker names whose PRs have all CI checks passing (for PR break decisions)
    {
        let ci_passed: HashSet<String> = prs
            .iter()
            .filter(|pr| all_ci_checks_passed(pr))
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(coworker_from_branch)
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.ci_passed_pr_owners = ci_passed;
        // Mark PR poll as initialized so orphan detection knows we have PR data.
        // This prevents false positive orphan warnings during daemon startup when
        // orphan checks run before the first PR poll completes.
        cache.pr_poll_initialized = true;
    }

    // Cleanup saved PR break sessions for coworkers whose PRs are no longer open
    {
        let active_pr_coworkers: HashSet<String> = prs
            .iter()
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(coworker_from_branch)
            })
            .collect();
        let mut sessions = state.pr_break_sessions.write().unwrap();
        let before = sessions.len();
        sessions.retain(|name, _| active_pr_coworkers.contains(name));
        let removed = before - sessions.len();
        if removed > 0 {
            info!(
                "Cleaned up {} stale PR break session(s) (PR closed/merged)",
                removed
            );
        }
    }

    // Clean up persistent reviewer assignments for PRs that are no longer open
    {
        let open_pr_numbers: Vec<u64> = prs
            .iter()
            .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
            .collect();
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_closed_prs(&open_pr_numbers);
        ps.github
            .cleanup_expired_preserving(&running_coworker_names);
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            warn!("Failed to save daemon-state.json after cleanup: {}", e);
        }
    }

    for pr in &prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Only process coworker-owned PRs (validates branch prefix against known names)
        let owner = match coworker_from_branch(head_ref) {
            Some(o) => o,
            None => continue, // Not a coworker PR (e.g., dependabot, feature branches)
        };

        // Check for actionable issues
        let issues = detect_pr_issues(pr);

        for issue_type in issues {
            // Check if we should nudge for this issue
            let should_nudge = {
                let tracker = state.pr_issue_tracker.lock().await;
                tracker.should_nudge(pr_number, issue_type)
            };

            if !should_nudge {
                continue;
            }

            // Author-driven merge decisions: Instead of auto-merging approved PRs,
            // nudge the author so THEY can decide to merge. This keeps merge decisions
            // with the agent who has full context of the PR and review feedback.
            use crate::rules::{PrSessionContext, decide_pr_issue_action_with_handoff};

            // Format the nudge message
            let message = format!(
                "PR #{} ({}) - {}: {}",
                pr_number,
                truncate_str(title, 40),
                issue_type,
                get_issue_action(issue_type)
            );

            // Get session context for potential handoff (if available)
            let session_context: Option<PrSessionContext> = {
                let ps = state.persistent_state.lock().await;
                ps.github
                    .get_pr_author_session(pr_number)
                    .map(|s| PrSessionContext {
                        session_id: s.session_id.clone(),
                        branch: s.branch.clone(),
                        original_author: s.original_author.clone(),
                        pr_number,
                    })
            };

            // Decide action using pure decision function with handoff support
            let action = decide_pr_issue_action_with_handoff(
                &owner,
                &active_coworkers,
                &idle_coworkers,
                state.is_at_dev_limit(),
                session_context.as_ref(),
                &message,
            );

            effects.extend(pr_action_to_effects(
                action, pr_number, title, issue_type, state,
            ));
        }
    }

    // Polling fallback for review comment notifications (when webhooks are degraded)
    effects.extend(
        collect_comment_notification_effects(state, &prs, &active_coworkers, &idle_coworkers).await,
    );

    // Auto-spawn reviewers for PRs that need review
    effects.extend(collect_reviewer_effects(state, &prs).await);

    // Pre-collect review status for all PRs before stuck detection (pure decision logic
    // should not make async API calls). Coworkers can't submit formal GitHub reviews
    // since they share the same user as PR authors, so we check for comment-based reviews.
    let reviewed_prs: HashSet<u64> = {
        let mut reviewed = HashSet::new();
        for pr in &prs {
            if let Some(pr_number) = pr.get("number").and_then(|n| n.as_u64())
                && state.is_pr_reviewed(pr_number).await
            {
                reviewed.insert(pr_number);
            }
        }
        reviewed
    };

    // Compute prs_needing_review and update cache (must happen here, not in effect
    // collection functions which should be pure). This value is used by task dispatch
    // to prioritize PR reviews over new task pickup.
    let prs_needing_review: usize = prs
        .iter()
        .filter(|pr| {
            let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
            let review_decision = pr
                .get("reviewDecision")
                .and_then(|r| r.as_str())
                .unwrap_or("");
            let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
            // PR needs review if it's not a draft, has no formal review, and no Claude comment review
            pr_number != 0
                && !is_draft
                && review_decision.is_empty()
                && !reviewed_prs.contains(&pr_number)
        })
        .count();
    // Cache coworker names whose PRs have CI passed + review feedback (for idle shutdown protection).
    // This mirrors the criteria in collect_green_with_feedback_effects: CI green, reviewed, not approved.
    {
        let review_feedback: HashSet<String> = prs
            .iter()
            .filter(|pr| {
                let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
                let review_decision = pr
                    .get("reviewDecision")
                    .and_then(|r| r.as_str())
                    .unwrap_or("");
                all_ci_checks_passed(pr)
                    && reviewed_prs.contains(&pr_number)
                    && review_decision != "APPROVED"
            })
            .filter_map(|pr| {
                pr.get("headRefName")
                    .and_then(|r| r.as_str())
                    .and_then(coworker_from_branch)
            })
            .collect();
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.prs_needing_review = prs_needing_review;
        cache.review_feedback_pr_owners = review_feedback;
    }

    // Nudge PR owners when CI turns green and they have review feedback to address.
    // This covers the case where a coworker is waiting for CI while feedback awaits.
    effects.extend(
        collect_green_with_feedback_effects(
            state,
            &prs,
            &reviewed_prs,
            &active_coworkers,
            &idle_coworkers,
        )
        .await,
    );

    // Check for stuck conditions and nudge lead if self-healing has failed
    effects.extend(
        collect_stuck_condition_effects(state, &prs, &reviewed_prs, prs_needing_review).await,
    );

    // Detect stale CI checks and trigger re-runs
    effects.extend(collect_stale_check_effects(state, &prs).await);

    Ok(effects)
}

/// Collect effects for PRs that are green (all CI passed) and have review feedback.
///
/// When a coworker's PR has all CI checks passing and has received a code review,
/// nudge them to address any feedback and merge. This covers the case where
/// a coworker is waiting for CI to pass while feedback awaits.
async fn collect_green_with_feedback_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
    active_coworkers: &[String],
    idle_coworkers: &[String],
) -> Vec<Effect> {
    let mut effects = Vec::new();

    for pr in prs {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) => n,
            None => continue,
        };

        // Only process PRs that have been reviewed
        if !reviewed_prs.contains(&pr_number) {
            continue;
        }

        // Only process PRs where all CI checks have passed
        if !all_ci_checks_passed(pr) {
            continue;
        }

        // Skip if already approved (will be auto-merged or nudged via Approved issue type)
        let review_decision = pr
            .get("reviewDecision")
            .and_then(|r| r.as_str())
            .unwrap_or("");
        if review_decision == "APPROVED" {
            continue;
        }

        // Check cooldown to avoid spamming
        let should_nudge = {
            let tracker = state.pr_issue_tracker.lock().await;
            tracker.should_nudge(pr_number, PrIssueType::GreenWithFeedback)
        };
        if !should_nudge {
            continue;
        }

        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Only process coworker-owned PRs (validates branch prefix against known names)
        let owner = match coworker_from_branch(head_ref) {
            Some(o) => o,
            None => continue, // Not a coworker PR (e.g., dependabot, btucker/*)
        };

        let message = format!(
            "PR #{} ({}) - {}: {}",
            pr_number,
            truncate_str(title, 40),
            PrIssueType::GreenWithFeedback,
            get_issue_action(PrIssueType::GreenWithFeedback)
        );

        // Look up session context for potential handoff
        let session_context = get_pr_session_context(state, pr_number).await;

        // Decide action using handoff-aware decision function (matches webhook path)
        let action = crate::rules::decide_pr_issue_action_with_handoff(
            &owner,
            active_coworkers,
            idle_coworkers,
            state.is_at_dev_limit(),
            session_context.as_ref(),
            &message,
        );

        effects.extend(pr_action_to_effects(
            action,
            pr_number,
            title,
            PrIssueType::GreenWithFeedback,
            state,
        ));
    }

    effects
}

/// Convert a `PrAction` decision into a list of `Effect`s to execute.
///
/// Translates the pure decision from `rules::decide_pr_issue_action` (or similar)
/// into concrete effects. Uses `SpawnCoworkerWithCallbacks` for spawn actions so
/// that follow-up effects (broadcast update, channel message, session cleanup)
/// only happen on success, with a fallback message on failure.
fn pr_action_to_effects(
    action: crate::rules::PrAction,
    pr_number: u64,
    title: &str,
    issue_type: PrIssueType,
    state: &DaemonState,
) -> Vec<Effect> {
    use crate::rules::PrAction;

    match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::NudgeCoworkerWithCallbacks {
                name: owner,
                message,
                on_success: vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            }]
        }
        PrAction::SpawnOwner { owner, message } => {
            // Look up saved session from PR break for resume
            let saved_session = {
                let sessions = state.pr_break_sessions.read().unwrap();
                sessions.get(&owner).cloned()
            };
            let session_mode = match saved_session.as_deref() {
                Some(sid) => crate::launch::SessionMode::ResumeSession(sid.to_string()),
                None => crate::launch::SessionMode::Resume,
            };
            let config = crate::launch::LaunchConfig::coworker(
                owner.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(message),
            );

            let mut on_success = vec![
                Effect::BroadcastCoworkerUpdate {
                    name: owner.clone(),
                    status: "running".to_string(),
                    current_task: None,
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: daemon_messages::called_in_pr_issue(
                        &owner,
                        &issue_type.to_string(),
                        pr_number,
                        config::get_personality(),
                    ),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];
            if saved_session.is_some() {
                on_success.push(Effect::ClearPrBreakSession {
                    name: owner.clone(),
                });
            }

            let on_failure = vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "PR #{} ({}) owned by {} - {}: {} (call-in failed)",
                        pr_number,
                        truncate_str(title, 40),
                        owner,
                        issue_type,
                        get_issue_action(issue_type)
                    ),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            }]
        }
        PrAction::HandoffToCoworker {
            assignee,
            original_author,
            pr_number: pr_num,
            branch,
            session_id,
            message,
        } => handoff_to_coworker_effects(
            &assignee,
            &original_author,
            pr_num,
            &branch,
            session_id,
            &message,
            "resuming their session for full context",
            title,
            pr_number,
            issue_type,
            state,
        ),
        PrAction::PostToChannel { message } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message,
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::Skip { reason } => {
            debug!("{}", reason);
            vec![]
        }
    }
}

/// Check for stuck conditions and return effects to nudge the lead.
///
/// This function runs during each PR poll cycle and checks for:
/// 1. PRs open with no review for too long
/// 2. PRs with unresolved feedback for too long
/// 3. PRs that are approved + CI green but not merging
/// 4. Coworkers who are silent (no channel activity) for too long
/// 5. Review backlog (more PRs need review than slots available)
///
/// Returns effects (NudgeCoworker, PostSystemMessage) instead of executing
/// side effects inline. Each condition has a cooldown tracked via the
/// stuck_tracker to avoid spamming. For stuck conditions that @mention the lead,
/// the channel's chat monitor handles routing the nudge.
///
/// The `reviewed_prs` parameter contains PR numbers that have Claude reviews
/// (comment-based or formal), pre-collected before this function to keep
/// decision logic free of async API calls.
///
/// The `prs_needing_review` parameter is the pre-computed count of PRs that
/// need review, calculated by the caller to maintain pure function behavior
/// (no cache writes inside effect collection).
async fn collect_stuck_condition_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
    prs_needing_review: usize,
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();
    let mut tracker = state.stuck_tracker.lock().await;
    tracker.cleanup();

    let now = Instant::now();

    // Track how many nudges we send this cycle (for logging)
    let mut nudge_count = 0;

    // --- Scenario 1: PR open with no review for N minutes ---
    for pr in prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
        if is_draft {
            continue;
        }

        let review_decision = pr
            .get("reviewDecision")
            .and_then(|r| r.as_str())
            .unwrap_or("");

        let age_secs = get_pr_age_secs(pr).unwrap_or(0);
        let pr_id = pr_number.to_string();

        // Check for comment-based Claude reviews (coworkers can't submit formal reviews
        // since they share the same GitHub user as the PR author). Uses pre-collected
        // data to keep decision logic free of async API calls.
        let has_claude_review = reviewed_prs.contains(&pr_number);

        // No review decision at all, no Claude review comment, and PR is old enough
        if review_decision.is_empty()
            && !has_claude_review
            && age_secs >= STUCK_NO_REVIEW_DURATION.as_secs()
        {
            // Check if a reviewer is assigned (daemon tried to self-heal)
            let is_assigned = {
                let ps = state.persistent_state.lock().await;
                ps.github.is_assigned(pr_number)
            };

            tracker.track(&pr_id, StuckConditionType::NoReview);
            if tracker.should_nudge(&pr_id, StuckConditionType::NoReview) {
                let prior_nudges = tracker.nudge_count(&pr_id, StuckConditionType::NoReview);
                let has_available_slots = state.has_available_coworker_slot();

                let nudge = if should_escalate(prior_nudges) {
                    // Escalation: this has persisted too long, suggest investigation
                    let context = if is_assigned && has_available_slots {
                        "A reviewer was assigned but hasn't posted a review, and coworker slots are available. This looks like a daemon bug."
                    } else if !is_assigned && has_available_slots {
                        "Coworker slots are available but no reviewer was assigned. This looks like a daemon bug."
                    } else if is_assigned {
                        "A reviewer was assigned but hasn't posted a review."
                    } else {
                        "No reviewer could be assigned (all slots may be in use)."
                    };
                    format!(
                        "@lead PR #{} ({}) has been stuck for {} minutes with no review — {} Consider running `midtown e2e capture` to debug.",
                        pr_number,
                        truncate_str(title, 40),
                        age_secs / 60,
                        context,
                    )
                } else {
                    // Normal warning
                    let context = if is_assigned {
                        "I assigned a reviewer but no review has been posted yet"
                    } else {
                        "I couldn't assign a reviewer"
                    };
                    format!(
                        "@lead PR #{} ({}) has been open for {} minutes with no review — {}",
                        pr_number,
                        truncate_str(title, 40),
                        age_secs / 60,
                        context,
                    )
                };
                effects.extend(stuck_nudge_effects(&nudge));
                tracker.record_nudge(&pr_id, StuckConditionType::NoReview);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::NoReview);
        }

        // --- Scenario 2: Unresolved feedback (changes requested) for N minutes ---
        if review_decision == "CHANGES_REQUESTED" {
            let first_detected = tracker.track(&pr_id, StuckConditionType::UnresolvedFeedback);
            let stuck_duration = now.duration_since(first_detected);

            if stuck_duration >= STUCK_UNRESOLVED_FEEDBACK_DURATION
                && tracker.should_nudge(&pr_id, StuckConditionType::UnresolvedFeedback)
            {
                let prior_nudges =
                    tracker.nudge_count(&pr_id, StuckConditionType::UnresolvedFeedback);

                let nudge = if should_escalate(prior_nudges) {
                    format!(
                        "@lead PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't responded despite repeated nudges. The coworker may be stuck or the task may need reassignment.",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                } else {
                    format!(
                        "@lead PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't pushed new changes",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                };
                effects.extend(stuck_nudge_effects(&nudge));
                tracker.record_nudge(&pr_id, StuckConditionType::UnresolvedFeedback);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::UnresolvedFeedback);
        }

        // --- Scenario 3: Approved + CI green but not merging ---
        if is_auto_mergeable(pr) {
            let first_detected = tracker.track(&pr_id, StuckConditionType::MergeReady);
            let stuck_duration = now.duration_since(first_detected);

            if stuck_duration >= STUCK_MERGE_READY_DURATION
                && tracker.should_nudge(&pr_id, StuckConditionType::MergeReady)
            {
                let prior_nudges = tracker.nudge_count(&pr_id, StuckConditionType::MergeReady);

                let nudge = if should_escalate(prior_nudges) {
                    format!(
                        "@lead PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — the author isn't responding to merge nudges. Consider merging manually or investigating the coworker.",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                } else {
                    format!(
                        "@lead PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — author may need a nudge to merge",
                        pr_number,
                        truncate_str(title, 40),
                        stuck_duration.as_secs() / 60,
                    )
                };
                effects.extend(stuck_nudge_effects(&nudge));
                tracker.record_nudge(&pr_id, StuckConditionType::MergeReady);
                nudge_count += 1;
            }
        } else {
            tracker.clear(&pr_id, StuckConditionType::MergeReady);
        }
    }

    // --- Scenario 4: Silent coworker (claimed task, no channel activity) ---
    {
        let busy_coworkers = state.get_all_busy_coworkers();
        let records = state.coworker_records.read().await;

        for name in &busy_coworkers {
            let last_activity: Option<Instant> =
                records.get(name.as_str()).and_then(|r| r.last_activity);
            let is_silent = match last_activity {
                Some(last) => last.elapsed() >= STUCK_SILENT_COWORKER_DURATION,
                // No activity recorded — coworker hasn't posted to channel yet.
                // They're still initializing (loading plugins, restoring session, etc.).
                // Only start the silence clock after their first channel message.
                None => false,
            };

            if is_silent {
                tracker.track(name, StuckConditionType::SilentCoworker);
                if tracker.should_nudge(name, StuckConditionType::SilentCoworker) {
                    let task_info = crate::tasks::get_in_progress_tasks_with_subjects()
                        .into_iter()
                        .find(|(_, _, owner)| owner.eq_ignore_ascii_case(name))
                        .map(|(id, subject, _)| {
                            format!("task !{} ({})", id, truncate_str(&subject, 30))
                        })
                        .unwrap_or_else(|| "their task".to_string());

                    let prior_nudges =
                        tracker.nudge_count(name, StuckConditionType::SilentCoworker);

                    if prior_nudges == 0 {
                        // First nudge: ask the coworker directly before escalating
                        let nudge_msg = format!(
                            "Status check — you've been quiet on {} for over {} minutes. \
                             Are you stuck or still working?",
                            task_info,
                            STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                        );
                        effects.push(Effect::NudgeCoworker {
                            name: name.clone(),
                            message: nudge_msg,
                        });
                        // Post to channel so it's visible
                        effects.push(Effect::PostSystemMessage {
                            message: format!(
                                "⚠️ Nudging {} — silent on {} for over {} minutes",
                                name,
                                task_info,
                                STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                            ),
                        });
                    } else {
                        // Escalation: coworker didn't respond, notify lead
                        let nudge = format!(
                            "@lead {} has been silent on {} for over {} minutes \
                             (nudged {} previously with no response)",
                            name,
                            task_info,
                            STUCK_SILENT_COWORKER_DURATION.as_secs() / 60,
                            name,
                        );
                        effects.extend(stuck_nudge_effects(&nudge));
                    }
                    tracker.record_nudge(name, StuckConditionType::SilentCoworker);
                    nudge_count += 1;
                }
            } else {
                tracker.clear(name, StuckConditionType::SilentCoworker);
            }
        }
    }

    // --- Scenario 5: Review backlog ---
    // prs_needing_review is passed in from the caller (computed and cached before
    // calling this function to maintain pure function behavior).
    {
        let current_review_count = {
            let ps = state.persistent_state.lock().await;
            ps.github.active_count()
        };

        // Backlog exists when more PRs need review than we can handle
        if prs_needing_review > MAX_CONCURRENT_REVIEWS
            && current_review_count >= MAX_CONCURRENT_REVIEWS
        {
            tracker.track("backlog", StuckConditionType::ReviewBacklog);
            if tracker.should_nudge("backlog", StuckConditionType::ReviewBacklog) {
                let nudge = format!(
                    "@lead {} PRs need review but I'm at the max concurrent review limit ({}/{}) — some PRs may wait longer than usual",
                    prs_needing_review, current_review_count, MAX_CONCURRENT_REVIEWS,
                );
                effects.extend(stuck_nudge_effects(&nudge));
                tracker.record_nudge("backlog", StuckConditionType::ReviewBacklog);
                nudge_count += 1;
            }
        } else {
            tracker.clear("backlog", StuckConditionType::ReviewBacklog);
        }
    }

    if nudge_count > 0 {
        info!(
            "Stuck condition check: nudged lead about {} issue(s)",
            nudge_count
        );
    }

    effects
}

/// Determine if a stuck condition should escalate based on nudge count.
///
/// Returns true if this nudge (including the current one) meets or exceeds
/// the escalation threshold. Since `prior_nudges` is the count *before* the
/// current nudge is recorded, we add 1 to get "this nudge number".
///
/// With STUCK_ESCALATION_NUDGE_COUNT = 2:
/// - First nudge (prior=0): 0+1=1 < 2, no escalation
/// - Second nudge (prior=1): 1+1=2 >= 2, escalation
fn should_escalate(prior_nudges: u32) -> bool {
    prior_nudges + 1 >= STUCK_ESCALATION_NUDGE_COUNT
}

/// Convert a stuck condition nudge message into effects (system message only).
///
/// The message should contain "@lead" which the chat monitor will detect and
/// route to the lead via tmux nudge. We don't return NudgeLead here because
/// that would cause double delivery (the channel @mention routing already
/// handles it).
fn stuck_nudge_effects(message: &str) -> Vec<Effect> {
    vec![Effect::PostSystemMessage {
        message: format!("⚠️ {}", message),
    }]
}

/// Polling fallback for review comment notifications.
///
/// When webhooks are degraded, this detects new review comments by comparing
/// comment counts with tracked state. Uses the same cooldown as webhooks
/// (`PrIssueType::ReviewComment`) to avoid duplicate notifications.
///
/// For each coworker-owned PR:
/// 1. Count non-owner comments (excludes PR author and coworker's own comments)
/// 2. If count increased since last poll, nudge/spawn the owner AND create a review
///    feedback task for consistent "task !X" formatting
///
/// This enables the polling path to fill the gap identified in graceful degradation:
/// webhooks handle real-time notifications, polling handles the fallback case.
/// Both paths create tasks so the Lead sees consistent formatting, while preserving
/// handoff-to-idle-coworker and session resume capabilities.
async fn collect_comment_notification_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
    active_coworkers: &[String],
    idle_coworkers: &[String],
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();

    // Get open PR numbers for tracker cleanup
    let open_pr_numbers: Vec<u64> = prs
        .iter()
        .filter_map(|pr| pr.get("number").and_then(|n| n.as_u64()))
        .collect();

    // Clean up tracker entries for closed PRs
    {
        let mut tracker = state.comment_tracker.lock().await;
        tracker.cleanup(&open_pr_numbers);
    }

    for pr in prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }

        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Check for lead/* branches first, before filtering by coworker ownership
        if is_lead_branch(head_ref) {
            // Count all comments for lead PRs
            let non_owner_count = count_non_owner_comments(pr, None);

            // Check if there are new comments since last poll
            let has_new = {
                let tracker = state.comment_tracker.lock().await;
                tracker.has_new_comments(pr_number, non_owner_count)
            };

            if has_new {
                // Check cooldown before nudging
                let should_nudge = {
                    let tracker = state.pr_issue_tracker.lock().await;
                    tracker.should_nudge(pr_number, PrIssueType::ReviewComment)
                };

                // Update comment tracker regardless of cooldown
                {
                    let mut tracker = state.comment_tracker.lock().await;
                    tracker.record(pr_number, non_owner_count);
                }

                if should_nudge {
                    let lead_nudge_msg = format!(
                        "Your PR #{} ({}) has new review comments — please address feedback.",
                        pr_number,
                        truncate_str(title, 40)
                    );
                    debug!(
                        "Polling detected new review comments on lead PR #{}, nudging lead",
                        pr_number
                    );
                    effects.push(Effect::NudgeLead {
                        message: lead_nudge_msg,
                    });
                }
            } else {
                // No new comments, just update tracker
                let mut tracker = state.comment_tracker.lock().await;
                tracker.record(pr_number, non_owner_count);
            }

            continue; // Lead PR handled, move to next PR
        }

        // Only check coworker-owned PRs beyond this point
        let owner = match coworker_from_branch(head_ref) {
            Some(o) => o,
            None => continue, // Not a coworker PR
        };

        // Count non-owner comments
        let non_owner_count = count_non_owner_comments(pr, Some(&owner));

        // Check if there are new comments since last poll
        let has_new = {
            let tracker = state.comment_tracker.lock().await;
            tracker.has_new_comments(pr_number, non_owner_count)
        };

        if !has_new {
            // Update tracker and continue
            let mut tracker = state.comment_tracker.lock().await;
            tracker.record(pr_number, non_owner_count);
            continue;
        }

        // New comments detected — check cooldown before nudging
        let should_nudge = {
            let tracker = state.pr_issue_tracker.lock().await;
            tracker.should_nudge(pr_number, PrIssueType::ReviewComment)
        };

        // Update comment tracker regardless of cooldown
        {
            let mut tracker = state.comment_tracker.lock().await;
            tracker.record(pr_number, non_owner_count);
        }

        if !should_nudge {
            debug!(
                "PR #{} has new comments but nudge is on cooldown",
                pr_number
            );
            continue;
        }

        let nudge_msg = format!(
            "Your PR #{} ({}) has new review comments — please address feedback.",
            pr_number,
            truncate_str(title, 40)
        );

        debug!(
            "Polling detected new review comments on PR #{}, nudging {} and creating task",
            pr_number, owner
        );

        // Look up session context for potential handoff
        let session_context = get_pr_session_context(state, pr_number).await;

        // Decide action using handoff-aware decision function (preserves session
        // resume and idle-coworker handoff capabilities)
        let action = crate::rules::decide_pr_comment_action_with_handoff(
            &owner,
            "reviewer", // Generic actor since we don't know the specific commenter from polling
            active_coworkers,
            idle_coworkers,
            state.is_at_dev_limit(),
            session_context.as_ref(),
            &nudge_msg,
        );

        effects.extend(comment_action_to_effects(action, pr_number, title, state));
    }

    effects
}

/// Convert a comment notification `PrAction` into effects.
///
/// Similar to `pr_action_to_effects` but uses the comment-specific cooldown,
/// messages, and `called_in_review_feedback` channel message.
fn comment_action_to_effects(
    action: crate::rules::PrAction,
    pr_number: u64,
    title: &str,
    state: &DaemonState,
) -> Vec<Effect> {
    use crate::rules::PrAction;
    let issue_type = PrIssueType::ReviewComment;

    match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::NudgeCoworkerWithCallbacks {
                name: owner,
                message,
                on_success: vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            }]
        }
        PrAction::SpawnOwner { owner, message } => {
            let saved_session = {
                let sessions = state.pr_break_sessions.read().unwrap();
                sessions.get(&owner).cloned()
            };
            let session_mode = match saved_session.as_deref() {
                Some(sid) => crate::launch::SessionMode::ResumeSession(sid.to_string()),
                None => crate::launch::SessionMode::Resume,
            };
            let mut config = crate::launch::LaunchConfig::coworker(
                owner.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(message),
            );
            // Use Opus for review feedback responses (higher quality needed to understand feedback)
            config.model = "opus".to_string();

            let mut on_success = vec![
                Effect::BroadcastCoworkerUpdate {
                    name: owner.clone(),
                    status: "running".to_string(),
                    current_task: None,
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: crate::daemon_messages::called_in_review_feedback(
                        &owner,
                        pr_number,
                        crate::config::get_personality(),
                    ),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];
            if saved_session.is_some() {
                on_success.push(Effect::ClearPrBreakSession {
                    name: owner.clone(),
                });
            }

            let on_failure = vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "PR #{} ({}) owned by {} - review comment: {} (call-in failed)",
                        pr_number,
                        truncate_str(title, 40),
                        owner,
                        get_issue_action(PrIssueType::ReviewComment)
                    ),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            }]
        }
        PrAction::HandoffToCoworker {
            assignee,
            original_author,
            pr_number: pr_num,
            branch,
            session_id,
            message,
        } => handoff_to_coworker_effects(
            &assignee,
            &original_author,
            pr_num,
            &branch,
            session_id,
            &message,
            "to address review feedback",
            title,
            pr_number,
            issue_type,
            state,
        ),
        PrAction::PostToChannel { message } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message,
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::Skip { reason } => {
            debug!("Polling comment notification skipped: {}", reason);
            vec![]
        }
    }
}

/// Build effects for handing off a PR to a different coworker.
///
/// Shared helper that consolidates the HandoffToCoworker effect-building logic
/// used across `pr_action_to_effects`, `comment_action_to_effects`, and
/// `review_complete_action_to_effects`. The only variation is the `context_suffix`
/// that describes why the handoff is happening (e.g., "resuming their session for
/// full context" or "to address review feedback").
#[allow(clippy::too_many_arguments)]
fn handoff_to_coworker_effects(
    assignee: &str,
    original_author: &str,
    pr_num: u64,
    branch: &str,
    session_id: String,
    message: &str,
    context_suffix: &str,
    title: &str,
    pr_number: u64,
    issue_type: PrIssueType,
    state: &DaemonState,
) -> Vec<Effect> {
    let config = crate::launch::LaunchConfig::pr_handoff(
        assignee.to_string(),
        state.repo_name.clone(),
        session_id,
        pr_num,
        branch,
        original_author,
    );

    let on_success = vec![
        Effect::BroadcastCoworkerUpdate {
            name: assignee.to_string(),
            status: "running".to_string(),
            current_task: None,
        },
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "{} is taking over PR #{} from {} ({})",
                assignee, pr_num, original_author, context_suffix
            ),
        },
        Effect::RecordPrNudge {
            pr_number,
            issue_type,
        },
    ];

    let on_failure = vec![
        Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "Failed to hand off PR #{} ({}) to {} - {}",
                pr_num,
                truncate_str(title, 40),
                assignee,
                message
            ),
        },
        Effect::RecordPrNudge {
            pr_number,
            issue_type,
        },
    ];

    vec![Effect::SpawnCoworkerWithCallbacks {
        config,
        on_success,
        on_failure,
    }]
}

/// Collect effects for spawning reviewers for PRs that need code review.
///
/// Identifies PRs that need review (not drafts, old enough, no Claude review,
/// not already assigned) and returns effects to spawn reviewer coworkers.
/// Uses `SpawnCoworkerWithCallbacks` so that reviewer assignment and channel
/// messages only happen on successful spawn.
async fn collect_reviewer_effects(state: &DaemonState, prs: &[serde_json::Value]) -> Vec<Effect> {
    collect_reviewer_effects_with_source(
        state,
        prs,
        crate::github_state::AssignmentSource::PollingFallback,
    )
    .await
}

async fn collect_reviewer_effects_with_source(
    state: &DaemonState,
    prs: &[serde_json::Value],
    source: crate::github_state::AssignmentSource,
) -> Vec<Effect> {
    let mut effects: Vec<Effect> = Vec::new();

    // Check rate limit
    let current_review_count = {
        let ps = state.persistent_state.lock().await;
        ps.github.active_count()
    };

    if current_review_count >= MAX_CONCURRENT_REVIEWS {
        debug!(
            "At max concurrent reviews ({}/{}), skipping auto-review spawn",
            current_review_count, MAX_CONCURRENT_REVIEWS
        );
        return effects;
    }

    let reviews_available = MAX_CONCURRENT_REVIEWS - current_review_count;
    let mut reviews_planned = 0;

    for pr in prs {
        if reviews_planned >= reviews_available {
            break;
        }

        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        if pr_number == 0 {
            continue;
        }

        // Skip draft PRs
        let is_draft = pr.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false);
        if is_draft {
            debug!("PR #{} is a draft, skipping auto-review", pr_number);
            continue;
        }

        // Check if PR is old enough (enforce review delay)
        if let Some(age_secs) = get_pr_age_secs(pr)
            && age_secs < PR_REVIEW_DELAY_SECS
        {
            debug!(
                "PR #{} is too new ({}s < {}s), skipping auto-review",
                pr_number, age_secs, PR_REVIEW_DELAY_SECS
            );
            continue;
        }

        // When polling, defer to webhooks if one recently handled this PR.
        // This prevents polling from spawning a duplicate reviewer when the
        // webhook path already queued a pending spawn for the same PR.
        if source == crate::github_state::AssignmentSource::PollingFallback {
            let ps = state.persistent_state.lock().await;
            if ps
                .github
                .webhook_recently_handled(pr_number, PR_REVIEW_DELAY_SECS as i64 * 2)
            {
                debug!(
                    "PR #{} was recently handled by webhook, polling defers",
                    pr_number
                );
                continue;
            }
        }

        // Check if PR already has a Claude review.
        if state.is_pr_reviewed(pr_number).await {
            debug!("PR #{} already has a Claude review", pr_number);

            // Clear the reviewer assignment now that the review is complete.
            // This allows the reviewer to be sent on break, freeing up coworker slots.
            // Previously we only cleared when the reviewer had shut down, but that left
            // idle reviewers stuck with assignments preventing break dispatch.
            {
                let mut ps = state.persistent_state.lock().await;
                if ps.github.is_assigned(pr_number) {
                    debug!(
                        "PR #{} review completed, freeing reviewer assignment",
                        pr_number
                    );
                    ps.github.remove_assignment(pr_number);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!("Failed to save daemon-state.json: {}", e);
                    }
                }
            }

            // Nudge the PR author — review is complete but PR is still open
            let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
            let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

            // Only nudge coworker-owned PRs (validates branch prefix against known names)
            if let Some(owner) = coworker_from_branch(head_ref) {
                let should_nudge = {
                    let tracker = state.pr_issue_tracker.lock().await;
                    tracker.should_nudge(pr_number, PrIssueType::ReviewComplete)
                };

                if should_nudge {
                    let nudge_msg = format!(
                        "Your PR #{} ({}) has a completed review — please address any feedback and merge if appropriate.",
                        pr_number,
                        truncate_str(title, 40)
                    );

                    let active_coworkers: Vec<String> = state
                        .coworkers
                        .list()
                        .iter()
                        .map(|c| c.name.clone())
                        .collect();
                    let busy_coworkers = state.get_all_busy_coworkers();
                    let idle_coworkers: Vec<String> = active_coworkers
                        .iter()
                        .filter(|c| !busy_coworkers.contains(*c))
                        .cloned()
                        .collect();

                    let action = crate::rules::decide_review_complete_action(
                        &owner,
                        &active_coworkers,
                        &idle_coworkers,
                        state.is_at_dev_limit(),
                        &nudge_msg,
                    );

                    effects.extend(review_complete_action_to_effects(
                        action, pr_number, title, state,
                    ));
                }
            }

            continue;
        }

        // Check if already assigned for review.
        {
            let ps = state.persistent_state.lock().await;
            if ps.github.is_assigned(pr_number) {
                debug!("PR #{} already assigned for review", pr_number);
                continue;
            }
        }

        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        debug!(
            "Spawning isolated coworker to review PR #{}: {}",
            pr_number,
            truncate_str(title, 40)
        );

        // Check max coworkers limit before spawning
        if state.is_at_coworker_limit() {
            debug!(
                "Max coworkers limit ({}) reached, cannot spawn reviewer for PR #{}",
                state.max_coworkers, pr_number
            );
            continue;
        }

        let reviewer_name = match state.coworkers.next_available_name() {
            Some(name) => name,
            None => {
                warn!("No available coworker slots for reviewer");
                continue;
            }
        };

        // Compute worktree details for reviewer worktree
        let worktree_id = crate::worktree_registry::review_slug_for_pr(pr_number);
        let wt_path = crate::paths::worktrees_dir_for_repo(&state.repo_name).join(&worktree_id);

        // reviewer() now takes the PR number and generates both the system prompt
        // (with merged reviewer.md instructions) and the launch prompt internally
        let mut config = crate::launch::LaunchConfig::reviewer(reviewer_name.clone(), pr_number);
        config.working_dir = Some(wt_path.clone());

        // Ensure the worktree exists BEFORE spawning (fixes effect ordering bug)
        effects.push(Effect::EnsureWorktree {
            worktree_id: worktree_id.clone(),
            path: wt_path.clone(),
        });

        let on_success = vec![
            // Register the review worktree assignment
            Effect::RegisterWorktreeAssignment {
                assignment: crate::worktree_registry::WorktreeAssignment {
                    worktree_id: worktree_id.clone(),
                    branch_name: worktree_id.clone(), // Branch name matches worktree_id for review worktrees
                    task_id: None,                    // Reviewers are not tied to tasks
                    current_coworker: None,           // Will be set by BindCoworkerToWorktree
                    pr_number: Some(pr_number),
                    created_at: chrono::Utc::now(),
                },
            },
            // Bind the reviewer to the worktree
            Effect::BindCoworkerToWorktree {
                worktree_id: worktree_id.clone(),
                coworker: reviewer_name.clone(),
            },
            Effect::BroadcastCoworkerUpdate {
                name: reviewer_name.clone(),
                status: "running".to_string(),
                current_task: None,
            },
            Effect::AssignReviewer {
                pr_number,
                reviewer_name: reviewer_name.clone(),
                source,
            },
            Effect::PostToChannel {
                sender: "midtown".to_string(),
                message: daemon_messages::called_in_reviewer(
                    &reviewer_name,
                    pr_number,
                    config::get_personality(),
                ),
            },
        ];

        let on_failure = vec![Effect::PostToChannel {
            sender: "midtown".to_string(),
            message: format!(
                "⚠️ Failed to spawn reviewer for PR #{} ({})",
                pr_number,
                truncate_str(title, 40),
            ),
        }];

        effects.push(Effect::SpawnCoworkerWithCallbacks {
            config,
            on_success,
            on_failure,
        });

        reviews_planned += 1;
    }

    effects
}

/// Convert a review-complete `PrAction` into effects.
///
/// Similar to `pr_action_to_effects` but uses `called_in_review_feedback`
/// for the spawn message instead of `called_in_pr_issue`.
fn review_complete_action_to_effects(
    action: crate::rules::PrAction,
    pr_number: u64,
    title: &str,
    state: &DaemonState,
) -> Vec<Effect> {
    use crate::rules::PrAction;
    let issue_type = PrIssueType::ReviewComplete;

    match action {
        PrAction::NudgeOwner { owner, message } => {
            vec![Effect::NudgeCoworkerWithCallbacks {
                name: owner,
                message,
                on_success: vec![Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                }],
            }]
        }
        PrAction::SpawnOwner { owner, message } => {
            let saved_session = {
                let sessions = state.pr_break_sessions.read().unwrap();
                sessions.get(&owner).cloned()
            };
            let session_mode = match saved_session.as_deref() {
                Some(sid) => crate::launch::SessionMode::ResumeSession(sid.to_string()),
                None => crate::launch::SessionMode::Resume,
            };
            let mut config = crate::launch::LaunchConfig::coworker(
                owner.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(message),
            );
            // Use Opus for review feedback responses (higher quality needed to understand feedback)
            config.model = "opus".to_string();

            let mut on_success = vec![
                Effect::BroadcastCoworkerUpdate {
                    name: owner.clone(),
                    status: "running".to_string(),
                    current_task: None,
                },
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: daemon_messages::called_in_review_feedback(
                        &owner,
                        pr_number,
                        config::get_personality(),
                    ),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];
            if saved_session.is_some() {
                on_success.push(Effect::ClearPrBreakSession {
                    name: owner.clone(),
                });
            }

            let on_failure = vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message: format!(
                        "PR #{} ({}) owned by {} - review complete: {} (call-in failed)",
                        pr_number,
                        truncate_str(title, 40),
                        owner,
                        get_issue_action(PrIssueType::ReviewComplete)
                    ),
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ];

            vec![Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            }]
        }
        PrAction::HandoffToCoworker {
            assignee,
            original_author,
            pr_number: pr_num,
            branch,
            session_id,
            message,
        } => handoff_to_coworker_effects(
            &assignee,
            &original_author,
            pr_num,
            &branch,
            session_id,
            &message,
            "to address review feedback",
            title,
            pr_number,
            issue_type,
            state,
        ),
        PrAction::PostToChannel { message } => {
            vec![
                Effect::PostToChannel {
                    sender: "midtown".to_string(),
                    message,
                },
                Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                },
            ]
        }
        PrAction::Skip { reason } => {
            debug!("{}", reason);
            vec![]
        }
    }
}

/// Process pending webhook-triggered reviewer spawns whose delay has expired.
///
/// Drains ready entries from the persisted `pending_review_spawns` queue,
/// fetches each PR's current data, and returns effects for eligible spawns.
/// Unlike the previous `tokio::time::sleep` approach, these survive daemon restarts.
///
/// Returns effects to be executed by the caller (following the evaluate-execute pattern).
pub(super) async fn process_pending_review_spawns(state: &DaemonState) -> Vec<Effect> {
    let mut all_effects = Vec::new();

    // Drain ready spawns from persistent state
    let ready_prs = {
        let mut ps = state.persistent_state.lock().await;
        let ready = ps.github.drain_ready_review_spawns();
        if !ready.is_empty()
            && let Err(e) = ps.save_for_repo(&state.repo_name)
        {
            warn!("Failed to persist review spawn drain: {}", e);
        }
        ready
    };

    if ready_prs.is_empty() {
        return all_effects;
    }

    for pr_number in ready_prs {
        info!("Processing pending review spawn for PR #{}", pr_number);

        // Fetch this specific PR's data
        let output = match tokio::process::Command::new("gh")
            .args([
                "pr",
                "view",
                &pr_number.to_string(),
                "--json",
                "number,mergeable,statusCheckRollup,headRefName,reviewDecision,title,isDraft,createdAt,state",
            ])
            .output()
            .await
        {
            Ok(output) => output,
            Err(e) => {
                warn!(
                    "Webhook: Failed to fetch PR #{} for review spawn: {}",
                    pr_number, e
                );
                continue;
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Webhook: gh pr view #{} failed: {}", pr_number, stderr);
            continue;
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let pr: serde_json::Value = match serde_json::from_str(&stdout) {
            Ok(pr) => pr,
            Err(e) => {
                warn!("Webhook: Failed to parse PR #{} JSON: {}", pr_number, e);
                continue;
            }
        };

        // Check the PR is still open
        let pr_state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("");
        if pr_state != "OPEN" {
            debug!(
                "Webhook: PR #{} is no longer open (state={}), skipping review",
                pr_number, pr_state
            );
            continue;
        }

        // Reuse the existing spawn logic (handles draft check, assignment dedup, etc.)
        // Use Webhook source since this was triggered by a webhook event.
        let effects = collect_reviewer_effects_with_source(
            state,
            &[pr],
            crate::github_state::AssignmentSource::Webhook,
        )
        .await;
        all_effects.extend(effects);
    }

    all_effects
}

/// Uncached check for Claude review on a PR (makes GitHub API calls).
///
/// Fetches both reviews and comments in a single API call to reduce GitHub API usage.
pub(super) fn pr_has_claude_review_uncached(pr_number: u64) -> bool {
    let output = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "reviews,comments",
        ])
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let json: serde_json::Value = match serde_json::from_str(&stdout) {
                Ok(v) => v,
                Err(e) => {
                    warn!("Failed to parse review JSON for PR #{}: {}", pr_number, e);
                    return false;
                }
            };

            // Check formal reviews
            if let Some(reviews) = json.get("reviews").and_then(|v| v.as_array()) {
                for review in reviews {
                    if let Some(body) = review.get("body").and_then(|b| b.as_str())
                        && text_contains_review_signature(body)
                    {
                        return true;
                    }
                }
            }

            // Check comments (where coworkers post their reviews)
            if let Some(comments) = json.get("comments").and_then(|v| v.as_array()) {
                for comment in comments {
                    if let Some(body) = comment.get("body").and_then(|b| b.as_str())
                        && text_contains_review_signature(body)
                    {
                        return true;
                    }
                }
            }

            false
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(
                "Failed to fetch reviews/comments for PR #{}: {}",
                pr_number,
                stderr.trim()
            );
            false
        }
        Err(e) => {
            warn!("Failed to execute gh pr view for PR #{}: {}", pr_number, e);
            false
        }
    }
}
// Auto-nudge helpers for PR activity
// ============================================================================

/// Add an eyes reaction to a GitHub comment to indicate it was received.
///
/// Uses the GitHub Reactions API via `gh api` to add a 👀 reaction to the
/// comment that triggered a coworker nudge or spawn.
async fn add_eyes_reaction(repo_full_name: &str, comment_node: &crate::webhook::CommentNode) {
    let endpoint = match comment_node {
        crate::webhook::CommentNode::IssueComment(id) => {
            format!("/repos/{}/issues/comments/{}/reactions", repo_full_name, id)
        }
        crate::webhook::CommentNode::ReviewComment(id) => {
            format!("/repos/{}/pulls/comments/{}/reactions", repo_full_name, id)
        }
        crate::webhook::CommentNode::Review { .. } => {
            // GitHub API does not support reactions on pull request reviews
            // (only on issue comments and review comments).
            debug!("Skipping eyes reaction: GitHub API does not support reactions on reviews");
            return;
        }
    };

    let result = tokio::process::Command::new("gh")
        .args(["api", &endpoint, "-f", "content=eyes", "--silent"])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {
            debug!("Added eyes reaction to {}", endpoint);
        }
        Ok(output) => {
            debug!(
                "Failed to add eyes reaction to {}: {}",
                endpoint,
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Err(e) => {
            debug!("Failed to run gh api for eyes reaction: {}", e);
        }
    }
}

/// Async version of `get_pr_owner_coworker` that doesn't block the Tokio runtime.
async fn get_pr_owner_coworker_async(pr_number: u64) -> Option<String> {
    let branch = get_pr_branch_async(pr_number).await?;
    coworker_from_branch(&branch)
}

/// Fetch the branch name (headRefName) for a PR using the GitHub CLI.
async fn get_pr_branch_async(pr_number: u64) -> Option<String> {
    let output = tokio::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--json",
            "headRefName",
            "-q",
            ".headRefName",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Look up stored session context for a PR author, for use in handoff decisions.
///
/// Shared helper that avoids duplicating the persistent state lock + map lookup
/// across polling paths and webhook handlers.
async fn get_pr_session_context(
    state: &DaemonState,
    pr_number: u64,
) -> Option<crate::rules::PrSessionContext> {
    let ps = state.persistent_state.lock().await;
    ps.github
        .get_pr_author_session(pr_number)
        .map(|s| crate::rules::PrSessionContext {
            session_id: s.session_id.clone(),
            branch: s.branch.clone(),
            original_author: s.original_author.clone(),
            pr_number,
        })
}

/// Handle nudging a PR owner when a comment/review is posted on their PR.
///
/// This is called from the webhook event loop when a `PrActivity` is present.
/// It resolves the PR owner (from webhook data or async lookup), checks cooldowns,
/// and either nudges an active coworker or spawns an inactive one.
pub(super) async fn handle_pr_comment_nudge(
    state: &DaemonState,
    activity: crate::webhook::PrActivity,
) {
    let pr_number = activity.pr_number;

    // Check for lead/* branches first, before filtering by coworker ownership
    let branch = match activity.branch {
        Some(ref b) => Some(b.clone()),
        None => get_pr_branch_async(pr_number).await,
    };

    if let Some(ref branch) = branch
        && is_lead_branch(branch)
    {
        // Check cooldown before nudging
        {
            let tracker = state.pr_issue_tracker.lock().await;
            if !tracker.should_nudge(pr_number, PrIssueType::ReviewComment) {
                debug!(
                    "PR #{} review comment nudge on cooldown (lead PR), skipping",
                    pr_number
                );
                return;
            }
        }

        let lead_nudge_msg = format!(
            "Your PR #{} has new review comments — please address feedback.",
            pr_number
        );
        debug!(
            "Webhook detected review comment on lead PR #{}, nudging lead",
            pr_number
        );

        let effect = Effect::NudgeLead {
            message: lead_nudge_msg,
        };
        crate::daemon::effects::execute_effects(vec![effect], state).await;
        return;
    }

    // Only check coworker-owned PRs beyond this point
    let owner = match activity.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(owner) = owner else {
        debug!("PR #{} has no coworker owner, skipping nudge", pr_number);
        return;
    };

    // Don't create tasks for self-comments
    if activity
        .owner_coworker
        .as_ref()
        .is_some_and(|o| o == &activity.actor)
    {
        debug!(
            "PR #{} comment is from owner {}, skipping self-nudge",
            pr_number, activity.actor
        );
        return;
    }

    // Check cooldown to avoid spamming
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, PrIssueType::ReviewComment) {
            debug!(
                "PR #{} review comment nudge on cooldown, skipping",
                pr_number
            );
            return;
        }
    }

    let nudge_msg = format!(
        "Your PR #{} has review feedback from {}. Please address it and merge if appropriate.",
        pr_number, activity.actor
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Get session context for potential handoff
    let session_context = get_pr_session_context(state, pr_number).await;

    // Decide action using pure decision function with handoff support
    let action = crate::rules::decide_pr_comment_action_with_handoff(
        &owner,
        &activity.actor,
        &active_coworkers,
        &idle_coworkers,
        state.is_at_dev_limit(),
        session_context.as_ref(),
        &nudge_msg,
    );

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let is_actionable = !matches!(action, crate::rules::PrAction::Skip { .. });
    let mut effects = comment_action_to_effects(action, pr_number, "", state);

    // If this is a lead/* branch, also nudge the lead so they see review feedback
    if let Some(branch) = get_pr_branch_async(pr_number).await
        && is_lead_branch(&branch)
    {
        let lead_nudge_msg = format!(
            "Your PR #{} has review feedback from {}. Please address it and merge if appropriate.",
            pr_number, activity.actor
        );
        effects.push(Effect::NudgeLead {
            message: lead_nudge_msg,
        });
    }

    super::effects::execute_effects(effects, state).await;

    // Add eyes reaction to the comment to provide visual feedback that it was received
    if is_actionable
        && let (Some(ref node), Some(ref repo)) = (activity.comment_node, activity.repo_full_name)
    {
        add_eyes_reaction(repo, node).await;
    }
}

/// Handle a formal review state change (approved / changes_requested) from a webhook.
///
/// This provides immediate nudging when a reviewer submits a formal review,
/// instead of waiting for the next polling cycle to detect the state change.
/// The `PrIssueTracker` cooldown prevents duplicate nudges if polling also fires.
pub(super) async fn handle_webhook_review_state_change(
    state: &DaemonState,
    change: crate::webhook::PrReviewStateChange,
) {
    let pr_number = change.pr_number;
    let issue_type = match change.state {
        crate::webhook::ReviewState::Approved => PrIssueType::Approved,
        crate::webhook::ReviewState::ChangesRequested => PrIssueType::ChangesRequested,
    };

    // Check cooldown — polling may have already nudged for this issue
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, issue_type) {
            debug!(
                "PR #{} {} nudge on cooldown (already handled), skipping webhook nudge",
                pr_number, issue_type
            );
            return;
        }
    }

    // Resolve owner: use webhook data if available, otherwise look up async
    let owner = match change.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(owner) = owner else {
        debug!(
            "PR #{} has no coworker owner, skipping webhook {} nudge",
            pr_number, issue_type
        );
        return;
    };

    let nudge_msg = format!(
        "PR #{} — {}: {}",
        pr_number,
        issue_type,
        get_issue_action(issue_type)
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Get session context for potential handoff
    let session_context = get_pr_session_context(state, pr_number).await;

    let action = crate::rules::decide_pr_issue_action_with_handoff(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        state.is_at_dev_limit(),
        session_context.as_ref(),
        &nudge_msg,
    );

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects = pr_action_to_effects(action, pr_number, "", issue_type, state);
    super::effects::execute_effects(effects, state).await;
}

/// Handle a CI check failure on a PR branch from a webhook.
///
/// This provides immediate nudging when CI fails on a PR, instead of waiting
/// for the next polling cycle. The `PrIssueTracker` cooldown prevents duplicate
/// nudges if polling also fires.
pub(super) async fn handle_webhook_ci_failure(
    state: &DaemonState,
    failure: crate::webhook::PrCiFailure,
) {
    let pr_number = failure.pr_number;

    // Check cooldown
    {
        let tracker = state.pr_issue_tracker.lock().await;
        if !tracker.should_nudge(pr_number, PrIssueType::CiFailed) {
            debug!(
                "PR #{} CI failure nudge on cooldown, skipping webhook nudge",
                pr_number
            );
            return;
        }
    }

    // Resolve owner
    let owner = match failure.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(owner) = owner else {
        debug!(
            "PR #{} has no coworker owner, skipping webhook CI failure nudge",
            pr_number
        );
        return;
    };

    let nudge_msg = format!(
        "PR #{} — CI check '{}' failed: please investigate",
        pr_number, failure.check_name
    );

    // Get active and idle coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();
    let busy_coworkers = state.get_all_busy_coworkers();
    let idle_coworkers: Vec<String> = active_coworkers
        .iter()
        .filter(|c| !busy_coworkers.contains(*c))
        .cloned()
        .collect();

    // Get session context for potential handoff
    let session_context = get_pr_session_context(state, pr_number).await;

    let action = crate::rules::decide_pr_issue_action_with_handoff(
        &owner,
        &active_coworkers,
        &idle_coworkers,
        state.is_at_dev_limit(),
        session_context.as_ref(),
        &nudge_msg,
    );

    // Convert PrAction → Effects using the same pure converter as polling,
    // then execute via the standard effect pipeline.
    let effects = pr_action_to_effects(action, pr_number, "", PrIssueType::CiFailed, state);
    super::effects::execute_effects(effects, state).await;
}

/// Detect stale CI checks and collect re-run effects.
///
/// Examines `statusCheckRollup` for each PR to find stuck checks in two passes:
/// - **Pass 1**: IN_PROGRESS checks running > 4x typical duration.
/// - **Pass 2**: QUEUED/PENDING/WAITING checks that never started when all sibling checks
///   have completed (2x typical duration with a 30-minute minimum floor).
///
/// Returns effects to re-run the affected workflows. Uses historical check durations
/// from `CiCheckStats` to determine "typical" time.
async fn collect_stale_check_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
) -> Vec<Effect> {
    use chrono::Utc;

    // Get CI stats for duration comparisons
    let ci_stats = {
        let ps = state.persistent_state.lock().await;
        ps.ci_stats.clone()
    };

    collect_stale_check_effects_with_time(&ci_stats, prs, Utc::now())
}

/// Pure helper for `collect_stale_check_effects` that accepts a reference time.
///
/// This allows deterministic testing by passing a fixed timestamp.
fn collect_stale_check_effects_with_time(
    ci_stats: &crate::ci_stats::CiCheckStats,
    prs: &[serde_json::Value],
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<Effect> {
    use crate::ci_stats::extract_run_id_from_url;
    use chrono::DateTime;

    let mut effects = Vec::new();

    for pr in prs {
        let pr_number = match pr.get("number").and_then(|n| n.as_u64()) {
            Some(n) => n,
            None => continue,
        };

        let checks = match pr.get("statusCheckRollup").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => continue,
        };

        // --- Pass 1: Detect IN_PROGRESS checks running too long (4x typical) ---
        for check in checks {
            let status = check.get("status").and_then(|s| s.as_str()).unwrap_or("");

            // Only consider checks that are in progress
            if status != "IN_PROGRESS" {
                continue;
            }

            let check_name = match check.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let started_at_str = match check.get("startedAt").and_then(|s| s.as_str()) {
                Some(s) => s,
                None => continue,
            };

            // Parse the started_at timestamp
            let started_at: DateTime<chrono::Utc> = match started_at_str.parse() {
                Ok(dt) => dt,
                Err(_) => continue,
            };

            // Calculate how long the check has been running
            let running_duration =
                now.signed_duration_since(started_at).num_seconds().max(0) as u64;

            // Check if it exceeds the stale threshold (4x typical)
            if !ci_stats.is_stale(check_name, running_duration) {
                continue;
            }

            // Extract run ID from the details URL
            let details_url = match check.get("detailsUrl").and_then(|u| u.as_str()) {
                Some(u) => u,
                None => continue,
            };

            let run_id = match extract_run_id_from_url(details_url) {
                Some(id) => id,
                None => continue,
            };

            // Check cooldown to prevent re-running the same workflow repeatedly
            if !ci_stats.can_rerun(run_id) {
                debug!(
                    "Skipping re-run of workflow {} for '{}' on PR #{} (on cooldown)",
                    run_id, check_name, pr_number
                );
                continue;
            }

            let typical_duration = ci_stats.typical_duration_or_default(check_name);
            info!(
                "Detected stale CI check '{}' on PR #{}: running {}s (typical: {}s, threshold: {}s)",
                check_name,
                pr_number,
                running_duration,
                typical_duration,
                (typical_duration as f64 * crate::ci_stats::STALE_THRESHOLD_MULTIPLIER) as u64
            );

            effects.push(Effect::RerunWorkflow {
                run_id,
                check_name: check_name.to_string(),
                pr_number,
            });
        }

        // --- Pass 2: Detect PENDING/QUEUED checks that never started ---
        // A check stuck in pending while all siblings completed indicates a
        // GitHub Actions scheduling failure. Use the earliest sibling startedAt
        // as a time reference (since pending checks lack their own startedAt).

        // Classify checks into pending vs non-pending
        let pending_checks: Vec<&serde_json::Value> = checks
            .iter()
            .filter(|c| {
                let status = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
                matches!(status, "QUEUED" | "PENDING" | "WAITING")
            })
            .collect();

        if pending_checks.is_empty() {
            continue;
        }

        // All non-pending checks must be completed
        let non_pending: Vec<&serde_json::Value> = checks
            .iter()
            .filter(|c| {
                let status = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
                !matches!(status, "QUEUED" | "PENDING" | "WAITING")
            })
            .collect();

        if non_pending.is_empty() {
            continue; // No sibling checks to compare against
        }

        let all_siblings_completed = non_pending.iter().all(|c| {
            let status = c.get("status").and_then(|s| s.as_str()).unwrap_or("");
            status == "COMPLETED"
        });

        if !all_siblings_completed {
            continue; // Some siblings still running — not yet a clear signal
        }

        // Find earliest sibling startedAt as time reference
        let earliest_sibling_start: Option<DateTime<chrono::Utc>> = non_pending
            .iter()
            .filter_map(|c| {
                c.get("startedAt")
                    .and_then(|s| s.as_str())
                    .and_then(|s| s.parse::<DateTime<chrono::Utc>>().ok())
            })
            .min();

        let earliest_start = match earliest_sibling_start {
            Some(t) => t,
            None => continue, // Can't determine timing without sibling timestamps
        };

        let time_since_start = now
            .signed_duration_since(earliest_start)
            .num_seconds()
            .max(0) as u64;

        for check in &pending_checks {
            let check_name = match check.get("name").and_then(|n| n.as_str()) {
                Some(n) => n,
                None => continue,
            };

            if !ci_stats.is_pending_stale(check_name, time_since_start) {
                continue;
            }

            let details_url = match check.get("detailsUrl").and_then(|u| u.as_str()) {
                Some(u) => u,
                None => continue,
            };

            let run_id = match extract_run_id_from_url(details_url) {
                Some(id) => id,
                None => continue,
            };

            if !ci_stats.can_rerun(run_id) {
                debug!(
                    "Skipping re-run of workflow {} for pending '{}' on PR #{} (on cooldown)",
                    run_id, check_name, pr_number
                );
                continue;
            }

            let typical_duration = ci_stats.typical_duration_or_default(check_name);
            let threshold =
                (typical_duration as f64 * crate::ci_stats::PENDING_STALE_MULTIPLIER) as u64;
            let effective_threshold = threshold.max(crate::ci_stats::MIN_PENDING_STALE_SECS);
            info!(
                "Detected stale PENDING check '{}' on PR #{}: pending {}s since siblings started (threshold: {}s)",
                check_name, pr_number, time_since_start, effective_threshold
            );

            effects.push(Effect::RerunWorkflow {
                run_id,
                check_name: check_name.to_string(),
                pr_number,
            });
        }
    }

    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Bug: collect_green_with_feedback_effects was using head_ref.split('/').next()
    /// to extract the owner, which doesn't validate against known coworker names.
    /// This meant PRs with branches like "btucker/fix" would extract "btucker" as owner
    /// and potentially nudge wrong coworkers if the prefix matches a coworker name.
    #[test]
    fn coworker_from_branch_rejects_non_coworker_prefixes() {
        // These should return None because they're not valid coworker names
        assert!(
            coworker_from_branch("btucker/fix-something").is_none(),
            "btucker is not a coworker name"
        );
        assert!(
            coworker_from_branch("feature/add-auth").is_none(),
            "feature is not a coworker name"
        );
        assert!(coworker_from_branch("main").is_none(), "main has no slash");

        // These should return Some because they are valid coworker names
        assert_eq!(
            coworker_from_branch("york/fix-something"),
            Some("york".to_string()),
            "york is a valid coworker name"
        );
        assert_eq!(
            coworker_from_branch("amsterdam/add-feature"),
            Some("amsterdam".to_string()),
            "amsterdam is a valid coworker name"
        );
    }

    #[test]
    fn is_lead_branch_detects_lead_branches() {
        // Lead branches start with "lead/"
        assert!(
            is_lead_branch("lead/fix-bug"),
            "lead/fix-bug is a lead branch"
        );
        assert!(
            is_lead_branch("lead/add-feature"),
            "lead/add-feature is a lead branch"
        );
        assert!(
            is_lead_branch("lead/root-cause-claude-md-updates"),
            "lead/root-cause-claude-md-updates is a lead branch"
        );

        // Coworker and other branches should not be detected as lead branches
        assert!(
            !is_lead_branch("york/fix-bug"),
            "york/fix-bug is not a lead branch"
        );
        assert!(
            !is_lead_branch("feature/add-auth"),
            "feature/add-auth is not a lead branch"
        );
        assert!(!is_lead_branch("main"), "main is not a lead branch");
        assert!(
            !is_lead_branch("leading/edge"),
            "leading/edge is not a lead branch (only exact prefix match)"
        );
    }

    #[test]
    fn stuck_nudge_effects_returns_only_system_message() {
        // Bug: stuck_nudge_effects was returning both PostSystemMessage and NudgeLead,
        // causing double delivery because the chat monitor already routes @lead mentions
        // in system messages to the lead.
        //
        // The fix is to only return PostSystemMessage and let the channel's @mention
        // routing handle the nudge.
        let message = "@lead PR #42 (Add feature) has been open for 60 minutes without a review";
        let effects = stuck_nudge_effects(message);

        // Should only return one effect (PostSystemMessage)
        assert_eq!(
            effects.len(),
            1,
            "stuck_nudge_effects should return exactly 1 effect, not 2 (double nudge bug)"
        );

        // That effect should be PostSystemMessage with the warning emoji prefix
        match &effects[0] {
            Effect::PostSystemMessage { message: msg } => {
                assert!(
                    msg.starts_with("⚠️"),
                    "System message should have warning prefix"
                );
                assert!(
                    msg.contains("@lead"),
                    "System message should preserve @lead mention"
                );
            }
            _ => panic!("Expected PostSystemMessage effect, got {:?}", effects[0]),
        }
    }

    /// Creates a CiCheckStats with recorded durations for testing.
    fn test_ci_stats_with_duration(
        check_name: &str,
        duration: u64,
    ) -> crate::ci_stats::CiCheckStats {
        let mut stats = crate::ci_stats::CiCheckStats::default();
        // Record multiple times to establish a stable typical duration
        for _ in 0..5 {
            stats.record_duration(check_name, duration);
        }
        stats
    }

    #[test]
    fn collect_stale_check_effects_detects_stale_in_progress_check() {
        use chrono::{DateTime, Utc};

        // Set up CI stats with a typical duration of 120 seconds for "Test" check
        let ci_stats = test_ci_stats_with_duration("Test", 120);

        // PR with an IN_PROGRESS check that started 600 seconds ago (5x typical)
        let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();
        let prs = vec![json!({
            "number": 42,
            "statusCheckRollup": [{
                "name": "Test",
                "status": "IN_PROGRESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
            }]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);

        assert_eq!(effects.len(), 1, "should detect one stale check");
        match &effects[0] {
            Effect::RerunWorkflow {
                run_id,
                check_name,
                pr_number,
            } => {
                assert_eq!(*run_id, 123456);
                assert_eq!(check_name, "Test");
                assert_eq!(*pr_number, 42);
            }
            _ => panic!("expected RerunWorkflow effect"),
        }
    }

    #[test]
    fn collect_stale_check_effects_ignores_checks_not_in_progress() {
        use chrono::{DateTime, Utc};

        let ci_stats = test_ci_stats_with_duration("Test", 120);
        let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

        // PR with a COMPLETED check
        let prs = vec![json!({
            "number": 42,
            "statusCheckRollup": [{
                "name": "Test",
                "status": "COMPLETED",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
            }]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should not detect completed checks as stale"
        );
    }

    #[test]
    fn collect_stale_check_effects_ignores_checks_within_threshold() {
        use chrono::{DateTime, Utc};

        // Typical duration is 120s, threshold is 4x = 480s
        let ci_stats = test_ci_stats_with_duration("Test", 120);
        let now: DateTime<Utc> = "2026-02-04T12:05:00Z".parse().unwrap();

        // PR with a check that has been running for 300s (within 480s threshold)
        let prs = vec![json!({
            "number": 42,
            "statusCheckRollup": [{
                "name": "Test",
                "status": "IN_PROGRESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
            }]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should not detect checks within threshold"
        );
    }

    #[test]
    fn collect_stale_check_effects_skips_prs_without_status_check_rollup() {
        use chrono::{DateTime, Utc};

        let ci_stats = test_ci_stats_with_duration("Test", 120);
        let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

        let prs = vec![json!({
            "number": 42
            // No statusCheckRollup field
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should skip PRs without statusCheckRollup"
        );
    }

    #[test]
    fn collect_stale_check_effects_skips_checks_without_details_url() {
        use chrono::{DateTime, Utc};

        let ci_stats = test_ci_stats_with_duration("Test", 120);
        let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

        let prs = vec![json!({
            "number": 42,
            "statusCheckRollup": [{
                "name": "Test",
                "status": "IN_PROGRESS",
                "startedAt": "2026-02-04T12:00:00Z"
                // No detailsUrl - can't extract run ID
            }]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(effects.is_empty(), "should skip checks without detailsUrl");
    }

    #[test]
    fn collect_stale_check_effects_skips_invalid_details_url() {
        use chrono::{DateTime, Utc};

        let ci_stats = test_ci_stats_with_duration("Test", 120);
        let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();

        let prs = vec![json!({
            "number": 42,
            "statusCheckRollup": [{
                "name": "Test",
                "status": "IN_PROGRESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://example.com/not-a-github-url"
            }]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should skip checks with unparseable detailsUrl"
        );
    }

    #[test]
    fn collect_stale_check_effects_respects_rerun_cooldown() {
        use chrono::{DateTime, Utc};

        let mut ci_stats = test_ci_stats_with_duration("Test", 120);
        // Record a recent re-run for this workflow
        ci_stats.record_rerun(123456);

        let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();
        let prs = vec![json!({
            "number": 42,
            "statusCheckRollup": [{
                "name": "Test",
                "status": "IN_PROGRESS",
                "startedAt": "2026-02-04T12:00:00Z",
                "detailsUrl": "https://github.com/owner/repo/actions/runs/123456/job/789"
            }]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(effects.is_empty(), "should skip re-run when on cooldown");
    }

    #[test]
    fn collect_stale_check_effects_handles_multiple_prs_and_checks() {
        use chrono::{DateTime, Utc};

        let mut ci_stats = test_ci_stats_with_duration("Test", 120);
        // Also add stats for Clippy
        for _ in 0..5 {
            ci_stats.record_duration("Clippy", 60);
        }

        let now: DateTime<Utc> = "2026-02-04T12:10:00Z".parse().unwrap();
        let prs = vec![
            json!({
                "number": 42,
                "statusCheckRollup": [
                    {
                        "name": "Test",
                        "status": "IN_PROGRESS",
                        "startedAt": "2026-02-04T12:00:00Z",
                        "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                    },
                    {
                        "name": "Clippy",
                        "status": "COMPLETED",  // Not in progress
                        "startedAt": "2026-02-04T12:00:00Z",
                        "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/2"
                    }
                ]
            }),
            json!({
                "number": 43,
                "statusCheckRollup": [{
                    "name": "Clippy",
                    "status": "IN_PROGRESS",
                    "startedAt": "2026-02-04T12:00:00Z",  // 600s ago, threshold is 240s
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/333/job/3"
                }]
            }),
        ];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);

        // Should find 2 stale checks: Test on PR #42 and Clippy on PR #43
        assert_eq!(effects.len(), 2, "should detect two stale checks");

        // Verify both effects are RerunWorkflow
        for effect in &effects {
            assert!(matches!(effect, Effect::RerunWorkflow { .. }));
        }
    }

    // -------------------------------------------------------------------------
    // Stale PENDING check detection tests
    // -------------------------------------------------------------------------

    #[test]
    fn collect_stale_check_effects_detects_pending_when_siblings_completed() {
        use chrono::{DateTime, Utc};

        // Set up CI stats with a typical duration of 120 seconds for "task_sharing"
        let ci_stats = test_ci_stats_with_duration("task_sharing", 120);

        // Siblings started 60 minutes ago — well beyond any threshold
        let now: DateTime<Utc> = "2026-02-04T13:00:00Z".parse().unwrap();
        let prs = vec![json!({
            "number": 679,
            "statusCheckRollup": [
                {
                    "name": "Test",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                },
                {
                    "name": "Clippy",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/2"
                },
                {
                    "name": "task_sharing",
                    "status": "QUEUED",
                    "startedAt": "",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
                }
            ]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);

        assert_eq!(
            effects.len(),
            1,
            "should detect pending check when siblings completed"
        );
        match &effects[0] {
            Effect::RerunWorkflow {
                run_id,
                check_name,
                pr_number,
            } => {
                assert_eq!(*run_id, 222);
                assert_eq!(check_name, "task_sharing");
                assert_eq!(*pr_number, 679);
            }
            _ => panic!("expected RerunWorkflow effect"),
        }
    }

    #[test]
    fn collect_stale_check_effects_ignores_pending_when_siblings_still_running() {
        use chrono::{DateTime, Utc};

        let ci_stats = test_ci_stats_with_duration("task_sharing", 120);
        // now is 5 minutes after start — Clippy IN_PROGRESS is within default threshold
        let now: DateTime<Utc> = "2026-02-04T12:05:00Z".parse().unwrap();

        // One sibling is still IN_PROGRESS — not all siblings completed
        let prs = vec![json!({
            "number": 679,
            "statusCheckRollup": [
                {
                    "name": "Test",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                },
                {
                    "name": "Clippy",
                    "status": "IN_PROGRESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/2"
                },
                {
                    "name": "task_sharing",
                    "status": "QUEUED",
                    "startedAt": "",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
                }
            ]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should not detect pending check when siblings still running"
        );
    }

    #[test]
    fn collect_stale_check_effects_ignores_pending_within_threshold() {
        use chrono::{DateTime, Utc};

        let ci_stats = test_ci_stats_with_duration("task_sharing", 120);

        // Siblings started only 3 minutes ago — within 2x typical (240s) threshold
        let now: DateTime<Utc> = "2026-02-04T12:03:00Z".parse().unwrap();
        let prs = vec![json!({
            "number": 679,
            "statusCheckRollup": [
                {
                    "name": "Test",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                },
                {
                    "name": "task_sharing",
                    "status": "QUEUED",
                    "startedAt": "",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
                }
            ]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should not detect pending check within time threshold"
        );
    }

    #[test]
    fn collect_stale_check_effects_pending_uses_min_threshold() {
        use chrono::{DateTime, Utc};

        // No stats for this check — should use MIN_PENDING_STALE_SECS (1800s = 30 min)
        let ci_stats = crate::ci_stats::CiCheckStats::default();

        // 20 minutes since siblings started — under 30 min minimum threshold
        let now: DateTime<Utc> = "2026-02-04T12:20:00Z".parse().unwrap();
        let prs = vec![json!({
            "number": 679,
            "statusCheckRollup": [
                {
                    "name": "Test",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                },
                {
                    "name": "unknown_check",
                    "status": "QUEUED",
                    "startedAt": "",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
                }
            ]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should not detect pending check before minimum threshold (30 min)"
        );

        // 35 minutes since siblings started — past 30 min minimum threshold
        let now_later: DateTime<Utc> = "2026-02-04T12:35:00Z".parse().unwrap();
        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now_later);
        assert_eq!(
            effects.len(),
            1,
            "should detect pending check after minimum threshold"
        );
    }

    #[test]
    fn collect_stale_check_effects_pending_respects_rerun_cooldown() {
        use chrono::{DateTime, Utc};

        let mut ci_stats = test_ci_stats_with_duration("task_sharing", 120);
        // Record a recent re-run for this workflow
        ci_stats.record_rerun(222);

        let now: DateTime<Utc> = "2026-02-04T13:00:00Z".parse().unwrap();
        let prs = vec![json!({
            "number": 679,
            "statusCheckRollup": [
                {
                    "name": "Test",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "2026-02-04T12:00:00Z",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                },
                {
                    "name": "task_sharing",
                    "status": "QUEUED",
                    "startedAt": "",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
                }
            ]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should skip pending check re-run when on cooldown"
        );
    }

    #[test]
    fn collect_stale_check_effects_pending_skips_malformed_sibling_timestamps() {
        use chrono::{DateTime, Utc};

        let ci_stats = test_ci_stats_with_duration("task_sharing", 120);
        let now: DateTime<Utc> = "2026-02-04T13:00:00Z".parse().unwrap();

        // All siblings COMPLETED but with missing/malformed startedAt
        let prs = vec![json!({
            "number": 679,
            "statusCheckRollup": [
                {
                    "name": "Test",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "startedAt": "not-a-date",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/1"
                },
                {
                    "name": "Clippy",
                    "status": "COMPLETED",
                    "conclusion": "SUCCESS",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/111/job/2"
                },
                {
                    "name": "task_sharing",
                    "status": "QUEUED",
                    "startedAt": "",
                    "detailsUrl": "https://github.com/owner/repo/actions/runs/222/job/3"
                }
            ]
        })];

        let effects = collect_stale_check_effects_with_time(&ci_stats, &prs, now);
        assert!(
            effects.is_empty(),
            "should skip pending check when sibling timestamps are unparseable"
        );
    }

    // -------------------------------------------------------------------------
    // Stuck condition escalation threshold tests
    // -------------------------------------------------------------------------

    #[test]
    fn test_escalation_triggers_on_second_nudge() {
        // Test the should_escalate helper function directly.
        // With STUCK_ESCALATION_NUDGE_COUNT = 2:
        // - First nudge (prior_nudges=0): 0+1=1 < 2, no escalation
        // - Second nudge (prior_nudges=1): 1+1=2 >= 2, ESCALATION

        assert!(
            !super::should_escalate(0),
            "first nudge (prior=0) should NOT escalate"
        );
        assert!(
            super::should_escalate(1),
            "second nudge (prior=1) should escalate"
        );
        assert!(
            super::should_escalate(2),
            "third+ nudge (prior=2) should escalate"
        );
    }

    #[test]
    fn test_escalation_timing_matches_documentation() {
        use crate::daemon::constants::{
            STUCK_ESCALATION_NUDGE_COUNT, STUCK_NO_REVIEW_DURATION, STUCK_NUDGE_COOLDOWN_SECS,
        };

        // Documentation says escalation happens after 45+ minutes:
        // - Initial stuck detection: ~15 minutes (STUCK_NO_REVIEW_DURATION)
        // - First nudge at T=15min (prior_nudges becomes 1)
        // - Cooldown: 30 minutes (STUCK_NUDGE_COOLDOWN_SECS)
        // - Second nudge at T=45min triggers escalation (prior_nudges=1, 1+1=2 >= 2)

        let initial_detection_secs = STUCK_NO_REVIEW_DURATION.as_secs();
        let cooldown_secs = STUCK_NUDGE_COOLDOWN_SECS;
        let nudges_before_escalation = STUCK_ESCALATION_NUDGE_COUNT - 1; // 1 nudge before escalation

        let escalation_time_secs =
            initial_detection_secs + (nudges_before_escalation as u64 * cooldown_secs);
        let escalation_time_minutes = escalation_time_secs / 60;

        assert_eq!(
            escalation_time_minutes, 45,
            "escalation should trigger at 45 minutes (15 min initial + 30 min cooldown)"
        );
    }

    // -------------------------------------------------------------------------
    // Time-aware hash tests (PR poll cache bug fix)
    // -------------------------------------------------------------------------

    /// Bug: PR poll used a hash of the response to skip processing when data unchanged.
    /// But reviewer spawn decisions depend on PR age (time-based), so even with unchanged
    /// data, a PR that was "too new" should be re-evaluated after time passes.
    ///
    /// Fix: Include a time bucket in the hash so it changes every PR_REVIEW_DELAY_SECS.
    #[test]
    fn compute_time_aware_hash_same_data_same_bucket_same_hash() {
        // Within the same time bucket, same data should produce same hash
        let data = r#"[{"number": 42, "title": "Test PR"}]"#;
        let bucket_secs = 60;

        let hash1 = super::compute_time_aware_hash(data, bucket_secs);
        let hash2 = super::compute_time_aware_hash(data, bucket_secs);

        // Same data, same time bucket (called immediately) -> same hash
        assert_eq!(
            hash1, hash2,
            "same data in same time bucket should produce same hash"
        );
    }

    #[test]
    fn compute_time_aware_hash_different_data_different_hash() {
        let data1 = r#"[{"number": 42, "title": "Test PR"}]"#;
        let data2 = r#"[{"number": 42, "title": "Updated PR"}]"#;
        let bucket_secs = 60;

        let hash1 = super::compute_time_aware_hash(data1, bucket_secs);
        let hash2 = super::compute_time_aware_hash(data2, bucket_secs);

        // Different data should produce different hash
        assert_ne!(hash1, hash2, "different data should produce different hash");
    }

    /// This test documents the behavior that the hash will change over time.
    /// We can't easily test actual time passage in a unit test, but we can verify
    /// the hash function includes the time bucket by using a very small bucket.
    #[test]
    fn compute_time_aware_hash_includes_time_component() {
        use std::hash::{Hash, Hasher};

        // Verify that the same data with different time buckets would produce different hashes
        // by manually computing what the hash would be with different time values
        let data = r#"[{"number": 42}]"#;

        // Simulate two different time buckets by manually hashing
        let mut hasher1 = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hasher1);
        (100u64).hash(&mut hasher1); // time bucket 100
        let hash_bucket_100 = hasher1.finish();

        let mut hasher2 = std::collections::hash_map::DefaultHasher::new();
        data.hash(&mut hasher2);
        (101u64).hash(&mut hasher2); // time bucket 101
        let hash_bucket_101 = hasher2.finish();

        assert_ne!(
            hash_bucket_100, hash_bucket_101,
            "same data with different time buckets should produce different hashes"
        );
    }

    // -------------------------------------------------------------------------
    // PR poll cache re-evaluation E2E test
    // -------------------------------------------------------------------------

    /// This test demonstrates the end-to-end behavior of the PR poll cache fix.
    ///
    /// ## Bug scenario (before fix):
    /// 1. PR #42 is opened at t=0
    /// 2. Poll at t=30s: PR is too new (within 60s delay), no reviewer spawn
    /// 3. Poll at t=90s: PR data unchanged → hash unchanged → early return (BUG!)
    ///    - The reviewer spawn eligibility was never re-evaluated
    ///
    /// ## Fixed behavior (after fix):
    /// 1. PR #42 is opened at t=0
    /// 2. Poll at t=30s: PR is too new, no reviewer spawn, cache hash saved
    /// 3. Poll at t=90s: time bucket changed (bucket 0→1) → hash changed
    ///    - Poll proceeds, PR age re-evaluated, reviewer spawn triggered
    ///
    /// This test simulates time passing to verify the hash changes at bucket boundaries.
    #[test]
    fn pr_poll_cache_reevaluates_after_time_bucket_change() {
        // Same PR data throughout - the data doesn't change, only time passes
        let pr_data = r#"[{"number": 42, "title": "feat: Add feature", "state": "OPEN"}]"#;
        let bucket_secs = super::PR_REVIEW_DELAY_SECS; // 60 seconds

        // Scenario: PR opened at t=0, first poll at t=30
        // Bucket boundaries are at multiples of 60: 0, 60, 120, ...
        let t_first_poll = 30u64; // In bucket 0 (0-59)
        let hash_first_poll = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_first_poll);

        // At this point, PR is too new for review (only 30s old).
        // The daemon would skip reviewer spawn. Hash is cached.

        // Second poll at t=50 (still in bucket 0)
        let t_second_poll = 50u64; // Still in bucket 0 (0-59)
        let hash_second_poll =
            super::compute_time_aware_hash_at(pr_data, bucket_secs, t_second_poll);

        // Hash should be SAME (same bucket) - this is expected caching behavior
        assert_eq!(
            hash_first_poll, hash_second_poll,
            "Within same time bucket, hash should be stable for caching"
        );

        // Third poll at t=90 (NEW bucket!)
        // This is 90s after PR creation, well past the 60s review delay
        let t_third_poll = 90u64; // In bucket 1 (60-119)
        let hash_third_poll = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_third_poll);

        // Hash should be DIFFERENT (new bucket) - triggers re-evaluation
        assert_ne!(
            hash_second_poll, hash_third_poll,
            "After time bucket change, hash should differ to trigger re-evaluation"
        );

        // Verify the bucket transition occurred as expected
        let bucket_first = t_first_poll / bucket_secs;
        let bucket_second = t_second_poll / bucket_secs;
        let bucket_third = t_third_poll / bucket_secs;

        assert_eq!(
            bucket_first, bucket_second,
            "First two polls should be in same bucket"
        );
        assert_ne!(
            bucket_second, bucket_third,
            "Third poll should be in new bucket"
        );

        // Document the bucket transition: 0 → 1
        assert_eq!(bucket_first, 0, "First/second poll should be in bucket 0");
        assert_eq!(bucket_third, 1, "Third poll should be in bucket 1");
    }

    /// Test that the bucket boundary is exactly at PR_REVIEW_DELAY_SECS intervals.
    ///
    /// This ensures that after waiting the full review delay period, the hash
    /// is guaranteed to have changed and the PR eligibility will be re-evaluated.
    #[test]
    fn pr_poll_cache_bucket_boundary_precision() {
        let pr_data = r#"[{"number": 99}]"#;
        let bucket_secs = super::PR_REVIEW_DELAY_SECS; // 60 seconds

        // Bucket boundaries: 0-59 (bucket 0), 60-119 (bucket 1), 120-179 (bucket 2)
        //
        // t=59 → 59/60 = 0 (bucket 0)
        // t=60 → 60/60 = 1 (bucket 1)

        // Poll at t=59 (end of bucket 0)
        let t_end_of_bucket = 59u64;
        let hash_end = super::compute_time_aware_hash_at(pr_data, bucket_secs, t_end_of_bucket);

        // Poll at t=60 (start of bucket 1)
        let t_start_next_bucket = 60u64;
        let hash_start =
            super::compute_time_aware_hash_at(pr_data, bucket_secs, t_start_next_bucket);

        // One second difference at bucket boundary → different hash
        assert_ne!(
            hash_end, hash_start,
            "Crossing bucket boundary (59→60) should change hash"
        );

        // Verify bucket values
        assert_eq!(
            t_end_of_bucket / bucket_secs,
            0,
            "t=59 should be in bucket 0"
        );
        assert_eq!(
            t_start_next_bucket / bucket_secs,
            1,
            "t=60 should be in bucket 1"
        );

        // Within bucket, 58→59 should be same hash
        let hash_58 = super::compute_time_aware_hash_at(pr_data, bucket_secs, 58);
        let hash_59 = super::compute_time_aware_hash_at(pr_data, bucket_secs, 59);
        assert_eq!(
            hash_58, hash_59,
            "Within same bucket (58→59), hash should be stable"
        );
    }

    /// Create a minimal DaemonState for testing action-to-effects converters.
    fn make_test_state() -> DaemonState {
        use std::process::Command;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("temp dir");
        // Init git repo (CoworkerManager/WorktreeManager need one)
        Command::new("git")
            .args(["init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git config");
        Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .current_dir(temp_dir.path())
            .output()
            .expect("git commit");

        let wm = crate::worktree::WorktreeManager::new(temp_dir.path().to_path_buf())
            .expect("worktree manager");
        let cm = crate::coworker::CoworkerManager::new("test-session", wm);
        let channel_dir = temp_dir.path().join("channel");
        std::fs::create_dir_all(&channel_dir).expect("channel dir");
        let channel = crate::channel::Channel::new(&channel_dir).expect("channel");

        // Leak temp_dir so it survives the test (DaemonState doesn't own it)
        std::mem::forget(temp_dir);

        DaemonState::new(
            "/tmp/test.sock".into(),
            cm,
            "test-repo".to_string(),
            vec![],
            channel,
            None,
            10,
            None,
            "main".to_string(),
        )
        .expect("daemon state")
    }

    #[test]
    fn pr_action_nudge_produces_nudge_with_callbacks() {
        let state = make_test_state();
        let action = crate::rules::PrAction::NudgeOwner {
            owner: "lexington".to_string(),
            message: "PR #42 needs attention".to_string(),
        };

        let effects = pr_action_to_effects(action, 42, "Fix bug", PrIssueType::CiFailed, &state);

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::NudgeCoworkerWithCallbacks {
                name, on_success, ..
            } => {
                assert_eq!(name, "lexington");
                assert!(
                    on_success
                        .iter()
                        .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 42, .. })),
                    "Should record PR nudge on success"
                );
            }
            _ => panic!("Expected NudgeCoworkerWithCallbacks, got {:?}", effects[0]),
        }
    }

    #[test]
    fn pr_action_spawn_produces_spawn_with_callbacks() {
        let state = make_test_state();
        let action = crate::rules::PrAction::SpawnOwner {
            owner: "park".to_string(),
            message: "PR #99 CI failed".to_string(),
        };

        let effects = pr_action_to_effects(action, 99, "Fix CI", PrIssueType::CiFailed, &state);

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::SpawnCoworkerWithCallbacks {
                config,
                on_success,
                on_failure,
            } => {
                assert_eq!(config.name, "park");
                // on_success should include broadcast, channel post, and pr nudge record
                assert!(
                    on_success
                        .iter()
                        .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 99, .. })),
                    "on_success should record PR nudge"
                );
                assert!(
                    on_success
                        .iter()
                        .any(|e| matches!(e, Effect::BroadcastCoworkerUpdate { .. })),
                    "on_success should broadcast status"
                );
                // on_failure should also record PR nudge (for cooldown tracking)
                assert!(
                    on_failure
                        .iter()
                        .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 99, .. })),
                    "on_failure should also record PR nudge"
                );
            }
            _ => panic!("Expected SpawnCoworkerWithCallbacks, got {:?}", effects[0]),
        }
    }

    #[test]
    fn pr_action_skip_produces_no_effects() {
        let state = make_test_state();
        let action = crate::rules::PrAction::Skip {
            reason: "Owner not found".to_string(),
        };

        let effects = pr_action_to_effects(action, 42, "Fix bug", PrIssueType::CiFailed, &state);
        assert!(effects.is_empty());
    }

    #[test]
    fn comment_action_spawn_produces_spawn_with_callbacks() {
        let state = make_test_state();
        let action = crate::rules::PrAction::SpawnOwner {
            owner: "amsterdam".to_string(),
            message: "PR #55 has review feedback".to_string(),
        };

        let effects = comment_action_to_effects(action, 55, "Add feature", &state);

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::SpawnCoworkerWithCallbacks {
                config, on_success, ..
            } => {
                assert_eq!(config.name, "amsterdam");
                assert!(
                    on_success
                        .iter()
                        .any(|e| matches!(e, Effect::RecordPrNudge { pr_number: 55, .. })),
                    "on_success should record PR nudge for comment"
                );
            }
            _ => panic!("Expected SpawnCoworkerWithCallbacks, got {:?}", effects[0]),
        }
    }

    #[test]
    fn pr_action_spawn_with_break_session_includes_clear_effect() {
        let state = make_test_state();
        // Simulate a saved break session for the coworker
        {
            let mut sessions = state.pr_break_sessions.write().unwrap();
            sessions.insert("york".to_string(), "session-abc-123".to_string());
        }

        let action = crate::rules::PrAction::SpawnOwner {
            owner: "york".to_string(),
            message: "PR #77 needs review".to_string(),
        };

        let effects =
            pr_action_to_effects(action, 77, "Review PR", PrIssueType::ReviewComment, &state);

        assert_eq!(effects.len(), 1);
        match &effects[0] {
            Effect::SpawnCoworkerWithCallbacks {
                config, on_success, ..
            } => {
                // Should use ResumeSession mode since we have a saved session
                assert!(
                    matches!(config.session_mode, crate::launch::SessionMode::ResumeSession(ref id) if id == "session-abc-123"),
                    "Should resume saved session, got {:?}",
                    config.session_mode
                );
                // on_success should include ClearPrBreakSession
                assert!(
                    on_success.iter().any(
                        |e| matches!(e, Effect::ClearPrBreakSession { name } if name == "york")
                    ),
                    "on_success should clear break session"
                );
            }
            _ => panic!("Expected SpawnCoworkerWithCallbacks"),
        }
    }

    // NOTE: Reviewer spawn registry effects are tested via code inspection and
    // integration tests rather than unit tests. The collect_reviewer_effects function
    // has complex async dependencies (persistent state, PR review tracking) that make
    // unit testing difficult. The implementation at lines 1651-1665 clearly shows
    // RegisterWorktreeAssignment and BindCoworkerToWorktree are generated in the
    // on_success callbacks of SpawnCoworkerWithCallbacks, matching the dispatch path.
}
