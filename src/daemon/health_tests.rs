use std::collections::{HashMap, HashSet};
use std::time::Duration;

use super::*;
use crate::coworker::{Coworker, CoworkerStatus};
use crate::daemon::snapshot::ProcessHealth;
use crate::daemon::state::{DaemonPersistentState, SessionRecord};
use crate::task_store::Task;

// ── Helpers ──────────────────────────────────────────────────────────────

#[allow(dead_code)]
fn make_task(id: &str, pr: Option<u64>) -> Task {
    Task {
        id: id.to_string(),
        subject: format!("Task {}", id),
        status: crate::task_store::TaskStatus::InProgress,
        pr,
        agent_name: String::new(),
        agent_type: "midtown-code-author".into(),
        ..Default::default()
    }
}

#[allow(dead_code)]
fn make_reviewer_task(id: &str, pr: Option<u64>) -> Task {
    Task {
        id: id.to_string(),
        subject: format!("Review PR #{}", pr.unwrap_or(0)),
        status: crate::task_store::TaskStatus::InProgress,
        pr,
        agent_name: String::new(),
        agent_type: "midtown-code-reviewer".into(),
        ..Default::default()
    }
}

fn make_coworker(name: &str) -> Coworker {
    Coworker {
        slot_id: uuid::Uuid::new_v4().to_string(),
        name: name.to_string(),
        status: CoworkerStatus::Running,
        working_dir: "/tmp/test".to_string(),
        started_at: chrono::Utc::now(),
        current_task: None,
        session_id: None,
        model: "sonnet".to_string(),
        provider: crate::auth::AuthProvider::Claude,
        profile: crate::auth::DEFAULT_PROFILE.to_string(),
    }
}

/// Create a minimal `DaemonPersistentState` with test defaults.
#[allow(clippy::field_reassign_with_default)]
fn test_ps() -> DaemonPersistentState {
    let mut ps = DaemonPersistentState::default();
    ps.tick_dir_key = "test-repo".into();
    ps.tick_project_name = "test-repo".into();
    ps.tick_default_channel = "test-repo".into();
    ps.tick_default_branch = "main".into();
    ps.tick_lead_refresh_interval_secs = 5400;
    ps.tick_now = chrono::Utc::now();
    ps
}

// ── Usage limit nudge tests ─────────────────────────────────────────────

#[test]
fn usage_limit_nudge_only_targets_running_coworkers() {
    let running = make_coworker("lexington");
    let mut stopping = make_coworker("park");
    stopping.status = CoworkerStatus::Stopping;

    let mut ps = test_ps();
    ps.tick_running_coworkers = vec![running.clone()];
    ps.tick_active_coworkers = vec![running, stopping];
    ps.tick_usage_limit_nudge_scheduled = true;
    ps.tick_usage_limit_nudge_at = Some(tokio::time::Instant::now() - Duration::from_secs(10));
    ps.tick_name_session_map
        .insert("lexington".into(), "sess-lexington".into());
    ps.tick_name_session_map
        .insert("park".into(), "sess-park".into());

    let effects = maybe_nudge_usage_limit_expiry(&ps);

    let nudge_session_ids: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        nudge_session_ids,
        vec!["sess-lexington"],
        "Only the Running coworker should be nudged"
    );
}

#[test]
fn usage_limit_nudge_includes_reviewers_and_leads_with_sessions() {
    let task_worker = make_coworker("lexington");
    let mut project_lead = make_coworker("test-repo");
    project_lead.provider = crate::auth::AuthProvider::Codex;
    let reviewer = make_coworker("amsterdam");

    let mut ps = test_ps();
    ps.tick_running_coworkers = vec![task_worker, project_lead, reviewer];
    ps.tick_usage_limit_nudge_scheduled = true;
    ps.tick_usage_limit_nudge_at = Some(tokio::time::Instant::now() - Duration::from_secs(10));
    ps.tick_name_session_map
        .insert("lexington".into(), "sess-lexington".into());
    ps.tick_name_session_map
        .insert("test-repo".into(), "sess-lead".into());
    ps.tick_name_session_map
        .insert("amsterdam".into(), "sess-reviewer".into());

    let effects = maybe_nudge_usage_limit_expiry(&ps);
    let nudge_session_ids: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::NudgeSession { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        nudge_session_ids,
        vec!["sess-lexington", "sess-lead", "sess-reviewer"],
        "all running sessions should be nudged when usage limit expires"
    );
}

// ── Reminder tests ───────────────────────────────────────────────────────

#[test]
fn fired_reminder_nudges_lead() {
    use crate::reminders::{Reminder, ReminderTrigger};

    let reminder = Reminder {
        id: "abc123".to_string(),
        trigger: ReminderTrigger::AllWorkMerged,
        message: "Cut new release".to_string(),
        created_at: chrono::Utc::now(),
        repeat_policy: crate::reminders::RepeatPolicy::Once,
        fire_count: 0,
        last_evaluated_at: None,
    };
    let fired = vec![&reminder];

    let effects = effects_for_fired_reminders(&fired, "test-repo", "test-repo");

    assert_eq!(effects.len(), 3, "Expected 3 effects");
    assert!(matches!(&effects[0], Effect::PostToChannel { .. }));
    assert!(matches!(&effects[1], Effect::NudgeChannelLead { .. }));
    assert!(matches!(&effects[2], Effect::MarkRemindersFired { .. }));
}

#[test]
fn fired_reminder_no_reminders_produces_no_effects() {
    let fired: Vec<&crate::reminders::Reminder> = vec![];
    let effects = effects_for_fired_reminders(&fired, "test-repo", "test-repo");
    assert!(effects.is_empty());
}

// ── Usage limit detection tests ──────────────────────────────────────────

#[test]
fn usage_limit_detected_schedules_nudge() {
    let reset_time = chrono::Utc::now() + chrono::Duration::hours(2);
    let mut ps = test_ps();
    ps.tick_active_coworkers = vec![make_coworker("amsterdam")];
    ps.tick_process_health.insert(
        "amsterdam".into(),
        ProcessHealth {
            is_alive: true,
            has_usage_limit: true,
            usage_limit_reset_at: Some(reset_time),
            ..Default::default()
        },
    );

    let effects = check_for_usage_limits(&ps);

    assert!(!effects.is_empty(), "Should produce effects");
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::SetUsageLimitNudge { .. }))
    );
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::PostToChannel { .. }))
    );
}

#[test]
fn usage_limit_already_scheduled_is_noop() {
    let mut ps = test_ps();
    ps.tick_usage_limit_nudge_scheduled = true;
    ps.tick_active_coworkers = vec![make_coworker("amsterdam")];
    ps.tick_process_health.insert(
        "amsterdam".into(),
        ProcessHealth {
            has_usage_limit: true,
            ..Default::default()
        },
    );

    let effects = check_for_usage_limits(&ps);
    assert!(effects.is_empty(), "Should not schedule duplicate nudge");
}

// ── Stale worktree tests ─────────────────────────────────────────────────

#[test]
fn stale_worktree_generates_cleanup_effect() {
    let mut registry = crate::worktree_registry::WorktreeRegistry::new();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-99-fix-bug".to_string(),
            branch_name: "task-99-fix-bug".to_string(),
            task_id: Some("99".to_string()),
            current_coworker: None,
            pr_number: Some(200),
            created_at: chrono::Utc::now() - chrono::Duration::hours(72),
            completed_at: Some(chrono::Utc::now() - chrono::Duration::hours(48)),
        })
        .unwrap();
    registry
        .assign_worktree(crate::worktree_registry::WorktreeAssignment {
            worktree_id: "task-100-add-test".to_string(),
            branch_name: "task-100-add-test".to_string(),
            task_id: Some("100".to_string()),
            current_coworker: None,
            pr_number: None,
            created_at: chrono::Utc::now() - chrono::Duration::hours(2),
            completed_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        })
        .unwrap();

    let active_coworkers = HashSet::new();
    let retention = chrono::Duration::hours(24);

    let effects = check_for_stale_worktrees(&registry, &active_coworkers, retention);

    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::CleanupStaleWorktree { worktree_id } if worktree_id == "task-99-fix-bug")
    );
}

// ── ensure_lead_alive tests ──────────────────────────────────────────────

#[test]
fn ensure_lead_alive_respawns_missing_lead() {
    let ps = test_ps();
    let effects = ensure_lead_alive(&ps);
    assert_eq!(effects.len(), 1, "Should spawn lead when missing");
    assert!(matches!(&effects[0], Effect::SpawnCoworker(config) if config.name == "test-repo"),);
}

#[test]
fn ensure_lead_alive_noop_when_registered() {
    let mut ps = test_ps();
    ps.tick_active_coworkers.push(make_coworker("test-repo"));

    let effects = ensure_lead_alive(&ps);
    assert!(effects.is_empty(), "Should not respawn when lead is alive");
}

#[test]
fn ensure_lead_alive_cooldown_prevents_respawn() {
    let mut ps = test_ps();
    ps.tick_coworker_stop_times.insert(
        "test-repo".into(),
        chrono::Utc::now() - chrono::Duration::minutes(1),
    );

    let effects = ensure_lead_alive(&ps);
    assert!(effects.is_empty(), "Should not respawn during cooldown");
}

#[test]
fn ensure_lead_alive_respawns_after_cooldown() {
    let mut ps = test_ps();
    ps.tick_coworker_stop_times.insert(
        "test-repo".into(),
        chrono::Utc::now() - chrono::Duration::minutes(10),
    );

    let effects = ensure_lead_alive(&ps);
    assert_eq!(effects.len(), 1, "Should respawn after cooldown");
}

#[test]
fn ensure_lead_alive_skips_when_attached() {
    let mut ps = test_ps();
    ps.tick_attached_coworkers
        .insert("test-repo".into(), chrono::Utc::now());

    let effects = ensure_lead_alive(&ps);
    assert!(effects.is_empty(), "Should not spawn when attached");
}

#[test]
fn ensure_lead_alive_respawns_immediately_when_stop_time_cleared() {
    let ps = test_ps();
    let effects = ensure_lead_alive(&ps);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::SpawnCoworker(config) if config.name == "test-repo"));
}

// ── detect_stale_attached_sessions tests ─────────────────────────────────

#[test]
fn stale_attached_session_not_detached_when_recent() {
    let mut ps = test_ps();
    let recent = ps.tick_now - chrono::Duration::minutes(5);
    ps.tick_attached_coworkers.insert("lead".into(), recent);

    let effects = detect_stale_attached_sessions(&ps);
    assert!(effects.is_empty());
}

#[test]
fn stale_attached_session_auto_detached_after_timeout() {
    let mut ps = test_ps();
    let stale = ps.tick_now - chrono::Duration::minutes(15);
    ps.tick_attached_coworkers.insert("lead".into(), stale);

    let effects = detect_stale_attached_sessions(&ps);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::AutoDetachCoworker { name } if name == "lead"));
}

#[test]
fn stale_attached_sessions_handles_multiple() {
    let mut ps = test_ps();
    let stale = ps.tick_now - chrono::Duration::minutes(15);
    let fresh = ps.tick_now - chrono::Duration::minutes(5);
    ps.tick_attached_coworkers.insert("lead".into(), stale);
    ps.tick_attached_coworkers.insert("amsterdam".into(), fresh);

    let effects = detect_stale_attached_sessions(&ps);
    assert_eq!(effects.len(), 1);
    assert!(matches!(&effects[0], Effect::AutoDetachCoworker { name } if name == "lead"));
}

// ── maybe_refresh_lead_session tests ─────────────────────────────────────

#[test]
fn lead_refresh_disabled_when_interval_zero() {
    let mut ps = test_ps();
    ps.tick_lead_refresh_interval_secs = 0;
    ps.tick_active_coworkers.push(make_coworker("test-repo"));
    ps.tick_coworker_start_times.insert(
        "test-repo".into(),
        ps.tick_now - chrono::Duration::minutes(120),
    );

    let effects = maybe_refresh_lead_session(&ps);
    assert!(effects.is_empty());
}

#[test]
fn lead_refresh_not_triggered_when_young() {
    let mut ps = test_ps();
    ps.tick_lead_refresh_interval_secs = 90 * 60;
    let started = ps.tick_now - chrono::Duration::minutes(30);
    let mut lead = make_coworker("lead");
    lead.started_at = started;
    ps.tick_active_coworkers.push(lead);
    ps.tick_coworker_start_times.insert("lead".into(), started);

    let effects = maybe_refresh_lead_session(&ps);
    assert!(effects.is_empty());
}

#[test]
fn lead_refresh_triggered_when_old() {
    let mut ps = test_ps();
    ps.tick_lead_refresh_interval_secs = 90 * 60;
    let started = ps.tick_now - chrono::Duration::minutes(91);
    ps.tick_active_coworkers
        .push(make_coworker(&ps.tick_project_name.clone()));
    ps.tick_coworker_start_times
        .insert(ps.tick_project_name.to_lowercase(), started);

    let effects = maybe_refresh_lead_session(&ps);
    assert_eq!(effects.len(), 2);
    assert!(matches!(&effects[0], Effect::PostToChannel { sender, .. } if sender == "midtown"));
    assert!(
        matches!(&effects[1], Effect::ShutdownCoworker { name, .. } if name == &ps.tick_project_name)
    );
}

#[test]
fn lead_refresh_skips_attached() {
    let mut ps = test_ps();
    ps.tick_lead_refresh_interval_secs = 90 * 60;
    let started = ps.tick_now - chrono::Duration::minutes(120);
    let mut lead = make_coworker("test-repo");
    lead.started_at = started;
    ps.tick_active_coworkers.push(lead);
    ps.tick_coworker_start_times
        .insert("test-repo".into(), started);
    ps.tick_attached_coworkers
        .insert("test-repo".into(), ps.tick_now);

    let effects = maybe_refresh_lead_session(&ps);
    assert!(effects.is_empty());
}

#[test]
fn lead_refresh_noop_when_no_lead() {
    let mut ps = test_ps();
    ps.tick_lead_refresh_interval_secs = 90 * 60;
    // No coworkers at all
    let effects = maybe_refresh_lead_session(&ps);
    assert!(effects.is_empty());
}

#[test]
fn lead_refresh_noop_when_no_start_time() {
    let mut ps = test_ps();
    ps.tick_lead_refresh_interval_secs = 90 * 60;
    let mut lead = make_coworker("lead");
    lead.started_at = ps.tick_now - chrono::Duration::minutes(120);
    ps.tick_active_coworkers.push(lead);
    // Intentionally don't insert coworker_start_times

    let effects = maybe_refresh_lead_session(&ps);
    assert!(effects.is_empty());
}

// ── Dead reviewer tests ──────────────────────────────────────────────────

#[test]
fn dead_reviewer_at_max_restarts_escalates_to_ops() {
    let mut ps = test_ps();
    ps.tick_process_health.insert(
        "riverside".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("riverside".into(), 1352);
    ps.tick_reviewer_restart_counts
        .insert(1352, MAX_REVIEWER_RESTARTS);

    let effects = check_and_restart_dead_reviewers(&ps, &[]);

    assert!(effects.iter().any(|e| {
        matches!(e, Effect::PostToChannel { channel: Some(ch), message, .. }
            if ch == OPS_CHANNEL && message.contains("1352"))
    }));
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::NudgeChannelLead { .. }))
    );
    assert!(effects.iter().any(|e| {
        matches!(e, Effect::RecordReviewerEscalation { pr_number } if *pr_number == 1352)
    }));
}

#[test]
fn dead_reviewer_escalation_not_repeated_after_recorded() {
    let mut ps = test_ps();
    ps.tick_process_health.insert(
        "riverside".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("riverside".into(), 1352);
    ps.tick_reviewer_restart_counts
        .insert(1352, MAX_REVIEWER_RESTARTS);
    ps.tick_reviewer_escalations_posted.insert(1352);

    let effects = check_and_restart_dead_reviewers(&ps, &[]);
    assert!(effects.is_empty());
}

#[test]
fn dead_reviewer_respawn_and_escalation_in_same_tick() {
    let mut ps = test_ps();

    // riverside: error exit, below max restarts → respawn
    ps.tick_process_health.insert(
        "riverside".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(1),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("riverside".into(), 100);

    // broadway: error exit, at max restarts → escalation
    ps.tick_process_health.insert(
        "broadway".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(1),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("broadway".into(), 200);
    ps.tick_reviewer_restart_counts
        .insert(200, MAX_REVIEWER_RESTARTS);

    let effects = check_and_restart_dead_reviewers(&ps, &[]);

    let has_respawn = effects.iter().any(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { config, .. } = e {
            config.name == "riverside"
        } else {
            false
        }
    });
    assert!(has_respawn, "Expected respawn for riverside");

    let has_escalation = effects
        .iter()
        .any(|e| matches!(e, Effect::RecordReviewerEscalation { pr_number } if *pr_number == 200));
    assert!(has_escalation, "Expected escalation for PR 200");
}

#[test]
fn dead_reviewer_clean_exit_and_error_exit_in_same_tick() {
    let mut ps = test_ps();
    ps.tick_repo_owner = Some("btucker".into());

    // riverside: clean exit → auto-post
    ps.tick_process_health.insert(
        "riverside".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("riverside".into(), 100);

    // broadway: error exit → respawn
    ps.tick_process_health.insert(
        "broadway".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(1),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("broadway".into(), 200);

    let effects = check_and_restart_dead_reviewers(&ps, &[]);

    // riverside should get auto-posted, not respawned
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::MarkPrReviewed { pr_number } if *pr_number == 100)),
        "Expected MarkPrReviewed for riverside (clean exit)"
    );
    let has_riverside_respawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworkerWithCallbacks { config, .. } if config.name == "riverside")
    });
    assert!(
        !has_riverside_respawn,
        "riverside should not be respawned (clean exit)"
    );

    // broadway should get respawned, not auto-posted
    let has_broadway_respawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworkerWithCallbacks { config, .. } if config.name == "broadway")
    });
    assert!(
        has_broadway_respawn,
        "Expected respawn for broadway (error exit)"
    );
}

#[test]
fn dead_reviewer_with_placeholder_emits_update_pr_comment() {
    let mut ps = test_ps();
    ps.tick_process_health.insert(
        "broadway".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(1),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("broadway".into(), 88);
    ps.tick_reviewer_in_progress_comment_ids.insert(88, 888);

    let effects = check_and_restart_dead_reviewers(&ps, &[]);

    assert!(effects.iter().any(|e| {
        matches!(e, Effect::UpdatePrComment { comment_id, .. } if *comment_id == 888)
    }));
}

#[test]
fn dead_reviewer_respawn_emits_coworker_stuck_event() {
    let mut ps = test_ps();
    let pr_number = 55u64;
    let task_id = "200";
    let channel_name = "billing-feature";

    ps.tick_process_health.insert(
        "lexington".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(1),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("lexington".into(), pr_number);
    ps.tick_name_session_map
        .insert("lexington".into(), "sess-rev-200".into());

    // PR → task → channel chain
    let task_to_pr: HashMap<String, u64> = [(task_id.to_string(), pr_number)].into_iter().collect();
    ps.tick_pr_task_index =
        crate::daemon::snapshot::PrTaskIndex::from_task_maps(task_to_pr, HashMap::new());

    let tasks = vec![crate::task_store::Task {
        id: task_id.to_string(),
        subject: "Review PR".to_string(),
        channel: Some(channel_name.to_string()),
        pr: Some(pr_number),
        ..Default::default()
    }];
    let effects = check_and_restart_dead_reviewers(&ps, &tasks);

    let stuck_event = effects.iter().find_map(|e| {
        if let Effect::EmitWorkflowEvent(crate::workflow::WorkflowEvent::CoworkerStuck {
            channel,
            task_id,
            coworker,
        }) = e
        {
            Some((channel.clone(), task_id.clone(), coworker.clone()))
        } else {
            None
        }
    });

    assert!(stuck_event.is_some());
    let (ch, tid, cw) = stuck_event.unwrap();
    assert_eq!(ch, channel_name);
    assert_eq!(tid, Some(task_id.to_string()));
    assert_eq!(cw, "lexington");
}

#[test]
fn dead_reviewer_respawn_inherits_task_channel() {
    let mut ps = test_ps();
    let pr_number = 55u64;
    let task_id = "200";
    let channel_name = "billing-feature";

    ps.tick_process_health.insert(
        "lexington".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(1),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("lexington".into(), pr_number);
    ps.tick_name_session_map
        .insert("lexington".into(), "sess-rev-200".into());

    let task_to_pr: HashMap<String, u64> = [(task_id.to_string(), pr_number)].into_iter().collect();
    ps.tick_pr_task_index =
        crate::daemon::snapshot::PrTaskIndex::from_task_maps(task_to_pr, HashMap::new());

    let tasks = vec![crate::task_store::Task {
        id: task_id.to_string(),
        subject: "Review PR".to_string(),
        channel: Some(channel_name.to_string()),
        pr: Some(pr_number),
        ..Default::default()
    }];
    let effects = check_and_restart_dead_reviewers(&ps, &tasks);

    let config = effects.iter().find_map(|e| {
        if let Effect::SpawnCoworkerWithCallbacks { config, .. } = e {
            Some(config)
        } else {
            None
        }
    });

    assert!(config.is_some());
    assert_eq!(config.unwrap().channel, Some(channel_name.to_string()));
}

// ── Auto-post review on clean reviewer exit ─────────────────────────────

#[test]
fn dead_reviewer_clean_exit_auto_posts_review_instead_of_respawn() {
    let mut ps = test_ps();
    ps.tick_repo_owner = Some("btucker".into());
    ps.tick_process_health.insert(
        "riverside".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(0), // clean exit — reviewer finished but didn't post
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("riverside".into(), 1351);
    ps.tick_reviewer_in_progress_comment_ids.insert(1351, 9001);
    ps.tick_name_session_map
        .insert("riverside".into(), "sess-riverside".into());

    let task_to_pr: HashMap<String, u64> = [("500".to_string(), 1351)].into_iter().collect();
    ps.tick_pr_task_index =
        crate::daemon::snapshot::PrTaskIndex::from_task_maps(task_to_pr, HashMap::new());

    let effects = check_and_restart_dead_reviewers(&ps, &[]);

    // Should update the placeholder comment with a clean review
    let update = effects
        .iter()
        .find(|e| matches!(e, Effect::UpdatePrComment { comment_id, .. } if *comment_id == 9001));
    assert!(update.is_some(), "Expected UpdatePrComment for placeholder");
    if let Some(Effect::UpdatePrComment { new_body, .. }) = update {
        assert!(
            new_body.contains("type:review"),
            "Review body should contain review frontmatter"
        );
        assert!(
            new_body.contains("No issues found"),
            "Review body should contain clean review text"
        );
    }

    // Should mark PR as reviewed
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::MarkPrReviewed { pr_number } if *pr_number == 1351)),
        "Expected MarkPrReviewed effect"
    );

    // Should NOT respawn
    let has_respawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworkerWithCallbacks { config, .. } if config.name == "riverside")
    });
    assert!(!has_respawn, "Should not respawn a cleanly exited reviewer");
}

#[test]
fn dead_reviewer_error_exit_still_respawns() {
    let mut ps = test_ps();
    ps.tick_process_health.insert(
        "riverside".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(1), // error exit
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("riverside".into(), 1351);
    ps.tick_name_session_map
        .insert("riverside".into(), "sess-riverside".into());

    let effects = check_and_restart_dead_reviewers(&ps, &[]);

    // Should still respawn on error exit
    let has_respawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworkerWithCallbacks { config, .. } if config.name == "riverside")
    });
    assert!(has_respawn, "Error exit should trigger respawn");
}

#[test]
fn dead_reviewer_clean_exit_without_placeholder_still_marks_reviewed() {
    let mut ps = test_ps();
    ps.tick_repo_owner = Some("btucker".into());
    ps.tick_process_health.insert(
        "riverside".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(0),
            ..Default::default()
        },
    );
    ps.tick_reviewer_pr_assignments
        .insert("riverside".into(), 1351);
    // No placeholder comment ID — placeholder was never posted or already overwritten

    let effects = check_and_restart_dead_reviewers(&ps, &[]);

    // Should still mark PR as reviewed (even without placeholder to update)
    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::MarkPrReviewed { pr_number } if *pr_number == 1351)),
        "Expected MarkPrReviewed even without placeholder"
    );

    // Should NOT respawn
    let has_respawn = effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworkerWithCallbacks { config, .. } if config.name == "riverside")
    });
    assert!(!has_respawn, "Should not respawn a cleanly exited reviewer");
}

// ── Tool name conflict tests ─────────────────────────────────────────────

#[test]
fn unrecoverable_session_error_restarts_project_lead_immediately() {
    let mut ps = test_ps();
    ps.tick_dir_key = "midtown".into();
    ps.tick_project_name = "midtown".into();
    ps.tick_default_channel = "midtown".into();
    ps.tick_process_health.insert(
        "midtown".into(),
        ProcessHealth {
            has_tool_name_conflict: true,
            ..Default::default()
        },
    );
    ps.tick_name_session_map
        .insert("midtown".into(), "sess-lead-1".into());

    let effects = check_and_restart_tool_name_conflicts(&ps);

    assert!(
        effects
            .iter()
            .any(|e| matches!(e, Effect::ClearSavedSessionId { name } if name == "midtown"))
    );
    assert!(effects.iter().any(
        |e| matches!(e, Effect::ShutdownSession { session_id, .. } if session_id == "sess-lead-1")
    ));
    assert!(effects.iter().any(|e| {
        matches!(e, Effect::SpawnCoworker(config)
            if config.name == "midtown"
                && config.agent_type == "midtown-project-lead"
                && matches!(config.session_mode, crate::launch::SessionMode::Fresh))
    }));
}

#[test]
fn unrecoverable_session_error_does_not_force_spawn_for_non_lead() {
    let mut ps = test_ps();
    ps.tick_dir_key = "midtown".into();
    ps.tick_project_name = "midtown".into();
    ps.tick_process_health.insert(
        "lexington".into(),
        ProcessHealth {
            has_tool_name_conflict: true,
            ..Default::default()
        },
    );
    ps.tick_name_session_map
        .insert("lexington".into(), "sess-lex-1".into());

    let effects = check_and_restart_tool_name_conflicts(&ps);

    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::SpawnCoworker(config) if config.name == "lexington")),
    );
}

// ── Auth profile pool: usage-limit marking ──────────────────────────────

#[test]
fn usage_limit_marks_pool_profile_limited() {
    let reset_time = chrono::Utc::now() + chrono::Duration::hours(2);
    let mut ps = test_ps();
    ps.tick_active_coworkers = vec![make_coworker("lexington")];
    ps.tick_process_health.insert(
        "lexington".into(),
        ProcessHealth {
            is_alive: true,
            has_usage_limit: true,
            usage_limit_reset_at: Some(reset_time),
            ..Default::default()
        },
    );
    ps.tick_session_profile_map
        .insert("lexington".into(), "alice@example.com".into());

    let effects = check_for_usage_limits(&ps);

    assert!(effects.iter().any(|e| {
        matches!(e, Effect::MarkProfileLimited { profile_email, .. } if profile_email == "alice@example.com")
    }));
}

#[test]
fn usage_limit_without_profile_map_skips_mark_limited() {
    let mut ps = test_ps();
    ps.tick_active_coworkers = vec![make_coworker("lexington")];
    ps.tick_process_health.insert(
        "lexington".into(),
        ProcessHealth {
            is_alive: true,
            has_usage_limit: true,
            ..Default::default()
        },
    );
    // session_profile_map is empty — no pool

    let effects = check_for_usage_limits(&ps);
    assert!(!effects.is_empty());
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::MarkProfileLimited { .. }))
    );
}

#[test]
fn usage_limit_expiry_clears_pool_profiles() {
    let past_instant = tokio::time::Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(tokio::time::Instant::now);

    let mut ps = test_ps();
    ps.tick_running_coworkers = vec![make_coworker("lexington")];
    ps.tick_name_session_map
        .insert("lexington".into(), "sess-1".into());
    ps.tick_usage_limit_nudge_scheduled = true;
    ps.tick_usage_limit_nudge_at = Some(past_instant);
    ps.tick_limited_pool_profiles =
        HashSet::from(["alice@example.com".into(), "bob@example.com".into()]);

    let effects = maybe_nudge_usage_limit_expiry(&ps);

    assert!(effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "alice@example.com")
    }));
    assert!(effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "bob@example.com")
    }));
}

#[test]
fn usage_limit_expiry_clears_profiles_even_when_session_map_empty() {
    let past_instant = tokio::time::Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(tokio::time::Instant::now);

    let mut ps = test_ps();
    ps.tick_usage_limit_nudge_scheduled = true;
    ps.tick_usage_limit_nudge_at = Some(past_instant);
    ps.tick_limited_pool_profiles = HashSet::from(["alice@example.com".to_string()]);

    let effects = maybe_nudge_usage_limit_expiry(&ps);

    assert!(effects.iter().any(|e| {
        matches!(e, Effect::ClearProfileLimit { profile_email } if profile_email == "alice@example.com")
    }));
}

#[test]
fn usage_limit_marks_all_limited_coworker_profiles() {
    let mut ps = test_ps();
    ps.tick_active_coworkers = vec![make_coworker("amsterdam"), make_coworker("lexington")];
    for name in &["amsterdam", "lexington"] {
        ps.tick_process_health.insert(
            name.to_string(),
            ProcessHealth {
                is_alive: true,
                has_usage_limit: true,
                ..Default::default()
            },
        );
    }
    ps.tick_session_profile_map
        .insert("amsterdam".into(), "alice@example.com".into());
    ps.tick_session_profile_map
        .insert("lexington".into(), "bob@example.com".into());

    let effects = check_for_usage_limits(&ps);

    let marked: Vec<&str> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::MarkProfileLimited { profile_email, .. } => Some(profile_email.as_str()),
            _ => None,
        })
        .collect();

    assert!(marked.contains(&"alice@example.com"));
    assert!(marked.contains(&"bob@example.com"));
    assert_eq!(marked.len(), 2);
}

// ── check_for_stale_notes tests ──────────────────────────────────────────

#[test]
fn stale_notes_skips_channels_without_leads() {
    let mut ps = test_ps();
    ps.tick_stale_channel_notes =
        HashMap::from([("orphan-channel".into(), vec!["old-note".into()])]);

    let effects = check_for_stale_notes(&ps);
    assert!(effects.is_empty());
}

#[test]
fn stale_notes_skips_channels_on_cooldown() {
    let mut ps = test_ps();
    ps.tick_stale_channel_notes = HashMap::from([("dev".into(), vec!["stale-note".into()])]);
    ps.channel_lead_sessions
        .insert("dev".into(), "sess-dev-lead".into());
    ps.tick_note_staleness_cooldown_channels = HashSet::from(["dev".into()]);

    let effects = check_for_stale_notes(&ps);
    assert!(effects.is_empty());
}

#[test]
fn stale_notes_emits_nudge_and_cooldown() {
    let mut ps = test_ps();
    ps.tick_stale_channel_notes =
        HashMap::from([("dev".into(), vec!["old-note".into(), "ancient-note".into()])]);
    ps.channel_lead_sessions
        .insert("dev".into(), "sess-dev-lead".into());

    let effects = check_for_stale_notes(&ps);
    assert_eq!(effects.len(), 2);
    assert!(
        matches!(&effects[0], Effect::NudgeChannelLead { channel_name, .. } if channel_name == "dev")
    );
    assert!(
        matches!(&effects[1], Effect::RecordCooldown { category, key } if category == "note_staleness" && key == "dev")
    );
}

// ── Dead process respawn tests ───────────────────────────────────────────

#[test]
fn dead_process_respawn_propagates_session_id() {
    let mut ps = test_ps();
    ps.tick_process_health.insert(
        "york".into(),
        ProcessHealth {
            is_alive: false,
            exit_code: Some(137),
            ..Default::default()
        },
    );
    ps.tick_name_session_map
        .insert("york".into(), "session-dead-xyz".into());

    let in_progress = vec![(
        "99".to_string(),
        "Add feature".to_string(),
        "york".to_string(),
    )];

    let respawns = crate::rules::decide_dead_process_respawns(
        &ps.tick_process_health,
        &in_progress,
        &ps.tick_name_session_map,
    );

    assert_eq!(respawns.len(), 1);
    assert_eq!(respawns[0].session_id, Some("session-dead-xyz".to_string()),);
}

// ── Session role determination ───────────────────────────────────────────

#[test]
fn session_role_determination_labels() {
    let project_name = "midtown";
    let channel_lead_names: HashSet<String> = HashSet::from(["ops".into()]);

    let determine_role = |name: &str| -> &'static str {
        let is_lead = crate::daemon::helpers::is_project_lead(name, project_name);
        let is_channel_lead = channel_lead_names.contains(name);
        if is_lead {
            "Lead"
        } else if is_channel_lead {
            "Channel lead"
        } else {
            "Coworker"
        }
    };

    assert_eq!(determine_role("midtown"), "Lead");
    assert_eq!(determine_role("Midtown"), "Lead");
    assert_eq!(determine_role("lead"), "Lead");
    assert_eq!(determine_role("ops"), "Channel lead");
    assert_eq!(determine_role("lexington"), "Coworker");
}

// ── ensure_channel_leads_alive tests ─────────────────────────────────────

#[test]
fn channel_lead_respawn_when_missing() {
    let mut ps = test_ps();
    ps.channel_lead_sessions
        .insert("ops".into(), "sess-ops".into());

    let effects = ensure_channel_leads_alive(&ps);
    assert_eq!(effects.len(), 1);
    assert!(
        matches!(&effects[0], Effect::RespawnChannelLead { channel_name } if channel_name == "ops")
    );
}

#[test]
fn channel_lead_noop_when_registered() {
    let mut ps = test_ps();
    ps.channel_lead_sessions
        .insert("ops".into(), "sess-ops".into());
    ps.tick_active_coworkers.push(make_coworker("ops"));

    let effects = ensure_channel_leads_alive(&ps);
    assert!(effects.is_empty());
}

#[test]
fn channel_lead_respects_cooldown() {
    let mut ps = test_ps();
    ps.channel_lead_sessions
        .insert("ops".into(), "sess-ops".into());
    ps.tick_coworker_stop_times
        .insert("ops".into(), ps.tick_now - chrono::Duration::seconds(1));

    let effects = ensure_channel_leads_alive(&ps);
    assert!(effects.is_empty());
}

#[test]
fn channel_lead_skips_when_attached() {
    let mut ps = test_ps();
    ps.channel_lead_sessions
        .insert("ops".into(), "sess-ops".into());
    ps.tick_attached_coworkers.insert("ops".into(), ps.tick_now);

    let effects = ensure_channel_leads_alive(&ps);
    assert!(effects.is_empty());
}

// ── State GC tests ──────────────────────────────────────────────────────

fn make_session(
    session_id: &str,
    is_running: bool,
    is_reviewer: bool,
    resume_on_startup: bool,
    last_active: chrono::DateTime<chrono::Utc>,
    task_id: Option<&str>,
) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        is_running,
        resume_on_startup,
        last_active,
        task_id: task_id.map(|s| s.to_string()),
        initial_prompt: Some("test prompt".to_string()),
        agent_type: if is_reviewer {
            "midtown-code-reviewer".to_string()
        } else {
            "midtown-code-author".to_string()
        },
        ..Default::default()
    }
}

#[test]
fn state_gc_prunes_dead_reviewer_sessions_immediately() {
    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();
    sessions.insert(
        "reviewer-1".into(),
        make_session(
            "reviewer-1",
            false,
            true,
            false,
            now - chrono::Duration::minutes(1),
            None,
        ),
    );
    sessions.insert(
        "dev-1".into(),
        make_session("dev-1", true, false, true, now, Some("42")),
    );

    let active_session_ids = HashSet::from(["dev-1".to_string()]);
    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            assert_eq!(dead_session_ids, &vec!["reviewer-1".to_string()]);
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

#[test]
fn state_gc_prunes_stale_dead_sessions_past_retention() {
    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();
    sessions.insert(
        "dead-old".into(),
        make_session(
            "dead-old",
            false,
            false,
            false,
            now - chrono::Duration::hours(48),
            Some("10"),
        ),
    );
    sessions.insert(
        "dead-recent".into(),
        make_session(
            "dead-recent",
            false,
            false,
            false,
            now - chrono::Duration::hours(1),
            Some("11"),
        ),
    );
    sessions.insert(
        "dead-resumable".into(),
        make_session(
            "dead-resumable",
            false,
            false,
            true,
            now - chrono::Duration::hours(48),
            Some("12"),
        ),
    );

    let retention = chrono::Duration::hours(24);
    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids, ..
        } => {
            assert_eq!(dead_session_ids, &vec!["dead-old".to_string()]);
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

#[test]
fn state_gc_preserves_initial_prompt_on_stopped_sessions() {
    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();
    sessions.insert(
        "stopped-1".into(),
        make_session(
            "stopped-1",
            false,
            false,
            true,
            now - chrono::Duration::hours(1),
            None,
        ),
    );
    sessions.insert(
        "running-1".into(),
        make_session("running-1", true, false, true, now, None),
    );

    let active_session_ids = HashSet::from(["running-1".to_string()]);
    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );
    assert!(effects.is_empty());
}

#[test]
fn state_gc_prunes_orphaned_task_metadata() {
    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();
    sessions.insert(
        "dev-1".into(),
        make_session("dev-1", true, false, true, now, Some("42")),
    );

    let active_session_ids = HashSet::from(["dev-1".to_string()]);
    let task_metadata_keys = HashSet::from(["42".into(), "99".into(), "100".into()]);
    let active_task_ids = HashSet::from(["42".into(), "100".into()]);
    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &task_metadata_keys,
        &active_task_ids,
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            orphaned_task_ids, ..
        } => {
            assert_eq!(orphaned_task_ids, &vec!["99".to_string()]);
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}

#[test]
fn state_gc_no_effect_when_nothing_to_clean() {
    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();
    sessions.insert(
        "dev-1".into(),
        make_session("dev-1", true, false, true, now, Some("42")),
    );

    let active_session_ids = HashSet::from(["dev-1".to_string()]);
    let retention = chrono::Duration::hours(24);

    let effects = check_for_state_gc(
        &sessions,
        &active_session_ids,
        &HashSet::new(),
        &HashSet::new(),
        retention,
    );
    assert!(effects.is_empty());
}

#[test]
fn state_gc_works_with_zero_retention_period() {
    let now = chrono::Utc::now();
    let mut sessions = HashMap::new();
    sessions.insert(
        "reviewer-1".into(),
        make_session(
            "reviewer-1",
            false,
            true,
            false,
            now - chrono::Duration::minutes(1),
            None,
        ),
    );
    sessions.insert(
        "dead-dev".into(),
        make_session(
            "dead-dev",
            false,
            false,
            false,
            now - chrono::Duration::minutes(1),
            Some("99"),
        ),
    );

    let task_metadata_keys = HashSet::from(["99".into()]);
    let retention = chrono::Duration::hours(0);

    let effects = check_for_state_gc(
        &sessions,
        &HashSet::new(),
        &task_metadata_keys,
        &HashSet::new(),
        retention,
    );

    assert_eq!(effects.len(), 1);
    match &effects[0] {
        Effect::GarbageCollectState {
            dead_session_ids,
            orphaned_task_ids,
        } => {
            assert_eq!(dead_session_ids.len(), 2);
            assert!(dead_session_ids.contains(&"reviewer-1".to_string()));
            assert!(dead_session_ids.contains(&"dead-dev".to_string()));
            assert_eq!(orphaned_task_ids, &vec!["99".to_string()]);
        }
        other => panic!("Expected GarbageCollectState, got {:?}", other),
    }
}
