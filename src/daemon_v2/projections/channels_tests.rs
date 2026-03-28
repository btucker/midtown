use super::*;
use crate::daemon_v2::events::DomainEvent;

#[test]
fn message_creates_channel_if_missing() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "ghost-town".into(),
        content: "hello".into(),
        thread_id: None,
    });
    assert!(idx.channels.contains_key("main"));
    assert!(idx.channels.get("main").unwrap().last_message_at.is_some());
}

#[test]
fn thread_message_increments_thread_count() {
    let mut idx = ChannelIndex::default();
    idx.apply(&DomainEvent::MessagePosted {
        id: "m1".into(),
        channel: "main".into(),
        sender: "user".into(),
        content: "parent".into(),
        thread_id: None,
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m2".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply".into(),
        thread_id: Some("m1".into()),
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
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m2".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply 1".into(),
        thread_id: Some("m1".into()),
    });
    idx.apply(&DomainEvent::MessagePosted {
        id: "m3".into(),
        channel: "main".into(),
        sender: "bot".into(),
        content: "reply 2".into(),
        thread_id: Some("m1".into()),
    });
    assert_eq!(idx.channels.get("main").unwrap().thread_count, 1);
}

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
