<script>
/**
 * ThreadActivityDrawer — slide-up drawer above the thread reply input showing tool call activity.
 *
 * Reads tool call data from msg.tool_data on thread messages (unified path).
 *
 * Props:
 *   messages       — array of thread messages to scan for tool_data
 *   channelName    — the channel name (used for reset tracking)
 *   threadParentId — thread identity (used for reset tracking and fork owner color)
 *   thinking       — true when the user just sent a message and we're waiting for tool calls
 *
 * When inlineToolCalls is false (channel setting), clicking the drawer expands it into
 * a scrollable panel showing the full tool call history. Click again or press Escape
 * to collapse. When inlineToolCalls is true (default), the drawer shows only typing
 * dots and tool blocks render inline in the message stream instead.
 */

import { onDestroy } from "svelte";
import { slide } from "svelte/transition";
import { getForkOwnerColor } from "./avenue-colors.js";
import { threadForkOwners } from "./store.js";

let { messages = [], channelName, threadParentId = null, thinking = false } = $props();

// Use the fork owner's avenue color for thinking dots instead of hardcoded lead gold.
let dotColor = $derived(getForkOwnerColor($threadForkOwners[threadParentId]));

const AGE_OUT_MS = 3000;
const MAX_VISIBLE = 10;

let expanded = $state(false);
let scrollContainer = $state(null);

// Two-phase age-out:
//   completedAt  Map<call_id, timestampMs> — when each tool completed
//   expired      Set<call_id>              — items hidden after AGE_OUT_MS
let completedAt = $state(new Map());
let expired = $state(new Set());

// Reset state when the thread changes
$effect(() => {
	channelName; // track dependency
	threadParentId; // track dependency
	completedAt = new Map();
	expired = new Set();
	expanded = false;
});

// Build merged view from msg.tool_data on thread messages.
// Collect all ToolBlocks, correlate by call_id to determine status.
let merged = $derived.by(() => {
	// Collect all tool blocks from messages
	const allBlocks = [];
	for (const msg of messages) {
		if (msg.tool_data?.length) {
			for (const block of msg.tool_data) {
				allBlocks.push(block);
			}
		}
	}
	// Build completion status map: call_id → 'error' | 'ok'
	const resultStatus = {};
	for (const block of allBlocks) {
		if (block.call_id && block.output != null) {
			resultStatus[block.call_id] = block.error ? "error" : "ok";
		}
	}
	// Deduplicate by call_id: keep only the first occurrence (the tool_use block)
	// to avoid showing duplicate entries when a result block arrives later.
	const seen = new Set();
	const out = [];
	for (const block of allBlocks) {
		if (!block.call_id || seen.has(block.call_id)) continue;
		seen.add(block.call_id);
		out.push({ block, status: resultStatus[block.call_id] ?? null });
	}
	return out;
});

// Interval: record completion timestamps; move items to `expired` after AGE_OUT_MS.
const intervalId = setInterval(() => {
	const now = Date.now();
	let changedCompleted = false;
	let changedExpired = false;
	const newCompleted = new Map(completedAt);
	const newExpired = new Set(expired);

	// Phase 1: stamp newly completed items
	for (const entry of merged) {
		if (entry.status !== null && entry.block.call_id && !newCompleted.has(entry.block.call_id)) {
			newCompleted.set(entry.block.call_id, now);
			changedCompleted = true;
		}
	}

	// Phase 2: expire stale items (skip when expanded)
	if (!expanded) {
		for (const [id, ts] of newCompleted) {
			if (!newExpired.has(id) && now - ts >= AGE_OUT_MS) {
				newExpired.add(id);
				changedExpired = true;
			}
		}
	}

	if (changedCompleted) completedAt = newCompleted;
	if (changedExpired) expired = newExpired;
}, 500);

onDestroy(() => clearInterval(intervalId));

// Visible list: exclude expired items, cap at MAX_VISIBLE newest
let visibleItems = $derived(merged.filter((entry) => !expired.has(entry.block.call_id)).slice(-MAX_VISIBLE));

let displayItems = $derived(expanded ? merged : visibleItems);

let isVisible = $derived(merged.length > 0 || thinking);

// Auto-scroll to bottom when new items arrive in expanded mode
const SCROLL_THRESHOLD = 50;
$effect(() => {
	if (expanded && scrollContainer && displayItems.length) {
		const { scrollTop, scrollHeight, clientHeight } = scrollContainer;
		const isNearBottom = scrollHeight - scrollTop - clientHeight < SCROLL_THRESHOLD;
		if (isNearBottom) {
			requestAnimationFrame(() => {
				if (scrollContainer) {
					scrollContainer.scrollTop = scrollContainer.scrollHeight;
				}
			});
		}
	}
});

function toggleExpanded() {
	if (expanded) {
		const now = Date.now();
		const refreshed = new Map(completedAt);
		let changed = false;
		for (const [id, ts] of refreshed) {
			if (now - ts >= AGE_OUT_MS && !expired.has(id)) {
				refreshed.set(id, now);
				changed = true;
			}
		}
		if (changed) completedAt = refreshed;
	}
	expanded = !expanded;
}

function handleKeydown(e) {
	if (e.key === "Escape" && expanded) {
		toggleExpanded();
	} else if (e.key === "Enter" || e.key === " ") {
		e.preventDefault();
		toggleExpanded();
	}
}

function describeBlock(block) {
	if (block.tool_name === "Bash" && block.input?.command) {
		const cmd = block.input.command;
		return cmd.length > 60 ? `${cmd.slice(0, 57)}...` : cmd;
	}
	if (block.input?.file_path) {
		const fp = block.input.file_path;
		const short = fp.split("/").slice(-2).join("/");
		return `${block.tool_name.toLowerCase()} ${short}`;
	}
	return block.tool_name?.toLowerCase() || "?";
}

let hiddenCount = $derived(merged.length - visibleItems.length);
</script>

{#if isVisible}
  <!-- svelte-ignore a11y_no_static_element_interactions a11y_no_noninteractive_tabindex -->
  <div
    class="activity-drawer border-t border-border {expanded ? 'expanded' : ''}"
    class:cursor-pointer={merged.length > 0}
    data-testid="thread-activity-drawer"
    onclick={merged.length > 0 ? toggleExpanded : undefined}
    onkeydown={handleKeydown}
    role={merged.length > 0 ? 'button' : undefined}
    tabindex={merged.length > 0 ? 0 : undefined}
    aria-expanded={merged.length > 0 ? expanded : undefined}
    aria-label={merged.length > 0 ? (expanded ? 'Collapse tool call history' : 'Expand tool call history') : undefined}
    transition:slide={{ duration: 180 }}
  >
    {#if merged.length > 0}
      <!-- Header bar with chevron and count -->
      <div class="flex items-center justify-between px-3 py-1 select-none">
        <span class="text-[0.72rem] text-muted-foreground/50 font-mono">
          {#if expanded}
            {merged.length} tool call{merged.length !== 1 ? 's' : ''}
          {:else if hiddenCount > 0}
            +{hiddenCount} more
          {/if}
        </span>
        <span class="text-[0.72rem] text-muted-foreground/40 transition-transform duration-150" class:rotate-180={expanded}>
          ▾
        </span>
      </div>
    {/if}

    <div
      bind:this={scrollContainer}
      class="px-3 {expanded ? 'overflow-y-auto' : 'overflow-hidden'}"
      class:pb-1.5={!expanded || displayItems.length === 0}
      style={expanded ? 'max-height: 50vh;' : ''}
    >
      {#each displayItems as entry (entry.block.call_id)}
        {@const dimmed = expanded && completedAt.has(entry.block.call_id)}
        <div class="flex items-center gap-[6px] py-[1px]" class:opacity-45={dimmed}>
          <span class="flex-shrink-0 select-none text-[0.82rem] leading-[1.35] text-muted-foreground/60">
            {#if entry.status === 'error'}
              <span class="text-destructive">✗</span>
            {:else if entry.status === 'ok'}
              ✓
            {:else}
              ›
            {/if}
          </span>
          <span
            class="font-mono text-[0.82rem] leading-[1.35] whitespace-nowrap overflow-hidden text-ellipsis min-w-0 {entry.status === 'error' ? 'text-destructive' : 'text-muted-foreground'}"
          >{describeBlock(entry.block)}</span>
        </div>
      {/each}
      {#if thinking}
        <div class="flex items-center gap-[6px] py-[1px]">
          <span class="typing-dots flex gap-[3px] items-center">
            <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {dotColor}"></span>
            <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {dotColor}"></span>
            <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {dotColor}"></span>
          </span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .activity-drawer {
    background: hsl(var(--card));
    transition: max-height 0.2s ease;
  }

  .activity-drawer:hover {
    background: hsl(var(--accent));
  }

  .activity-drawer.expanded {
    background: hsl(var(--card));
  }

  .activity-drawer.expanded:hover {
    background: hsl(var(--card));
  }
</style>
