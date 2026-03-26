# Per-Session Async Stdout Readers with mpsc Channels

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace polling-based session output drain with event-driven mpsc channel aggregation so the main event loop receives session events with near-zero latency.

**Architecture:** Each HeadlessSession already spawns a background tokio task that reads stdout lines and sends parsed `StreamEvent`s into an `mpsc::UnboundedReceiver`. Currently, `drain_events()` polls these per-session receivers with 10ms timeouts on a 2-second tick. This plan introduces a shared aggregated `mpsc` channel that all sessions feed into (tagged with session name), so the main event loop can `select!` on a single receiver. The drain interval becomes a health-check-only fallback.

**Tech Stack:** Rust, tokio (mpsc channels, select!), serde_json

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src/daemon/session_events.rs` | **Create** | `SessionEvent` type (name + event/stderr/stopped), aggregated channel types, `SessionEventForwarder` that takes per-session receivers and forwards into shared channel |
| `src/daemon/sessions.rs` | **Modify** | Store shared `mpsc::UnboundedSender` in `SessionManager`, pass it to spawned sessions. Add `take_event_receiver()`. Simplify `drain_events()` to health-check-only. |
| `src/daemon/mod.rs` | **Modify** | Add `select!` branch on aggregated receiver. Extract event-processing logic into helper. Rename/repurpose drain interval to health-check interval. |
| `src/daemon/session_events_tests.rs` | **Create** | Unit tests for `SessionEventForwarder` and `SessionEvent` |
| `src/daemon/sessions_tests.rs` | **Modify** | Update drain_events tests for new behavior |

---

## Chunk 1: SessionEvent Type and Forwarder

### Task 1: Define SessionEvent enum and channel types

**Files:**
- Create: `src/daemon/session_events.rs`
- Modify: `src/daemon/mod.rs` (add module declaration)

- [ ] **Step 1: Write the failing test**

Create `src/daemon/session_events_tests.rs`:

```rust
use crate::daemon::session_events::SessionEvent;
use crate::headless::StreamEvent;

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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test session_event_carries_name -- --nocapture 2>&1 | tail -5`
Expected: FAIL — module `session_events` doesn't exist

- [ ] **Step 3: Create session_events.rs with SessionEvent enum**

Create `src/daemon/session_events.rs`:

```rust
use crate::headless::StreamEvent;
use tokio::sync::mpsc;

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
        name: String,
        slot_id: String,
        line: String,
    },
    /// A session's stdout closed (process exited).
    Stopped {
        name: String,
        slot_id: String,
    },
}

/// Create a new aggregated session event channel.
pub fn channel() -> (mpsc::UnboundedSender<SessionEvent>, mpsc::UnboundedReceiver<SessionEvent>) {
    mpsc::unbounded_channel()
}
```

- [ ] **Step 4: Add module declarations**

In `src/daemon/mod.rs`, add near the other module declarations:

```rust
pub mod session_events;
#[path = "session_events_tests.rs"]
#[cfg(test)]
mod session_events_tests;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test session_event_ -- --nocapture`
Expected: 3 tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/daemon/session_events.rs src/daemon/session_events_tests.rs src/daemon/mod.rs
git commit -m "feat: add SessionEvent enum and aggregated channel types [Midtown !2518]"
```

---

### Task 2: Build SessionEventForwarder

The forwarder is a tokio task that takes a session's per-session stdout/stderr receivers and forwards events into the shared aggregated channel, tagged with the session name.

**Files:**
- Modify: `src/daemon/session_events.rs`
- Modify: `src/daemon/session_events_tests.rs`

- [ ] **Step 1: Write the failing test for stdout forwarding**

Add to `src/daemon/session_events_tests.rs`:

```rust
use crate::daemon::session_events::{self, SessionEvent};
use crate::headless::StreamEvent;
use tokio::sync::mpsc;

#[tokio::test]
async fn forwarder_sends_stdout_events_to_aggregated_channel() {
    let (agg_tx, mut agg_rx) = session_events::channel();
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<StreamEvent>();

    // No stderr for this test
    let (_stderr_tx, stderr_rx) = mpsc::unbounded_channel::<String>();

    session_events::spawn_forwarder(
        "ghost-town".to_string(),
        "slot-1".to_string(),
        stdout_rx,
        stderr_rx,
        agg_tx,
    );

    // Send a stream event through the per-session channel
    let event = StreamEvent::Assistant {
        message: serde_json::Value::String("hello".to_string()),
        session_id: None,
        extra: serde_json::Value::Null,
    };
    stdout_tx.send(event).unwrap();
    drop(stdout_tx); // close channel

    // Should arrive tagged in the aggregated channel
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

    // Close stdout immediately
    drop(stdout_tx);

    // Should get a Stopped event
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

    // Should get a Stderr event
    let received = agg_rx.recv().await.unwrap();
    match received {
        SessionEvent::Stderr { name, line, .. } => {
            assert_eq!(name, "live-wire");
            assert_eq!(line, "error line 1");
        }
        other => panic!("expected Stderr, got {:?}", other),
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test forwarder_sends -- --nocapture 2>&1 | tail -5`
Expected: FAIL — `spawn_forwarder` doesn't exist

- [ ] **Step 3: Implement spawn_forwarder**

Add to `src/daemon/session_events.rs`:

```rust
use tokio::task::JoinHandle;
use tracing::debug;

/// Spawn a forwarder task that reads from per-session stdout/stderr receivers
/// and sends tagged events into the aggregated channel.
///
/// The task runs until both stdout and stderr channels close, then sends
/// a `Stopped` event. Returns the JoinHandle for the spawned task.
pub fn spawn_forwarder(
    name: String,
    slot_id: String,
    mut stdout_rx: mpsc::UnboundedReceiver<StreamEvent>,
    mut stderr_rx: mpsc::UnboundedReceiver<String>,
    agg_tx: mpsc::UnboundedSender<SessionEvent>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Use a select! loop to forward from both stdout and stderr concurrently.
        // When stdout closes, we're done (stderr is a secondary concern).
        loop {
            tokio::select! {
                biased;
                // Prioritize stdout events (main data path)
                event = stdout_rx.recv() => {
                    match event {
                        Some(stream_event) => {
                            if agg_tx.send(SessionEvent::Event {
                                name: name.clone(),
                                slot_id: slot_id.clone(),
                                event: stream_event,
                            }).is_err() {
                                // Aggregated receiver dropped — daemon is shutting down
                                break;
                            }
                        }
                        None => {
                            // stdout closed — session exited
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
                            // stderr closed but stdout still open — keep forwarding stdout only.
                            // This can happen if the stderr pipe fills and the reader exits.
                            debug!("Session '{}' stdout forwarder: stderr closed, continuing stdout", name);
                            // Fall through to stdout-only loop below
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test forwarder_sends -- --nocapture`
Expected: 3 tests PASS

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`
Expected: No warnings

- [ ] **Step 6: Commit**

```bash
git add src/daemon/session_events.rs src/daemon/session_events_tests.rs
git commit -m "feat: add SessionEventForwarder to multiplex per-session channels [Midtown !2518]"
```

---

## Chunk 2: Wire Aggregated Channel into SessionManager

### Task 3: Add aggregated sender to SessionManager

**Files:**
- Modify: `src/daemon/sessions.rs`

- [ ] **Step 1: Add `agg_tx` field to SessionManager**

In `src/daemon/sessions.rs`, add a new field to `SessionManager`:

```rust
pub struct SessionManager {
    sessions: RwLock<HashMap<String, CoworkerSession>>,
    repo_name: String,
    /// Sender for the aggregated session event channel.
    /// Each spawned session gets a forwarder task that feeds into this.
    agg_tx: mpsc::UnboundedSender<super::session_events::SessionEvent>,
    #[cfg(test)]
    test_send_message_to_session_id_hook: std::sync::Mutex<Option<TestSendMessageToSessionIdHook>>,
    #[cfg(test)]
    test_is_alive_hook: std::sync::Mutex<Option<TestIsAliveHook>>,
}
```

- [ ] **Step 2: Update `new()` to accept and store `agg_tx`**

Update the constructor:

```rust
pub fn new(
    repo_name: String,
    agg_tx: mpsc::UnboundedSender<super::session_events::SessionEvent>,
) -> Self {
    Self {
        sessions: RwLock::new(HashMap::new()),
        repo_name,
        agg_tx,
        #[cfg(test)]
        test_send_message_to_session_id_hook: std::sync::Mutex::new(None),
        #[cfg(test)]
        test_is_alive_hook: std::sync::Mutex::new(None),
    }
}
```

- [ ] **Step 3: Fix all call sites of `SessionManager::new()`**

Search for all places `SessionManager::new(` is called and add the `agg_tx` parameter. This includes:
- `src/daemon/mod.rs` (main daemon init) — create the channel here, pass `agg_tx` to `SessionManager::new()`, keep `agg_rx` for the select! loop
- Test files that construct `SessionManager` — create a throwaway channel in each test

Run: `cargo build 2>&1 | head -30` to find all broken call sites

- [ ] **Step 4: Run tests to verify everything compiles**

Run: `cargo test --no-run 2>&1 | tail -10`
Expected: Compiles successfully

- [ ] **Step 5: Commit**

```bash
git add src/daemon/sessions.rs src/daemon/mod.rs src/daemon/sessions_tests.rs
git commit -m "feat: add aggregated event channel to SessionManager [Midtown !2518]"
```

Note: `dispatch_session_tests.rs` and `rpc_session_tests.rs` don't construct `SessionManager` directly, so they don't need changes. Check with `cargo build` to confirm.

---

### Task 4: Hook forwarder into session spawn

When `SessionManager::spawn()` creates a new `HeadlessSession`, take ownership of the per-session `stdout_rx`/`stderr_rx` from the `HeadlessSession` and spawn a forwarder that feeds them into `self.agg_tx`.

**Files:**
- Modify: `src/headless.rs` (add method to take receivers)
- Modify: `src/daemon/sessions.rs` (call forwarder on spawn)

- [ ] **Step 1: Add `take_receivers()` method to HeadlessSession**

In `src/headless.rs`, add:

```rust
/// Take ownership of the stdout and stderr receivers.
///
/// After this call, `next_event()` and `drain_stderr()` will no longer work
/// on this session (the receivers are moved to the forwarder). Returns None
/// if receivers were already taken or not available (e.g., Codex backend).
pub fn take_receivers(
    &mut self,
) -> Option<(
    mpsc::UnboundedReceiver<StreamEvent>,
    mpsc::UnboundedReceiver<String>,
)> {
    let stdout = self.stdout_rx.take()?;
    let stderr = self.stderr_rx.take()?;
    Some((stdout, stderr))
}
```

- [ ] **Step 2: Wire forwarder into `SessionManager::spawn()`**

In `src/daemon/sessions.rs`, in the `spawn()` method, after creating the `CoworkerSession` but before inserting it, take the receivers and spawn the forwarder:

```rust
// After: let mut cs = CoworkerSession::new(...);
// Before: sessions.insert(slot_id.to_string(), cs);

// Take per-session receivers and spawn aggregated forwarder
if let Some((stdout_rx, stderr_rx)) = session.take_receivers() {
    super::session_events::spawn_forwarder(
        name.to_string(),
        slot_id.to_string(),
        stdout_rx,
        stderr_rx,
        self.agg_tx.clone(),
    );
}
```

Wait — `session` has been moved into `CoworkerSession::new()` by this point. We need to take receivers **before** creating `CoworkerSession`. Adjust:

```rust
// Take per-session receivers BEFORE moving session into CoworkerSession
let receivers = session.take_receivers();

let mut cs = CoworkerSession::new(
    slot_id.to_string(),
    name.to_string(),
    session,
    &self.repo_name,
    session_id.clone(),
);

// Spawn aggregated forwarder if receivers are available
if let Some((stdout_rx, stderr_rx)) = receivers {
    super::session_events::spawn_forwarder(
        name.to_string(),
        slot_id.to_string(),
        stdout_rx,
        stderr_rx,
        self.agg_tx.clone(),
    );
}
```

- [ ] **Step 3: Also wire forwarder into `spawn_fork()`**

**Important:** `spawn_fork()` calls `session.next_event()` in an init-event discovery loop (waiting up to 30s for the session_id). `take_receivers()` must be called **after** that loop completes (after the session_id is known), not before. Place the `take_receivers()` call just before inserting the `CoworkerSession` into the sessions map.

- [ ] **Step 4: Also wire forwarder into `resume_session()` / `replace_session()` if they exist**

Search for any other methods that create or replace `HeadlessSession`s in `SessionManager` and apply the same pattern.

Run: `cargo grep 'HeadlessSession::spawn\|session\.take_receivers\|\.session = Some' src/daemon/sessions.rs` to find all sites.

- [ ] **Step 5: Run tests**

Run: `cargo test -- --nocapture 2>&1 | tail -20`
Expected: All tests pass

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`

- [ ] **Step 7: Commit**

```bash
git add src/headless.rs src/daemon/sessions.rs
git commit -m "feat: spawn forwarder on session creation to feed aggregated channel [Midtown !2518]"
```

---

## Chunk 3: Main Event Loop Integration

### Task 5: Add select! branch for aggregated receiver

This is the core change: add a new branch to the main `tokio::select!` loop in `src/daemon/mod.rs` that receives `SessionEvent`s from the aggregated channel.

**Files:**
- Modify: `src/daemon/mod.rs`

- [ ] **Step 1: Store `agg_rx` in the daemon startup**

Where `SessionManager` is created (around line 3253 area), store the receiver:

```rust
let (agg_tx, mut agg_rx) = crate::daemon::session_events::channel();
// Pass agg_tx to SessionManager::new()
```

- [ ] **Step 2: Add select! branch for session events**

Add a new branch to the `tokio::select!` macro, **before** the `session_drain_interval` branch:

```rust
// Receive events from sessions in real-time via the aggregated channel.
// After the first event, drain any buffered events to batch-process them
// (reduces lock contention on persistent_state).
Some(first_event) = agg_rx.recv() => {
    let mut batch = vec![first_event];
    while let Ok(ev) = agg_rx.try_recv() {
        batch.push(ev);
    }
    handle_session_event_batch(batch, &state).await;
}
```

**Performance note:** The `try_recv` drain after the first `recv` batches events that arrived while other select branches ran. This prevents acquiring `persistent_state.lock()` per-event under high throughput.

- [ ] **Step 3: Implement `handle_session_event_batch()` helper**

Process a batch of `SessionEvent`s. Group events by session name, update health flags, run effects:

For `SessionEvent::Event { name, slot_id, event }`:
1. Update `CoworkerSession` health flags (last_event_at, status, session_id, cost, error states, tool state) — same logic currently in the drain loop (lines 1207-1323 of sessions.rs)
2. Log to output file
3. Backfill session_id in persistent state (same as lines 3788-3858 of mod.rs)
4. Process through stream effects (lead_effects, coworker_effects)

For `SessionEvent::Stderr { name, slot_id, line }`:
1. Check for "Tool names must be unique" error
2. Store for crash diagnostics (optional — can buffer in CoworkerSession)

For `SessionEvent::Stopped { name, slot_id }`:
1. Final stderr drain
2. Mark session as stopped
3. Deregister, record stop time, post to channel — same as lines 3930-3988 of mod.rs

**Important:** This is a large refactor. The event processing currently happens inside `drain_events()` (which holds the sessions write lock) and inside the `session_drain_interval` branch of `select!`. We need to:
- Move the health-flag updates to a new `SessionManager::update_session_health()` method
- Move the persistent-state backfill logic out of the drain branch
- Move the effects processing to work with single events (not batched)

This is the most complex step. Break it into sub-steps:

- [ ] **Step 3a: Add `update_session_health()` to SessionManager**

```rust
/// Update a session's health flags based on a received event.
/// Called from the event-driven path (not drain_events).
pub async fn update_session_health(&self, slot_id: &str, event: &StreamEvent) {
    let mut sessions = self.sessions.write().await;
    if let Some(cs) = sessions.get_mut(slot_id) {
        cs.last_event_at = Some(Utc::now());
        // Same match arms as drain_events lines 1210-1321
        match event { ... }
    }
}
```

- [ ] **Step 3b: Add `mark_stopped()` to SessionManager**

```rust
/// Mark a session as stopped (stdout closed).
pub async fn mark_stopped(&self, slot_id: &str) -> Option<(String, Vec<String>)> {
    let mut sessions = self.sessions.write().await;
    if let Some(cs) = sessions.get_mut(slot_id) {
        // drain_stderr_final already happened in the forwarder
        cs.status = SessionStatus::Stopped;
        cs.session = None;
        Some((cs.name.clone(), vec![]))
    } else {
        None
    }
}
```

- [ ] **Step 3c: Add `log_event()` to SessionManager**

```rust
/// Log a single event to the session's output log file.
pub async fn log_event(&self, slot_id: &str, event: &StreamEvent) {
    let sessions = self.sessions.read().await;
    if let Some(cs) = sessions.get(slot_id)
        && cs.output_log.is_some()
    {
        let log_path = cs.output_log_path.clone();
        let json = serde_json::to_string(event).ok();
        drop(sessions);
        if let Some(json) = json {
            tokio::task::spawn_blocking(move || {
                if let Ok(mut file) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                {
                    let _ = writeln!(file, "{}", json);
                    let _ = file.flush();
                }
            });
        }
    }
}
```

- [ ] **Step 3d: Wire the batch handler to call these methods**

```rust
async fn handle_session_event_batch(batch: Vec<SessionEvent>, state: &DaemonState) {
    let mut events_by_name: HashMap<String, Vec<StreamEvent>> = HashMap::new();
    let mut stopped_sessions: Vec<(String, String)> = Vec::new(); // (name, slot_id)

    for session_event in batch {
        match session_event {
            SessionEvent::Event { name, slot_id, event } => {
                debug!(coworker = %name, event = ?event, "session event (realtime)");
                state.session_manager.update_session_health(&slot_id, &event).await;
                state.session_manager.log_event(&slot_id, &event).await;
                events_by_name.entry(name).or_default().push(event);
            }
            SessionEvent::Stderr { name, slot_id, line } => {
                state.session_manager.handle_stderr_line(&slot_id, &line).await;
            }
            SessionEvent::Stopped { name, slot_id } => {
                stopped_sessions.push((name, slot_id));
            }
        }
    }

    // Process effects for the batch (same as existing drain path but with live events)
    if !events_by_name.is_empty() {
        process_session_events_batch(&state, &events_by_name).await;
    }

    // Handle stopped sessions
    for (name, slot_id) in stopped_sessions {
        handle_session_stopped(&state, &name, &slot_id).await;
    }
}
```

- [ ] **Step 4: Implement `process_session_events_batch()`**

This replaces the batched effects processing from the old drain path. Takes the already-grouped `events_by_name` map:

```rust
async fn process_session_events_batch(
    state: &DaemonState,
    events_by_name: &HashMap<String, Vec<StreamEvent>>,
) {
    // Backfill session_id on init events
    let mut needs_persist_save = false;
    for (name, session_events) in events_by_name {
        for event in session_events {
            // ... same backfill logic as current drain branch (lines ~3788-3858 of mod.rs) ...
        }
    }
    if needs_persist_save {
        let ps = state.persistent_state.lock().await;
        if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
            warn!("Failed to save persistent state: {}", e);
        }
    }

    // Process through existing lead/coworker effects (single lock acquisition for batch)
    let (lead_effects, coworker_effects) = {
        let ps = state.persistent_state.lock().await;
        // ... same fork_bound_channels, suppress_auto_output logic as current drain branch ...
        let lead_effects = stream::process_lead_output(events_by_name, ...);
        let coworker_effects = stream::process_agent_output(events_by_name, ...);
        (lead_effects, coworker_effects)
    };
    effects::execute_effects(lead_effects, state).await;
    effects::execute_effects(coworker_effects, state).await;

    // NOTE: Do NOT call collect_health() here — it's done in the periodic
    // health interval (every 5s). Per-batch health updates are unnecessary
    // overhead and the health snapshot is only consumed by decision functions
    // on tick boundaries anyway.
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -- --nocapture 2>&1 | tail -20`
Expected: All tests pass

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`

- [ ] **Step 7: Commit**

```bash
git add src/daemon/mod.rs src/daemon/sessions.rs
git commit -m "feat: add realtime session event select! branch in main loop [Midtown !2518]"
```

---

### Task 6: Repurpose drain interval as health-check-only

**Files:**
- Modify: `src/daemon/mod.rs`
- Modify: `src/daemon/sessions.rs`

- [ ] **Step 1: Rename and repurpose the drain interval**

In `src/daemon/mod.rs`:

```rust
// Before:
// let mut session_drain_interval = interval(std::time::Duration::from_secs(2));

// After:
let mut session_health_interval = interval(std::time::Duration::from_secs(5));
session_health_interval.tick().await;
```

- [ ] **Step 2: Simplify the drain interval branch**

The branch should now only:
1. Check plugin daemon health
2. Collect health snapshot
3. Run `reconcile_process_health()`
4. Handle any stopped sessions found by reconciliation

Remove the `drain_events()` call, event processing, and effects logic from this branch (they're now in the realtime `agg_rx` branch).

```rust
_ = session_health_interval.tick() => {
    // Plugin health
    state.plugin_daemon.check_health().await;
    if state.plugin_daemon.has_plugins() {
        state.plugin_daemon.ensure_running().await;
    }

    // Refresh health snapshot
    let health = state.session_manager.collect_health().await;
    {
        let mut hh = state.headless_health.write().unwrap();
        *hh = health;
    }
    state.headless_health_generation.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    // Defense-in-depth: reconcile process liveness
    let (reconciled, reconciled_stderr) =
        state.session_manager.reconcile_process_health().await;
    for name in reconciled {
        warn!("Process reconciliation found dead session: {}", name);
        handle_session_stopped_by_name(&state, &name).await;
    }
}
```

- [ ] **Step 3: Keep `drain_events()` but simplify it**

`drain_events()` in `sessions.rs` can be simplified to a health-check-only method, or kept as-is for backward compatibility with tests. Since forwarders now own the receivers, `drain_events()` will naturally return empty — `next_event()` will return `None` immediately since `stdout_rx` was taken.

Actually, `drain_events()` needs the receivers to work. Since we've taken them for the forwarder, `drain_events()` as written will panic (`expect("missing claude stdout channel")`). Two options:

**Option A (preferred):** Remove calls to `drain_events()` entirely from the main loop. It's replaced by the realtime path.

**Option B:** Make `drain_events()` gracefully handle missing receivers (return empty if receivers were taken).

Go with Option A — cleaner.

- [ ] **Step 4: Run all tests**

Run: `cargo test -- --nocapture 2>&1 | tail -20`
Expected: All pass

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`

- [ ] **Step 6: Commit**

```bash
git add src/daemon/mod.rs src/daemon/sessions.rs
git commit -m "refactor: repurpose session drain interval as health-check-only fallback [Midtown !2518]"
```

---

## Chunk 4: Cleanup and Verification

### Task 7: Handle edge cases and verify

**Files:**
- Modify: `src/daemon/sessions.rs` (handle missing receivers gracefully)
- Modify: `src/daemon/session_events_tests.rs` (add edge case tests)

- [ ] **Step 1: Write test for forwarder with interleaved stdout/stderr**

```rust
#[tokio::test]
async fn forwarder_interleaves_stdout_and_stderr() {
    let (agg_tx, mut agg_rx) = session_events::channel();
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel();
    let (stderr_tx, stderr_rx) = mpsc::unbounded_channel();

    session_events::spawn_forwarder(
        "test".to_string(), "slot-1".to_string(),
        stdout_rx, stderr_rx, agg_tx,
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
            SessionEvent::Stopped { .. } => { stopped = true; break; }
        }
    }
    assert_eq!(event_count, 2);
    assert!(stderr_count >= 1);
    assert!(stopped);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test forwarder_interleaves -- --nocapture`
Expected: PASS

- [ ] **Step 3: Ensure `next_event()` handles taken receivers**

In `src/headless.rs`, the `next_claude_event()` currently does `self.stdout_rx.as_mut().expect(...)`. After receivers are taken, this would panic. Update to return `None`:

```rust
async fn next_claude_event(&mut self) -> Option<StreamEvent> {
    let rx = self.stdout_rx.as_mut()?; // was: .expect("missing claude stdout channel")
    // ... rest unchanged
}
```

Similarly for `drain_stderr()`:
```rust
// If receivers were taken by forwarder, return empty
let rx = match session.stderr_rx.as_mut() {
    Some(rx) => rx,
    None => return Vec::new(),
};
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: All pass

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings 2>&1 | tail -10`

- [ ] **Step 6: Commit**

```bash
git add src/headless.rs src/daemon/sessions.rs src/daemon/session_events_tests.rs
git commit -m "fix: handle taken receivers gracefully, add interleaving test [Midtown !2518]"
```

---

### Task 8: Coverage check and final verification

**Files:**
- All modified files

- [ ] **Step 1: Run coverage diff**

Run: `./scripts/coverage-diff.sh 2>&1 | tail -30`
Review: Check coverage for new code in `session_events.rs`, changes in `sessions.rs` and `mod.rs`

- [ ] **Step 2: Add any missing test coverage**

If coverage shows uncovered paths (error cases, edge cases), add targeted tests.

- [ ] **Step 3: Run E2E tests locally**

Run: `midtown e2e run coordination`
Expected: Pass

- [ ] **Step 4: Final clippy + fmt check**

Run: `cargo clippy --all-targets --all-features -- -D warnings && cargo fmt --all -- --check`
Expected: Clean

- [ ] **Step 5: Commit any remaining test coverage**

```bash
git add -A
git commit -m "test: add coverage for session event forwarding edge cases [Midtown !2518]"
```

- [ ] **Step 6: Open PR**

```bash
gh pr create --title "feat: per-session async stdout readers with mpsc channels" --body "$(cat <<'EOF'
<!-- midtown session:251697cc-705e-4a43-ad45-2cf1ddb3ecb5 -->

## Summary
- Adds `SessionEvent` enum and aggregated `mpsc` channel for real-time session event delivery
- Spawns per-session forwarder tasks that multiplex stdout/stderr into a single receiver
- Main event loop `select!`s on the aggregated channel for near-zero latency event processing
- Repurposes the 2-second drain interval as a 5-second health-check-only fallback

Closes !2518

## Test plan
- [ ] Unit tests for SessionEvent, forwarder (stdout, stderr, stopped, interleaving)
- [ ] Existing session tests still pass
- [ ] Coordination E2E tests pass
- [ ] Coverage diff shows reasonable coverage for new code

🌃 Co-built with [Midtown](https://github.com/btucker/midtown)
EOF
)"
```
