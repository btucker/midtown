# Tool Block Auto-Collapse Design

## Problem

Tool blocks in DM channels and threads show full output permanently, creating visual noise. Completed tool calls should collapse over time so the chat remains scannable.

## Design

All tool blocks start with a preview state showing partial output, then auto-collapse to header-only after 30 seconds (based on message timestamp). Clicking expands fully.

### States

Each tool block has three states: **preview**, **collapsed**, **expanded**.

| Block | Preview (< 30s old) | Collapsed (>= 30s old) | Expanded (click) |
|-------|---------------------|------------------------|------------------|
| BashBlock | Command header + 1 output line + fade | Command header only | Full output |
| EditBlock | File path header + 6 diff lines + fade | File path header only | Full diff |
| TodoBlock | Fully expanded (all todos visible) | Header with summary (e.g. "Todos (2/3 done)") | Full list |
| ToolBlockGeneric | Summary header + 1 output line + fade | Summary header only | Full JSON |

### Age detection

- Each block receives the parent message's timestamp via a `timestamp` prop.
- On mount: if `Date.now() - timestamp > 30_000`, render collapsed immediately.
- Otherwise: render preview, schedule a timeout for the remaining time to transition to collapsed.
- The timeout is cleaned up on unmount.

### User interaction

- Clicking a collapsed or preview block toggles to expanded.
- Clicking an expanded block toggles to collapsed.
- Once the user manually expands, the auto-collapse timer is cancelled — the block stays in whatever state the user set.

### Fade effect

Preview states use a CSS gradient overlay on the bottom edge (the existing `bash-collapsed::after` linear-gradient pattern) to indicate more content is available.

### Implementation approach

Add a shared `autoCollapseState(timestamp)` helper that returns a reactive state (`'preview' | 'collapsed' | 'expanded'`) and manages the timer. Each block component uses this to determine its render mode.

## Files changed

- `web-app/src/lib/BashBlock.svelte` — add timestamp prop, auto-collapse states
- `web-app/src/lib/EditBlock.svelte` — add timestamp prop, preview/collapsed states, clickable header
- `web-app/src/lib/TodoBlock.svelte` — add timestamp prop, collapsed state with summary header
- `web-app/src/lib/ToolBlockGeneric.svelte` — add timestamp prop, auto-collapse states
- `web-app/src/lib/ToolDataBlocks.svelte` — pass message timestamp to child blocks
- `web-app/src/lib/MessageRow.svelte` — pass msg.timestamp to ToolDataBlocks
