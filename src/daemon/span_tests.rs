use super::*;
use chrono::Duration;

fn make_span(
    task_id: &str,
    agent_name: &str,
    agent_type: &str,
    session_id: &str,
    start_offset_secs: i64,
    end_offset_secs: Option<i64>,
) -> TaskSessionSpan {
    let now = Utc::now();
    TaskSessionSpan {
        task_id: task_id.to_string(),
        agent_name: agent_name.to_string(),
        agent_type: agent_type.to_string(),
        session_id: session_id.to_string(),
        start_time: now + Duration::seconds(start_offset_secs),
        end_time: end_offset_secs.map(|s| now + Duration::seconds(s)),
    }
}

fn make_running_session(session_id: &str) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        is_running: true,
        ..Default::default()
    }
}

fn make_stopped_session(session_id: &str) -> SessionRecord {
    SessionRecord {
        session_id: session_id.to_string(),
        is_running: false,
        ..Default::default()
    }
}

#[test]
fn test_active_span_for_task() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "sess-1", 0, None));

    let span = ps.active_span_for_task("task-1");
    assert!(span.is_some(), "Should find active span");
    assert_eq!(span.unwrap().task_id, "task-1");
}

#[test]
fn test_active_span_for_task_closed() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "sess-1", 0, Some(100)));

    let span = ps.active_span_for_task("task-1");
    assert!(span.is_none(), "Should not find closed span");
}

#[test]
fn test_spans_for_task_ordered() {
    let mut ps = DaemonPersistentState::default();
    // Insert in reverse order to verify sorting
    ps.task_session_spans.push(make_span(
        "task-1",
        "river",
        "dev",
        "sess-2",
        100,
        Some(200),
    ));
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "sess-1", 0, Some(50)));
    ps.task_session_spans
        .push(make_span("task-2", "lake", "dev", "sess-3", 50, None));

    let spans = ps.spans_for_task("task-1");
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].session_id, "sess-1");
    assert_eq!(spans[1].session_id, "sess-2");
}

#[test]
fn test_active_reviewer_for_pr() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(make_span(
        "review-42",
        "river",
        "reviewer",
        "sess-rev",
        0,
        None,
    ));
    ps.sessions.insert("sess-rev".to_string(), {
        let mut s = make_running_session("sess-rev");
        s.pr_number = Some(42);
        s
    });

    let span = ps.active_reviewer_for_pr(42);
    assert!(span.is_some(), "Should find active reviewer span for PR");
    assert_eq!(span.unwrap().session_id, "sess-rev");
}

#[test]
fn test_active_reviewer_for_pr_via_task_pr_number() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(make_span(
        "review-99",
        "river",
        "reviewer",
        "sess-rev2",
        0,
        None,
    ));
    ps.task_pr_number.insert("review-99".to_string(), 99);

    let span = ps.active_reviewer_for_pr(99);
    assert!(
        span.is_some(),
        "Should find reviewer span via task_pr_number"
    );
}

#[test]
fn test_pr_has_active_reviewer_running() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(make_span(
        "review-42",
        "river",
        "reviewer",
        "sess-rev",
        0,
        None,
    ));
    ps.sessions.insert("sess-rev".to_string(), {
        let mut s = make_running_session("sess-rev");
        s.pr_number = Some(42);
        s
    });

    assert!(
        ps.pr_has_active_reviewer(42),
        "Running reviewer session → true"
    );
}

#[test]
fn test_pr_has_active_reviewer_stopped() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(make_span(
        "review-42",
        "river",
        "reviewer",
        "sess-rev",
        0,
        None,
    ));
    ps.sessions.insert("sess-rev".to_string(), {
        let mut s = make_stopped_session("sess-rev");
        s.pr_number = Some(42);
        s
    });

    assert!(
        !ps.pr_has_active_reviewer(42),
        "Stopped reviewer session → false"
    );
}

#[test]
fn test_active_reviewers() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans.push(make_span(
        "review-1", "river", "reviewer", "sess-r1", 0, None,
    ));
    ps.task_session_spans
        .push(make_span("dev-task", "lake", "dev", "sess-d1", 0, None));
    ps.task_session_spans.push(make_span(
        "review-2", "brook", "reviewer", "sess-r2", 0, None,
    ));
    // Closed reviewer — should not appear
    ps.task_session_spans.push(make_span(
        "review-3",
        "creek",
        "reviewer",
        "sess-r3",
        0,
        Some(100),
    ));

    let reviewers = ps.active_reviewer_spans();
    assert_eq!(reviewers.len(), 2, "Only open reviewer spans returned");
    let ids: Vec<_> = reviewers.iter().map(|s| s.session_id.as_str()).collect();
    assert!(ids.contains(&"sess-r1"));
    assert!(ids.contains(&"sess-r2"));
    assert!(!ids.contains(&"sess-d1"));
    assert!(!ids.contains(&"sess-r3"));
}

#[test]
fn test_close_span() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "sess-1", 0, None));

    ps.close_span("sess-1", "task-1");

    let span = &ps.task_session_spans[0];
    assert!(span.end_time.is_some(), "Span should be closed");
}

#[test]
fn test_close_span_does_not_affect_other_spans() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "sess-1", 0, None));
    ps.task_session_spans
        .push(make_span("task-2", "lake", "dev", "sess-2", 0, None));

    ps.close_span("sess-1", "task-1");

    assert!(ps.task_session_spans[0].end_time.is_some());
    assert!(ps.task_session_spans[1].end_time.is_none());
}

#[test]
fn test_close_spans_for_session() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "sess-a", 0, None));
    ps.task_session_spans
        .push(make_span("task-2", "river", "dev", "sess-a", 50, None));
    ps.task_session_spans
        .push(make_span("task-3", "lake", "dev", "sess-b", 0, None));

    ps.close_spans_for_session("sess-a");

    for span in &ps.task_session_spans {
        if span.session_id == "sess-a" {
            assert!(span.end_time.is_some(), "sess-a spans should be closed");
        } else {
            assert!(span.end_time.is_none(), "sess-b spans should remain open");
        }
    }
}

#[test]
fn test_close_spans_for_task() {
    let mut ps = DaemonPersistentState::default();
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "sess-1", 0, None));
    ps.task_session_spans
        .push(make_span("task-1", "lake", "dev", "sess-2", 50, None));
    ps.task_session_spans
        .push(make_span("task-2", "brook", "dev", "sess-3", 0, None));

    ps.close_spans_for_task("task-1");

    for span in &ps.task_session_spans {
        if span.task_id == "task-1" {
            assert!(span.end_time.is_some(), "task-1 spans should be closed");
        } else {
            assert!(span.end_time.is_none(), "task-2 spans should remain open");
        }
    }
}

#[test]
fn test_create_span() {
    let mut ps = DaemonPersistentState::default();
    ps.create_span("task-99", "river", "dev", "sess-new");

    assert_eq!(ps.task_session_spans.len(), 1);
    let span = &ps.task_session_spans[0];
    assert_eq!(span.task_id, "task-99");
    assert_eq!(span.agent_name, "river");
    assert_eq!(span.agent_type, "dev");
    assert_eq!(span.session_id, "sess-new");
    assert!(span.end_time.is_none());
}

#[test]
fn test_gc_closes_orphaned_spans() {
    let mut ps = DaemonPersistentState::default();
    // Span for a session that does NOT exist in ps.sessions → should be force-closed
    ps.task_session_spans
        .push(make_span("task-1", "river", "dev", "orphan-sess", 0, None));
    // Span for a session that exists → should stay open
    ps.task_session_spans
        .push(make_span("task-2", "lake", "dev", "live-sess", 0, None));
    ps.sessions
        .insert("live-sess".to_string(), make_running_session("live-sess"));

    ps.apply_gc(&[], &[]);

    let orphan = ps
        .task_session_spans
        .iter()
        .find(|s| s.session_id == "orphan-sess")
        .unwrap();
    assert!(orphan.end_time.is_some(), "Orphaned span should be closed");

    let live = ps
        .task_session_spans
        .iter()
        .find(|s| s.session_id == "live-sess")
        .unwrap();
    assert!(
        live.end_time.is_none(),
        "Live session span should stay open"
    );
}

#[test]
fn test_gc_preserves_empty_session_id_spans() {
    let mut ps = DaemonPersistentState::default();
    // Span with empty session_id (optimistic assignment before session spawns)
    // should NOT be force-closed by GC — empty means "pending", not "stale".
    ps.task_session_spans
        .push(make_span("task-1", "river", "reviewer", "", 0, None));

    ps.apply_gc(&[], &[]);

    let span = ps
        .task_session_spans
        .iter()
        .find(|s| s.task_id == "task-1")
        .unwrap();
    assert!(
        span.end_time.is_none(),
        "Span with empty session_id should stay open (optimistic assignment)"
    );
}

#[test]
fn test_gc_removes_old_closed_spans() {
    let mut ps = DaemonPersistentState::default();
    let now = Utc::now();

    // Old closed span (> 48 hours ago)
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "old-task".to_string(),
        agent_name: "river".to_string(),
        agent_type: "dev".to_string(),
        session_id: "old-sess".to_string(),
        start_time: now - Duration::hours(72),
        end_time: Some(now - Duration::hours(50)),
    });

    // Recent closed span (< 48 hours ago)
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "recent-task".to_string(),
        agent_name: "lake".to_string(),
        agent_type: "dev".to_string(),
        session_id: "recent-sess".to_string(),
        start_time: now - Duration::hours(30),
        end_time: Some(now - Duration::hours(10)),
    });

    // Open span — never removed by age
    ps.task_session_spans.push(TaskSessionSpan {
        task_id: "open-task".to_string(),
        agent_name: "brook".to_string(),
        agent_type: "dev".to_string(),
        session_id: "open-sess".to_string(),
        start_time: now - Duration::hours(100),
        end_time: None,
    });
    ps.sessions
        .insert("open-sess".to_string(), make_running_session("open-sess"));

    ps.apply_gc(&[], &[]);

    let ids: Vec<_> = ps
        .task_session_spans
        .iter()
        .map(|s| s.session_id.as_str())
        .collect();
    assert!(
        !ids.contains(&"old-sess"),
        "Old closed span should be removed"
    );
    assert!(
        ids.contains(&"recent-sess"),
        "Recent closed span should be kept"
    );
    assert!(ids.contains(&"open-sess"), "Open span should be kept");
}

#[test]
fn test_gc_caps_at_500() {
    let mut ps = DaemonPersistentState::default();
    let now = Utc::now();

    // Insert 600 closed spans (all recent enough to survive age-based GC)
    for i in 0..600usize {
        ps.task_session_spans.push(TaskSessionSpan {
            task_id: format!("task-{i}"),
            agent_name: "river".to_string(),
            agent_type: "dev".to_string(),
            session_id: format!("sess-{i}"),
            start_time: now - Duration::seconds(600 - i as i64),
            end_time: Some(now - Duration::seconds(600 - i as i64 - 1)),
        });
    }

    ps.apply_gc(&[], &[]);

    assert!(
        ps.task_session_spans.len() <= 500,
        "Should be capped at 500 spans, got {}",
        ps.task_session_spans.len()
    );
}


#[test]
fn test_gc_cap_keeps_newest_and_open_spans() {
    let mut ps = DaemonPersistentState::default();
    let now = Utc::now();

    // 3 open spans (should always survive the cap)
    for i in 0..3usize {
        let sess_id = format!("open-sess-{i}");
        ps.task_session_spans.push(TaskSessionSpan {
            task_id: format!("open-task-{i}"),
            agent_name: "river".to_string(),
            agent_type: "dev".to_string(),
            session_id: sess_id.clone(),
            start_time: now - Duration::seconds(1000 + i as i64),
            end_time: None,
        });
        ps.sessions
            .insert(sess_id.clone(), make_running_session(&sess_id));
    }

    // 600 closed spans — oldest (i=0) should be dropped, newest (i=599) kept
    for i in 0..600usize {
        ps.task_session_spans.push(TaskSessionSpan {
            task_id: format!("closed-task-{i}"),
            agent_name: "river".to_string(),
            agent_type: "dev".to_string(),
            session_id: format!("closed-sess-{i}"),
            start_time: now - Duration::seconds(600 - i as i64),
            end_time: Some(now - Duration::seconds(600 - i as i64 - 1)),
        });
    }

    ps.apply_gc(&[], &[]);

    assert_eq!(ps.task_session_spans.len(), 500);

    // All 3 open spans must survive
    let open_count = ps
        .task_session_spans
        .iter()
        .filter(|s| s.end_time.is_none())
        .count();
    assert_eq!(open_count, 3, "All open spans must survive the cap");

    // The newest closed span (i=599, start_time closest to now) should survive
    assert!(
        ps.task_session_spans
            .iter()
            .any(|s| s.task_id == "closed-task-599"),
        "Newest closed span should survive"
    );

    // The oldest closed span (i=0) should be dropped
    assert!(
        !ps.task_session_spans
            .iter()
            .any(|s| s.task_id == "closed-task-0"),
        "Oldest closed span should be dropped"
    );
}
