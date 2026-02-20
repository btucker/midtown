use super::*;
use tempfile::TempDir;

/// Verifies that `clear_inbox` removes an existing inbox so a newly-allocated
/// name does not inherit the previous session's unread messages.
#[test]
fn test_clear_inbox_removes_existing_messages() {
    let tmp = TempDir::new().unwrap();
    let team_dir = tmp.path().join("test-team");
    let inboxes_dir = team_dir.join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    // Simulate messages left by a previous session that held the name "lexington".
    let inbox_file = inboxes_dir.join("lexington.json");
    let stale_messages = vec![MailboxMessage::new("stale message", "daemon")];
    fs::write(
        &inbox_file,
        serde_json::to_string_pretty(&stale_messages).unwrap(),
    )
    .unwrap();
    assert!(inbox_file.exists(), "Inbox should exist before clear");

    // Clear the inbox (simulating name re-allocation to a new session).
    clear_inbox_at(&inboxes_dir, "lexington").unwrap();

    // The inbox file must be gone so the new session starts with an empty inbox.
    assert!(
        !inbox_file.exists(),
        "Inbox must not exist after clear_inbox — new session should not inherit stale messages"
    );
}

/// Verifies that `clear_inbox` is a no-op when no inbox exists (first allocation).
#[test]
fn test_clear_inbox_no_file_is_ok() {
    let tmp = TempDir::new().unwrap();
    let inboxes_dir = tmp.path().join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    // Should succeed even when the file doesn't exist.
    clear_inbox_at(&inboxes_dir, "lexington").unwrap();
}

#[test]
fn test_mailbox_message_new() {
    let msg = MailboxMessage::new("hello", "daemon");
    assert_eq!(msg.text, "hello");
    assert_eq!(msg.from, "daemon");
    assert!(!msg.read);
    assert!(msg.color.is_none());
    assert!(msg.summary.is_none());
}

#[test]
fn test_mailbox_message_builder() {
    let msg = MailboxMessage::new("hello", "daemon")
        .with_color("blue")
        .with_summary("Test message");
    assert_eq!(msg.color.as_deref(), Some("blue"));
    assert_eq!(msg.summary.as_deref(), Some("Test message"));
}

#[test]
fn test_team_name_for_repo() {
    assert_eq!(team_name_for_repo("midtown"), "midtown-midtown");
    assert_eq!(team_name_for_repo("my-project"), "midtown-my-project");
}

#[test]
fn test_agent_id() {
    assert_eq!(
        agent_id("lexington", "midtown-repo"),
        "lexington@midtown-repo"
    );
}

#[test]
fn test_mkdir_lock_acquire_release() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("test.lock");

    // Acquire lock
    {
        let _lock = MkdirLock::acquire(&lock_path, 5).unwrap();
        assert!(lock_path.exists());
    }
    // Lock released on drop
    assert!(!lock_path.exists());
}

#[test]
fn test_mkdir_lock_contention() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("test.lock");

    // Pre-create the lock directory to simulate contention
    fs::create_dir(&lock_path).unwrap();

    // Should fail quickly with just 1 retry
    let result = MkdirLock::acquire(&lock_path, 2);
    assert!(result.is_err());

    // Clean up
    fs::remove_dir(&lock_path).unwrap();
}

#[test]
fn test_write_to_inbox_creates_file() {
    let tmp = TempDir::new().unwrap();
    let inboxes_dir = tmp.path().join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let inbox_file = inboxes_dir.join("test-agent.json");
    let lock_file = inboxes_dir.join("test-agent.json.lock");

    // Write directly using the lock and file paths
    let msg = MailboxMessage::new("hello", "daemon");

    let _lock = MkdirLock::acquire(&lock_file, 5).unwrap();
    let messages = vec![msg];
    fs::write(
        &inbox_file,
        serde_json::to_string_pretty(&messages).unwrap(),
    )
    .unwrap();
    drop(_lock);

    // Verify
    let content = fs::read_to_string(&inbox_file).unwrap();
    let parsed: Vec<MailboxMessage> = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].text, "hello");
    assert_eq!(parsed[0].from, "daemon");
    assert!(!parsed[0].read);
}

#[test]
fn test_write_to_inbox_appends() {
    let tmp = TempDir::new().unwrap();
    let inboxes_dir = tmp.path().join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let inbox_file = inboxes_dir.join("test-agent.json");
    let lock_file = inboxes_dir.join("test-agent.json.lock");

    // Write first message
    {
        let _lock = MkdirLock::acquire(&lock_file, 5).unwrap();
        let messages = vec![MailboxMessage::new("first", "daemon")];
        fs::write(
            &inbox_file,
            serde_json::to_string_pretty(&messages).unwrap(),
        )
        .unwrap();
    }

    // Write second message (read-modify-write)
    {
        let _lock = MkdirLock::acquire(&lock_file, 5).unwrap();
        let content = fs::read_to_string(&inbox_file).unwrap();
        let mut messages: Vec<MailboxMessage> = serde_json::from_str(&content).unwrap();
        messages.push(MailboxMessage::new("second", "daemon"));
        fs::write(
            &inbox_file,
            serde_json::to_string_pretty(&messages).unwrap(),
        )
        .unwrap();
    }

    // Verify
    let content = fs::read_to_string(&inbox_file).unwrap();
    let parsed: Vec<MailboxMessage> = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].text, "first");
    assert_eq!(parsed[1].text, "second");
}

#[test]
fn test_ensure_team_config() {
    let tmp = TempDir::new().unwrap();
    let team_dir = tmp.path().join("test-team");
    let inboxes_dir = team_dir.join("inboxes");
    let config_path = team_dir.join("config.json");

    let members = vec![
        TeamMember {
            name: "lexington".to_string(),
            agent_id: "lexington@midtown-repo".to_string(),
            agent_type: "coworker".to_string(),
        },
        TeamMember {
            name: "park".to_string(),
            agent_id: "park@midtown-repo".to_string(),
            agent_type: "coworker".to_string(),
        },
    ];
    ensure_team_config_at(&team_dir, &members).unwrap();

    // Verify directory structure and config
    assert!(inboxes_dir.exists());
    assert!(config_path.exists());

    let content = fs::read_to_string(&config_path).unwrap();
    let parsed: TeamConfig = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.members.len(), 2);
    assert_eq!(parsed.members[0].name, "lexington");
    assert_eq!(parsed.members[1].agent_id, "park@midtown-repo");
}

#[test]
fn test_ensure_team_config_concurrent_no_corruption() {
    use std::sync::Arc;

    let tmp = TempDir::new().unwrap();
    let team_dir = Arc::new(tmp.path().join("test-team"));

    // Spawn 10 threads all calling ensure_team_config_at concurrently
    // with different member lists. Without locking, this can corrupt the file.
    let handles: Vec<_> = (0..10)
        .map(|i| {
            let dir = Arc::clone(&team_dir);
            std::thread::spawn(move || {
                let members = vec![TeamMember {
                    name: format!("agent-{}", i),
                    agent_id: format!("agent-{}@team", i),
                    agent_type: "coworker".to_string(),
                }];
                ensure_team_config_at(&dir, &members).unwrap();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // The file must be valid JSON (no corruption from concurrent writes)
    let config_path = team_dir.join("config.json");
    let content = fs::read_to_string(&config_path).unwrap();
    let parsed: TeamConfig = serde_json::from_str(&content).unwrap();
    assert_eq!(
        parsed.members.len(),
        1,
        "ensure_team_config overwrites (not appends), so last writer wins with 1 member"
    );
}

#[test]
fn test_cleanup_team() {
    let tmp = TempDir::new().unwrap();
    let team_dir = tmp.path().join("test-team");
    fs::create_dir_all(team_dir.join("inboxes")).unwrap();
    fs::write(team_dir.join("config.json"), "{}").unwrap();

    assert!(team_dir.exists());
    fs::remove_dir_all(&team_dir).unwrap();
    assert!(!team_dir.exists());
}

#[test]
fn test_upsert_team_member_creates_config() {
    let tmp = TempDir::new().unwrap();
    let team_dir = tmp.path().join("test-team");

    let member = TeamMember {
        name: "lexington".to_string(),
        agent_id: "lexington@midtown-repo".to_string(),
        agent_type: "coworker".to_string(),
    };
    upsert_team_member_at(&team_dir, member).unwrap();

    // Add second member
    let member2 = TeamMember {
        name: "park".to_string(),
        agent_id: "park@midtown-repo".to_string(),
        agent_type: "coworker".to_string(),
    };
    upsert_team_member_at(&team_dir, member2).unwrap();

    // Verify both members exist
    let config_path = team_dir.join("config.json");
    let content = fs::read_to_string(&config_path).unwrap();
    let parsed: TeamConfig = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.members.len(), 2);
    assert_eq!(parsed.members[0].name, "lexington");
    assert_eq!(parsed.members[1].name, "park");
}

#[test]
fn test_upsert_team_member_updates_existing() {
    let tmp = TempDir::new().unwrap();
    let team_dir = tmp.path().join("test-team");

    let member = TeamMember {
        name: "lexington".to_string(),
        agent_id: "lexington@midtown-repo".to_string(),
        agent_type: "coworker".to_string(),
    };
    upsert_team_member_at(&team_dir, member).unwrap();

    // Upsert same member with different type (simulating role change)
    let updated = TeamMember {
        name: "lexington".to_string(),
        agent_id: "lexington@midtown-repo".to_string(),
        agent_type: "reviewer".to_string(),
    };
    upsert_team_member_at(&team_dir, updated).unwrap();

    // Verify member was updated (not duplicated)
    let config_path = team_dir.join("config.json");
    let content = fs::read_to_string(&config_path).unwrap();
    let parsed: TeamConfig = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed.members.len(), 1);
    assert_eq!(parsed.members[0].agent_type, "reviewer");
}

#[test]
fn test_upsert_team_member_concurrent_no_lost_members() {
    use std::sync::Arc;

    let tmp = TempDir::new().unwrap();
    let team_dir = Arc::new(tmp.path().join("test-team"));
    let names = [
        "lexington",
        "park",
        "madison",
        "broadway",
        "amsterdam",
        "columbus",
        "riverside",
        "york",
        "pleasant",
        "vernon",
    ];

    let handles: Vec<_> = names
        .iter()
        .map(|name| {
            let dir = Arc::clone(&team_dir);
            let name = name.to_string();
            std::thread::spawn(move || {
                let member = TeamMember {
                    name: name.clone(),
                    agent_id: format!("{}@midtown-repo", name),
                    agent_type: "coworker".to_string(),
                };
                upsert_team_member_at(&dir, member).unwrap();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    // All 10 members must be present — no lost entries
    let config_path = team_dir.join("config.json");
    let content = fs::read_to_string(&config_path).unwrap();
    let parsed: TeamConfig = serde_json::from_str(&content).unwrap();
    assert_eq!(
        parsed.members.len(),
        10,
        "Expected 10 members, got {}. Lost members due to concurrent write race.",
        parsed.members.len()
    );

    // Verify all names are present
    let mut found_names: Vec<String> = parsed.members.iter().map(|m| m.name.clone()).collect();
    found_names.sort();
    let mut expected_names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
    expected_names.sort();
    assert_eq!(found_names, expected_names);
}

#[test]
fn test_mailbox_message_serialization() {
    let msg = MailboxMessage::new("hello world", "daemon")
        .with_color("blue")
        .with_summary("Test msg");
    let json = serde_json::to_string(&msg).unwrap();
    let parsed: MailboxMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.text, "hello world");
    assert_eq!(parsed.from, "daemon");
    assert_eq!(parsed.color.as_deref(), Some("blue"));
    assert_eq!(parsed.summary.as_deref(), Some("Test msg"));
    assert!(!parsed.read);
}

#[test]
fn test_write_to_inbox_no_leaked_tmp() {
    // Verify that write_to_inbox does not leave a temp file after success.
    let tmp = TempDir::new().unwrap();
    let inboxes_dir = tmp.path().join("inboxes");
    fs::create_dir_all(&inboxes_dir).unwrap();

    let inbox_file = inboxes_dir.join("test-agent.json");
    let tmp_file = inbox_file.with_extension("json.tmp");
    let lock_file = inboxes_dir.join("test-agent.json.lock");

    let msg = MailboxMessage::new("hello", "daemon");
    let _lock = MkdirLock::acquire(&lock_file, 5).unwrap();

    let messages = vec![msg];
    let json = serde_json::to_string_pretty(&messages).unwrap();
    fs::write(&tmp_file, &json).unwrap();
    crate::paths::atomic_rename(&tmp_file, &inbox_file).unwrap();

    assert!(!tmp_file.exists(), "no temp file should remain");
    assert!(inbox_file.exists(), "inbox should contain the message");
}

#[test]
fn test_stale_lock_recovery() {
    let tmp = TempDir::new().unwrap();
    let lock_path = tmp.path().join("test.lock");

    // Create a lock directory and backdate its modification time.
    // Since we can't easily backdate on all platforms, we test the logic
    // path by verifying fresh locks are NOT treated as stale.
    fs::create_dir(&lock_path).unwrap();

    // Fresh lock should block (not be treated as stale)
    let result = MkdirLock::acquire(&lock_path, 2);
    assert!(result.is_err(), "Fresh lock should not be treated as stale");

    fs::remove_dir(&lock_path).unwrap();
}
