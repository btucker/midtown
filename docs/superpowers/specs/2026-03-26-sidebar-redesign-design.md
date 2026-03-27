# Sidebar Redesign

## Problem

The current sidebar organizes information by channel, with tasks and threads nested underneath each channel. As the number of channels, tasks, and threads grows, this structure makes it hard to:

1. **Know what needs your attention** — actionable items are buried inside channel trees
2. **Understand where work stands** — task status is scattered across channels as small pips
3. **Navigate quickly** — too many taps/scrolls to reach what you need, especially on mobile where nested content is too small and dense

## Design

Restructure the sidebar into three primary sections ordered by urgency, with the header and footer preserved.

### Section Order (top to bottom)

1. Header (unchanged)
2. Needs Attention
3. Tasks
4. Channels
5. DMs (unchanged)
6. Footer (unchanged)

### Header (unchanged)

- Midtown logo (desktop only)
- Project selector dropdown with status dots
- Search button (⌘K shortcut)

### Section 1: Needs Attention

A priority inbox at the top of the sidebar. Surfaces items that require the user's action, with a badge count on the section header.

**Trigger types:**

| Icon | Trigger | Example second line |
|------|---------|-------------------|
| ✓ | Task completed (worker finished, PR ready) | "Task completed by ghost-town · PR #142 ready · #api" |
| ↩ | Thread waiting on user (someone replied, ball in your court) | "neon-spark replied 5m ago · waiting on you · #auth" |
| @ | @mention in a thread | "silver-fox mentioned you 12m ago · #api" |
| ⏱ | Stale work (no progress change for 2+ hours) | "No progress from drift-wave for 2h · 22% complete · #mobile" |

**Item layout:** Two lines per item.
- Line 1: Icon + item name + dismiss button (✕)
- Line 2: Context — who (worker name in their color), what happened, when, which channel

**Visual treatment:** Each item has a subtle colored background and border matching its icon color (green for completed, blue for waiting, amber for mention, red for stale).

**Behavior:**
- Clicking an item navigates to the relevant thread/task
- Dismiss button (✕) removes the item. Dismissals persist across page reloads via localStorage (consistent with existing `dismissedThreads` pattern).
- Items appear in reverse chronological order (newest first)

### Section 2: Tasks

A unified flat list of all tasks across all channels, replacing the per-channel task pips.

**Layout per task:**
- Line 1: Task name + right-aligned channel tag (e.g., `#api`)
- Line 2: Multi-segment progress bar (owner color + reviewer color) with percentage label

**Sort order:** Most actionable first — in review → in progress → completed.

**Completed tasks:** Grouped under a collapsible "Completed today" divider, dimmed. Shows checkmark (✓) + name + channel tag, no progress bar. "Today" means calendar day in the user's local timezone.

**No colored status dots** — the progress bar color already encodes status.

### Section 3: Channels

A clean list for navigation. No task pips.

**Channel items:**
- `#` prefix, channel name
- Drag-to-reorder (preserving existing DnD implementation)
- "+ New channel" button at bottom

**Threads inline (flat mixed style):**
- Threads appear directly under their parent channel at the same font size
- Prefixed with `⌇` glyph to show thread relationship
- Unread indicator: blue dot right-aligned
- Close button (✕) on each thread

**Thread visibility model — `openThreads` set (server-side):**

Instead of tracking dismissed threads client-side (`dismissedThreads` in localStorage), track the inverse: an `openThreads` set of thread IDs the user wants visible. This set is persisted server-side so it syncs across all clients (mobile, desktop, multiple browsers).

- **Added to set when:** user opens a thread, or a new thread is created in the user's channel
- **Removed from set when:** user clicks ✕ (manual close), or thread auto-closes after 12 hours of no new messages
- **Re-added when:** a new message arrives in a previously closed thread (the thread becomes relevant again)

Only threads in `openThreads` appear in the channel list. The "needs attention" heuristic also runs against this set.

Requires a new daemon API: `GET/PUT /api/channels/{channel}/open-threads` to read/write the set. WebSocket should push updates when the set changes (e.g., new thread created by an agent).

**Thread lifecycle:**
- Thread in `openThreads` with no user action needed → visible under its channel
- Thread in `openThreads` that needs user action → moves to Needs Attention, disappears from channel list
- Thread idle (no new messages for 12 hours) → removed from `openThreads`, disappears
- User manually closes (✕) → removed from `openThreads`, disappears
- Recovering closed threads: search (⌘K) or scroll back in channel history

**Thread "needs attention" heuristic:** A thread moves to Needs Attention when:
1. The most recent message is not from the user
2. AND at least one of:
   - The most recent message is older than 10 minutes (catches forgotten/idle threads)
   - The message @mentions the user (surfaces immediately)
   - The message ends with `?` AND does not @mention someone else (surfaces immediately — the question is directed at the user, not another agent)

This means @mentions and direct questions surface immediately, while other threads get a 10-minute grace period before appearing. This catches threads where an agent asked the user something even if they forgot to @mention.

### DMs (unchanged)

Collapsible section, `@` prefixed names, unread badge when collapsed.

### Footer (unchanged)

- Push notification toggle (bell icon)
- Theme toggle (sun/moon)
- Account panel (user avatar and settings)

## Mobile Considerations

The redesign addresses mobile density issues through structural changes rather than a separate mobile layout:

- Removing nested task pips from channels reduces vertical space consumption
- Needs Attention at the top means the most important items are visible without scrolling
- Flat task list is more scannable than expanding individual channels
- Same font size for threads avoids the "too small" problem of the current nested display

The mobile sheet width (288px) and slide-out behavior remain unchanged.

## What's Preserved

- Project selector dropdown in header
- Search (⌘K) in header
- Channel drag-to-reorder
- Push notification toggle in footer
- Theme toggle (sun/moon) in footer
- Account switcher in footer
- Task progress bars (multi-segment, owner + reviewer colors)
- Thread close/dismiss affordance
- Sidebar resize handle (desktop)
- Keyboard shortcuts (⌘B toggle, ⌘K search)
- localStorage persistence for sidebar width and state
- Needs Attention dismiss state persisted via localStorage

## What's Removed

- Task pips nested under channels
- Colored status dots on tasks (redundant with progress bar colors)
- Smaller font size for threads (now same size as channels)
- Client-side `dismissedThreads` localStorage (replaced by server-side `openThreads`)

## What's New

- Needs Attention section with expanded trigger types (completed tasks, threads waiting on user, @mentions, stale work)
- Two-line attention items with contextual second line
- Unified Tasks section (flat list across all channels)
- "Completed today" collapsible section in Tasks
- Thread auto-close on idle (12 hours)
- Thread migration to Needs Attention when user action needed
- Server-side `openThreads` set (replaces client-side `dismissedThreads`), synced across clients
- Daemon API endpoint for `openThreads` read/write

## Backend Requirements

All four Needs Attention trigger types require changes beyond the frontend:

| Trigger | Data needed | Source |
|---------|------------|--------|
| Task completed | Task status change + PR number | Existing: task status is already in coworker data via WebSocket. PR number may need to be added to the task/coworker payload. |
| Thread waiting on user | Thread messages with sender + timestamp | Existing: message data is available client-side. Heuristic runs in the frontend by inspecting the last message in each tracked thread. |
| @mention | Message content parsing | Existing: `@mention` detection already exists in the codebase (`f6d666b9`). Extend to also check for `?`-ending messages per the heuristic above. |
| Stale work | Task progress + last update timestamp | Existing: coworker progress is already sent via WebSocket. Need to track "last progress change" timestamp client-side to detect staleness. |

Most triggers can be computed client-side from existing WebSocket data. The main new requirement is tracking temporal state (when progress last changed, when a message was received) to evaluate staleness and the 10-minute attention delay.

## Task Channel Display

When a task's `channel` field is undefined, display the project name (the main channel name) as the channel tag. This matches the current behavior where channel-less tasks appear under the main channel.

## Components Affected

- `ChannelList.svelte` — Remove task pip rendering, restructure into three sections
- `TaskRow.svelte` — Adapt for standalone task list (remove channel-context assumptions)
- `store.ts` — Add needs-attention item tracking, thread lifecycle state
- `types.ts` — Add NeedsAttentionItem type, thread lifecycle status
- New: `NeedsAttention.svelte` — Needs Attention section component
- New: `TasksSidebar.svelte` — Unified task list component
- Daemon: new RPC endpoint for `openThreads` per channel (read/write)
- Daemon: `openThreads` state persisted in `DaemonPersistentState`
- `api.ts` — Add `openThreads` API calls, replace `dismissedThreads` localStorage usage
