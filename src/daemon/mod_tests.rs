use super::helpers::*;
use super::*;
use crate::rules::{UsageLimitExpiryDecision, decide_usage_limit_expiry};

fn test_headless_info(
    session_id: &str,
    task_id: Option<u64>,
) -> crate::daemon::state::HeadlessSessionInfo {
    crate::daemon::state::HeadlessSessionInfo {
        session_id: session_id.to_string(),
        last_active: chrono::Utc::now(),
        purpose: "test".to_string(),
        pid: Some(12345),
        coworker_type: Some("dev".to_string()),
        task_id,
        pr_number: None,
        working_dir: Some("/tmp/worktree".to_string()),
        provider: Some(crate::auth::AuthProvider::Claude),
        profile: Some("test-profile".to_string()),
        resume_on_startup: true,
    }
}

#[test]
fn test_merge_headless_sessions_preserves_history_and_marks_running_resumable() {
    let mut persisted = HashMap::new();
    persisted.insert(
        "park".to_string(),
        crate::daemon::state::HeadlessSessionInfo {
            resume_on_startup: true,
            ..test_headless_info("old-park", Some(10))
        },
    );
    persisted.insert(
        "lexington".to_string(),
        crate::daemon::state::HeadlessSessionInfo {
            resume_on_startup: true,
            ..test_headless_info("old-lex", Some(11))
        },
    );

    let mut running = HashMap::new();
    running.insert("park".to_string(), test_headless_info("new-park", Some(10)));
    running.insert(
        "madison".to_string(),
        test_headless_info("new-madison", Some(12)),
    );

    let running_count = merge_headless_sessions(&mut persisted, running);

    assert_eq!(running_count, 2);
    assert_eq!(persisted.len(), 3);
    assert!(persisted["park"].resume_on_startup);
    assert_eq!(persisted["park"].session_id, "new-park");
    assert!(!persisted["lexington"].resume_on_startup);
    assert!(persisted["lexington"].pid.is_none());
    assert!(persisted["madison"].resume_on_startup);
}

#[test]
fn test_merge_headless_sessions_marks_all_historical_when_none_running() {
    let mut persisted = HashMap::new();
    persisted.insert("park".to_string(), test_headless_info("sid-park", Some(10)));

    let running_count = merge_headless_sessions(&mut persisted, HashMap::new());

    assert_eq!(running_count, 0);
    assert_eq!(persisted.len(), 1);
    assert!(!persisted["park"].resume_on_startup);
    assert!(persisted["park"].pid.is_none());
}

#[test]
fn test_parse_historical_session_info_from_log_extracts_session_metadata() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log_path = dir.path().join("headless-park.jsonl");
    std::fs::write(
        &log_path,
        r#"{"type":"system","subtype":"init","session_id":"sid-123","model":"claude-opus-4-6","cwd":"/tmp/worktrees/task-42-fix-tests"}
{"type":"assistant","session_id":"sid-123"}"#,
    )
    .expect("write log");

    let info = parse_historical_session_info_from_log(&log_path, "park").expect("parsed");
    assert_eq!(info.session_id, "sid-123");
    assert_eq!(info.task_id, Some(42));
    assert_eq!(
        info.working_dir.as_deref(),
        Some("/tmp/worktrees/task-42-fix-tests")
    );
    assert!(!info.resume_on_startup);
    assert_eq!(info.provider, Some(crate::auth::AuthProvider::Claude));
}

#[test]
fn test_backfill_headless_sessions_from_logs_populates_empty_index_only() {
    let dir = tempfile::tempdir().expect("temp dir");
    let project_dir = dir.path().join("project");
    std::fs::create_dir_all(&project_dir).expect("create project dir");
    std::fs::write(
        project_dir.join("headless-york.jsonl"),
        r#"{"type":"system","subtype":"init","session_id":"sid-york","model":"claude-sonnet-4-5-20250929","cwd":"/tmp/worktrees/task-7-foo"}"#,
    )
    .expect("write york log");

    let mut persisted = HashMap::new();
    let recovered = backfill_headless_sessions_from_dir(&project_dir, &mut persisted);
    assert_eq!(recovered, 1);
    assert!(persisted.contains_key("york"));
    assert!(!persisted["york"].resume_on_startup);

    // No-op when index already has data.
    let recovered_again = backfill_headless_sessions_from_dir(&project_dir, &mut persisted);
    assert_eq!(recovered_again, 0);
    assert_eq!(persisted.len(), 1);
}

// URL parsing tests for extract_repo_name_from_url
#[test]
fn test_extract_repo_name_https_url() {
    assert_eq!(
        extract_repo_name_from_url("https://github.com/owner/repo.git"),
        Some("owner/repo".to_string())
    );
    assert_eq!(
        extract_repo_name_from_url("https://github.com/owner/repo"),
        Some("owner/repo".to_string())
    );
    assert_eq!(
        extract_repo_name_from_url("https://github.com/btucker/midtown.git"),
        Some("btucker/midtown".to_string())
    );
}

#[test]
fn test_extract_repo_name_ssh_url() {
    assert_eq!(
        extract_repo_name_from_url("git@github.com:owner/repo.git"),
        Some("owner/repo".to_string())
    );
    assert_eq!(
        extract_repo_name_from_url("git@github.com:owner/repo"),
        Some("owner/repo".to_string())
    );
    assert_eq!(
        extract_repo_name_from_url("git@github.com:btucker/midtown.git"),
        Some("btucker/midtown".to_string())
    );
}

#[test]
fn test_extract_repo_name_with_whitespace() {
    assert_eq!(
        extract_repo_name_from_url("  https://github.com/owner/repo.git  \n"),
        Some("owner/repo".to_string())
    );
    assert_eq!(
        extract_repo_name_from_url("git@github.com:owner/repo.git\n"),
        Some("owner/repo".to_string())
    );
}

#[test]
fn test_extract_repo_name_invalid() {
    assert_eq!(extract_repo_name_from_url("not a url"), None);
    assert_eq!(extract_repo_name_from_url(""), None);
}

// Auto-nudge helper tests
#[test]
fn test_extract_pr_number_pr_hash() {
    assert_eq!(extract_pr_number("opened PR #42: Add feature"), Some(42));
    assert_eq!(extract_pr_number("merged PR #123"), Some(123));
    assert_eq!(extract_pr_number("btucker approved PR #99"), Some(99));
}

#[test]
fn test_extract_pr_number_standalone_hash() {
    assert_eq!(extract_pr_number("commented on #55: looks good"), Some(55));
    assert_eq!(
        extract_pr_number("Check 'build' passed on PR #77"),
        Some(77)
    );
}

#[test]
fn test_extract_pr_number_none() {
    assert_eq!(extract_pr_number("no pr reference here"), None);
    assert_eq!(extract_pr_number("just some text"), None);
}

#[test]
fn test_coworker_from_branch() {
    assert_eq!(
        coworker_from_branch("lexington/fix-auth"),
        Some("lexington".to_string())
    );
    assert_eq!(
        coworker_from_branch("park/add-feature"),
        Some("park".to_string())
    );
    assert_eq!(
        coworker_from_branch("madison/refactor"),
        Some("madison".to_string())
    );
}

#[test]
fn test_coworker_from_branch_case_insensitive() {
    assert_eq!(
        coworker_from_branch("LEXINGTON/fix"),
        Some("lexington".to_string())
    );
    assert_eq!(coworker_from_branch("Park/thing"), Some("park".to_string()));
}

#[test]
fn test_coworker_from_branch_not_coworker() {
    assert_eq!(coworker_from_branch("feature/something"), None);
    assert_eq!(coworker_from_branch("fix/bug"), None);
    assert_eq!(coworker_from_branch("main"), None);
}

// Lead nudge tests
#[test]
fn test_is_coworker_sender() {
    // System senders should not be coworkers
    assert!(!is_coworker_sender("Lead"));
    assert!(!is_coworker_sender("lead"));
    assert!(!is_coworker_sender("github"));
    assert!(!is_coworker_sender("GitHub"));
    assert!(!is_coworker_sender("system"));

    // Actual coworker names should be detected
    assert!(is_coworker_sender("lexington"));
    assert!(is_coworker_sender("park"));
    assert!(is_coworker_sender("amsterdam"));
    assert!(is_coworker_sender("madison"));
}

#[test]
fn test_lead_nudge_only_on_explicit_at_lead() {
    // Only explicit @lead mentions should trigger nudges.
    // Heuristic keywords like "feedback", "help", "blocked" should NOT trigger.
    let triggers = |msg: &str| msg.to_lowercase().contains("@lead");

    // Should trigger: explicit @lead mentions
    assert!(triggers("@lead should this handle the error case?"));
    assert!(triggers("@Lead can you review this approach?"));
    assert!(triggers("Hey @lead, I'm blocked on the API design"));

    // Should NOT trigger: heuristic keywords without @lead
    assert!(!triggers("I need some feedback on the API design"));
    assert!(!triggers("I'm blocked on the auth issue"));
    assert!(!triggers("I'm stuck here, not sure how to proceed"));
    assert!(!triggers("What do you think about this approach?"));
    assert!(!triggers("I have a question about the architecture"));

    // Should NOT trigger: status updates mentioning "feedback"
    assert!(!triggers("addressing review feedback on PR #227"));
    assert!(!triggers("/me addressing feedback from code review"));

    // Should NOT trigger: coworker-to-coworker messages
    assert!(!triggers("@lexington can you help with this?"));
    assert!(!triggers("@pleasant any progress on task 304?"));
}

#[test]
fn test_system_message_with_at_lead_should_trigger_nudge() {
    // System messages containing @lead should be detected as needing a lead
    // nudge. The chat_monitor_loop checks for @lead in SKIP_SENDERS messages
    // before skipping them. This test validates the detection logic.
    // Mirrors the chat_monitor_loop logic: skip-sender messages nudge lead
    // for @lead, EXCEPT "user" messages (already handled in handle_channel_post).
    let should_nudge = |from: &str, content: &str| -> bool {
        let is_skip_sender = SKIP_SENDERS.iter().any(|&s| s.eq_ignore_ascii_case(from));
        is_skip_sender
            && !from.eq_ignore_ascii_case("user")
            && content.to_lowercase().contains("@lead")
    };

    // System messages with @lead should trigger nudge
    assert!(should_nudge(
        "system",
        "⚠️ @lead Orphaned worktrees with unmerged commits: amsterdam, park"
    ));

    // Midtown daemon messages with @lead should also trigger
    assert!(should_nudge(
        "midtown",
        "⚠️ @lead something needs attention"
    ));

    // System messages WITHOUT @lead should NOT trigger
    assert!(!should_nudge(
        "system",
        "Channel log rotated: 50 old messages archived"
    ));

    // User messages with @lead should NOT trigger here (handled in handle_channel_post)
    assert!(!should_nudge("user", "@lead what do you think?"));

    // Coworker messages should NOT be in SKIP_SENDERS at all
    assert!(!should_nudge("lexington", "@lead can you review this?"));
}

#[test]
fn test_pr_merge_channel_message_no_at_lead() {
    // The PR merge channel message should NOT contain @lead.
    // This prevents a double-nudge: one from the direct nudge_lead() call,
    // and another from the chat monitor detecting @lead in the system message.
    //
    // The channel message is informational only:
    //   "PR #42 merged into main."
    // The direct nudge includes the actionable instruction:
    //   "PR #42 merged into main. Run `git pull` to stay current."
    let pr_number = 42u64;
    let default_branch = "main";

    // Channel message format (should NOT contain @lead)
    let channel_text = format!("PR #{} merged into {}.", pr_number, default_branch);
    assert!(
        !channel_text.to_lowercase().contains("@lead"),
        "PR merge channel message should not contain @lead: {}",
        channel_text
    );

    // Nudge text format (used for direct nudge, includes instruction)
    let nudge_text = format!(
        "PR #{} merged into {}. Run `git pull` to stay current.",
        pr_number, default_branch
    );
    assert!(
        !nudge_text.to_lowercase().contains("@lead"),
        "PR merge nudge text should not contain @lead (it's for direct nudge): {}",
        nudge_text
    );
}

#[test]
fn test_pr_issue_tracker_should_nudge_new() {
    let tracker = PrIssueTracker::new();
    assert!(tracker.should_nudge(42, PrIssueType::MergeConflict));
    assert!(tracker.should_nudge(42, PrIssueType::CiFailed));
    assert!(tracker.should_nudge(42, PrIssueType::ReviewComplete));
}

#[test]
fn test_pr_issue_tracker_should_nudge_after_record() {
    let mut tracker = PrIssueTracker::new();
    tracker.record_nudge(42, PrIssueType::MergeConflict);

    // Same issue should not be nudged again immediately
    assert!(!tracker.should_nudge(42, PrIssueType::MergeConflict));

    // Different issue type for same PR should be nudged
    assert!(tracker.should_nudge(42, PrIssueType::CiFailed));

    // Same issue type for different PR should be nudged
    assert!(tracker.should_nudge(43, PrIssueType::MergeConflict));
}

#[test]
fn test_pr_issue_tracker_review_complete_independent_of_other_types() {
    let mut tracker = PrIssueTracker::new();

    // Recording a ReviewComment nudge should not block ReviewComplete
    tracker.record_nudge(42, PrIssueType::ReviewComment);
    assert!(tracker.should_nudge(42, PrIssueType::ReviewComplete));

    // Recording ReviewComplete should block itself but not others
    tracker.record_nudge(42, PrIssueType::ReviewComplete);
    assert!(!tracker.should_nudge(42, PrIssueType::ReviewComplete));
    assert!(tracker.should_nudge(42, PrIssueType::Approved));
}

#[test]
fn test_pr_issue_type_display() {
    assert_eq!(PrIssueType::MergeConflict.to_string(), "merge conflict");
    assert_eq!(PrIssueType::CiFailed.to_string(), "CI failed");
    assert_eq!(
        PrIssueType::ChangesRequested.to_string(),
        "changes requested"
    );
    assert_eq!(PrIssueType::Approved.to_string(), "approved");
    assert_eq!(PrIssueType::NeedsReview.to_string(), "needs review");
    assert_eq!(PrIssueType::ReviewComment.to_string(), "review comment");
    assert_eq!(PrIssueType::ReviewComplete.to_string(), "review complete");
    assert_eq!(
        PrIssueType::GreenWithFeedback.to_string(),
        "CI green with feedback"
    );
}

#[test]
fn test_detect_pr_issues_merge_conflict() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "CONFLICTING",
        "statusCheckRollup": [],
        "reviewDecision": ""
    });
    let issues = detect_pr_issues(&pr);
    assert!(issues.contains(&PrIssueType::MergeConflict));
}

#[test]
fn test_detect_pr_issues_ci_failed() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "name": "lint"},
            {"conclusion": "FAILURE", "name": "test"}
        ],
        "reviewDecision": ""
    });
    let issues = detect_pr_issues(&pr);
    assert!(issues.contains(&PrIssueType::CiFailed));
}

#[test]
fn test_detect_pr_issues_changes_requested() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [],
        "reviewDecision": "CHANGES_REQUESTED"
    });
    let issues = detect_pr_issues(&pr);
    assert!(issues.contains(&PrIssueType::ChangesRequested));
}

#[test]
fn test_detect_pr_issues_approved() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [],
        "reviewDecision": "APPROVED"
    });
    let issues = detect_pr_issues(&pr);
    assert!(issues.contains(&PrIssueType::Approved));
}

#[test]
fn test_detect_pr_issues_no_issues() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "name": "test"}
        ],
        "reviewDecision": ""
    });
    let issues = detect_pr_issues(&pr);
    assert!(issues.is_empty());
}

#[test]
fn test_detect_pr_issues_multiple() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "CONFLICTING",
        "statusCheckRollup": [
            {"conclusion": "FAILURE", "name": "test"}
        ],
        "reviewDecision": "CHANGES_REQUESTED"
    });
    let issues = detect_pr_issues(&pr);
    assert_eq!(issues.len(), 3);
    assert!(issues.contains(&PrIssueType::MergeConflict));
    assert!(issues.contains(&PrIssueType::CiFailed));
    assert!(issues.contains(&PrIssueType::ChangesRequested));
}

// -----------------------------------------------------------------------
// is_auto_mergeable tests
// -----------------------------------------------------------------------

#[test]
fn test_auto_mergeable_approved_all_checks_pass() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "name": "test"},
            {"conclusion": "SUCCESS", "name": "lint"}
        ],
        "reviewDecision": "APPROVED"
    });
    assert!(is_auto_mergeable(&pr));
}

#[test]
fn test_auto_mergeable_not_approved() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "name": "test"}
        ],
        "reviewDecision": "REVIEW_REQUIRED"
    });
    assert!(!is_auto_mergeable(&pr));
}

#[test]
fn test_auto_mergeable_has_ci_failure() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "FAILURE", "name": "test"}
        ],
        "reviewDecision": "APPROVED"
    });
    assert!(!is_auto_mergeable(&pr));
}

#[test]
fn test_auto_mergeable_has_merge_conflict() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "CONFLICTING",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "name": "test"}
        ],
        "reviewDecision": "APPROVED"
    });
    assert!(!is_auto_mergeable(&pr));
}

#[test]
fn test_auto_mergeable_has_pending_checks() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [
            {"conclusion": "SUCCESS", "name": "test"},
            {"conclusion": "", "name": "deploy"}
        ],
        "reviewDecision": "APPROVED"
    });
    assert!(!is_auto_mergeable(&pr));
}

#[test]
fn test_auto_mergeable_empty_checks() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "statusCheckRollup": [],
        "reviewDecision": "APPROVED"
    });
    assert!(is_auto_mergeable(&pr));
}

#[test]
fn test_auto_mergeable_no_checks_field() {
    let pr = serde_json::json!({
        "number": 42,
        "mergeable": "MERGEABLE",
        "reviewDecision": "APPROVED"
    });
    assert!(is_auto_mergeable(&pr));
}

#[test]
fn test_get_issue_action() {
    assert_eq!(
        get_issue_action(PrIssueType::MergeConflict),
        "please rebase"
    );
    assert_eq!(
        get_issue_action(PrIssueType::CiFailed),
        "please investigate"
    );
    assert_eq!(
        get_issue_action(PrIssueType::ChangesRequested),
        "please address feedback"
    );
    assert_eq!(
        get_issue_action(PrIssueType::Approved),
        "approved with CI green — please merge (use --auto if checks pending)"
    );
    assert_eq!(
        get_issue_action(PrIssueType::NeedsReview),
        "calling in reviewer"
    );
    assert_eq!(
        get_issue_action(PrIssueType::ReviewComment),
        "please address review feedback and merge if appropriate"
    );
    assert_eq!(
        get_issue_action(PrIssueType::ReviewComplete),
        "review is complete — please address feedback and merge if appropriate"
    );
    assert_eq!(
        get_issue_action(PrIssueType::GreenWithFeedback),
        "CI is green — please address review feedback and merge"
    );
}

#[test]
fn test_truncate_str() {
    assert_eq!(truncate_str("hello", 10), "hello");
    assert_eq!(truncate_str("hello world", 8), "hello...");
    assert_eq!(truncate_str("hi", 2), "hi");
}

// Stuck condition tracker tests
#[test]
fn test_stuck_tracker_track_and_should_nudge() {
    let mut tracker = StuckConditionTracker::new();

    // Not tracked yet — should_nudge returns false
    assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));

    // Track it — now should_nudge returns true (never nudged before)
    tracker.track("42", StuckConditionType::NoReview);
    assert!(tracker.should_nudge("42", StuckConditionType::NoReview));
}

#[test]
fn test_stuck_tracker_record_nudge_cooldown() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("42", StuckConditionType::NoReview);

    // Before recording nudge — should_nudge is true
    assert!(tracker.should_nudge("42", StuckConditionType::NoReview));

    // After recording nudge — should_nudge is false (within cooldown)
    tracker.record_nudge("42", StuckConditionType::NoReview);
    assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
}

#[test]
fn test_stuck_tracker_independent_conditions() {
    let mut tracker = StuckConditionTracker::new();

    // Track two different conditions for the same PR
    tracker.track("42", StuckConditionType::NoReview);
    tracker.track("42", StuckConditionType::MergeReady);

    // Nudging one doesn't affect the other
    tracker.record_nudge("42", StuckConditionType::NoReview);
    assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
    assert!(tracker.should_nudge("42", StuckConditionType::MergeReady));
}

#[test]
fn test_stuck_tracker_clear() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("42", StuckConditionType::NoReview);
    assert!(tracker.should_nudge("42", StuckConditionType::NoReview));

    // Clear the condition
    tracker.clear("42", StuckConditionType::NoReview);
    assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
}

#[test]
fn test_stuck_tracker_different_prs() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("42", StuckConditionType::NoReview);
    tracker.track("43", StuckConditionType::NoReview);

    // Nudging one PR doesn't affect the other
    tracker.record_nudge("42", StuckConditionType::NoReview);
    assert!(!tracker.should_nudge("42", StuckConditionType::NoReview));
    assert!(tracker.should_nudge("43", StuckConditionType::NoReview));
}

#[test]
fn test_stuck_condition_type_display() {
    assert_eq!(StuckConditionType::NoReview.to_string(), "no review");
    assert_eq!(
        StuckConditionType::UnresolvedFeedback.to_string(),
        "unresolved feedback"
    );
    assert_eq!(
        StuckConditionType::MergeReady.to_string(),
        "merge-ready but not merged"
    );
    assert_eq!(
        StuckConditionType::SilentCoworker.to_string(),
        "silent coworker"
    );
    assert_eq!(
        StuckConditionType::ReviewBacklog.to_string(),
        "review backlog"
    );
}

#[test]
fn test_stuck_tracker_nudge_count() {
    let mut tracker = StuckConditionTracker::new();

    // Not tracked yet — nudge count is 0
    assert_eq!(
        tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
        0
    );

    // Track and first nudge
    tracker.track("lex", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
        0
    );

    tracker.record_nudge("lex", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
        1
    );

    // Second nudge (would be escalation)
    tracker.record_nudge("lex", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
        2
    );
}

#[test]
fn test_stuck_tracker_nudge_count_cleared_on_clear() {
    let mut tracker = StuckConditionTracker::new();
    tracker.track("lex", StuckConditionType::SilentCoworker);
    tracker.record_nudge("lex", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
        1
    );

    // Clear resets everything
    tracker.clear("lex", StuckConditionType::SilentCoworker);
    assert_eq!(
        tracker.nudge_count("lex", StuckConditionType::SilentCoworker),
        0
    );
}

// Chat monitor @mention tests
#[test]
fn test_extract_mentions_single() {
    let mentions = extract_mentions("@park please review this");
    assert_eq!(mentions, vec!["park"]);
}

#[test]
fn test_extract_mentions_multiple() {
    let mentions = extract_mentions("@park and @lexington please coordinate");
    assert_eq!(mentions.len(), 2);
    assert!(mentions.contains(&"park".to_string()));
    assert!(mentions.contains(&"lexington".to_string()));
}

#[test]
fn test_extract_mentions_case_insensitive() {
    let mentions = extract_mentions("@PARK please review");
    assert_eq!(mentions, vec!["park"]);
}

#[test]
fn test_extract_mentions_no_duplicates() {
    let mentions = extract_mentions("@park @park @park");
    assert_eq!(mentions, vec!["park"]);
}

#[test]
fn test_extract_mentions_word_boundary() {
    // @parkway should not match @park
    let mentions = extract_mentions("@parkway is not a coworker");
    assert!(mentions.is_empty());
}

#[test]
fn test_extract_mentions_at_end() {
    let mentions = extract_mentions("cc @amsterdam");
    assert_eq!(mentions, vec!["amsterdam"]);
}

#[test]
fn test_extract_mentions_no_mentions() {
    let mentions = extract_mentions("just a regular message");
    assert!(mentions.is_empty());
}

#[test]
fn test_extract_mentions_invalid_names() {
    // feature is not a valid coworker name
    let mentions = extract_mentions("@feature @bug @test");
    assert!(mentions.is_empty());
}

#[test]
fn test_extract_mentions_all_coworker_names() {
    // Verify all coworker names work
    for &name in COWORKER_NAMES {
        let msg = format!("@{} please help", name);
        let mentions = extract_mentions(&msg);
        assert_eq!(mentions, vec![name], "Failed for coworker: {}", name);
    }
}

#[test]
fn test_skip_senders() {
    // Verify SKIP_SENDERS contains expected values.
    // "user" is skipped because handle_channel_post routes user @mentions
    // directly, similar to how the webhook handler routes "github" mentions.
    assert!(SKIP_SENDERS.contains(&"midtown"));
    assert!(SKIP_SENDERS.contains(&"system"));
    assert!(SKIP_SENDERS.contains(&"github"));
    assert!(SKIP_SENDERS.contains(&"user"));
    // "architect" is skipped to prevent diagram messages from triggering
    // @mention routing in the chat monitor.
    assert!(SKIP_SENDERS.contains(&"architect"));
}

#[test]
fn test_webhook_mentions_should_be_extracted() {
    // Webhook messages from "github" contain @mentions that should be routed.
    // The chat monitor skips "github" messages for loop protection, so the
    // webhook handler must call route_mentions directly — but only for
    // non-CI-success events (see test_ci_check_passed_should_not_route_mentions).
    //
    // Example: "@riverside merged PR #178" from sender "github"
    // The @riverside mention should be extracted and routed.
    let webhook_content = "@riverside merged PR #178";
    let mentions = extract_mentions(webhook_content);
    assert_eq!(mentions, vec!["riverside"]);

    // PR merge notifications often include PR author in the message
    let merge_content = "@lexington PR #42 was merged by btucker";
    let mentions = extract_mentions(merge_content);
    assert_eq!(mentions, vec!["lexington"]);

    // Multiple mentions in webhook messages
    let review_content = "@park @madison please review PR #99";
    let mentions = extract_mentions(review_content);
    assert!(mentions.contains(&"park".to_string()));
    assert!(mentions.contains(&"madison".to_string()));
}

#[test]
fn test_ci_check_passed_should_not_route_mentions() {
    // Bug: CI check pass webhook events (e.g., "@madison Check 'build' passed
    // on PR #99") contain @mentions that the webhook handler was routing via
    // route_mentions(). This caused a loop: madison gets called in → goes
    // idle → next CI check @mention triggers another call-in.
    //
    // CI success notifications are informational — they should NOT trigger
    // coworker spawn/nudge. The webhook handler must skip route_mentions
    // when ci_check_passed is set.

    // CI check pass messages contain @mentions
    let ci_content = "@madison Check 'build' passed on PR #99";
    let mentions = extract_mentions(ci_content);
    assert_eq!(
        mentions,
        vec!["madison"],
        "CI message does contain @mention"
    );

    // Construct a WebhookEvent with ci_check_passed set
    let mut event = crate::webhook::WebhookEvent::github(ci_content);
    event.ci_check_passed = Some(crate::webhook::CiCheckPassed {
        check_name: "build".to_string(),
        target: "PR #99".to_string(),
        mention_prefix: "@madison ".to_string(),
    });

    // The webhook handler should skip route_mentions when ci_check_passed is set.
    // We verify this by checking the flag that the handler uses to decide.
    assert!(
        event.ci_check_passed.is_some(),
        "ci_check_passed flag should be set for CI success events"
    );

    // Batched CI notifications (which replace ci_check_passed events) also
    // contain @mentions, but they're posted with from="github" and caught
    // by the chat monitor's SKIP_SENDERS filter.
    let batched_content = "@madison 5 checks passed on PR #99";
    let batched_mentions = extract_mentions(batched_content);
    assert_eq!(
        batched_mentions,
        vec!["madison"],
        "batched CI message also contains @mention"
    );

    // The "github" sender is in SKIP_SENDERS, so chat monitor correctly
    // skips batched messages.
    assert!(
        SKIP_SENDERS.contains(&"github"),
        "github must be in SKIP_SENDERS"
    );
}

#[test]
fn test_contains_at_all_basic() {
    assert!(contains_at_all("@all please check the latest changes"));
    assert!(contains_at_all("Hey @all, important update"));
    assert!(contains_at_all("message for @all"));
}

#[test]
fn test_contains_at_all_case_insensitive() {
    assert!(contains_at_all("@ALL please review"));
    assert!(contains_at_all("@All heads up"));
    assert!(contains_at_all("@aLl check this"));
}

#[test]
fn test_contains_at_all_word_boundary() {
    // Should NOT match @allison or @alliance (part of a longer word)
    assert!(!contains_at_all("@allison please help"));
    assert!(!contains_at_all("@alliance meeting at 3"));
    assert!(!contains_at_all("@allowed to proceed"));
}

#[test]
fn test_contains_at_all_at_end() {
    assert!(contains_at_all("message to @all"));
}

#[test]
fn test_contains_at_all_with_punctuation() {
    assert!(contains_at_all("@all: important update"));
    assert!(contains_at_all("@all, please check"));
    assert!(contains_at_all("@all!"));
}

#[test]
fn test_contains_at_all_no_match() {
    assert!(!contains_at_all("just a regular message"));
    assert!(!contains_at_all("@ all with space"));
}

#[test]
fn test_extract_mentions_does_not_include_at_all() {
    // @all is not a coworker name, so extract_mentions should not return it
    let mentions = extract_mentions("@all please check");
    assert!(mentions.is_empty());
}

#[test]
fn test_user_mentions_coworker_should_be_extracted() {
    // When a user @mentions a coworker in their message, the mention should
    // be extracted so it can be routed directly to that coworker (not just
    // to the lead). This validates the contract that handle_channel_post
    // relies on when calling route_mentions for user messages.
    let user_msg = "@lexington please review PR #42";
    let mentions = extract_mentions(user_msg);
    assert_eq!(mentions, vec!["lexington"]);

    // User mentioning multiple coworkers
    let multi_msg = "@park and @madison can you pair on this?";
    let mentions = extract_mentions(multi_msg);
    assert!(mentions.contains(&"park".to_string()));
    assert!(mentions.contains(&"madison".to_string()));

    // User mentioning @lead should NOT appear in coworker mentions
    // (@lead is handled separately in handle_channel_post)
    let lead_msg = "@lead what do you think?";
    let mentions = extract_mentions(lead_msg);
    assert!(mentions.is_empty() || !mentions.contains(&"lead".to_string()));
}

#[test]
fn test_user_mention_routing_skips_lead() {
    // When a user message @mentions a coworker, the lead should NOT be
    // nudged — the daemon routes directly to the coworker. This test
    // validates the detection logic used in handle_channel_post.

    // User @mentions a coworker → has_coworker_mentions = true, skip lead
    let content = "@riverside continue";
    let has_coworker_mentions = !extract_mentions(content).is_empty() || contains_at_all(content);
    let has_lead_mention = content.to_lowercase().contains("@lead");
    assert!(has_coworker_mentions);
    assert!(!has_lead_mention);
    // Should skip lead: has_coworker_mentions && !has_lead_mention
    assert!(has_coworker_mentions && !has_lead_mention);

    // User sends a regular message → no mentions, nudge lead
    let content = "how is task 5 going?";
    let has_coworker_mentions = !extract_mentions(content).is_empty() || contains_at_all(content);
    assert!(!has_coworker_mentions);

    // User @mentions coworker AND @lead → nudge lead too
    let content = "@riverside @lead please coordinate on this";
    let has_coworker_mentions = !extract_mentions(content).is_empty() || contains_at_all(content);
    let has_lead_mention = content.to_lowercase().contains("@lead");
    assert!(has_coworker_mentions);
    assert!(has_lead_mention);

    // User uses @all → coworker mentions detected, skip lead
    // (route_at_all already broadcasts to lead)
    let content = "@all stand up time";
    let has_coworker_mentions = !extract_mentions(content).is_empty() || contains_at_all(content);
    let has_lead_mention = content.to_lowercase().contains("@lead");
    assert!(has_coworker_mentions);
    assert!(!has_lead_mention);
}

// Review signature detection tests
#[test]
fn test_text_contains_review_signature_emoji() {
    // Legacy formal review signature
    assert!(text_contains_review_signature("🤖 Reviewed by lexington"));
    assert!(text_contains_review_signature(
        "Some preamble\n🤖 Reviewed by park\nMore text"
    ));
}

#[test]
fn test_text_contains_review_signature_plain() {
    // Plain "Reviewed by" without emoji
    assert!(text_contains_review_signature("Reviewed by columbus"));
    assert!(text_contains_review_signature("LGTM! Reviewed by york"));
}

#[test]
fn test_text_contains_review_signature_frontmatter() {
    // Frontmatter ALONE should NOT match (all coworker comments have frontmatter)
    assert!(!text_contains_review_signature(
        "<!-- midtown: lexington -->"
    ));
    assert!(!text_contains_review_signature(
        "<!-- midtown: park -->\n\n## Summary\nLooks good!"
    ));
    assert!(!text_contains_review_signature(
        "Some text\n<!-- midtown: york -->\nMore text"
    ));

    // But frontmatter + review header SHOULD match
    assert!(text_contains_review_signature(
        "<!-- midtown: lexington -->\n\n## Code Review by lexington\n\nFound issues..."
    ));
}

#[test]
fn test_text_contains_review_signature_code_review_header() {
    // Code review header used by review agent
    assert!(text_contains_review_signature("## Code Review by madison"));
    assert!(text_contains_review_signature(
        "<!-- midtown: madison -->\n\n## Code Review by madison\n\nNice work!"
    ));
}

#[test]
fn test_text_contains_review_signature_code_review_skill_output() {
    // The code-review skill posts comments in this exact format.
    // The <!-- midtown: name --> frontmatter is the primary signature.
    let skill_output_clean = r#"<!-- midtown: pleasant -->

### Code review

No issues found. Checked for bugs and CLAUDE.md compliance.

🤖 Generated with [Claude Code](https://claude.ai/code)

<sub>- If this code review was useful, please react with 👍. Otherwise, react with 👎.</sub>"#;
    assert!(text_contains_review_signature(skill_output_clean));

    let skill_output_issues = r#"<!-- midtown: vernon -->

### Code review

Found 2 issues:

1. Missing null check (bug due to `unwrap()`)

https://github.com/org/repo/blob/abc123/src/main.rs#L10-L12

2. Config not validated (CLAUDE.md says "validate all config")

https://github.com/org/repo/blob/abc123/CLAUDE.md#L5-L7

🤖 Generated with [Claude Code](https://claude.ai/code)

<sub>- If this code review was useful, please react with 👍. Otherwise, react with 👎.</sub>"#;
    assert!(text_contains_review_signature(skill_output_issues));
}

#[test]
fn test_text_contains_review_signature_code_review_without_frontmatter() {
    // Regression test for PR #869: code-review skill sometimes posts reviews
    // without the <!-- midtown: --> frontmatter. The "### Code review" heading
    // alone should still be detected as a review.
    //
    // Real comment from PR #869 that failed detection:
    let review_without_frontmatter = r#"### Code review

No issues found. Checked for bugs and CLAUDE.md compliance.

🤖 Generated with [Claude Code](https://claude.ai/code)

<sub>- If this code review was useful, please react with 👍. Otherwise, react with 👎.</sub>"#;

    // This should be detected as a review, but currently fails:
    assert!(
        text_contains_review_signature(review_without_frontmatter),
        "Code review heading without frontmatter should still be detected"
    );

    // Case insensitive variant:
    let review_lowercase = r#"### code review

Found 1 issue:

1. Missing error handling

🤖 Generated with [Claude Code](https://claude.ai/code)"#;

    assert!(
        text_contains_review_signature(review_lowercase),
        "Lowercase 'code review' heading should be detected"
    );
}

#[test]
fn test_text_contains_review_signature_none() {
    // Text without any review signature should return false
    assert!(!text_contains_review_signature("Just a regular comment"));
    assert!(!text_contains_review_signature("LGTM!"));
    assert!(!text_contains_review_signature(
        "Thanks for the changes, looks good to me."
    ));
    assert!(!text_contains_review_signature(""));
    // Partial matches shouldn't count
    assert!(!text_contains_review_signature("midtown"));
    assert!(!text_contains_review_signature("Code Review"));
}

#[test]
fn test_review_headroom_constant() {
    assert_eq!(REVIEW_HEADROOM, 2);
}

#[test]
fn test_dev_limit_calculation() {
    // Helper: compute dev cap the same way is_at_dev_limit does
    let dev_cap =
        |max_coworkers: usize| -> usize { max_coworkers.saturating_sub(REVIEW_HEADROOM).max(1) };

    // Normal case: max_coworkers=6, dev cap should be 4
    assert_eq!(dev_cap(6), 4);

    // max_coworkers=4, dev cap should be 2
    assert_eq!(dev_cap(4), 2);

    // max_coworkers=3, dev cap should be 1
    assert_eq!(dev_cap(3), 1);

    // Edge case: max_coworkers=2, dev cap should be 1 (not 0)
    assert_eq!(dev_cap(2), 1);

    // Edge case: max_coworkers=1, dev cap should be 1 (floor at 1)
    assert_eq!(dev_cap(1), 1);

    // Edge case: max_coworkers=0, dev cap should be 1 (floor at 1 via .max(1))
    assert_eq!(dev_cap(0), 1);

    // Large case: max_coworkers=10, dev cap should be 8
    assert_eq!(dev_cap(10), 8);
}

#[test]
fn test_usage_limit_patterns_detect_common_messages() {
    // The usage limit pattern is "/upgrade" or "/extra-usage" which appears
    // on Claude Code's actual usage limit screen. This avoids false positives
    // from code that mentions "usage limit" in comments.
    let messages = vec![
        "You've hit your usage limit. /upgrade to increase your limit.",
        "Usage limit reached for this model. Options: /upgrade or wait.",
        "Try /upgrade to get more tokens or wait 15 minutes.",
        // Claude Code v2.1.33+ uses /extra-usage instead of /upgrade
        "You've hit your limit · resets 11pm (America/Chicago)\n     /extra-usage to finish what you're working on.",
        "/extra-usage to continue working on this task.",
    ];

    for msg in messages {
        assert!(
            crate::rules::has_usage_limit_pattern(msg),
            "Pattern not detected in: {}",
            msg
        );
    }
}

#[test]
fn test_usage_limit_patterns_no_false_positives() {
    let messages = vec![
        "Reading file src/main.rs",
        "Editing src/daemon.rs",
        "Running tests...",
        "Build succeeded",
    ];

    for msg in messages {
        assert!(
            !crate::rules::has_usage_limit_pattern(msg),
            "False positive in: {}",
            msg
        );
    }
}

// ─── Usage Limit Expiry Tests ──────────────────────────────────────

#[test]
fn test_usage_limit_expiry_nudge_now() {
    let now = tokio::time::Instant::now();
    // Nudge was scheduled 1 second ago
    let nudge_at = Some(now - std::time::Duration::from_secs(1));

    let decision = decide_usage_limit_expiry(nudge_at, now);
    assert_eq!(decision, UsageLimitExpiryDecision::NudgeNow);
}

#[test]
fn test_usage_limit_expiry_not_yet() {
    let now = tokio::time::Instant::now();
    // Nudge is 10 minutes in the future
    let nudge_at = Some(now + std::time::Duration::from_secs(600));

    let decision = decide_usage_limit_expiry(nudge_at, now);
    assert_eq!(decision, UsageLimitExpiryDecision::NotYet);
}

#[test]
fn test_usage_limit_expiry_no_nudge() {
    let now = tokio::time::Instant::now();

    let decision = decide_usage_limit_expiry(None, now);
    assert_eq!(decision, UsageLimitExpiryDecision::NoNudge);
}

// -------------------------------------------------------------------------
// Stuck escalation constant tests
// -------------------------------------------------------------------------

#[test]
fn test_stuck_escalation_threshold_is_reasonable() {
    use super::constants::{STUCK_ESCALATION_NUDGE_COUNT, STUCK_NUDGE_COOLDOWN_SECS};

    // Verify the escalation threshold results in a reasonable time before escalation.
    // With STUCK_ESCALATION_NUDGE_COUNT=2 and STUCK_NUDGE_COOLDOWN_SECS=1800 (30 min),
    // escalation happens after 2 nudges, meaning at least 45+ minutes have elapsed
    // (15 min initial detection + 30 min cooldown before second nudge).
    assert_eq!(
        STUCK_ESCALATION_NUDGE_COUNT, 2,
        "escalation should trigger after 2 nudges (45+ min)"
    );

    // Verify the cooldown is long enough to avoid spam but short enough to escalate
    // within a reasonable timeframe (30 minutes between nudges).
    assert_eq!(
        STUCK_NUDGE_COOLDOWN_SECS,
        30 * 60,
        "nudge cooldown should be 30 minutes"
    );

    // Calculate minimum time before escalation:
    // Initial stuck detection (15 min) + 1 cooldown (30 min) = 45 min minimum
    let min_escalation_minutes =
        15 + (STUCK_ESCALATION_NUDGE_COUNT - 1) as u64 * (STUCK_NUDGE_COOLDOWN_SECS / 60);
    assert!(
        min_escalation_minutes >= 45,
        "escalation should not trigger before 45 minutes"
    );
}

// ── Task assignment tracking tests ──────────────────────────────────

/// Helper to create a minimal task assignment tracker for testing.
fn new_task_assignment_tracker() -> std::sync::Mutex<HashMap<String, String>> {
    std::sync::Mutex::new(HashMap::new())
}

#[test]
fn test_task_assignment_record_and_lookup() {
    let tracker = new_task_assignment_tracker();

    // Record an assignment
    {
        let mut map = tracker.lock().unwrap();
        map.insert("park".to_string(), "42".to_string());
    }

    // Verify lookup
    let busy: HashSet<String> = {
        let map = tracker.lock().unwrap();
        map.keys().cloned().collect()
    };
    assert!(busy.contains("park"));
    assert!(!busy.contains("madison"));
}

#[test]
fn test_task_assignment_clear_by_task() {
    let tracker = new_task_assignment_tracker();

    // Record assignments for two coworkers
    {
        let mut map = tracker.lock().unwrap();
        map.insert("park".to_string(), "42".to_string());
        map.insert("madison".to_string(), "43".to_string());
    }

    // Clear by task ID (simulates task completion)
    {
        let mut map = tracker.lock().unwrap();
        map.retain(|_, tid| tid != "42");
    }

    let busy: HashSet<String> = {
        let map = tracker.lock().unwrap();
        map.keys().cloned().collect()
    };
    assert!(
        !busy.contains("park"),
        "park should be free after task completion"
    );
    assert!(busy.contains("madison"), "madison should still be busy");
}

#[test]
fn test_task_assignment_clear_by_coworker() {
    let tracker = new_task_assignment_tracker();

    // Record assignment
    {
        let mut map = tracker.lock().unwrap();
        map.insert("park".to_string(), "42".to_string());
    }

    // Clear by coworker name (simulates shutdown)
    {
        let mut map = tracker.lock().unwrap();
        map.remove("park");
    }

    let busy: HashSet<String> = {
        let map = tracker.lock().unwrap();
        map.keys().cloned().collect()
    };
    assert!(
        busy.is_empty(),
        "no coworkers should be busy after shutdown"
    );
}

#[test]
fn test_busy_coworkers_prevents_duplicate_assignment() {
    // This test verifies the core fix: busy_coworkers should contain
    // coworkers from the internal tracker, preventing duplicate assignments.
    let mut busy_coworkers: HashSet<String> = HashSet::new();

    // Simulate daemon's internal tracking (replaces disk-based detection)
    let internal_tracking: HashMap<String, String> = [("park".to_string(), "42".to_string())]
        .into_iter()
        .collect();
    busy_coworkers.extend(internal_tracking.keys().cloned());

    // Verify park is detected as busy
    assert!(
        busy_coworkers.contains("park"),
        "park should be busy (has assigned task)"
    );

    // Simulate dispatch check: already_running AND busy → skip
    let already_running = true;
    let is_busy = busy_coworkers.contains("park");
    let was_grouped = false;

    assert!(
        already_running && is_busy && !was_grouped,
        "should skip busy non-grouped coworker"
    );
}

#[test]
fn test_grouped_tasks_bypass_snapshot_busy_check() {
    // Grouped tasks (same PR, blockedBy) should be allowed even if the
    // coworker is busy from a *previous tick* (in busy_coworkers snapshot).
    let busy_coworkers: HashSet<String> = ["park".to_string()].into_iter().collect();

    let already_running = true;
    let is_busy_from_snapshot = busy_coworkers.contains("park");
    let assigned_this_tick = false; // Not assigned this tick
    let was_grouped = true; // Task was grouped to park via PR/blockedBy
    let is_coworker_reviewer = false;

    // Grouped tasks bypass the snapshot busy check (cross-tick grouping)
    let should_skip = already_running
        && (is_coworker_reviewer || assigned_this_tick || (is_busy_from_snapshot && !was_grouped));
    assert!(
        !should_skip,
        "grouped tasks should bypass snapshot busy check"
    );
}

#[test]
fn test_names_assigned_this_tick_prevents_duplicate_spawn() {
    // Within a single tick, if two unrelated tasks both get fresh names,
    // the second should be prevented if the first already claimed the name.
    let names_assigned_this_tick: HashSet<String> = ["park".to_string()].into_iter().collect();

    // Second task tries to use "park" (not grouped)
    let assigned_this_tick = names_assigned_this_tick.contains("park");
    let is_busy_from_snapshot = false;
    let was_grouped = false;
    let already_running = false;

    let should_skip =
        !already_running && (assigned_this_tick || is_busy_from_snapshot) && !was_grouped;
    assert!(
        should_skip,
        "should skip duplicate fresh-spawn within same tick"
    );
}

#[test]
fn test_grouped_tasks_should_not_duplicate_nudge_to_running_coworker() {
    // Bug fix: When two grouped tasks (same PR) target an already-running coworker,
    // the second should be skipped because the coworker was already assigned this tick.
    // Previously, the condition `(is_busy && !was_grouped)` exempted grouped tasks
    // from the busy check entirely, allowing duplicate nudges.
    let names_assigned_this_tick: HashSet<String> = ["pleasant".to_string()].into_iter().collect();

    // Second grouped task tries to use "pleasant" (already assigned this tick)
    let assigned_this_tick = names_assigned_this_tick.contains("pleasant");
    let is_busy_from_snapshot = true; // Also busy from snapshot
    let was_grouped = true;
    let already_running = true;
    let is_coworker_reviewer = false;

    let should_skip = already_running
        && (is_coworker_reviewer || assigned_this_tick || (is_busy_from_snapshot && !was_grouped));
    assert!(
        should_skip,
        "should skip duplicate nudge to already-running coworker within same tick, \
         even for grouped tasks (same PR)"
    );

    // Verify it's specifically the assigned_this_tick that catches it
    assert!(
        assigned_this_tick,
        "assigned_this_tick should be the trigger for skipping"
    );
}

#[test]
fn test_mark_in_flight_spawns_covers_all_effect_variants() {
    // mark_in_flight_spawns_from_effects must track task IDs from:
    // 1. AssignAndSpawn (Case 2 fresh spawns)
    // 2. NudgeCoworkerWithCallbacks with RecordTaskAssignment (Case 2 nudges)
    // 3. SpawnCoworkerWithCallbacks with RecordTaskAssignment (Case 1 owned spawns)
    let effects = vec![
        effects::Effect::NudgeCoworkerWithCallbacks {
            name: "pleasant".to_string(),
            message: "task prompt".to_string(),
            session_id: None,
            on_success: vec![effects::Effect::RecordTaskAssignment {
                coworker: "pleasant".to_string(),
                task_id: "873".to_string(),
            }],
        },
        effects::Effect::AssignAndSpawn {
            task_id: "874".to_string(),
            owner: "park".to_string(),
            repo_name: "test-repo".to_string(),
            config: crate::launch::LaunchConfig::coworker(
                "park".to_string(),
                "test-repo".to_string(),
                crate::launch::SessionMode::Fresh,
                None,
            ),
            on_success: vec![],
            on_failure: vec![],
        },
        effects::Effect::SpawnCoworkerWithCallbacks {
            config: crate::launch::LaunchConfig::coworker(
                "broadway".to_string(),
                "test-repo".to_string(),
                crate::launch::SessionMode::Resume,
                None,
            ),
            on_success: vec![effects::Effect::RecordTaskAssignment {
                coworker: "broadway".to_string(),
                task_id: "875".to_string(),
            }],
            on_failure: vec![],
        },
    ];

    // Extract task IDs that should be in-flight (mirror the logic in
    // mark_in_flight_spawns_from_effects for test verification)
    let mut in_flight_tasks = HashSet::new();
    for effect in &effects {
        match effect {
            effects::Effect::AssignAndSpawn { task_id, .. } => {
                in_flight_tasks.insert(task_id.clone());
            }
            effects::Effect::NudgeCoworkerWithCallbacks { on_success, .. }
            | effects::Effect::SpawnCoworkerWithCallbacks { on_success, .. } => {
                for sub_effect in on_success {
                    if let effects::Effect::RecordTaskAssignment { task_id, .. } = sub_effect {
                        in_flight_tasks.insert(task_id.clone());
                    }
                }
            }
            _ => {}
        }
    }

    assert!(
        in_flight_tasks.contains("873"),
        "NudgeCoworkerWithCallbacks with RecordTaskAssignment should be tracked"
    );
    assert!(
        in_flight_tasks.contains("874"),
        "AssignAndSpawn should be tracked"
    );
    assert!(
        in_flight_tasks.contains("875"),
        "SpawnCoworkerWithCallbacks with RecordTaskAssignment should be tracked"
    );
}

/// Regression test for task !1288: stopped coworkers should not count toward limits.
///
/// The bug was that `is_at_dev_limit()` and `is_at_coworker_limit()` used
/// `coworkers.list().len()`, which includes stopped coworkers. This caused
/// the daemon to block spawning new coworkers when all existing coworkers
/// were stopped but not yet cleaned up from the internal map.
///
/// After the fix, both functions use `list_running().len()`, so only
/// coworkers with Running status count toward the limits.
///
/// This test verifies the logic without needing to construct a full DaemonState.
#[test]
fn test_limit_checks_exclude_stopped_coworkers() {
    // Simulate 8 stopped coworkers and 0 running coworkers
    let all_coworkers = 8;
    let running_coworkers = 0;

    // Limit calculation logic
    let max_coworkers: usize = 6;
    let review_headroom: usize = 2; // REVIEW_HEADROOM constant from mod.rs
    let dev_cap = max_coworkers.saturating_sub(review_headroom).max(1);

    // Before the fix: limit checks used all_coworkers count (WRONG)
    let old_is_at_coworker_limit = all_coworkers >= max_coworkers; // 8 >= 6 = true
    let old_is_at_dev_limit = all_coworkers >= dev_cap; // 8 >= 4 = true

    // After the fix: limit checks use running_coworkers count (CORRECT)
    let new_is_at_coworker_limit = running_coworkers >= max_coworkers; // 0 >= 6 = false
    let new_is_at_dev_limit = running_coworkers >= dev_cap; // 0 >= 4 = false

    // The bug: old logic would block spawning
    assert!(
        old_is_at_coworker_limit,
        "OLD logic (WRONG): 8 total coworkers >= 6 max → would incorrectly block spawning"
    );
    assert!(
        old_is_at_dev_limit,
        "OLD logic (WRONG): 8 total coworkers >= 4 dev_cap → would incorrectly block spawning"
    );

    // The fix: new logic allows spawning
    assert!(
        !new_is_at_coworker_limit,
        "NEW logic (CORRECT): 0 running coworkers < 6 max → allows spawning"
    );
    assert!(
        !new_is_at_dev_limit,
        "NEW logic (CORRECT): 0 running coworkers < 4 dev_cap → allows spawning"
    );

    // Test scenario 2: 6 running coworkers (at limit)
    let running_coworkers = 6;
    assert!(
        running_coworkers >= max_coworkers,
        "6 running >= 6 max → should be at coworker limit"
    );
    assert!(
        running_coworkers >= dev_cap,
        "6 running >= 4 dev_cap → should be at dev limit"
    );

    // Test scenario 3: 3 running coworkers (below limits)
    let running_coworkers = 3;
    assert!(
        running_coworkers < max_coworkers,
        "3 running < 6 max → should not be at coworker limit"
    );
    assert!(
        running_coworkers < dev_cap,
        "3 running < 4 dev_cap → should not be at dev limit"
    );
}

// ── extract_lead_text tests ─────────────────────────────────────────

#[test]
fn test_extract_lead_text_single_text_block() {
    let events = vec![crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({
            "content": [{"type": "text", "text": "Hello world"}]
        }),
        session_id: None,
        extra: serde_json::Value::Null,
    }];
    assert_eq!(extract_lead_text(&events), "Hello world");
}

#[test]
fn test_extract_lead_text_aggregates_multiple_events() {
    let events = vec![
        crate::headless::StreamEvent::Assistant {
            message: serde_json::json!({
                "content": [{"type": "text", "text": "Hello "}]
            }),
            session_id: None,
            extra: serde_json::Value::Null,
        },
        crate::headless::StreamEvent::Assistant {
            message: serde_json::json!({
                "content": [{"type": "text", "text": "world"}]
            }),
            session_id: None,
            extra: serde_json::Value::Null,
        },
    ];
    assert_eq!(extract_lead_text(&events), "Hello world");
}

#[test]
fn test_extract_lead_text_skips_non_text_blocks() {
    let events = vec![crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({
            "content": [
                {"type": "tool_use", "id": "123", "name": "Read"},
                {"type": "text", "text": "Reading file..."}
            ]
        }),
        session_id: None,
        extra: serde_json::Value::Null,
    }];
    assert_eq!(extract_lead_text(&events), "Reading file...");
}

#[test]
fn test_extract_lead_text_empty_content_array() {
    let events = vec![crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({"content": []}),
        session_id: None,
        extra: serde_json::Value::Null,
    }];
    assert_eq!(extract_lead_text(&events), "");
}

#[test]
fn test_extract_lead_text_ignores_system_events() {
    let events = vec![
        crate::headless::StreamEvent::System {
            subtype: "init".to_string(),
            session_id: Some("sid-123".to_string()),
            model: Some("claude-opus-4-6".to_string()),
            extra: serde_json::Value::Null,
        },
        crate::headless::StreamEvent::Assistant {
            message: serde_json::json!({
                "content": [{"type": "text", "text": "actual output"}]
            }),
            session_id: None,
            extra: serde_json::Value::Null,
        },
    ];
    assert_eq!(extract_lead_text(&events), "actual output");
}

#[test]
fn test_extract_lead_text_no_events() {
    let events: Vec<crate::headless::StreamEvent> = vec![];
    assert_eq!(extract_lead_text(&events), "");
}

#[test]
fn test_extract_lead_text_multiple_text_blocks_in_single_event() {
    let events = vec![crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({
            "content": [
                {"type": "text", "text": "Part 1 "},
                {"type": "text", "text": "Part 2"}
            ]
        }),
        session_id: None,
        extra: serde_json::Value::Null,
    }];
    assert_eq!(extract_lead_text(&events), "Part 1 Part 2");
}

#[test]
fn test_extract_lead_text_missing_content_field() {
    let events = vec![crate::headless::StreamEvent::Assistant {
        message: serde_json::json!({"role": "assistant"}),
        session_id: None,
        extra: serde_json::Value::Null,
    }];
    assert_eq!(extract_lead_text(&events), "");
}
