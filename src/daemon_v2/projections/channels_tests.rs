use super::*;
use crate::daemon_v2::events::DomainEvent;

// ── Section 6.3: ChannelIndex ───────────────────────────────────────────────

/// Spec 6.3: WHEN MessagePosted is applied THEN channel ensured to exist,
/// last_message_at updated
#[test]
fn message_creates_channel_if_missing() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "ghost-town".into(),
        content: "hello".into(),
        thread_id: None,
        tool_data: None,
        auto_output: false,
    });
    assert!(idx.channels.contains_key("main"));
    assert!(idx.channels.get("main").unwrap().last_message_at.is_some());
}

/// Spec 6.3: WHEN MessagePosted has thread_id THEN thread added to
/// known_threads and thread_count incremented
#[test]
fn thread_message_increments_thread_count() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "user".into(),
        content: "parent".into(),
        thread_id: None,
        tool_data: None,
        auto_output: false,
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m2".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply".into(),
        thread_id: Some("m1".into()),
        tool_data: None,
        auto_output: false,
    });
    assert_eq!(idx.channels.get("main").unwrap().thread_count, 1);
}

#[test]
fn multiple_replies_same_thread_no_double_count() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "user".into(),
        content: "parent".into(),
        thread_id: None,
        tool_data: None,
        auto_output: false,
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m2".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply 1".into(),
        thread_id: Some("m1".into()),
        tool_data: None,
        auto_output: false,
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m3".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply 2".into(),
        thread_id: Some("m1".into()),
        tool_data: None,
        auto_output: false,
    });
    assert_eq!(idx.channels.get("main").unwrap().thread_count, 1);
}

/// Spec 6.3: WHEN ChannelLeadDrivenSet is applied THEN lead_driven setting updated
#[test]
fn lead_driven_flag() {
    let mut idx = ChannelIndex::default();
    assert!(!idx.is_lead_driven("test"));

    idx.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "test".into(),
        lead_driven: true,
    });
    assert!(idx.is_lead_driven("test"));

    idx.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "test".into(),
        lead_driven: false,
    });
    assert!(!idx.is_lead_driven("test"));
}

/// Spec 6.3: WHEN ChannelDirectorySet is applied THEN directory setting updated
#[test]
fn channel_directory_set() {
    let mut idx = ChannelIndex::default();
    assert!(idx.channel_directory("docs").is_none());

    idx.apply(&DomainEvent::ChannelDirectorySet {
        channel: "docs".into(),
        directory: Some("packages/docs".into()),
    });
    assert_eq!(
        idx.channel_directory("docs"),
        Some("packages/docs"),
        "directory should be set"
    );

    idx.apply(&DomainEvent::ChannelDirectorySet {
        channel: "docs".into(),
        directory: None,
    });
    assert!(
        idx.channel_directory("docs").is_none(),
        "directory should be cleared"
    );
}

/// ChannelCreated adds channel to index
#[test]
fn channel_created_adds_to_index() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::ChannelCreated {
        channel: "new-chan".into(),
    });
    assert!(idx.channels.contains_key("new-chan"));
    assert!(!idx.channels.get("new-chan").unwrap().archived);
}

/// ChannelArchived sets archived flag
#[test]
fn channel_archived_sets_flag() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::ChannelCreated {
        channel: "test".into(),
    });
    idx.apply(&DomainEvent::ChannelArchived {
        channel: "test".into(),
    });
    assert!(idx.channels.get("test").unwrap().archived);
}

/// ChannelUnarchived clears archived flag
#[test]
fn channel_unarchived_clears_flag() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::ChannelCreated {
        channel: "test".into(),
    });
    idx.apply(&DomainEvent::ChannelArchived {
        channel: "test".into(),
    });
    idx.apply(&DomainEvent::ChannelUnarchived {
        channel: "test".into(),
    });
    assert!(!idx.channels.get("test").unwrap().archived);
}
