# Sidebar Activity Feed & Read State Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the three-section sidebar (Needs Attention / Tasks / Channels+threads) with a two-section model (Activity feed / Channels), add server-synced read state for threads and channels, and remove the `openThreads` infrastructure.

**Architecture:** A single `ActivityFeed.svelte` component renders a unified sorted list (attention items → tasks → recent threads → collapsed older threads). Read state is persisted server-side per user via new `read_state` daemon RPC endpoints. Thread visibility is derived from activity timestamps rather than an explicit set. The `openThreads` daemon state, RPC, REST, and WebSocket infrastructure is removed.

**Tech Stack:** Svelte 5, TypeScript, Tailwind CSS 4, Rust (daemon RPC + persistent state)

**Spec:** `docs/superpowers/specs/2026-03-27-sidebar-activity-feed-design.md`

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `web-app/src/lib/ActivityFeed.svelte` | Unified activity list: attention items, tasks, recent threads, collapsed older threads |
| `src/daemon/rpc_read_state.rs` | RPC handlers for `read_state.get` and `read_state.mark_read` |
| `src/daemon/rpc_read_state_tests.rs` | Tests for read state RPC handlers |

### Modified Files
| File | Changes |
|------|---------|
| `src/daemon/state.rs` | Add `ReadState` struct and `read_state` field, remove `open_threads` field |
| `src/daemon/rpc.rs` | Add `read_state.*` dispatch entries, remove `channel.open_threads.*` entries |
| `src/daemon/mod.rs` | Add `mod rpc_read_state`, remove `mod rpc_open_threads` |
| `src/web.rs` | Add `ReadStateChanged` WebUpdate, add REST routes, remove `OpenThreadsChanged` and open-threads routes |
| `web-app/src/lib/store.ts` | Add `threadReadState`, remove `openThreads`, `dismissedAttentionItems`, `threadUnreadCounts` |
| `web-app/src/lib/api.ts` | Add read state fetch/mark/WS handling, remove openThreads functions and auto-close timer |
| `web-app/src/lib/needsAttention.ts` | Update `computeAttentionItems` signature: replace `openThreads`/`dismissed` with `trackedThreads`/`readState` |
| `web-app/src/lib/needsAttention.test.ts` | Update tests to match new signature |
| `web-app/src/lib/ChannelList.svelte` | Replace `NeedsAttention`+`TasksSidebar` with `ActivityFeed`, remove thread rendering and openThreads usage |
| `web-app/src/lib/types.ts` | Add `ReadState` type, remove dismiss-related fields from `NeedsAttentionItem` |

### Deleted Files
| File | Reason |
|------|--------|
| `web-app/src/lib/NeedsAttention.svelte` | Merged into ActivityFeed |
| `web-app/src/lib/TasksSidebar.svelte` | Merged into ActivityFeed |
| `src/daemon/rpc_open_threads.rs` | Replaced by rpc_read_state |
| `src/daemon/rpc_open_threads_tests.rs` | Replaced by rpc_read_state_tests |

---

## Task 1: Daemon — ReadState persistent state

**Files:**
- Modify: `src/daemon/state.rs`
- Create: `src/daemon/rpc_read_state_tests.rs`
- Modify: `src/daemon/mod.rs`

- [ ] **Step 1: Write failing tests for ReadState persistence**

Create `src/daemon/rpc_read_state_tests.rs`:

```rust
//! Tests for read state RPC handlers and persistent state.

use crate::daemon::state::{DaemonPersistentState, ReadState};
use std::collections::HashMap;

#[test]
fn read_state_default_empty() {
    let ps = DaemonPersistentState::default();
    assert!(ps.read_state.is_empty());
}

#[test]
fn read_state_struct_default_empty() {
    let rs = ReadState::default();
    assert!(rs.threads.is_empty());
    assert!(rs.channels.is_empty());
}

#[test]
fn read_state_roundtrip_serde() {
    let mut ps = DaemonPersistentState::default();
    let mut rs = ReadState::default();
    rs.threads.insert("thread-1".to_string(), "2026-03-27T10:00:00Z".to_string());
    rs.channels.insert("auth-refactor".to_string(), "2026-03-27T09:00:00Z".to_string());
    ps.read_state.insert("default".to_string(), rs);

    let json = serde_json::to_string(&ps).unwrap();
    let ps2: DaemonPersistentState = serde_json::from_str(&json).unwrap();

    let rs2 = ps2.read_state.get("default").unwrap();
    assert_eq!(rs2.threads.get("thread-1").unwrap(), "2026-03-27T10:00:00Z");
    assert_eq!(rs2.channels.get("auth-refactor").unwrap(), "2026-03-27T09:00:00Z");
}

#[test]
fn read_state_deserialize_missing_field() {
    let json = r#"{}"#;
    let ps: DaemonPersistentState = serde_json::from_str(json).unwrap();
    assert!(ps.read_state.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -- read_state`
Expected: FAIL — `ReadState` and `read_state` field don't exist

- [ ] **Step 3: Add ReadState struct and field to DaemonPersistentState**

In `src/daemon/state.rs`, add the struct (near other state structs):

```rust
/// Per-user read state for threads and channels.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadState {
    /// Thread read timestamps. Maps thread_id → ISO last_read timestamp.
    #[serde(default)]
    pub threads: HashMap<String, String>,
    /// Channel read timestamps. Maps channel_name → ISO last_read timestamp.
    #[serde(default)]
    pub channels: HashMap<String, String>,
}
```

Add the field to `DaemonPersistentState` (after `open_threads`):

```rust
    /// Per-user read state for threads and channels.
    /// Maps user_id → ReadState. Uses "default" for single-user;
    /// multi-user support is a key change, not a schema change.
    #[serde(default)]
    pub read_state: HashMap<String, ReadState>,
```

In `src/daemon/mod.rs`, add module declarations:

```rust
mod rpc_read_state;
#[path = "rpc_read_state_tests.rs"]
#[cfg(test)]
mod rpc_read_state_tests;
```

Create an empty stub `src/daemon/rpc_read_state.rs` so the mod compiles.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -- read_state`
Expected: PASS (all 4 tests)

- [ ] **Step 5: Commit**

```bash
git add src/daemon/state.rs src/daemon/mod.rs src/daemon/rpc_read_state.rs src/daemon/rpc_read_state_tests.rs
git commit -m "feat: add ReadState struct and read_state field to DaemonPersistentState"
```

---

## Task 2: Daemon — ReadState RPC handlers + WebUpdate

**Files:**
- Modify: `src/daemon/rpc_read_state.rs`
- Modify: `src/daemon/rpc.rs`
- Modify: `src/web.rs`
- Extend: `src/daemon/rpc_read_state_tests.rs`

- [ ] **Step 1: Write failing tests for RPC handlers**

Add to `src/daemon/rpc_read_state_tests.rs`. Use the same test fixture pattern from `rpc_open_threads_tests.rs` (the `make_test_state` / `make_test_state_with_web_tx` functions). Copy the fixture functions, adapting the repo names.

```rust
// Add these tests after the persistent state tests:

#[tokio::test]
async fn test_read_state_get_empty_returns_empty() {
    let (state, _temp_dir, _guard) = make_test_state("test-repo-rs-get");
    let response = handle_read_state_get(1_i64.into(), &state).await;
    assert!(response.error.is_none());
    let result = response.result.unwrap();
    assert!(result.get("threads").unwrap().as_object().unwrap().is_empty());
    assert!(result.get("channels").unwrap().as_object().unwrap().is_empty());
}

#[tokio::test]
async fn test_mark_thread_read_then_get() {
    let (state, _temp_dir, _guard) = make_test_state("test-repo-rs-mark");
    let ts = "2026-03-27T10:00:00Z";

    let mark_resp = handle_read_state_mark_read(
        1_i64.into(), "thread", "thread-123", ts, &state,
    ).await;
    assert!(mark_resp.error.is_none());

    let get_resp = handle_read_state_get(2_i64.into(), &state).await;
    let result = get_resp.result.unwrap();
    let threads = result.get("threads").unwrap().as_object().unwrap();
    assert_eq!(threads.get("thread-123").unwrap().as_str().unwrap(), ts);
}

#[tokio::test]
async fn test_mark_channel_read_then_get() {
    let (state, _temp_dir, _guard) = make_test_state("test-repo-rs-chan");
    let ts = "2026-03-27T12:00:00Z";

    handle_read_state_mark_read(1_i64.into(), "channel", "auth-refactor", ts, &state).await;

    let get_resp = handle_read_state_get(2_i64.into(), &state).await;
    let result = get_resp.result.unwrap();
    let channels = result.get("channels").unwrap().as_object().unwrap();
    assert_eq!(channels.get("auth-refactor").unwrap().as_str().unwrap(), ts);
}

#[tokio::test]
async fn test_mark_read_broadcasts_web_update() {
    let (updates_tx, mut rx) = crate::web::create_updates_channel();
    let (state, _temp_dir, _guard) = make_test_state_with_web_tx("test-repo-rs-bc", Some(updates_tx));

    handle_read_state_mark_read(
        1_i64.into(), "thread", "t1", "2026-03-27T10:00:00Z", &state,
    ).await;

    let update = rx.try_recv().expect("should have broadcast");
    match update {
        crate::web::WebUpdate::ReadStateChanged(data) => {
            assert_eq!(data.item_type, "thread");
            assert_eq!(data.id, "t1");
            assert_eq!(data.timestamp, "2026-03-27T10:00:00Z");
        }
        _ => panic!("wrong update type"),
    }
}

#[tokio::test]
async fn test_mark_read_invalid_type_returns_error() {
    let (state, _temp_dir, _guard) = make_test_state("test-repo-rs-invalid");
    let resp = handle_read_state_mark_read(
        1_i64.into(), "invalid", "foo", "2026-03-27T10:00:00Z", &state,
    ).await;
    assert!(resp.error.is_some());
}
```

- [ ] **Step 2: Implement RPC handlers**

In `src/daemon/rpc_read_state.rs`:

```rust
//! RPC handlers for per-user read state (threads and channels).
//!
//! Read state tracks when a user last read a thread or channel,
//! enabling unread indicators that sync across devices.

use tracing::error;

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

const DEFAULT_USER: &str = "default";

/// Handle `read_state.get` — returns all read timestamps for current user.
pub(super) async fn handle_read_state_get(
    id: RequestId,
    state: &DaemonState,
) -> Response {
    let ps = state.persistent_state.lock().await;
    let read_state = ps.read_state.get(DEFAULT_USER);

    let threads = read_state
        .map(|rs| &rs.threads)
        .cloned()
        .unwrap_or_default();
    let channels = read_state
        .map(|rs| &rs.channels)
        .cloned()
        .unwrap_or_default();

    Response::success(
        id,
        serde_json::json!({ "threads": threads, "channels": channels }),
    )
}

/// Handle `read_state.mark_read` — marks a thread or channel as read.
pub(super) async fn handle_read_state_mark_read(
    id: RequestId,
    item_type: &str,
    item_id: &str,
    timestamp: &str,
    state: &DaemonState,
) -> Response {
    if item_type != "thread" && item_type != "channel" {
        return Response::error(
            id,
            RpcError::new(-32602, format!("type must be 'thread' or 'channel', got '{item_type}'")),
        );
    }

    let mut ps = state.persistent_state.lock().await;
    let read_state = ps
        .read_state
        .entry(DEFAULT_USER.to_string())
        .or_default();

    match item_type {
        "thread" => {
            read_state.threads.insert(item_id.to_string(), timestamp.to_string());
        }
        "channel" => {
            read_state.channels.insert(item_id.to_string(), timestamp.to_string());
        }
        _ => unreachable!(),
    }

    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
        error!("Failed to save daemon-state.json: {}", e);
        return Response::error(id, RpcError::new(-32603, format!("failed to persist: {e}")));
    }
    drop(ps);

    state.broadcast_web_update(crate::web::WebUpdate::ReadStateChanged(
        crate::web::ReadStateChangedData {
            item_type: item_type.to_string(),
            id: item_id.to_string(),
            timestamp: timestamp.to_string(),
        },
    ));

    Response::success(id, serde_json::json!({ "ok": true }))
}
```

- [ ] **Step 3: Add ReadStateChanged WebUpdate variant**

In `src/web.rs`, add to the `WebUpdate` enum:

```rust
    /// Read state changed for a thread or channel
    #[serde(rename = "read_state_changed")]
    ReadStateChanged(ReadStateChangedData),
```

Add the data struct:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct ReadStateChangedData {
    #[serde(rename = "type")]
    pub item_type: String,
    pub id: String,
    pub timestamp: String,
}
```

- [ ] **Step 4: Add dispatch entries in rpc.rs**

In `src/daemon/rpc.rs`, add after the `channel.open_threads` entries (which will be removed in Task 3):

```rust
        "read_state.get" => {
            super::rpc_read_state::handle_read_state_get(request.id, state).await
        }

        "read_state.mark_read" => {
            let item_type = require_str!(params, "type", request.id);
            let id = require_str!(params, "id", request.id);
            let timestamp = require_str!(params, "timestamp", request.id);
            super::rpc_read_state::handle_read_state_mark_read(
                request.id, item_type, id, timestamp, state,
            ).await
        }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -- read_state && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add src/daemon/rpc_read_state.rs src/daemon/rpc_read_state_tests.rs src/daemon/rpc.rs src/web.rs
git commit -m "feat: add read_state RPC endpoints and WebSocket broadcast"
```

---

## Task 3: Daemon — REST endpoints + Remove openThreads

**Files:**
- Modify: `src/web.rs` (add read-state REST routes, remove open-threads routes)
- Modify: `src/daemon/rpc.rs` (remove `channel.open_threads.*` dispatch)
- Modify: `src/daemon/state.rs` (remove `open_threads` field)
- Modify: `src/daemon/mod.rs` (remove `mod rpc_open_threads`)
- Delete: `src/daemon/rpc_open_threads.rs`
- Delete: `src/daemon/rpc_open_threads_tests.rs`

- [ ] **Step 1: Add REST endpoints for read-state**

In `src/web.rs`, add routes following the existing pattern (look at `api_channel_open_threads_get` as a template for how REST handlers proxy to RPC):

```
GET  /api/read-state           → calls read_state.get RPC
PUT  /api/read-state/:type/:id → calls read_state.mark_read RPC
```

Add the route in the router:

```rust
        .route("/api/read-state", get(api_read_state_get))
        .route("/api/read-state/{type}/{id}", put(api_read_state_mark_read))
```

Implement `api_read_state_get` and `api_read_state_mark_read` following the same spawn_blocking + RPC-forwarding pattern as existing REST handlers.

- [ ] **Step 2: Remove openThreads infrastructure**

1. In `src/web.rs`: remove the `/api/channels/{channel}/open-threads` route, `api_channel_open_threads_get`, `api_channel_open_threads_set` functions, `OpenThreadsChanged` variant from `WebUpdate`, and `OpenThreadsChangedData` struct.
2. In `src/daemon/rpc.rs`: remove `"channel.open_threads"` and `"channel.open_threads.set"` match arms.
3. In `src/daemon/state.rs`: remove `pub open_threads: HashMap<String, HashSet<String>>` field from `DaemonPersistentState`.
4. In `src/daemon/mod.rs`: remove `mod rpc_open_threads;` and the test module declaration.
5. Delete `src/daemon/rpc_open_threads.rs` and `src/daemon/rpc_open_threads_tests.rs`.

- [ ] **Step 3: Run full Rust test suite**

Run: `cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS. If tests in `rpc_open_threads_tests` were being run, they should now be gone.

- [ ] **Step 4: Commit**

```bash
git add -u src/daemon/ src/web.rs
git commit -m "feat: add read-state REST endpoints, remove openThreads infrastructure"
```

---

## Task 4: Frontend — Types and store changes

**Files:**
- Modify: `web-app/src/lib/types.ts`
- Modify: `web-app/src/lib/store.ts`

- [ ] **Step 1: Add ReadState type, clean up NeedsAttentionItem**

In `web-app/src/lib/types.ts`, add:

```typescript
// ── Read State ──────────────────────────────────────────────────────────────

export interface ReadState {
	threads: Record<string, string>; // thread_id → ISO last_read timestamp
	channels: Record<string, string>; // channel_name → ISO last_read timestamp
}
```

- [ ] **Step 2: Update stores**

In `web-app/src/lib/store.ts`:

1. Remove `openThreads` store and its import in api.ts
2. Remove `dismissedAttentionItems` store and its localStorage subscription
3. Remove `threadUnreadCounts` store and its localStorage subscription
4. Add `threadReadState` store:

```typescript
// ── Read state (server-synced) ──────────────────────────────────────────────
// Per-thread and per-channel read timestamps. Synced from daemon API.
export const threadReadState = writable<Record<string, string>>({});
export const channelReadState = writable<Record<string, string>>({});
```

- [ ] **Step 3: Commit**

```bash
git add web-app/src/lib/types.ts web-app/src/lib/store.ts
git commit -m "feat: add read state stores, remove openThreads/dismissedAttentionItems/threadUnreadCounts"
```

---

## Task 5: Frontend — API integration for read state

**Files:**
- Modify: `web-app/src/lib/api.ts`

- [ ] **Step 1: Add read state API functions**

```typescript
export async function fetchReadState(): Promise<void> {
	try {
		const res = await fetch(`${getApiBase()}/read-state`);
		if (res.ok) {
			const data = await res.json();
			threadReadState.set(data.threads || {});
			channelReadState.set(data.channels || {});
		}
	} catch (err) {
		console.warn("Failed to fetch read state:", err);
	}
}

export async function markRead(type: "thread" | "channel", id: string): Promise<void> {
	const timestamp = new Date().toISOString();
	// Optimistic update
	if (type === "thread") {
		threadReadState.update((s) => ({ ...s, [id]: timestamp }));
	} else {
		channelReadState.update((s) => ({ ...s, [id]: timestamp }));
	}
	try {
		await fetch(`${getApiBase()}/read-state/${type}/${encodeURIComponent(id)}`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ timestamp }),
		});
	} catch (err) {
		console.warn("Failed to mark read:", err);
	}
}
```

- [ ] **Step 2: Handle ReadStateChanged WebSocket message**

In the WebSocket `handleUpdate` switch, add:

```typescript
case "read_state_changed": {
	const { type, id, timestamp } = update.data;
	if (type === "thread") {
		threadReadState.update((s) => ({ ...s, [id]: timestamp }));
	} else if (type === "channel") {
		channelReadState.update((s) => ({ ...s, [id]: timestamp }));
	}
	break;
}
```

- [ ] **Step 3: Load read state on connect**

In the WebSocket connect / initialization flow, call `fetchReadState()` after `fetchChannels()`.

- [ ] **Step 4: Mark channel as read on channel switch**

In the existing `selectChannel` / channel switch flow (or wherever `fetchHistory` is called), add `markRead("channel", channelName)`.

- [ ] **Step 5: Mark thread as read on thread open**

In the `openThread` function, add `markRead("thread", threadId)`.

- [ ] **Step 6: Remove openThreads API functions and auto-close timer**

Remove:
- `fetchOpenThreads` function
- `setOpenThreads` function
- The `setInterval` auto-close timer (TWELVE_HOURS_MS / AUTO_CLOSE_INTERVAL_MS)
- The `open_threads_changed` WebSocket handler
- The `openThreads` import from store
- The openThreads loading loop inside `fetchChannels`

- [ ] **Step 7: Commit**

```bash
git add web-app/src/lib/api.ts
git commit -m "feat: add read state API, remove openThreads API and auto-close timer"
```

---

## Task 6: Frontend — Update needsAttention.ts

**Files:**
- Modify: `web-app/src/lib/needsAttention.ts`
- Modify: `web-app/src/lib/needsAttention.test.ts`

- [ ] **Step 1: Update computeAttentionItems signature**

Replace the `openThreads` and `dismissed` parameters with `readState`:

```typescript
export function computeAttentionItems(opts: {
	trackedThreads: Record<string, TrackedThread>;
	lastMessages: Record<string, LastMessage>;
	coworkers: Coworker[];
	tasks: Task[];
	progressTimestamps: Record<string, number>;
	threadReadState: Record<string, string>;
	userSender: string;
	mainChannel: string;
	now?: number;
}): NeedsAttentionItem[] {
```

- [ ] **Step 2: Update thread iteration logic**

Instead of iterating `openThreads` entries, iterate all `trackedThreads`. Remove the `dismissed.has(id)` check (no more dismiss). A thread needs attention if the heuristic says so and the thread is unread (lastActivity > readState timestamp).

```typescript
	// 1. Threads needing attention
	for (const [threadId, tracked] of Object.entries(opts.trackedThreads)) {
		const lastMsg = opts.lastMessages[threadId];
		if (!lastMsg) continue;

		// Skip if thread is read (user has seen it since last message)
		const lastRead = opts.threadReadState[threadId];
		if (lastRead && new Date(lastRead) >= new Date(lastMsg.timestamp)) continue;

		if (threadNeedsAttention(lastMsg, opts.userSender, now)) {
			const ageMs = now - new Date(lastMsg.timestamp).getTime();
			const agoText = formatAgo(ageMs);

			items.push({
				id: `thread:${threadId}`,
				type: lastMsg.content.includes(`@${opts.userSender}`) ? "mention" : "thread_waiting",
				title: tracked.subject,
				context: `${lastMsg.sender} replied ${agoText} · waiting on you`,
				channel: tracked.channelName,
				threadId,
				timestamp: new Date(lastMsg.timestamp).getTime(),
				workerName: lastMsg.sender,
				workerColor: getSenderColor(lastMsg.sender, null),
			});
		}
	}
```

- [ ] **Step 3: Remove dismissed check from completed tasks and stale tasks**

In the completed tasks and stale tasks sections, remove the `if (opts.dismissed.has(id)) continue;` lines. These items are self-resolving — they disappear when the task status changes or progress resumes.

- [ ] **Step 4: Update tests**

In `needsAttention.test.ts`, update any tests that use the old `openThreads` or `dismissed` parameters to use `threadReadState` instead. If `computeAttentionItems` tests exist, update their call signatures.

- [ ] **Step 5: Run tests**

Run: `cd web-app && npx vitest run src/lib/needsAttention.test.ts`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add web-app/src/lib/needsAttention.ts web-app/src/lib/needsAttention.test.ts
git commit -m "feat: update attention heuristics to use readState, remove dismiss logic"
```

---

## Task 7: Frontend — ActivityFeed.svelte component

**Files:**
- Create: `web-app/src/lib/ActivityFeed.svelte`

- [ ] **Step 1: Create the unified activity feed component**

This component subscribes to `trackedThreads`, `threadReadState`, `channelReadState`, `coworkers`, `kanbanData`, `userSenderName`, `activeProject` stores and renders one sorted list.

**Template structure:**

```
Section header: "ACTIVITY"

For each attention item:
  - Colored background per type
  - Line 1: icon + title + #channel (right-aligned)
  - Line 2: context with worker name colored

For each active task (sorted: review → in-progress):
  - Line 1: task name + #channel (right-aligned)
  - Line 2: progress bar + percentage
  (Reuse TaskRow variant="row" or inline the rendering)

For each recent thread (lastActivity < 15 min):
  - Line 1: subject + unread dot (if unread) + #channel (right-aligned)

Collapsed divider: "▸ N older threads · M unread"
  When expanded: all older threads, reverse chronological, with unread dots
```

**Key implementation details:**

- Use `$derived` to compute `recentThreads` (activity < 15 min) and `olderThreads` (>= 15 min) from `trackedThreads`
- Unread = `trackedThreads[id].lastActivity > threadReadState[id]` (or no read entry)
- Use `$derived` to call `computeAttentionItems()` from `needsAttention.ts`
- `$state` for `olderCollapsed` (default: true)
- For tasks, use the same rendering as TasksSidebar: `TaskRow` with `variant="row"`, channel tag from flex layout
- Click handlers: attention items and threads call provided `onItemClick` callback, tasks call `openTaskThread`

**Props:**
```typescript
interface Props {
	onItemClick?: (item: { threadId?: string; taskId?: number; channel: string }) => void;
}
```

**Styling:** Match the existing sidebar aesthetic. Attention items use the colored backgrounds from the spec. Tasks and threads use the same font sizes and spacing as the current sidebar items. Channel tags: `font-size: 10px; color: #555; flex-shrink: 0`.

- [ ] **Step 2: Verify build**

Run: `cd web-app && npx vite build`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add web-app/src/lib/ActivityFeed.svelte
git commit -m "feat: add ActivityFeed unified sidebar component"
```

---

## Task 8: Frontend — Restructure ChannelList.svelte

**Files:**
- Modify: `web-app/src/lib/ChannelList.svelte`
- Delete: `web-app/src/lib/NeedsAttention.svelte`
- Delete: `web-app/src/lib/TasksSidebar.svelte`

- [ ] **Step 1: Replace NeedsAttention + TasksSidebar with ActivityFeed**

In `ChannelList.svelte`:

1. Replace imports: remove `NeedsAttention` and `TasksSidebar`, add `ActivityFeed`
2. Remove `NeedsAttentionItem` type import if no longer needed
3. Remove `handleAttentionItemClick` function (move logic into ActivityFeed's `onItemClick` or replace with a simpler handler)
4. Replace the two component invocations with:

```svelte
<ActivityFeed onItemClick={handleActivityItemClick} />
```

5. Remove all inline thread rendering (`.thread-row`, `getVisibleThreads`, `handleCloseThread`, `handleOpenThread` if only used for inline threads)
6. Remove `openThreads` and `setOpenThreads` imports
7. Remove thread-related CSS (`.thread-row`, `.thread-glyph`, `.thread-subject`, `.thread-unread-dot`, `.thread-close`)

- [ ] **Step 2: Update channel unread to use read state**

Replace the current `channel.unread > 0` bold logic with derived unread from `channelReadState`:

```typescript
// Import channelReadState from store
// A channel is unread if it has messages newer than the read timestamp
// For now, keep the existing channel.unread counter as fallback
// until we wire up the read state marking in the channel switch flow
```

Actually, the simplest approach: keep using `channel.unread` for now as it already works, and the read state marking in Task 5 (markRead on channel switch) will keep it in sync. The full migration from `channel.unread` counter to derived-from-readState can be done incrementally.

- [ ] **Step 3: Delete old components**

```bash
rm web-app/src/lib/NeedsAttention.svelte web-app/src/lib/TasksSidebar.svelte
```

- [ ] **Step 4: Verify build and tests**

Run: `cd web-app && npx vite build && npx vitest run`
Expected: Build succeeds, all tests pass

- [ ] **Step 5: Commit**

```bash
git add -u web-app/src/lib/
git commit -m "feat: replace NeedsAttention+TasksSidebar with unified ActivityFeed"
```

---

## Task 9: Integration testing & cleanup

**Files:** All modified files

- [ ] **Step 1: Run full Rust test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS (no warnings)

- [ ] **Step 3: Run full frontend test suite**

Run: `cd web-app && npx vitest run`
Expected: All tests pass

- [ ] **Step 4: Run biome check**

Run: `cd web-app && npx biome check .`
Expected: Only pre-existing warnings

- [ ] **Step 5: Verify build**

Run: `cd web-app && npx vite build`
Expected: Build succeeds without errors

- [ ] **Step 6: Manual testing checklist**

Start dev server and verify:
- Activity section appears with "ACTIVITY" header
- Completed tasks show as attention items (green, two-line, channel tag right)
- Active tasks show with progress bars and channel tags
- Recently active threads (< 15 min) appear below tasks
- "N older threads" collapsed divider appears
- Expanding shows older threads reverse chronological
- Unread threads have blue dot
- Opening a thread marks it as read (dot disappears)
- Read state syncs across two browser tabs
- Switching channels marks the channel as read
- Channel bold/unbold reflects read state
- Channels section has no inline threads
- Channel drag-to-reorder still works (handle only)
- Mobile sidebar renders cleanly

- [ ] **Step 7: Check coverage**

Run: `./scripts/coverage-diff.sh`
Review for uncovered lines in changed files.

- [ ] **Step 8: Commit any cleanup**

```bash
git add web-app/src/lib/ src/daemon/ src/web.rs
git commit -m "chore: integration testing cleanup"
```
