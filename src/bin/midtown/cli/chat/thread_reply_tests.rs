use midtown::{Channel, Message};
use tempfile::TempDir;

use super::app::tests::test_app_with_channel;

/// Bug #3: Thread replies must not accumulate in `messages` or `thread_messages`
/// across multiple refresh cycles.
///
/// Before the fix, thread replies stored with `thread_parent_id` were included
/// in the main channel view. The fix adds a `thread_parent_id.is_none()` filter
/// in `draw_chat_messages()`. The cursor-based reader ensures each reply is
/// only ingested once regardless of how many times `refresh()` is called.
#[test]
fn test_thread_replies_not_duplicated_across_refresh_cycles() {
    let temp_dir = TempDir::new().unwrap();
    let channel = Channel::new(temp_dir.path(), "test-channel").unwrap();

    // Write a parent message and a first thread reply to the channel file.
    let parent = Message::text("alice", "parent message");
    let parent_id = parent.id.clone();
    let mut reply1 = Message::text("bob", "first thread reply");
    reply1.thread_parent_id = Some(parent_id.clone());
    channel.send(&parent).unwrap();
    channel.send(&reply1).unwrap();

    // Create an App with the real channel. `test_app_with_channel()` sets
    // `initial_load_done: true` and `session_id: "test-session"`, so the
    // first `refresh()` uses cursor-based reading starting at byte 0,
    // which reads all pre-existing messages exactly once.
    let mut app = test_app_with_channel(channel.clone());

    // First refresh reads both parent and reply from the cursor.
    app.refresh();
    assert_eq!(
        app.messages.len(),
        2,
        "Initial refresh must read parent + reply1"
    );

    // Open the thread — open_thread() collects existing replies from messages.
    app.open_thread(&parent_id);
    assert_eq!(
        app.thread_messages.len(),
        1,
        "Thread panel must show exactly 1 reply after open"
    );

    // Write a second thread reply AFTER the cursor is positioned at EOF.
    // This exercises the live-message routing path in refresh().
    let mut reply2 = Message::text("carol", "second thread reply");
    reply2.thread_parent_id = Some(parent_id.clone());
    channel.send(&reply2).unwrap();

    // refresh() must route reply2 to thread_messages (thread is open) and
    // also append it to messages — each exactly once.
    app.refresh();
    assert_eq!(
        app.messages.len(),
        3,
        "After reply2 arrives: messages must contain parent + reply1 + reply2"
    );
    assert_eq!(
        app.thread_messages.len(),
        2,
        "After reply2 arrives: thread_messages must contain reply1 + reply2"
    );

    // Bug #3: subsequent refresh cycles with no new messages must NOT
    // re-add anything — the cursor is now at EOF.
    app.refresh();
    app.refresh();

    assert_eq!(
        app.messages.len(),
        3,
        "messages must not accumulate across refresh cycles (bug #3)"
    );
    assert_eq!(
        app.thread_messages.len(),
        2,
        "thread_messages must not accumulate across refresh cycles (bug #3)"
    );
}
