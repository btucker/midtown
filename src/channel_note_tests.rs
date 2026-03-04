use chrono::{Duration, Utc};
use std::fs;
use tempfile::TempDir;

use super::*;

// ── parse_note_reviewed_at tests ──────────────────────────────────────────

#[test]
fn test_parse_reviewed_at_valid_rfc3339() {
    let content = "---\nreviewed_at: 2026-03-01T12:00:00Z\n---\n# Note content";
    let result = parse_note_reviewed_at(content);
    assert!(result.is_some());
    let dt = result.unwrap();
    assert_eq!(
        dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "2026-03-01T12:00:00Z"
    );
}

#[test]
fn test_parse_reviewed_at_no_frontmatter() {
    let content = "# Just a note\nWith some content";
    assert!(parse_note_reviewed_at(content).is_none());
}

#[test]
fn test_parse_reviewed_at_no_reviewed_field() {
    let content = "---\ntitle: My Note\n---\n# Content";
    assert!(parse_note_reviewed_at(content).is_none());
}

#[test]
fn test_parse_reviewed_at_empty_content() {
    assert!(parse_note_reviewed_at("").is_none());
}

#[test]
fn test_parse_reviewed_at_incomplete_frontmatter() {
    let content = "---\nreviewed_at: 2026-03-01T12:00:00Z\n# No closing delimiter";
    assert!(parse_note_reviewed_at(content).is_none());
}

#[test]
fn test_parse_reviewed_at_preserves_other_fields() {
    let content =
        "---\ntitle: Test\nreviewed_at: 2026-03-01T12:00:00Z\nauthor: alice\n---\n# Content";
    let result = parse_note_reviewed_at(content);
    assert!(result.is_some());
}

// ── stamp_reviewed_at_in_content tests ────────────────────────────────────

#[test]
fn test_stamp_no_frontmatter() {
    let content = "# My Note\nSome content";
    let result = stamp_reviewed_at_in_content(content, "2026-03-04T10:00:00Z");
    assert!(result.starts_with("---\nreviewed_at: 2026-03-04T10:00:00Z\n---\n"));
    assert!(result.contains("# My Note"));
}

#[test]
fn test_stamp_existing_reviewed_at() {
    let content = "---\nreviewed_at: 2026-03-01T12:00:00Z\n---\n# Content";
    let result = stamp_reviewed_at_in_content(content, "2026-03-04T10:00:00Z");
    assert!(result.contains("reviewed_at: 2026-03-04T10:00:00Z"));
    assert!(!result.contains("2026-03-01"));
    assert!(result.contains("# Content"));
    // Bug regression: re-stamped content must remain parseable
    let re_parsed = parse_note_reviewed_at(&result);
    assert!(
        re_parsed.is_some(),
        "Re-stamped content must be parseable; got:\n{}",
        result
    );
}

#[test]
fn test_stamp_frontmatter_without_reviewed_at() {
    let content = "---\ntitle: Test Note\n---\n# Content";
    let result = stamp_reviewed_at_in_content(content, "2026-03-04T10:00:00Z");
    assert!(result.contains("reviewed_at: 2026-03-04T10:00:00Z"));
    assert!(result.contains("title: Test Note"));
    assert!(result.contains("# Content"));
}

// ── stamp_note_reviewed integration test ──────────────────────────────────

#[test]
fn test_stamp_note_reviewed_file() {
    let dir = TempDir::new().unwrap();
    let note_path = dir.path().join("test-note.md");
    fs::write(&note_path, "# My Note\nContent here").unwrap();

    stamp_note_reviewed(&note_path).unwrap();

    let content = fs::read_to_string(&note_path).unwrap();
    assert!(content.contains("reviewed_at:"));
    assert!(content.contains("# My Note"));

    // Parse it back
    let reviewed = parse_note_reviewed_at(&content);
    assert!(reviewed.is_some());
    // Should be very recent (within last minute)
    let age = Utc::now() - reviewed.unwrap();
    assert!(age.num_seconds() < 60);
}

// ── list_channel_note_infos tests ─────────────────────────────────────────

fn setup_channel_with_notes(dir: &TempDir, channel_name: &str, notes: &[(&str, Option<&str>)]) {
    // Create channel directory structure with history
    let channel_dir = dir.path().join("channels").join(channel_name);
    let history_dir = channel_dir.join("history");
    fs::create_dir_all(&history_dir).unwrap();
    fs::write(history_dir.join("current.jsonl"), "").unwrap();

    let notes_dir = channel_dir.join("notes");
    fs::create_dir_all(&notes_dir).unwrap();
    for (name, reviewed_at) in notes {
        let content = if let Some(ts) = reviewed_at {
            format!("---\nreviewed_at: {}\n---\n# {}", ts, name)
        } else {
            format!("# {}\nSome content", name)
        };
        fs::write(notes_dir.join(format!("{}.md", name)), content).unwrap();
    }
}

#[test]
fn test_list_channel_note_infos_basic() {
    let dir = TempDir::new().unwrap();
    setup_channel_with_notes(
        &dir,
        "test-channel",
        &[("note-a", Some("2026-03-01T12:00:00Z")), ("note-b", None)],
    );

    let notes = list_channel_note_infos(dir.path(), "test-channel");
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].name, "note-a");
    assert!(notes[0].reviewed_at.is_some());
    assert_eq!(notes[1].name, "note-b");
    assert!(notes[1].reviewed_at.is_none());
}

#[test]
fn test_list_channel_note_infos_empty() {
    let dir = TempDir::new().unwrap();
    let notes = list_channel_note_infos(dir.path(), "nonexistent");
    assert!(notes.is_empty());
}

// ── find_stale_notes tests ────────────────────────────────────────────────

#[test]
fn test_find_stale_notes_identifies_stale() {
    let dir = TempDir::new().unwrap();
    let old_ts = (Utc::now() - Duration::days(5))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let fresh_ts = (Utc::now() - Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();

    setup_channel_with_notes(
        &dir,
        "chan",
        &[
            ("stale-note", Some(&old_ts)),
            ("fresh-note", Some(&fresh_ts)),
            ("never-reviewed", None),
        ],
    );

    let now = Utc::now();
    let threshold = Duration::hours(NOTE_STALENESS_THRESHOLD_HOURS);
    let stale = find_stale_notes(dir.path(), now, threshold);

    assert_eq!(stale.len(), 1);
    let (channel_name, stale_names) = stale.iter().next().unwrap();
    assert_eq!(channel_name, "chan");
    assert!(stale_names.contains(&"stale-note".to_string()));
    assert!(stale_names.contains(&"never-reviewed".to_string()));
    assert!(!stale_names.contains(&"fresh-note".to_string()));
}

#[test]
fn test_find_stale_notes_skips_archived() {
    let dir = TempDir::new().unwrap();

    // Create an archived channel with stale notes
    let archived_dir = dir.path().join("channels").join("old-channel.archived");
    let history_dir = archived_dir.join("history");
    fs::create_dir_all(&history_dir).unwrap();
    fs::write(history_dir.join("current.jsonl"), "").unwrap();
    let notes_dir = archived_dir.join("notes");
    fs::create_dir_all(&notes_dir).unwrap();
    fs::write(notes_dir.join("old-note.md"), "# Old").unwrap();

    let now = Utc::now();
    let threshold = Duration::hours(NOTE_STALENESS_THRESHOLD_HOURS);
    let stale = find_stale_notes(dir.path(), now, threshold);
    assert!(stale.is_empty());
}

#[test]
fn test_find_stale_notes_all_fresh() {
    let dir = TempDir::new().unwrap();
    let fresh_ts = (Utc::now() - Duration::hours(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    setup_channel_with_notes(&dir, "chan", &[("fresh-note", Some(&fresh_ts))]);

    let now = Utc::now();
    let threshold = Duration::hours(NOTE_STALENESS_THRESHOLD_HOURS);
    let stale = find_stale_notes(dir.path(), now, threshold);
    assert!(stale.is_empty());
}
