# Daemon v2 Phase 6: Lead-Driven Workflows, Web API, and Cutover Prep

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add lead-driven workflow support (skip auto-dispatch/review for lead-driven channels), add a web API layer (Axum HTTP + WebSocket), and prepare for cutover.

**Architecture:** ChannelSettings gets a `lead_driven: bool` flag. Dispatch and PR decisions check it before spawning. The web API is an Axum router that proxies HTTP requests to the same RPC dispatch. WebSocket broadcasts DomainEvents for real-time UI updates.

**Depends on:** Phase 5

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `src/daemon_v2/projections/channels.rs` | Modify | Add lead_driven to ChannelSettings |
| `src/daemon_v2/projections/channels_tests.rs` | Modify | Test lead_driven |
| `src/daemon_v2/decisions/dispatch.rs` | Modify | Skip lead-driven channels |
| `src/daemon_v2/decisions/dispatch_tests.rs` | Modify | Test lead-driven skip |
| `src/daemon_v2/decisions/prs.rs` | Modify | Skip PR review for lead-driven |
| `src/daemon_v2/rpc/handlers.rs` | Modify | Add channel.update for lead_driven toggle |
| `src/daemon_v2/rpc/mod.rs` | Modify | Route channel.update |
| `src/daemon_v2/events/mod.rs` | Modify | Add ChannelLeadDrivenSet event |
| `src/daemon_v2/web/mod.rs` | Create | Axum router |
| `src/daemon_v2/web/routes.rs` | Create | HTTP → RPC translation |
| `src/daemon_v2/web/websocket.rs` | Create | Event → WebSocket broadcast |
| `tests/daemon_v2_e2e.rs` | Modify | E2E tests for lead-driven + web API |

---

### Task 1: Lead-driven flag in channels and dispatch

**Files:**
- Modify: `src/daemon_v2/projections/channels.rs`
- Modify: `src/daemon_v2/events/mod.rs`
- Modify: `src/daemon_v2/decisions/dispatch.rs`
- Modify: `src/daemon_v2/decisions/dispatch_tests.rs`

- [ ] **Step 1: Add ChannelLeadDrivenSet event**

In `src/daemon_v2/events/mod.rs`, add to DomainEvent:

```rust
ChannelLeadDrivenSet {
    channel: String,
    lead_driven: bool,
},
```

- [ ] **Step 2: Add lead_driven to ChannelSettings**

In `src/daemon_v2/projections/channels.rs`:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChannelSettings {
    pub show_full_lead_output: bool,
    pub lead_driven: bool,
}
```

Handle ChannelLeadDrivenSet in ChannelIndex::apply():

```rust
DomainEvent::ChannelLeadDrivenSet { channel, lead_driven } => {
    let meta = self.channels.entry(channel.clone()).or_insert_with(|| ChannelMeta {
        name: channel.clone(),
        archived: false,
        settings: ChannelSettings::default(),
        workflow: None,
        thread_count: 0,
        last_message_at: None,
        known_threads: HashSet::new(),
    });
    meta.settings.lead_driven = *lead_driven;
}
```

Add helper method on ChannelIndex:

```rust
pub fn is_lead_driven(&self, channel: &str) -> bool {
    self.channels.get(channel)
        .map_or(false, |m| m.settings.lead_driven)
}
```

- [ ] **Step 3: Skip lead-driven channels in dispatch**

In `src/daemon_v2/decisions/dispatch.rs`, update `dispatch_pending_tasks()`:

```rust
pub fn dispatch_pending_tasks(proj: &Projections, max_in_progress: usize) -> Vec<Command> {
    // ... existing limit check ...

    pending
        .into_iter()
        .take(slots)
        .filter_map(|task_id| {
            let task = proj.work.tasks.get(task_id)?;
            // Skip tasks in lead-driven channels
            if proj.channels.is_lead_driven(&task.channel) {
                return None;
            }
            Some(Command::SpawnAgent(/* ... */))
        })
        .collect()
}
```

- [ ] **Step 4: Add test for lead-driven skip**

In dispatch_tests.rs:

```rust
#[test]
fn skips_lead_driven_channel_tasks() {
    let mut proj = Projections::default();
    proj.apply(&DomainEvent::ChannelLeadDrivenSet {
        channel: "manual".into(), lead_driven: true,
    });
    proj.apply(&DomainEvent::TaskCreated {
        id: "t1".into(), subject: "Manual task".into(),
        channel: "manual".into(), blocked_by: vec![],
    });

    let commands = dispatch::dispatch_pending_tasks(&proj, 5);
    assert!(commands.is_empty(), "lead-driven channel tasks should not be auto-dispatched");
}
```

- [ ] **Step 5: Add channel.update RPC**

In handlers.rs, add `handle_channel_update()` that accepts `lead_driven` parameter and returns `ChannelLeadDrivenSet` event. Route as "channel.update" in mod.rs.

- [ ] **Step 6: Test and commit**

Run: `cargo test --lib daemon_v2`

```bash
git commit -m "feat(daemon-v2): add lead-driven workflow support to channels and dispatch"
```

---

### Task 2: Web API — Axum HTTP router

**Files:**
- Create: `src/daemon_v2/web/mod.rs`
- Create: `src/daemon_v2/web/routes.rs`
- Create: `src/daemon_v2/web/websocket.rs`
- Modify: `src/daemon_v2/daemon.rs`
- Modify: `src/daemon_v2/mod.rs`

- [ ] **Step 1: Create web/routes.rs — HTTP → RPC proxy**

Each HTTP endpoint translates to an RPC call:

```rust
use axum::{extract::State, http::StatusCode, Json};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::daemon_v2::daemon::DaemonV2Shared;

pub async fn handle_status(State(state): State<Arc<DaemonV2Shared>>) -> Json<Value> {
    let proj = state.projections.lock().await;
    let (response, _) = crate::daemon_v2::rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "status", "id": 1}),
        &proj,
        &state.channels_dir,
    );
    Json(response["result"].clone())
}

pub async fn handle_agent_list(State(state): State<Arc<DaemonV2Shared>>) -> Json<Value> {
    let proj = state.projections.lock().await;
    let (response, _) = crate::daemon_v2::rpc::dispatch_request(
        json!({"jsonrpc": "2.0", "method": "agent.list", "id": 1}),
        &proj,
        &state.channels_dir,
    );
    Json(response["result"].clone())
}

// Similar for channels, tasks, etc.
```

- [ ] **Step 2: Create web/websocket.rs — event broadcast**

```rust
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use std::sync::Arc;
use tokio::sync::broadcast;

use crate::daemon_v2::events::DomainEvent;

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    State(tx): State<Arc<broadcast::Sender<DomainEvent>>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, tx))
}

async fn handle_socket(mut socket: WebSocket, tx: Arc<broadcast::Sender<DomainEvent>>) {
    let mut rx = tx.subscribe();
    while let Ok(event) = rx.recv().await {
        let json = serde_json::to_string(&event).unwrap_or_default();
        if socket.send(Message::Text(json.into())).await.is_err() {
            break;
        }
    }
}
```

- [ ] **Step 3: Create web/mod.rs — Axum router**

```rust
pub mod routes;
pub mod websocket;

use axum::{routing::get, Router};
use std::sync::Arc;

use crate::daemon_v2::daemon::DaemonV2Shared;

pub fn create_router(shared: Arc<DaemonV2Shared>) -> Router {
    Router::new()
        .route("/api/status", get(routes::handle_status))
        .route("/api/agents", get(routes::handle_agent_list))
        .route("/api/ws", get(websocket::ws_handler))
        .with_state(shared)
}
```

- [ ] **Step 4: Add DaemonV2Shared and web server to daemon.rs**

Extract shared state into `DaemonV2Shared` (Arc<Mutex<Projections>>, channels_dir, broadcast sender). Start Axum on a configurable port alongside the Unix socket.

- [ ] **Step 5: Add web_port to DaemonV2Config**

Add `web_port: Option<u16>` to config. If set, start the Axum server.

- [ ] **Step 6: Test and commit**

```bash
cargo build
git commit -m "feat(daemon-v2): add Axum web API with HTTP routes and WebSocket"
```

---

### Task 3: E2E tests for lead-driven and web API

**Files:**
- Modify: `tests/daemon_v2_e2e.rs`

- [ ] **Step 1: Add lead-driven E2E test**

```rust
#[test]
#[ignore]
fn test_daemon_v2_lead_driven_skips_auto_dispatch() {
    let harness = V2Harness::start();

    // Set channel to lead-driven
    let resp = harness.rpc_call("channel.update", Some(serde_json::json!({
        "channel": "manual-chan",
        "lead_driven": true,
    })));
    assert!(resp["error"].is_null(), "channel.update error: {resp}");

    // Create a task in the lead-driven channel
    let resp = harness.rpc_call("task.create", Some(serde_json::json!({
        "id": "manual-t1",
        "subject": "Manual task",
        "channel": "manual-chan",
    })));
    assert!(resp["error"].is_null());

    // Wait for dispatch cycle (5s)
    std::thread::sleep(Duration::from_secs(8));

    // Verify no new worker was spawned for this task
    let resp = harness.rpc_call("agent.list", Some(serde_json::json!({
        "kind": "worker",
    })));
    let agents = resp["result"].as_array().unwrap();
    let manual_workers: Vec<_> = agents.iter()
        .filter(|a| a["task_id"] == "manual-t1")
        .collect();
    assert!(manual_workers.is_empty(),
        "lead-driven task should not auto-dispatch: {agents:?}");
}
```

- [ ] **Step 2: Run ALL E2E tests**

```bash
cargo build && cargo test --test daemon_v2_e2e -- --ignored --test-threads=1
```

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(daemon-v2): add E2E tests for lead-driven workflows"
```

---

## Summary

After Phase 6:
- **Lead-driven workflows** — channels with `lead_driven: true` skip auto-dispatch and auto-review
- **channel.update RPC** — toggle lead_driven via RPC
- **Web API** — Axum HTTP routes proxying to RPC, WebSocket for real-time event broadcast
- **E2E verified** — lead-driven skip confirmed with real daemon
