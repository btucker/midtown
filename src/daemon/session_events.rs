use crate::headless::StreamEvent;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::debug;

/// A tagged event from a specific session, sent through the aggregated channel.
///
/// The main event loop receives these from all sessions through a single
/// `mpsc::UnboundedReceiver<SessionEvent>`, eliminating the need to poll
/// individual session receivers on a timer.
#[derive(Debug)]
pub enum SessionEvent {
    /// A parsed stdout event from a session.
    Event {
        name: String,
        slot_id: String,
        event: StreamEvent,
    },
    /// A stderr line from a session.
    Stderr {
        #[allow(dead_code)]
        name: String,
        slot_id: String,
        line: String,
    },
    /// A session's stdout closed (process exited).
    Stopped { name: String, slot_id: String },
}

/// Create a new aggregated session event channel.
pub fn channel() -> (
    mpsc::UnboundedSender<SessionEvent>,
    mpsc::UnboundedReceiver<SessionEvent>,
) {
    mpsc::unbounded_channel()
}

/// Spawn a forwarder task that reads from per-session stdout/stderr receivers
/// and sends tagged events into the aggregated channel.
///
/// The task runs until both stdout and stderr channels close, then sends
/// a `Stopped` event. Returns the JoinHandle for the spawned task.
#[allow(dead_code)]
pub fn spawn_forwarder(
    name: String,
    slot_id: String,
    mut stdout_rx: mpsc::UnboundedReceiver<StreamEvent>,
    mut stderr_rx: mpsc::UnboundedReceiver<String>,
    agg_tx: mpsc::UnboundedSender<SessionEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                event = stdout_rx.recv() => {
                    match event {
                        Some(stream_event) => {
                            if agg_tx.send(SessionEvent::Event {
                                name: name.clone(),
                                slot_id: slot_id.clone(),
                                event: stream_event,
                            }).is_err() {
                                break;
                            }
                        }
                        None => {
                            debug!("Session '{}' stdout forwarder: stdout closed", name);
                            // Drain any remaining stderr before sending Stopped
                            while let Ok(line) = stderr_rx.try_recv() {
                                let _ = agg_tx.send(SessionEvent::Stderr {
                                    name: name.clone(),
                                    slot_id: slot_id.clone(),
                                    line,
                                });
                            }
                            let _ = agg_tx.send(SessionEvent::Stopped {
                                name: name.clone(),
                                slot_id: slot_id.clone(),
                            });
                            break;
                        }
                    }
                }
                line = stderr_rx.recv() => {
                    match line {
                        Some(stderr_line) => {
                            let _ = agg_tx.send(SessionEvent::Stderr {
                                name: name.clone(),
                                slot_id: slot_id.clone(),
                                line: stderr_line,
                            });
                        }
                        None => {
                            debug!("Session '{}' stdout forwarder: stderr closed, continuing stdout", name);
                            // stderr closed but stdout still open — drain stdout only
                            loop {
                                match stdout_rx.recv().await {
                                    Some(stream_event) => {
                                        if agg_tx.send(SessionEvent::Event {
                                            name: name.clone(),
                                            slot_id: slot_id.clone(),
                                            event: stream_event,
                                        }).is_err() {
                                            return;
                                        }
                                    }
                                    None => {
                                        let _ = agg_tx.send(SessionEvent::Stopped {
                                            name: name.clone(),
                                            slot_id: slot_id.clone(),
                                        });
                                        return;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}
