use crate::daemon::session_events::{self, SessionEvent};
use crate::headless::StreamEvent;
use tokio::sync::mpsc;

#[tokio::test]
async fn session_event_carries_name_and_stream_event() {
    let event = SessionEvent::Event {
        name: "ghost-town".to_string(),
        slot_id: "slot-1".to_string(),
        event: StreamEvent::Unknown,
    };
    match event {
        SessionEvent::Event { name, slot_id, .. } => {
            assert_eq!(name, "ghost-town");
            assert_eq!(slot_id, "slot-1");
        }
        _ => panic!("wrong variant"),
    }
}

#[tokio::test]
async fn session_event_stderr_variant() {
    let event = SessionEvent::Stderr {
        name: "live-wire".to_string(),
        slot_id: "slot-2".to_string(),
        line: "some error".to_string(),
    };
    match event {
        SessionEvent::Stderr { name, line, .. } => {
            assert_eq!(name, "live-wire");
            assert_eq!(line, "some error");
        }
        _ => panic!("wrong variant"),
    }
}

#[tokio::test]
async fn session_event_stopped_variant() {
    let event = SessionEvent::Stopped {
        name: "park".to_string(),
        slot_id: "slot-3".to_string(),
    };
    match event {
        SessionEvent::Stopped { name, slot_id } => {
            assert_eq!(name, "park");
            assert_eq!(slot_id, "slot-3");
        }
        _ => panic!("wrong variant"),
    }
}

#[tokio::test]
async fn forwarder_sends_stdout_events_to_aggregated_channel() {
    let (agg_tx, mut agg_rx) = session_events::channel();
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<StreamEvent>();
    let (_stderr_tx, stderr_rx) = mpsc::unbounded_channel::<String>();

    session_events::spawn_forwarder(
        "ghost-town".to_string(),
        "slot-1".to_string(),
        stdout_rx,
        stderr_rx,
        agg_tx,
    );

    let event = StreamEvent::Assistant {
        message: serde_json::Value::String("hello".to_string()),
        session_id: None,
        extra: serde_json::Value::Null,
    };
    stdout_tx.send(event).unwrap();
    drop(stdout_tx);

    let received = agg_rx.recv().await.unwrap();
    match received {
        SessionEvent::Event { name, slot_id, .. } => {
            assert_eq!(name, "ghost-town");
            assert_eq!(slot_id, "slot-1");
        }
        other => panic!("expected Event, got {:?}", other),
    }
}

#[tokio::test]
async fn forwarder_sends_stopped_when_stdout_closes() {
    let (agg_tx, mut agg_rx) = session_events::channel();
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<StreamEvent>();
    let (_stderr_tx, stderr_rx) = mpsc::unbounded_channel::<String>();

    session_events::spawn_forwarder(
        "park".to_string(),
        "slot-2".to_string(),
        stdout_rx,
        stderr_rx,
        agg_tx,
    );

    drop(stdout_tx);

    let received = agg_rx.recv().await.unwrap();
    match received {
        SessionEvent::Stopped { name, slot_id } => {
            assert_eq!(name, "park");
            assert_eq!(slot_id, "slot-2");
        }
        other => panic!("expected Stopped, got {:?}", other),
    }
}

#[tokio::test]
async fn forwarder_sends_stderr_lines() {
    let (agg_tx, mut agg_rx) = session_events::channel();
    let (_stdout_tx, stdout_rx) = mpsc::unbounded_channel::<StreamEvent>();
    let (stderr_tx, stderr_rx) = mpsc::unbounded_channel::<String>();

    session_events::spawn_forwarder(
        "live-wire".to_string(),
        "slot-3".to_string(),
        stdout_rx,
        stderr_rx,
        agg_tx,
    );

    stderr_tx.send("error line 1".to_string()).unwrap();
    drop(stderr_tx);

    let received = agg_rx.recv().await.unwrap();
    match received {
        SessionEvent::Stderr { name, line, .. } => {
            assert_eq!(name, "live-wire");
            assert_eq!(line, "error line 1");
        }
        other => panic!("expected Stderr, got {:?}", other),
    }
}

#[tokio::test]
async fn forwarder_interleaves_stdout_and_stderr() {
    let (agg_tx, mut agg_rx) = session_events::channel();
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel();
    let (stderr_tx, stderr_rx) = mpsc::unbounded_channel();

    session_events::spawn_forwarder(
        "test".to_string(),
        "slot-1".to_string(),
        stdout_rx,
        stderr_rx,
        agg_tx,
    );

    stdout_tx.send(StreamEvent::Unknown).unwrap();
    stderr_tx.send("err1".to_string()).unwrap();
    stdout_tx.send(StreamEvent::Unknown).unwrap();
    drop(stdout_tx);
    drop(stderr_tx);

    let mut event_count = 0;
    let mut stderr_count = 0;
    let mut stopped = false;
    while let Some(ev) = agg_rx.recv().await {
        match ev {
            SessionEvent::Event { .. } => event_count += 1,
            SessionEvent::Stderr { .. } => stderr_count += 1,
            SessionEvent::Stopped { .. } => {
                stopped = true;
                break;
            }
        }
    }
    assert_eq!(event_count, 2);
    assert!(stderr_count >= 1);
    assert!(stopped);
}
