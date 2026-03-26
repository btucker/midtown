# Sidebar Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the web-app sidebar into three priority-ordered sections (Needs Attention → Tasks → Channels), replace client-side `dismissedThreads` with server-side `openThreads`, and add thread lifecycle management.

**Architecture:** The sidebar's `ChannelList.svelte` gets decomposed into focused components: `NeedsAttention.svelte` (priority inbox), `TasksSidebar.svelte` (unified flat task list), and a slimmed-down channel list. A new daemon RPC endpoint (`channel.open_threads`) persists the open thread set server-side. Thread lifecycle heuristics run client-side using existing WebSocket data.

**Tech Stack:** Svelte 5, TypeScript, Tailwind CSS 4, Rust (daemon RPC + persistent state)

**Spec:** `docs/superpowers/specs/2026-03-26-sidebar-redesign-design.md`

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `web-app/src/lib/NeedsAttention.svelte` | Needs Attention section — computes and renders attention items |
| `web-app/src/lib/TasksSidebar.svelte` | Unified flat task list with progress bars |
| `web-app/src/lib/needsAttention.ts` | Pure functions: attention heuristics, staleness detection |
| `web-app/src/lib/needsAttention.test.ts` | Tests for attention heuristics |
| `src/daemon/rpc_open_threads.rs` | RPC handlers for `channel.open_threads` get/set |
| `src/daemon/rpc_open_threads_tests.rs` | Tests for open_threads RPC handlers |

### Modified Files
| File | Changes |
|------|---------|
| `src/daemon/state.rs` | Add `open_threads: HashMap<String, HashSet<String>>` to `DaemonPersistentState` |
| `src/daemon/rpc.rs` | Add dispatch entries for `channel.open_threads` and `channel.open_threads.set` |
| `src/daemon/mod.rs` | Add `mod rpc_open_threads;` |
| `src/web.rs` | Add `OpenThreadsChanged` WebUpdate variant |
| `web-app/src/lib/types.ts` | Add `NeedsAttentionItem` interface |
| `web-app/src/lib/store.ts` | Add `openThreads` store, `dismissedAttentionItems` store, remove `dismissedThreads` |
| `web-app/src/lib/api.ts` | Add `fetchOpenThreads()`, `setOpenThreads()`, handle `open_threads_changed` WebSocket message, replace `dismissedThreads` usage |
| `web-app/src/lib/ChannelList.svelte` | Remove task pips, restructure section order, inline threads at same font size with `⌇` glyph, wire up new components |
| `web-app/src/lib/channelUtils.ts` | Update thread filtering to use `openThreads` instead of `dismissedThreads` |
| `web-app/src/lib/TaskRow.svelte` | Remove status dot from row variant (progress bar is sufficient) |

---

## Task 1: Daemon — `openThreads` Persistent State

**Files:**
- Modify: `src/daemon/state.rs:201-274` (add field to `DaemonPersistentState`)
- Test: `src/daemon/rpc_open_threads_tests.rs` (new)

- [ ] **Step 1: Write failing test for openThreads state persistence**

Create `src/daemon/rpc_open_threads_tests.rs`:

```rust
#[cfg(test)]
mod tests {
    use crate::daemon::state::DaemonPersistentState;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn open_threads_default_empty() {
        let ps = DaemonPersistentState::default();
        assert!(ps.open_threads.is_empty());
    }

    #[test]
    fn open_threads_roundtrip_serde() {
        let mut ps = DaemonPersistentState::default();
        let mut threads = HashSet::new();
        threads.insert("thread-1".to_string());
        threads.insert("thread-2".to_string());
        ps.open_threads.insert("my-channel".to_string(), threads);

        let json = serde_json::to_string(&ps).unwrap();
        let ps2: DaemonPersistentState = serde_json::from_str(&json).unwrap();

        assert_eq!(ps2.open_threads.get("my-channel").unwrap().len(), 2);
        assert!(ps2.open_threads.get("my-channel").unwrap().contains("thread-1"));
    }

    #[test]
    fn open_threads_deserialize_missing_field() {
        // Old state files won't have open_threads — should default to empty
        let json = r#"{}"#;
        let ps: DaemonPersistentState = serde_json::from_str(json).unwrap();
        assert!(ps.open_threads.is_empty());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test rpc_open_threads_tests`
Expected: FAIL — `open_threads` field doesn't exist on `DaemonPersistentState`

- [ ] **Step 3: Add `open_threads` field to `DaemonPersistentState`**

In `src/daemon/state.rs`, add after the `workflow_state` field (~line 274):

```rust
    /// Per-channel set of thread IDs the user wants visible in the sidebar.
    ///
    /// Maps channel name → set of thread parent IDs. Synced to all web clients
    /// via WebSocket so thread visibility is consistent across devices.
    /// Threads are added when opened or created, removed on manual close or
    /// 12-hour idle timeout.
    #[serde(default)]
    pub open_threads: HashMap<String, HashSet<String>>,
```

Add `mod rpc_open_threads;` and the test path declaration in `src/daemon/mod.rs`:

```rust
mod rpc_open_threads;
#[path = "rpc_open_threads_tests.rs"]
#[cfg(test)]
mod rpc_open_threads_tests;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test rpc_open_threads_tests`
Expected: PASS (all 3 tests)

- [ ] **Step 5: Commit**

```bash
git add src/daemon/state.rs src/daemon/mod.rs src/daemon/rpc_open_threads_tests.rs
git commit -m "feat: add open_threads field to DaemonPersistentState"
```

---

## Task 2: Daemon — `openThreads` RPC Handlers

**Files:**
- Create: `src/daemon/rpc_open_threads.rs`
- Modify: `src/daemon/rpc.rs:495-525` (add dispatch entries)
- Test: `src/daemon/rpc_open_threads_tests.rs` (extend)

- [ ] **Step 1: Write failing tests for RPC handlers**

Add to `src/daemon/rpc_open_threads_tests.rs`:

```rust
use crate::daemon::rpc_open_threads::{handle_open_threads_get, handle_open_threads_set};
// Test that get returns empty set for unknown channel
// Test that set persists and get returns the set
// Test that set broadcasts WebUpdate
```

These tests will use the standard `DaemonState` test helper pattern from existing `rpc_*_tests.rs` files. Look at `rpc_channel_tests.rs` for the fixture pattern.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test rpc_open_threads_tests`
Expected: FAIL — `rpc_open_threads` module doesn't exist

- [ ] **Step 3: Implement RPC handlers**

Create `src/daemon/rpc_open_threads.rs`:

```rust
//! RPC handlers for managing per-channel open thread sets.
//!
//! The `openThreads` set tracks which threads a user wants visible in
//! the sidebar. Persisted server-side so it syncs across all clients.

use tracing::error;

use crate::rpc::{RequestId, Response, RpcError};

use super::DaemonState;

/// Handle `channel.open_threads` — get the open thread set for a channel.
pub(super) async fn handle_open_threads_get(
    id: RequestId,
    channel: &str,
    state: &DaemonState,
) -> Response {
    let ps = state.persistent_state.lock().await;
    let threads: Vec<&String> = ps
        .open_threads
        .get(channel)
        .map(|s| s.iter().collect())
        .unwrap_or_default();
    Response::success(id, serde_json::json!({ "threads": threads }))
}

/// Handle `channel.open_threads.set` — replace the open thread set for a channel.
pub(super) async fn handle_open_threads_set(
    id: RequestId,
    channel: &str,
    threads: Vec<String>,
    state: &DaemonState,
) -> Response {
    let thread_set: std::collections::HashSet<String> = threads.into_iter().collect();

    let mut ps = state.persistent_state.lock().await;
    ps.open_threads.insert(channel.to_string(), thread_set.clone());

    if let Err(e) = ps.save_for_repo(state.paths.dir_key()) {
        error!("Failed to save daemon-state.json: {}", e);
        return Response::error(id, RpcError::new(-32603, format!("failed to persist: {e}")));
    }
    drop(ps);

    // Broadcast to all connected web clients
    let _ = state.web_updates_tx.send(crate::web::WebUpdate::OpenThreadsChanged(
        crate::web::OpenThreadsChangedData {
            channel: channel.to_string(),
            threads: thread_set.into_iter().collect(),
        },
    ));

    Response::success(id, serde_json::json!({ "ok": true }))
}
```

- [ ] **Step 4: Add `OpenThreadsChanged` WebUpdate variant**

In `src/web.rs`, add to the `WebUpdate` enum:

```rust
    /// Open threads set changed for a channel
    #[serde(rename = "open_threads_changed")]
    OpenThreadsChanged(OpenThreadsChangedData),
```

And the data struct:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct OpenThreadsChangedData {
    pub channel: String,
    pub threads: Vec<String>,
}
```

- [ ] **Step 5: Add dispatch entries in `rpc.rs`**

In `src/daemon/rpc.rs`, add after the `channel.rename` block (~line 525):

```rust
        "channel.open_threads" => {
            let channel = require_str!(params, "channel", request.id);
            super::rpc_open_threads::handle_open_threads_get(request.id, channel, state).await
        }

        "channel.open_threads.set" => {
            let channel = require_str!(params, "channel", request.id);
            let threads = params.str_array_param("threads").unwrap_or_default();
            super::rpc_open_threads::handle_open_threads_set(request.id, channel, threads, state).await
        }
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test rpc_open_threads_tests && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add src/daemon/rpc_open_threads.rs src/daemon/rpc.rs src/web.rs
git commit -m "feat: add channel.open_threads RPC endpoints"
```

---

## Task 3: Daemon — Wire OpenThreads into Web HTTP API

**Files:**
- Modify: `src/web.rs` (add REST endpoints for open_threads)

Look at how existing channel endpoints are wired in `src/web.rs` (the `axum` router). Add:
- `GET /api/channels/:channel/open-threads` → calls `channel.open_threads` RPC
- `PUT /api/channels/:channel/open-threads` → calls `channel.open_threads.set` RPC

Follow the existing pattern for channel REST endpoints in `src/web.rs`.

- [ ] **Step 1: Find the existing REST endpoint pattern in `src/web.rs`**

Read how routes like `/api/channels` are defined and how they call RPC internally.

- [ ] **Step 2: Add REST endpoints**

Add to the router and implement handlers following the existing pattern.

- [ ] **Step 3: Handle `open_threads_changed` in WebSocket broadcast**

Ensure the `OpenThreadsChanged` variant is serialized and sent to WS clients. Check that the existing `broadcast` mechanism in `src/web.rs` handles all `WebUpdate` variants generically (it likely does via `serde`).

- [ ] **Step 4: Run full test suite**

Run: `cargo test && cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/web.rs
git commit -m "feat: add REST endpoints for open_threads"
```

---

## Task 4: Frontend — Types and Store Infrastructure

**Files:**
- Modify: `web-app/src/lib/types.ts:60-70` (add `NeedsAttentionItem`)
- Modify: `web-app/src/lib/store.ts:225-239` (add `openThreads`, `dismissedAttentionItems`, remove `dismissedThreads`)

- [ ] **Step 1: Add `NeedsAttentionItem` type**

In `web-app/src/lib/types.ts`, add after the `TrackedThread` interface:

```typescript
// ── Needs Attention ─────────────────────────────────────────────────────────

export type AttentionType = "task_completed" | "thread_waiting" | "mention" | "stale_work";

export interface NeedsAttentionItem {
	id: string; // unique key for dedup/dismiss
	type: AttentionType;
	title: string; // primary line (e.g., task name or thread subject)
	context: string; // secondary line (who, what, when, channel)
	channel: string;
	threadId?: string; // for thread-based items — navigate on click
	taskId?: number; // for task-based items — navigate on click
	timestamp: number; // for sorting (ms since epoch)
	workerName?: string; // for coloring the worker name
	workerColor?: string;
}
```

- [ ] **Step 2: Update stores**

In `web-app/src/lib/store.ts`:

Replace the `dismissedThreads` block (lines 236-239) with:

```typescript
// ── Open threads (server-synced) ────────────────────────────────────────────
// Per-channel set of thread IDs visible in sidebar. Synced from daemon API.
// Format: { [channelName]: Set<threadParentId> }
export const openThreads = writable<Record<string, Set<string>>>({});

// ── Dismissed attention items (client-side) ─────────────────────────────────
// IDs of needs-attention items the user has dismissed. Persists via localStorage.
const _dismissedAttentionArr = loadFromLocalStorage<string[]>("midtown_dismissed_attention", []);
export const dismissedAttentionItems = writable<Set<string>>(new Set(_dismissedAttentionArr));
dismissedAttentionItems.subscribe((s) =>
	debouncedSaveToLocalStorage("midtown_dismissed_attention", [...s]),
);
```

- [ ] **Step 3: Run type check**

Run: `cd web-app && npx tsc --noEmit`
Expected: May have errors from files still importing `dismissedThreads` — that's OK, we'll fix in later tasks.

- [ ] **Step 4: Commit**

```bash
git add web-app/src/lib/types.ts web-app/src/lib/store.ts
git commit -m "feat: add NeedsAttentionItem type and openThreads store"
```

---

## Task 5: Frontend — Needs Attention Heuristics

**Files:**
- Create: `web-app/src/lib/needsAttention.ts`
- Create: `web-app/src/lib/needsAttention.test.ts`

- [ ] **Step 1: Write failing tests for attention heuristics**

Create `web-app/src/lib/needsAttention.test.ts`:

```typescript
import { describe, it, expect } from "vitest";
import {
	threadNeedsAttention,
	isTaskStale,
	computeAttentionItems,
} from "./needsAttention.ts";
import type { TrackedThread } from "./types.ts";

describe("threadNeedsAttention", () => {
	const now = Date.now();
	const userSender = "human";

	it("returns false when last message is from user", () => {
		expect(
			threadNeedsAttention(
				{ sender: userSender, content: "hello", timestamp: new Date(now - 15 * 60000).toISOString() },
				userSender,
				now,
			),
		).toBe(false);
	});

	it("returns true when message is >10min old and not from user", () => {
		expect(
			threadNeedsAttention(
				{ sender: "ghost-town", content: "done with the refactor", timestamp: new Date(now - 15 * 60000).toISOString() },
				userSender,
				now,
			),
		).toBe(true);
	});

	it("returns true immediately when message @mentions user", () => {
		expect(
			threadNeedsAttention(
				{ sender: "ghost-town", content: "hey @human what do you think?", timestamp: new Date(now - 1000).toISOString() },
				userSender,
				now,
			),
		).toBe(true);
	});

	it("returns true immediately when message ends with ? and doesn't mention someone else", () => {
		expect(
			threadNeedsAttention(
				{ sender: "ghost-town", content: "should I use Option or Result?", timestamp: new Date(now - 1000).toISOString() },
				userSender,
				now,
			),
		).toBe(true);
	});

	it("returns false when message ends with ? but mentions someone else", () => {
		expect(
			threadNeedsAttention(
				{ sender: "ghost-town", content: "@silver-fox should I use Option?", timestamp: new Date(now - 1000).toISOString() },
				userSender,
				now,
			),
		).toBe(false);
	});

	it("returns false when message is <10min old and no special triggers", () => {
		expect(
			threadNeedsAttention(
				{ sender: "ghost-town", content: "working on it", timestamp: new Date(now - 5 * 60000).toISOString() },
				userSender,
				now,
			),
		).toBe(false);
	});
});

describe("isTaskStale", () => {
	const now = Date.now();

	it("returns false when progress changed recently", () => {
		expect(isTaskStale(50, now - 30 * 60000, now)).toBe(false);
	});

	it("returns true when no progress change for 2+ hours", () => {
		expect(isTaskStale(50, now - 3 * 3600000, now)).toBe(true);
	});

	it("returns false when task is done (progress 100)", () => {
		expect(isTaskStale(100, now - 3 * 3600000, now)).toBe(false);
	});
});
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd web-app && npx vitest run src/lib/needsAttention.test.ts`
Expected: FAIL — module doesn't exist

- [ ] **Step 3: Implement heuristics**

Create `web-app/src/lib/needsAttention.ts`:

```typescript
import type { NeedsAttentionItem, Coworker, Task, TrackedThread } from "./types.ts";
import { getSenderColor } from "./messageUtils.ts";

const TEN_MINUTES_MS = 10 * 60 * 1000;
const TWO_HOURS_MS = 2 * 60 * 60 * 1000;

interface LastMessage {
	sender: string;
	content: string;
	timestamp: string; // ISO
}

/**
 * Determine if a thread needs the user's attention based on the last message.
 *
 * Returns true when:
 * 1. Last message is NOT from the user, AND
 * 2. At least one of:
 *    - Message is >10 minutes old
 *    - Message @mentions the user
 *    - Message ends with ? and doesn't @mention someone else
 */
export function threadNeedsAttention(
	lastMsg: LastMessage,
	userSender: string,
	now: number = Date.now(),
): boolean {
	if (lastMsg.sender === userSender) return false;

	const ageMs = now - new Date(lastMsg.timestamp).getTime();
	const content = lastMsg.content.trim();

	// Immediate: @mentions user
	if (content.includes(`@${userSender}`)) return true;

	// Immediate: ends with ? and doesn't @mention someone else
	if (content.endsWith("?")) {
		const mentionPattern = /@[\w-]+/g;
		const mentions = content.match(mentionPattern) || [];
		const mentionsOther = mentions.some((m) => m !== `@${userSender}`);
		if (!mentionsOther) return true;
	}

	// Delayed: >10 minutes old
	if (ageMs > TEN_MINUTES_MS) return true;

	return false;
}

/**
 * Determine if a task is stale (no progress change for 2+ hours).
 */
export function isTaskStale(
	progress: number | null | undefined,
	lastProgressChangeMs: number,
	now: number = Date.now(),
): boolean {
	if (progress === 100 || progress == null) return false;
	return now - lastProgressChangeMs > TWO_HOURS_MS;
}

/**
 * Build the full list of needs-attention items from current state.
 */
export function computeAttentionItems(opts: {
	trackedThreads: Record<string, TrackedThread>;
	openThreads: Record<string, Set<string>>;
	lastMessages: Record<string, LastMessage>;
	coworkers: Coworker[];
	tasks: Task[];
	progressTimestamps: Record<string, number>; // taskId → last progress change ms
	dismissed: Set<string>;
	userSender: string;
	mainChannel: string;
	now?: number;
}): NeedsAttentionItem[] {
	const now = opts.now ?? Date.now();
	const items: NeedsAttentionItem[] = [];

	// 1. Threads needing attention (from openThreads)
	for (const [channel, threadIds] of Object.entries(opts.openThreads)) {
		for (const threadId of threadIds) {
			const tracked = opts.trackedThreads[threadId];
			const lastMsg = opts.lastMessages[threadId];
			if (!tracked || !lastMsg) continue;

			if (threadNeedsAttention(lastMsg, opts.userSender, now)) {
				const id = `thread:${threadId}`;
				if (opts.dismissed.has(id)) continue;

				const ageMs = now - new Date(lastMsg.timestamp).getTime();
				const agoText = formatAgo(ageMs);

				items.push({
					id,
					type: lastMsg.content.includes(`@${opts.userSender}`) ? "mention" : "thread_waiting",
					title: tracked.subject,
					context: `${lastMsg.sender} replied ${agoText} · waiting on you · #${channel}`,
					channel,
					threadId,
					timestamp: new Date(lastMsg.timestamp).getTime(),
					workerName: lastMsg.sender,
					workerColor: getSenderColor(lastMsg.sender),
				});
			}
		}
	}

	// 2. Completed tasks
	for (const task of opts.tasks) {
		if (task.status !== "done") continue;
		const id = `task:${task.id}`;
		if (opts.dismissed.has(id)) continue;

		const cw = opts.coworkers.find((c) => c.name === task.owner);
		const channel = task.channel || opts.mainChannel;

		items.push({
			id,
			type: "task_completed",
			title: task.subject,
			context: `Task completed by ${task.owner || "unknown"}${cw?.pr_number ? ` · PR #${cw.pr_number} ready` : ""} · #${channel}`,
			channel,
			taskId: task.id,
			timestamp: now, // completed tasks surface immediately
			workerName: task.owner,
			workerColor: task.owner ? getSenderColor(task.owner) : undefined,
		});
	}

	// 3. Stale tasks
	for (const task of opts.tasks) {
		if (task.status !== "in_progress") continue;
		const cw = opts.coworkers.find((c) => c.name === task.owner);
		const lastChange = opts.progressTimestamps[String(task.id)];
		if (!lastChange) continue;

		if (isTaskStale(cw?.progress ?? null, lastChange, now)) {
			const id = `stale:${task.id}`;
			if (opts.dismissed.has(id)) continue;

			const channel = task.channel || opts.mainChannel;
			const staleHours = Math.floor((now - lastChange) / 3600000);

			items.push({
				id,
				type: "stale_work",
				title: task.subject,
				context: `No progress from ${task.owner || "unknown"} for ${staleHours}h · ${cw?.progress ?? 0}% complete · #${channel}`,
				channel,
				taskId: task.id,
				timestamp: lastChange,
				workerName: task.owner,
				workerColor: task.owner ? getSenderColor(task.owner) : undefined,
			});
		}
	}

	// Sort newest first
	items.sort((a, b) => b.timestamp - a.timestamp);
	return items;
}

function formatAgo(ms: number): string {
	const minutes = Math.floor(ms / 60000);
	if (minutes < 1) return "just now";
	if (minutes < 60) return `${minutes}m ago`;
	const hours = Math.floor(minutes / 60);
	return `${hours}h ago`;
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cd web-app && npx vitest run src/lib/needsAttention.test.ts`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add web-app/src/lib/needsAttention.ts web-app/src/lib/needsAttention.test.ts
git commit -m "feat: add needs-attention heuristic functions with tests"
```

---

## Task 6: Frontend — API Integration for openThreads

**Files:**
- Modify: `web-app/src/lib/api.ts` (add fetch/set functions, handle WS message)

- [ ] **Step 1: Add `fetchOpenThreads` and `setOpenThreads` functions**

In `web-app/src/lib/api.ts`, add:

```typescript
export async function fetchOpenThreads(channel: string): Promise<string[]> {
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/open-threads`);
		if (res.ok) {
			const data = await res.json();
			return data.threads || [];
		}
	} catch (err) {
		console.warn("Failed to fetch open threads:", err);
	}
	return [];
}

export async function setOpenThreads(channel: string, threads: string[]): Promise<void> {
	try {
		await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/open-threads`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ threads }),
		});
	} catch (err) {
		console.warn("Failed to set open threads:", err);
	}
}
```

- [ ] **Step 2: Handle `open_threads_changed` WebSocket message**

In the WebSocket message handler in `api.ts`, add a case for the new message type:

```typescript
case "open_threads_changed": {
	const { channel, threads } = update.data;
	openThreads.update((ot) => ({
		...ot,
		[channel]: new Set(threads),
	}));
	break;
}
```

- [ ] **Step 3: Load openThreads on channel switch**

In the `selectChannel` / channel init flow, call `fetchOpenThreads` and populate the store.

- [ ] **Step 4: Replace `dismissedThreads` usage with `openThreads`**

Search for all imports/usages of `dismissedThreads` in `api.ts` and replace with the new model. The key change: instead of "show everything except dismissed", it's now "show only what's in openThreads".

- [ ] **Step 5: Commit**

```bash
git add web-app/src/lib/api.ts
git commit -m "feat: add openThreads API integration and WebSocket handling"
```

---

## Task 7: Frontend — `NeedsAttention.svelte` Component

**Files:**
- Create: `web-app/src/lib/NeedsAttention.svelte`

- [ ] **Step 1: Create the component**

Create `web-app/src/lib/NeedsAttention.svelte`. This component:

- Subscribes to `trackedThreads`, `openThreads`, `coworkers`, `kanbanData`, `dismissedAttentionItems`, `userSenderName`
- Calls `computeAttentionItems()` reactively
- Renders two-line items with colored backgrounds per attention type
- Dismiss button removes the item (adds to `dismissedAttentionItems`)
- Click navigates to relevant thread/task

**Visual treatment per type:**
- `task_completed`: green background `rgba(74,222,128,0.06)`, border `rgba(74,222,128,0.12)`, icon ✓
- `thread_waiting`: blue background `rgba(59,130,246,0.06)`, border `rgba(59,130,246,0.12)`, icon ↩
- `mention`: amber background `rgba(245,158,11,0.06)`, border `rgba(245,158,11,0.12)`, icon @
- `stale_work`: red background `rgba(239,68,68,0.06)`, border `rgba(239,68,68,0.12)`, icon ⏱

**Layout per item:**
- Line 1: icon + title (flex:1, ellipsis) + dismiss ✕
- Line 2: context text (smaller, muted, with worker name in their color)
- `padding: 8px`, `border-radius: 6px`, `margin-bottom: 3px`
- Font size: 12px line 1, 11px line 2

**Section header:** "NEEDS ATTENTION" (uppercase, 10px) with amber badge count.

- [ ] **Step 2: Test rendering manually**

Verify the component renders by temporarily importing it in `ChannelList.svelte`.

- [ ] **Step 3: Commit**

```bash
git add web-app/src/lib/NeedsAttention.svelte
git commit -m "feat: add NeedsAttention sidebar component"
```

---

## Task 8: Frontend — `TasksSidebar.svelte` Component

**Files:**
- Create: `web-app/src/lib/TasksSidebar.svelte`
- Modify: `web-app/src/lib/TaskRow.svelte` (remove status dot from row variant)

- [ ] **Step 1: Remove status dot from TaskRow row variant**

In `web-app/src/lib/TaskRow.svelte`, find the colored status dot rendered in row variant and remove it. The progress bar already encodes status via color. Keep the dot for card variant (Kanban board).

- [ ] **Step 2: Create `TasksSidebar.svelte`**

This component:

- Subscribes to `kanbanData` and `coworkers`
- Combines all tasks from `inProgress`, `backlog`, and done into a flat list
- Sorts: review → in_progress → completed
- For each active task: renders task name + channel tag + progress bar (reusing `TaskRow` row variant or extracting progress bar rendering)
- For completed tasks: renders under collapsible "Completed today" divider (dimmed, ✓ + name + channel)
- Channel tag: `task.channel || mainChannelName` right-aligned in muted text

**Section header:** "TASKS" (uppercase, 10px) with count badge.

**"Completed today" divider:** `font-size: 9px`, muted color, horizontal rule, collapse toggle (▾/▸). Filter to tasks completed in current calendar day (user's local timezone).

- [ ] **Step 3: Test rendering manually**

Verify the component renders by temporarily importing it in `ChannelList.svelte`.

- [ ] **Step 4: Commit**

```bash
git add web-app/src/lib/TasksSidebar.svelte web-app/src/lib/TaskRow.svelte
git commit -m "feat: add TasksSidebar component, remove status dot from TaskRow"
```

---

## Task 9: Frontend — Restructure ChannelList.svelte

**Files:**
- Modify: `web-app/src/lib/ChannelList.svelte` (major refactor)

This is the largest task. The key changes:

1. **Remove**: task pip rendering from channels
2. **Remove**: old "Needs Attention" section (replaced by `NeedsAttention.svelte`)
3. **Remove**: `TaskList` / `ThreadList` expansion under channels
4. **Add**: Import and render `NeedsAttention` and `TasksSidebar` components
5. **Change**: Section order to Header → Needs Attention → Tasks → Channels → DMs → Footer
6. **Change**: Threads render inline at same font size with `⌇` glyph prefix
7. **Preserve**: Channel drag-to-reorder, channel create, archive toggle
8. **Preserve**: DM section (unchanged)

- [ ] **Step 1: Import new components**

```svelte
import NeedsAttention from "./NeedsAttention.svelte";
import TasksSidebar from "./TasksSidebar.svelte";
```

- [ ] **Step 2: Remove task pip rendering**

Remove the task pip `<span>` elements inside channel buttons (~lines 391-413) and the associated `getChannelTasks` / `getChannelTaskCount` computed values.

- [ ] **Step 3: Remove old Needs Attention section**

Remove the `completedThreads` block (~lines 339-369) and the `getAllCompletedThreads` computation.

- [ ] **Step 4: Remove TaskList/ThreadList expansion**

Remove the expanded channel content block (~lines 426-438) that renders `TaskList` and `ThreadList` sub-components.

- [ ] **Step 5: Add inline threads under channels**

After each channel button, render its open threads (from `openThreads` store) as flat inline items:

```svelte
{#each channelOpenThreads as thread (thread.id)}
	<button class="thread-row" onclick={() => openThread(thread.id)}>
		<span class="thread-glyph">⌇</span>
		<span class="thread-subject">{thread.subject}</span>
		{#if thread.unread > 0}
			<span class="thread-unread-dot"></span>
		{/if}
		<button class="thread-close" onclick={(e) => { e.stopPropagation(); closeThread(thread.id); }}>✕</button>
	</button>
{/each}
```

Style threads at the **same font size** as channels. The `⌇` glyph and slight indent (padding-left) differentiate them.

- [ ] **Step 6: Restructure section order**

Reorder the template:
1. Channel create header (archive toggle, + button)
2. `<NeedsAttention />` component
3. `<TasksSidebar />` component
4. Channels section (with inline threads, drag-to-reorder)
5. DMs section

- [ ] **Step 7: Wire thread close to openThreads API**

When user clicks ✕ on a thread:
1. Remove thread ID from the local `openThreads` store (optimistic update)
2. Call `setOpenThreads(channel, updatedSet)` to persist server-side

- [ ] **Step 8: Test the full sidebar**

Run: `cd web-app && npm run dev`
Verify: Sidebar renders with new section order, threads inline, no task pips.

- [ ] **Step 9: Commit**

```bash
git add web-app/src/lib/ChannelList.svelte
git commit -m "feat: restructure sidebar — attention first, unified tasks, inline threads"
```

---

## Task 10: Frontend — Thread Lifecycle (Auto-close & Reopen)

**Files:**
- Modify: `web-app/src/lib/api.ts` (auto-close timer, reopen on new message)
- Modify: `web-app/src/lib/channelUtils.ts` (update filtering to use `openThreads`)

- [ ] **Step 1: Implement 12-hour auto-close**

In the WebSocket message handler or a periodic timer, check `trackedThreads` entries against `openThreads`. For any thread in `openThreads` where `lastActivity` is >12 hours old, remove it from `openThreads` and call `setOpenThreads()`.

This can be a `setInterval` that runs every 5 minutes.

- [ ] **Step 2: Implement reopen on new message**

When a new thread message arrives via WebSocket:
- If the thread is NOT in `openThreads` for its channel, add it (a new message in a closed thread reopens it)
- Call `setOpenThreads()` to persist

- [ ] **Step 3: Update `channelUtils.ts` thread filtering**

Replace `dismissedThreads` filtering with `openThreads` filtering. `getChannelThreads` should only return threads that are in the channel's `openThreads` set.

- [ ] **Step 4: Remove `dismissedThreads` store entirely**

Remove the `dismissedThreads` export from `store.ts` and all remaining imports across the codebase. Clean up the localStorage key `"midtown_dismissed_threads"`.

- [ ] **Step 5: Run full frontend tests**

Run: `cd web-app && npx vitest run`
Expected: PASS (may need to update existing tests that reference `dismissedThreads`)

- [ ] **Step 6: Commit**

```bash
git add web-app/src/lib/api.ts web-app/src/lib/channelUtils.ts web-app/src/lib/store.ts
git commit -m "feat: add thread lifecycle — 12h auto-close, reopen on new message"
```

---

## Task 11: Integration Testing & Cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run full Rust test suite**

Run: `cargo test`
Expected: PASS

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: PASS

- [ ] **Step 3: Run full frontend test suite**

Run: `cd web-app && npx vitest run`
Expected: PASS

- [ ] **Step 4: Run biome check**

Run: `cd web-app && npx biome check .`
Expected: PASS (warnings OK)

- [ ] **Step 5: Manual testing**

Start the app and verify:
- Needs Attention section appears at top with badge count
- Completed tasks surface in Needs Attention with two-line context
- Dismissing an attention item persists across reload
- Tasks section shows flat list with progress bars, sorted by actionability
- "Completed today" is collapsible
- Channels have no task pips
- Threads appear inline at same font size with `⌇` glyph
- Thread ✕ closes it across all clients (test with two browser tabs)
- Thread auto-reopens when new message arrives
- Channel drag-to-reorder still works
- Mobile sidebar (< 768px) renders cleanly without density issues
- Project selector, search, theme toggle, account panel all still work

- [ ] **Step 6: Check code coverage**

Run: `./scripts/coverage-diff.sh`
Review the summary for uncovered lines in changed files.

- [ ] **Step 7: Final commit if any cleanup needed**

```bash
git add web-app/src/lib/ src/daemon/ src/web.rs
git commit -m "chore: integration testing cleanup"
```
