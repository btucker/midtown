use super::{RebaseRegressionInput, evaluate_rebase_regression};
use crate::daemon::effects::Effect;

fn make_input(
    coworker: &str,
    main_files: &[&str],
    recent_files: &[&str],
    rebase_detected: bool,
) -> RebaseRegressionInput {
    RebaseRegressionInput {
        coworker_name: coworker.to_string(),
        files_changed_on_main: main_files.iter().map(|s| s.to_string()).collect(),
        files_in_recent_commits: recent_files.iter().map(|s| s.to_string()).collect(),
        rebase_detected,
    }
}

#[test]
fn flags_overlapping_files_after_rebase() {
    let input = make_input(
        "lexington",
        &["src/lib.rs", "src/main.rs"],
        &["src/lib.rs", "src/new.rs"],
        true,
    );

    let effects = evaluate_rebase_regression(&input);

    // Should have: nudge + cooldown + ops message
    assert_eq!(effects.len(), 3);

    let nudge = effects
        .iter()
        .find(|e| matches!(e, Effect::NudgeCoworkerByName { .. }));
    assert!(nudge.is_some(), "should emit a nudge");

    if let Some(Effect::NudgeCoworkerByName { name, message, .. }) = nudge {
        assert_eq!(name, "lexington");
        assert!(
            message.contains("src/lib.rs"),
            "nudge should list the overlapping file"
        );
        assert!(
            message.contains("Post-rebase regression"),
            "nudge should mention regression"
        );
    }

    let cooldown = effects.iter().find(
        |e| matches!(e, Effect::RecordCooldown { category, .. } if category == "rebase_regression"),
    );
    assert!(cooldown.is_some(), "should record cooldown");

    let ops = effects
        .iter()
        .find(|e| matches!(e, Effect::PostSystemMessage { channel, .. } if channel.as_deref() == Some("ops")));
    assert!(ops.is_some(), "should post to ops channel");
}

#[test]
fn no_flag_when_no_rebase_detected() {
    let input = make_input(
        "lexington",
        &["src/lib.rs"],
        &["src/lib.rs"],
        false, // no rebase
    );

    let effects = evaluate_rebase_regression(&input);
    assert!(effects.is_empty(), "no effects when rebase not detected");
}

#[test]
fn no_flag_when_no_file_overlap() {
    let input = make_input(
        "park",
        &["src/lib.rs", "src/old.rs"],
        &["src/new.rs", "tests/test.rs"],
        true,
    );

    let effects = evaluate_rebase_regression(&input);
    assert!(effects.is_empty(), "no effects when files don't overlap");
}

#[test]
fn no_flag_when_main_has_no_changes() {
    let input = make_input(
        "madison",
        &[], // no changes on main
        &["src/lib.rs"],
        true,
    );

    let effects = evaluate_rebase_regression(&input);
    assert!(
        effects.is_empty(),
        "no effects when main has no file changes"
    );
}

#[test]
fn multiple_overlapping_files_listed_in_nudge() {
    let input = make_input(
        "broadway",
        &["src/a.rs", "src/b.rs", "src/c.rs"],
        &["src/b.rs", "src/c.rs", "src/d.rs"],
        true,
    );

    let effects = evaluate_rebase_regression(&input);
    assert_eq!(effects.len(), 3);

    if let Some(Effect::NudgeCoworkerByName { message, .. }) = effects
        .iter()
        .find(|e| matches!(e, Effect::NudgeCoworkerByName { .. }))
    {
        assert!(message.contains("src/b.rs"), "should list src/b.rs");
        assert!(message.contains("src/c.rs"), "should list src/c.rs");
        assert!(
            !message.contains("src/a.rs"),
            "should not list non-overlapping src/a.rs"
        );
        assert!(
            !message.contains("src/d.rs"),
            "should not list non-overlapping src/d.rs"
        );
    }
}

#[test]
fn empty_recent_commits_no_flag() {
    let input = make_input(
        "lexington",
        &["src/lib.rs"],
        &[], // no recent commits
        true,
    );

    let effects = evaluate_rebase_regression(&input);
    assert!(effects.is_empty());
}

#[test]
fn parse_reflog_timestamp_recent() {
    // Test the timestamp parsing helper (exposed via the module)
    let recent_line = format!(
        "rebase (finish): returning to refs/heads/branch {}",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S +0000")
    );
    assert!(
        super::parse_reflog_timestamp_is_recent(
            &recent_line,
            crate::daemon::constants::REBASE_REGRESSION_WINDOW_SECS as i64,
        ),
        "a just-now timestamp should be considered recent"
    );
}

#[test]
fn parse_reflog_timestamp_old() {
    let old_line = "rebase (finish): returning to refs/heads/branch 2020-01-01 00:00:00 +0000";
    assert!(
        !super::parse_reflog_timestamp_is_recent(
            old_line,
            crate::daemon::constants::REBASE_REGRESSION_WINDOW_SECS as i64,
        ),
        "a very old timestamp should not be considered recent"
    );
}

#[test]
fn parse_reflog_timestamp_no_match_fails_open() {
    let no_timestamp = "rebase (finish): some weird line without a date";
    assert!(
        super::parse_reflog_timestamp_is_recent(
            no_timestamp,
            crate::daemon::constants::REBASE_REGRESSION_WINDOW_SECS as i64,
        ),
        "should fail-open when no timestamp can be parsed"
    );
}

#[test]
fn rebase_regression_nudge_carries_nudge_type() {
    let input = make_input("lexington", &["src/lib.rs"], &["src/lib.rs"], true);

    let effects = evaluate_rebase_regression(&input);

    let nudge_type = effects.iter().find_map(|e| match e {
        Effect::NudgeCoworkerByName { nudge_type, .. } => Some(nudge_type.as_str()),
        _ => None,
    });

    assert_eq!(nudge_type, Some("rebase_regression"));
}
