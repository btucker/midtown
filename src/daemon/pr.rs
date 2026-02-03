//! PR management — polling, auto-merge, reviewer spawning, comment nudging.
//!
//! This module runs in the background to:
//! - Poll open PRs for merge conflicts, CI failures, and review status
//! - Auto-merge approved PRs with all checks passing
//! - Spawn reviewer coworkers for unreviewed PRs
//! - Process pending review spawns from webhook-triggered delays
//! - Nudge PR owners when their PR receives comments

use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use tracing::{debug, info, warn};

use crate::message::{Message, MessageType};
use crate::{config, daemon_messages};

use super::DaemonState;
use super::constants::*;
use super::effects::Effect;
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
        _ => {
            debug!("Failed to get PRs from gh CLI for idle check");
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
            "headRefName",
        ])
        .output();

    let (coworker_names, branch_names): (HashSet<String>, HashSet<String>) = match output {
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
                (coworkers, branches)
            } else {
                (HashSet::new(), HashSet::new())
            }
        }
        _ => {
            debug!("Failed to get merged PRs from gh CLI for idle check");
            (HashSet::new(), HashSet::new())
        }
    };

    // Update cache
    {
        let mut cache = state.pr_coworker_cache.write().unwrap();
        cache.merged_pr_owners = coworker_names.clone();
        cache.merged_pr_branches = branch_names;
    }
    {
        let mut cooldowns = state.cooldowns.lock().unwrap();
        cooldowns.record("merged_pr_fetch", "global");
    }

    coworker_names
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
    let response_hash = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        stdout.hash(&mut hasher);
        hasher.finish()
    };
    {
        let mut last_hash = state.last_pr_poll_hash.lock().await;
        if *last_hash == response_hash && response_hash != 0 {
            debug!("PR poll: data unchanged, skipping processing");
            return Ok(effects);
        }
        *last_hash = response_hash;
    }

    let prs: Vec<serde_json::Value> = serde_json::from_str(&stdout)?;

    // Cleanup old tracking entries, but preserve assignments for active coworkers
    // so reviewers don't lose their PR tracking while still running
    let active_coworker_names: HashSet<String> = snap
        .active_coworkers
        .iter()
        .map(|cw| cw.name.clone())
        .collect();
    {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.cleanup();
    }
    {
        let mut ps = state.persistent_state.lock().await;
        ps.github.cleanup_expired_preserving(&active_coworker_names);
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
        ps.github.cleanup_expired_preserving(&active_coworker_names);
        if let Err(e) = ps.save_for_repo(&state.repo_name) {
            warn!("Failed to save daemon-state.json after cleanup: {}", e);
        }
    }

    for pr in &prs {
        let pr_number = pr.get("number").and_then(|n| n.as_u64()).unwrap_or(0);
        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");

        // Extract owner from branch prefix (e.g., "amsterdam/feature" -> "amsterdam")
        let owner = head_ref.split('/').next().unwrap_or("");

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

            // For approved PRs with all checks passing, auto-merge instead of nudging
            use crate::rules::decide_pr_issue_action;
            if issue_type == PrIssueType::Approved && is_auto_mergeable(pr) {
                info!(
                    "PR #{} ({}) is approved with all checks passing — auto-merging",
                    pr_number, title
                );
                effects.push(Effect::AutoMergePr {
                    pr_number,
                    title: title.to_string(),
                });
                // Always record the cooldown, even on failure, to prevent
                // retrying every poll interval (30s) for persistent failures.
                effects.push(Effect::RecordPrNudge {
                    pr_number,
                    issue_type,
                });
                continue;
            }

            // Format the nudge message
            let message = format!(
                "PR #{} ({}) - {}: {}",
                pr_number,
                truncate_str(title, 40),
                issue_type,
                get_issue_action(issue_type)
            );

            // Decide action using pure decision function
            let action =
                decide_pr_issue_action(owner, &active_coworkers, state.is_at_dev_limit(), &message);

            effects.extend(pr_action_to_effects(
                action, pr_number, title, issue_type, state,
            ));
        }
    }

    // Polling fallback for review comment notifications (when webhooks are degraded)
    effects.extend(collect_comment_notification_effects(state, &prs, &active_coworkers).await);

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

    // Check for stuck conditions and nudge lead if self-healing has failed
    effects.extend(collect_stuck_condition_effects(state, &prs, &reviewed_prs).await);

    Ok(effects)
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
                Some(sid) => crate::tmux::SessionMode::ResumeSession(sid.to_string()),
                None => crate::tmux::SessionMode::Resume,
            };
            let config = crate::tmux::ClaudeLaunchConfig::coworker(
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
async fn collect_stuck_condition_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
    reviewed_prs: &HashSet<u64>,
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
                let context = if is_assigned {
                    "I assigned a reviewer but no review has been posted yet"
                } else {
                    "I couldn't assign a reviewer"
                };
                let nudge = format!(
                    "@lead PR #{} ({}) has been open for {} minutes with no review — {}",
                    pr_number,
                    truncate_str(title, 40),
                    age_secs / 60,
                    context,
                );
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
                let nudge = format!(
                    "@lead PR #{} ({}) has had unresolved review feedback for {} minutes — the author hasn't pushed new changes",
                    pr_number,
                    truncate_str(title, 40),
                    stuck_duration.as_secs() / 60,
                );
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
                let nudge = format!(
                    "@lead PR #{} ({}) is approved and CI is green but hasn't merged after {} minutes — auto-merge may have failed",
                    pr_number,
                    truncate_str(title, 40),
                    stuck_duration.as_secs() / 60,
                );
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
        let busy_coworkers = crate::tasks::get_busy_coworkers_for_repo(&state.repo_name);
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
                            format!("task #{} ({})", id, truncate_str(&subject, 30))
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
    {
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
/// 2. If count increased since last poll, check cooldown and nudge owner
///
/// This enables the polling path to fill the gap identified in graceful degradation:
/// webhooks handle real-time notifications, polling handles the fallback case.
async fn collect_comment_notification_effects(
    state: &DaemonState,
    prs: &[serde_json::Value],
    active_coworkers: &[String],
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

        // Only check coworker-owned PRs
        let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
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

        let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
        let nudge_msg = format!(
            "Your PR #{} ({}) has new review comments — please address feedback.",
            pr_number,
            truncate_str(title, 40)
        );

        debug!(
            "Polling detected new review comments on PR #{}, nudging {}",
            pr_number, owner
        );

        // Decide action using pure decision function (same as webhook path)
        let action = crate::rules::decide_pr_comment_action(
            &owner,
            "reviewer", // Generic actor since we don't know the specific commenter from polling
            active_coworkers.contains(&owner),
            state.is_at_dev_limit(),
            &nudge_msg,
        );

        effects.extend(comment_action_to_effects(action, pr_number, title, state));
    }

    effects
}

/// Convert a comment notification `PrAction` into effects.
///
/// Similar to `pr_action_to_effects` but uses the comment-specific cooldown
/// and messages.
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
                Some(sid) => crate::tmux::SessionMode::ResumeSession(sid.to_string()),
                None => crate::tmux::SessionMode::Resume,
            };
            let config = crate::tmux::ClaudeLaunchConfig::coworker(
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

            // Before cleaning up the assignment, check if the reviewer is still running.
            let reviewer_still_running = {
                let ps = state.persistent_state.lock().await;
                if let Some(reviewer_name) = ps.github.get_reviewer(pr_number) {
                    state.coworkers.get(reviewer_name).is_some()
                } else {
                    false
                }
            };

            if reviewer_still_running {
                debug!(
                    "PR #{} has Claude review but reviewer is still running — keeping assignment",
                    pr_number
                );
            } else {
                // Free the tracker slot — the review completed and the reviewer is gone
                let mut ps = state.persistent_state.lock().await;
                if ps.github.is_assigned(pr_number) {
                    debug!("PR #{} review completed, freeing tracker slot", pr_number);
                    ps.github.remove_assignment(pr_number);
                    if let Err(e) = ps.save_for_repo(&state.repo_name) {
                        warn!("Failed to save daemon-state.json: {}", e);
                    }
                }
            }

            // Nudge the PR author — review is complete but PR is still open
            let head_ref = pr.get("headRefName").and_then(|s| s.as_str()).unwrap_or("");
            let title = pr.get("title").and_then(|s| s.as_str()).unwrap_or("");
            let owner = head_ref.split('/').next().unwrap_or("");

            if !owner.is_empty() {
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

                    let action = crate::rules::decide_review_complete_action(
                        owner,
                        &active_coworkers,
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

        // reviewer() now takes the PR number and generates both the system prompt
        // (with merged reviewer.md instructions) and the launch prompt internally
        let config = crate::tmux::ClaudeLaunchConfig::reviewer(reviewer_name.clone(), pr_number);

        let on_success = vec![
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
                Some(sid) => crate::tmux::SessionMode::ResumeSession(sid.to_string()),
                None => crate::tmux::SessionMode::Resume,
            };
            let config = crate::tmux::ClaudeLaunchConfig::coworker(
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
                    debug!("Failed to parse review JSON for PR #{}: {}", pr_number, e);
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
        _ => {
            debug!("Failed to fetch reviews/comments for PR #{}", pr_number);
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

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    coworker_from_branch(&branch)
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

    // Resolve the PR owner: use webhook data if available, otherwise look up async
    let owner = match activity.owner_coworker {
        Some(ref o) => Some(o.clone()),
        None => get_pr_owner_coworker_async(pr_number).await,
    };

    let Some(owner) = owner else {
        debug!("PR #{} has no coworker owner, skipping nudge", pr_number);
        return;
    };

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

    // Decide action using pure decision function
    let is_active = state.coworkers.get(&owner).is_some();
    let action = crate::rules::decide_pr_comment_action(
        &owner,
        &activity.actor,
        is_active,
        state.is_at_dev_limit(),
        &nudge_msg,
    );

    let success = match action {
        crate::rules::PrAction::NudgeOwner {
            owner: ref o,
            message: ref msg,
        } => match state.coworkers.nudge(o, msg) {
            Ok(()) => {
                info!(
                    "Nudged {} about review comment on PR #{} from {}",
                    o, pr_number, activity.actor
                );
                true
            }
            Err(e) => {
                warn!("Failed to nudge {} about PR #{}: {}", o, pr_number, e);
                false
            }
        },
        crate::rules::PrAction::SpawnOwner {
            owner: ref o,
            message: ref msg,
        } => {
            info!(
                "PR #{} owner {} is not active, spawning to address review feedback",
                pr_number, o
            );
            let saved_session = {
                let sessions = state.pr_break_sessions.read().unwrap();
                sessions.get(o).cloned()
            };
            if saved_session.is_some() {
                info!("Resuming saved PR break session for {}", o);
            }
            let session_mode = match saved_session.as_deref() {
                Some(sid) => crate::tmux::SessionMode::ResumeSession(sid.to_string()),
                None => crate::tmux::SessionMode::Resume,
            };
            let config = crate::tmux::ClaudeLaunchConfig::coworker(
                o.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(msg.clone()),
            );
            match state.spawn_coworker(&config).await {
                Ok(_) => {
                    if saved_session.is_some() {
                        let mut sessions = state.pr_break_sessions.write().unwrap();
                        sessions.remove(o);
                    }
                    info!(
                        "Spawned {} to address review feedback on PR #{}",
                        o, pr_number
                    );
                    let call_msg = Message::text(
                        "daemon",
                        format!(
                            "Called in {} to address review feedback on PR #{}",
                            o, pr_number
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&call_msg) {
                        warn!("Failed to post call-in message: {}", e);
                    }
                    true
                }
                Err(e) => {
                    warn!(
                        "Failed to spawn {} for PR #{} review feedback: {}",
                        o, pr_number, e
                    );
                    false
                }
            }
        }
        crate::rules::PrAction::PostToChannel { message: ref msg } => {
            let channel_msg = Message::new("midtown", msg.clone(), MessageType::Text);
            if let Err(e) = state.send_and_broadcast(&channel_msg) {
                warn!("Failed to post PR comment to channel: {}", e);
            }
            true
        }
        crate::rules::PrAction::Skip { ref reason } => {
            debug!("{}", reason);
            false
        }
    };

    // Record the nudge to prevent spamming
    if success {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.record_nudge(pr_number, PrIssueType::ReviewComment);
    }

    // Add eyes reaction to the comment to provide visual feedback that it was received
    if success
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

    // Get active coworkers for the decision function
    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();

    let action = crate::rules::decide_pr_issue_action(
        &owner,
        &active_coworkers,
        state.is_at_dev_limit(),
        &nudge_msg,
    );

    let nudged = match action {
        crate::rules::PrAction::NudgeOwner {
            owner: ref o,
            message: ref msg,
        } => match state.coworkers.nudge(o, msg) {
            Ok(()) => {
                info!(
                    "Webhook: nudged {} about {} on PR #{}",
                    o, issue_type, pr_number
                );
                true
            }
            Err(e) => {
                warn!(
                    "Webhook: failed to nudge {} about {} on PR #{}: {}",
                    o, issue_type, pr_number, e
                );
                false
            }
        },
        crate::rules::PrAction::SpawnOwner {
            owner: ref o,
            message: ref msg,
        } => {
            info!(
                "Webhook: PR #{} owner {} is not active, spawning to address {}",
                pr_number, o, issue_type
            );
            let saved_session = {
                let sessions = state.pr_break_sessions.read().unwrap();
                sessions.get(o).cloned()
            };
            if saved_session.is_some() {
                info!("Resuming saved PR break session for {}", o);
            }
            let session_mode = match saved_session.as_deref() {
                Some(sid) => crate::tmux::SessionMode::ResumeSession(sid.to_string()),
                None => crate::tmux::SessionMode::Resume,
            };
            let config = crate::tmux::ClaudeLaunchConfig::coworker(
                o.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(msg.clone()),
            );
            match state.spawn_coworker(&config).await {
                Ok(_) => {
                    if saved_session.is_some() {
                        let mut sessions = state.pr_break_sessions.write().unwrap();
                        sessions.remove(o);
                    }
                    info!(
                        "Webhook: spawned {} to address {} on PR #{}",
                        o, issue_type, pr_number
                    );
                    state.broadcast_coworker_update(o, "running", None);
                    let call_msg = Message::text(
                        "midtown",
                        crate::daemon_messages::called_in_pr_issue(
                            o,
                            &issue_type.to_string(),
                            pr_number,
                            crate::config::get_personality(),
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&call_msg) {
                        warn!("Failed to post call-in message: {}", e);
                    }
                    true
                }
                Err(e) => {
                    warn!(
                        "Webhook: failed to spawn {} for PR #{} {}: {}",
                        o, pr_number, issue_type, e
                    );
                    false
                }
            }
        }
        crate::rules::PrAction::PostToChannel { message: ref msg } => {
            let channel_msg = Message::new("midtown", msg.clone(), MessageType::Text);
            if let Err(e) = state.send_and_broadcast(&channel_msg) {
                warn!("Failed to post PR issue to channel: {}", e);
            }
            true
        }
        crate::rules::PrAction::Skip { ref reason } => {
            debug!("Webhook: {}", reason);
            false
        }
    };

    if nudged {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.record_nudge(pr_number, issue_type);
    }
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

    let active_coworkers: Vec<String> = state
        .coworkers
        .list()
        .iter()
        .map(|c| c.name.clone())
        .collect();

    let action = crate::rules::decide_pr_issue_action(
        &owner,
        &active_coworkers,
        state.is_at_dev_limit(),
        &nudge_msg,
    );

    let nudged = match action {
        crate::rules::PrAction::NudgeOwner {
            owner: ref o,
            message: ref msg,
        } => match state.coworkers.nudge(o, msg) {
            Ok(()) => {
                info!(
                    "Webhook: nudged {} about CI failure on PR #{}",
                    o, pr_number
                );
                true
            }
            Err(e) => {
                warn!(
                    "Webhook: failed to nudge {} about CI failure on PR #{}: {}",
                    o, pr_number, e
                );
                false
            }
        },
        crate::rules::PrAction::SpawnOwner {
            owner: ref o,
            message: ref msg,
        } => {
            info!(
                "Webhook: PR #{} owner {} is not active, spawning to address CI failure",
                pr_number, o
            );
            let saved_session = {
                let sessions = state.pr_break_sessions.read().unwrap();
                sessions.get(o).cloned()
            };
            if saved_session.is_some() {
                info!("Resuming saved PR break session for {}", o);
            }
            let session_mode = match saved_session.as_deref() {
                Some(sid) => crate::tmux::SessionMode::ResumeSession(sid.to_string()),
                None => crate::tmux::SessionMode::Resume,
            };
            let config = crate::tmux::ClaudeLaunchConfig::coworker(
                o.clone(),
                state.repo_name.clone(),
                session_mode,
                Some(msg.clone()),
            );
            match state.spawn_coworker(&config).await {
                Ok(_) => {
                    if saved_session.is_some() {
                        let mut sessions = state.pr_break_sessions.write().unwrap();
                        sessions.remove(o);
                    }
                    info!(
                        "Webhook: spawned {} to address CI failure on PR #{}",
                        o, pr_number
                    );
                    state.broadcast_coworker_update(o, "running", None);
                    let call_msg = Message::text(
                        "midtown",
                        crate::daemon_messages::called_in_pr_issue(
                            o,
                            "CI failed",
                            pr_number,
                            crate::config::get_personality(),
                        ),
                    );
                    if let Err(e) = state.send_and_broadcast(&call_msg) {
                        warn!("Failed to post call-in message: {}", e);
                    }
                    true
                }
                Err(e) => {
                    warn!(
                        "Webhook: failed to spawn {} for PR #{} CI failure: {}",
                        o, pr_number, e
                    );
                    false
                }
            }
        }
        crate::rules::PrAction::PostToChannel { message: ref msg } => {
            let channel_msg = Message::new("midtown", msg.clone(), MessageType::Text);
            if let Err(e) = state.send_and_broadcast(&channel_msg) {
                warn!("Failed to post PR CI failure to channel: {}", e);
            }
            true
        }
        crate::rules::PrAction::Skip { ref reason } => {
            debug!("Webhook: {}", reason);
            false
        }
    };

    if nudged {
        let mut tracker = state.pr_issue_tracker.lock().await;
        tracker.record_nudge(pr_number, PrIssueType::CiFailed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
