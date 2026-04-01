# Sidebar Activity Feed & Read State Sync

## Problem

The sidebar redesign (2026-03-26) introduced three separate sections (Needs Attention, Tasks, Channels with inline threads) that fragment the user's view of active work. Threads nested under channels make the channel list hard to scan. Read/unread state is client-local (localStorage), so it doesn't sync across devices. The `openThreads` server-side set adds complexity (auto-close timers, reopen logic, API calls) when thread visibility could be derived from activity timestamps.

## Design

Replace the three-section sidebar with a two-section model:

1. **Activity** — one unified sorted list of everything that matters (attention items, tasks, threads)
2. **Channels** — pure navigation

Add server-synced read state for both threads and channels, per-user keyed for multi-user future.

### Sidebar Structure (top to bottom)

1. Header (unchanged — project selector, ⌘K search)
2. **Activity** section
3. **Channels** section (drag-to-reorder, create, archive)
4. DMs (unchanged)
5. Footer (unchanged — notifications, theme, account)

### Activity Section

A single sorted list rendered by one `ActivityFeed.svelte` component. Items are sorted by urgency:

**1. Attention items (top)**

Items needing user action. Each has a colored background/border and two lines.

| Icon | Trigger | Color | Example context line |
|------|---------|-------|---------------------|
| ✓ | Task completed | green `rgba(74,222,128,0.06)` | "Task completed by ghost-town · PR #142 ready" |
| ↩ | Thread waiting on user | blue `rgba(59,130,246,0.06)` | "neon-spark replied 5m ago · waiting on you" |
| @ | @mention | amber `rgba(245,158,11,0.06)` | "silver-fox mentioned you 12m ago" |
| ⏱ | Stale work (2h+ no progress) | red `rgba(239,68,68,0.06)` | "No progress from drift-wave for 2h · 22% complete" |

Layout per attention item:
- Line 1: icon + title (flex:1, ellipsis) + channel tag right-aligned
- Line 2: context text with worker name in their color

**Attention items are self-resolving.** No dismiss buttons. An item disappears when its underlying condition clears (user replies to thread, reviews the PR, task resumes progress). The attention heuristic from `needsAttention.ts` determines which items appear.

**Click behavior:** Clicking any activity item navigates to the item's channel (switching the main view if needed) and opens the associated thread panel. Specifically:
- Thread items → switch to thread's channel, open thread
- Task items (completed, stale, or active) → switch to task's channel, open task thread
- Items without thread or task → switch to channel only

**Thread "needs attention" heuristic** (unchanged from previous spec):
1. Last message is not from the user
2. AND at least one of:
   - Message is > 10 minutes old
   - Message @mentions the user (surfaces immediately)
   - Message ends with `?` and doesn't @mention someone else (surfaces immediately)

**2. Active tasks (middle)**

In-progress tasks with progress bars. Sorted: in review → in progress.

Layout per task:
- Line 1: task name (flex:1, ellipsis) + channel tag right-aligned
- Line 2: multi-segment progress bar (owner color + reviewer color) + percentage

No coworker/reviewer avatars in the sidebar (progress bar color encodes who's working).

**3. Recently active threads (< 15 minutes)**

Threads with any activity (message, progress update) in the last 15 minutes. Plain items with unread blue dot if applicable.

Layout: thread subject (flex:1, ellipsis) + unread dot + channel tag right-aligned

**4. Older threads (collapsed)**

All threads with `lastActivity` > 15 minutes. Hidden behind a collapsible divider: `"▸ N older threads · M unread"`. Expanded shows reverse chronological list with unread dots.

### Channels Section

Pure navigation — no threads, no task pips.

- Channel name with `#` prefix
- Drag-to-reorder via grip handle only (`dragHandleSelector: ".drag-handle"`)
- Bold text if channel has unread messages (derived from read state)
- `+ New channel` button
- Archive toggle

### Channel tag consistency

All item types in the Activity section show the channel tag right-aligned on line 1: `#channel-name` in muted text (color `#555`, font-size 10px, flex-shrink: 0). When `task.channel` is undefined, display the project name.

## Data Model

### Server-side read state (daemon)

New field on `DaemonPersistentState`:

```rust
/// Per-user read state for threads and channels.
/// Maps user_id → ReadState.
#[serde(default)]
pub read_state: HashMap<String, ReadState>,
```

```rust
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

**User ID:** Use `"default"` for now. The schema is `user_id → ReadState` so multi-user support is a key change, not a schema change.

### Derived state (client-side)

**Thread unread:** `trackedThreads[id].lastActivity > readState.threads[id]` (or no read entry = unread)

**Channel unread:** latest message timestamp in channel > `readState.channels[channelName]` (or no entry = unread, replaces client-local `channel.unread` counter)

**Thread visibility:**
- Recent: `trackedThreads[id].lastActivity` within last 15 minutes
- Older: everything else in `trackedThreads`, reverse chronological

No explicit visibility set (`openThreads` removed).

### RPC endpoints

| Method | Params | Returns | Description |
|--------|--------|---------|-------------|
| `read_state.get` | — | `{ threads: Record<string, string>, channels: Record<string, string> }` | All read timestamps for current user |
| `read_state.mark_read` | `{ type: "thread" \| "channel", id: string, timestamp: string }` | `{ ok: true }` | Mark a thread or channel as read |

### REST endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/api/read-state` | Get all read state for current user |
| PUT | `/api/read-state/:type/:id` | Mark thread or channel as read. Body: `{ timestamp: string }` |

### WebSocket

New variant: `ReadStateChanged` — broadcasts `{ type: "thread" | "channel", id: string, timestamp: string }` to all connected clients when read state changes. This ensures all open browser tabs / devices stay in sync.

## What's New

- `ActivityFeed.svelte` — unified sorted list component
- `read_state` field on `DaemonPersistentState` (per-user, keyed for multi-user future)
- `read_state.get` / `read_state.mark_read` RPC endpoints
- `GET/PUT /api/read-state` REST endpoints
- `ReadStateChanged` WebSocket variant
- `threadReadState` client-side store (synced from daemon)
- Channel unread derived from read state (replaces client-local counter)

## What's Removed

- `NeedsAttention.svelte` — merged into ActivityFeed
- `TasksSidebar.svelte` — merged into ActivityFeed
- `open_threads` field from `DaemonPersistentState`
- `channel.open_threads` / `channel.open_threads.set` RPC endpoints
- `GET/PUT /api/channels/:channel/open-threads` REST endpoints
- `OpenThreadsChanged` WebSocket variant
- `openThreads` client-side store
- `dismissedAttentionItems` client-side store
- `threadUnreadCounts` client-side store (replaced by derived computation)
- Inline thread rendering in `ChannelList.svelte`
- Dismiss buttons on attention items

## What's Preserved

- `needsAttention.ts` — pure heuristic functions reused by ActivityFeed
- `trackedThreads` store — metadata (subject, channel, lastActivity)
- `TaskRow.svelte` — reused for task rendering inside ActivityFeed (row variant, no avatars)
- Channel drag-to-reorder (handle only)
- Channel create, archive toggle
- DM section
- Header, footer (project selector, search, theme, account)
- All keyboard shortcuts

## Components Affected

- Delete: `web-app/src/lib/NeedsAttention.svelte`
- Delete: `web-app/src/lib/TasksSidebar.svelte`
- Create: `web-app/src/lib/ActivityFeed.svelte`
- Modify: `web-app/src/lib/ChannelList.svelte` — remove thread rendering, replace NeedsAttention/TasksSidebar with ActivityFeed, remove openThreads usage
- Modify: `web-app/src/lib/store.ts` — add `threadReadState`, remove `openThreads`, `dismissedAttentionItems`, `threadUnreadCounts`
- Modify: `web-app/src/lib/api.ts` — add read state fetch/mark, handle `ReadStateChanged` WS, mark channel/thread as read on open, remove openThreads API functions and auto-close timer
- Modify: `web-app/src/lib/needsAttention.ts` — update `computeAttentionItems` to use `threadReadState` instead of `dismissed` set
- Modify: `web-app/src/lib/types.ts` — remove `dismissedAttentionItems` from `NeedsAttentionItem` if referenced, add `ReadState` type
- Modify: `src/daemon/state.rs` — add `read_state` field, `ReadState` struct, remove `open_threads`
- Modify: `src/daemon/rpc.rs` — add `read_state.*` dispatch, remove `channel.open_threads.*` dispatch
- Delete or gut: `src/daemon/rpc_open_threads.rs` — replace with `rpc_read_state.rs`
- Modify: `src/web.rs` — add `ReadStateChanged` variant, add REST routes, remove `OpenThreadsChanged` and open-threads routes

## Mobile Considerations

The unified Activity section is more compact than three separate sections, improving mobile density. The collapsed "older threads" section keeps the visible list short. Channel list is cleaner without inline threads.
