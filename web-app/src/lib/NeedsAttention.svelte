<script lang="ts">
import { computeAttentionItems, type LastMessage } from "./needsAttention.ts";
import {
	activeProject,
	coworkers,
	dismissedAttentionItems,
	kanbanData,
	openThreads,
	progressTimestamps,
	trackedThreads,
	userSenderName,
} from "./store.ts";
import type { NeedsAttentionItem, TrackedThread } from "./types.ts";

interface Props {
	onItemClick?: (item: NeedsAttentionItem) => void;
}

let { onItemClick }: Props = $props();

function deriveLastMessages(threads: Record<string, TrackedThread>): Record<string, LastMessage> {
	const result: Record<string, LastMessage> = {};
	for (const [id, t] of Object.entries(threads)) {
		if (t.lastReplySender && t.fullText) {
			result[id] = { sender: t.lastReplySender, content: t.fullText, timestamp: t.lastActivity };
		}
	}
	return result;
}

const items = $derived(
	computeAttentionItems({
		trackedThreads: $trackedThreads,
		openThreads: $openThreads,
		lastMessages: deriveLastMessages($trackedThreads),
		coworkers: $coworkers,
		tasks: [...($kanbanData.inProgress ?? []), ...($kanbanData.backlog ?? []), ...($kanbanData.completedTasks ?? [])],
		progressTimestamps: $progressTimestamps,
		dismissed: $dismissedAttentionItems,
		userSender: $userSenderName,
		mainChannel: $activeProject ?? "midtown",
	}),
);

const typeConfig: Record<string, { bg: string; border: string; icon: string }> = {
	task_completed: {
		bg: "rgba(74,222,128,0.06)",
		border: "rgba(74,222,128,0.12)",
		icon: "✓",
	},
	thread_waiting: {
		bg: "rgba(59,130,246,0.06)",
		border: "rgba(59,130,246,0.12)",
		icon: "↩",
	},
	mention: {
		bg: "rgba(245,158,11,0.06)",
		border: "rgba(245,158,11,0.12)",
		icon: "@",
	},
	stale_work: {
		bg: "rgba(239,68,68,0.06)",
		border: "rgba(239,68,68,0.12)",
		icon: "⏱",
	},
};

function dismiss(e: MouseEvent, item: NeedsAttentionItem) {
	e.stopPropagation();
	dismissedAttentionItems.update((s) => {
		s.add(item.id);
		return new Set(s);
	});
}
</script>

{#if items.length > 0}
  <div class="mb-2">
    <!-- Section header -->
    <div class="flex items-center gap-1.5 px-1 mb-1.5">
      <span
        class="text-muted-foreground/60 font-semibold"
        style="font-size: 10px; letter-spacing: 0.5px; text-transform: uppercase;"
      >
        Needs Attention
      </span>
      <span
        style="background: #f59e0b; color: #000; font-size: 9px; padding: 1px 5px; border-radius: 8px; font-weight: 600;"
      >
        {items.length}
      </span>
    </div>

    <!-- Items -->
    {#each items as item (item.id)}
      {@const config = typeConfig[item.type] ?? typeConfig.thread_waiting}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="attention-item group relative cursor-pointer"
        style="background: {config.bg}; border: 1px solid {config.border}; padding: 8px; border-radius: 6px; margin-bottom: 3px;"
        onclick={() => onItemClick?.(item)}
      >
        <!-- Line 1: icon + title + dismiss -->
        <div class="flex items-center gap-1.5" style="font-size: 12px;">
          <span class="shrink-0 text-muted-foreground/70" style="font-size: 11px; line-height: 1;">{config.icon}</span>
          <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-sidebar-foreground">
            {item.title}
          </span>
          <button
            class="dismiss-btn shrink-0 text-muted-foreground/40 hover:text-muted-foreground/80 border-none bg-transparent p-0 cursor-pointer leading-none"
            style="font-size: 11px; opacity: 0; transition: opacity 0.15s;"
            onclick={(e) => dismiss(e, item)}
            aria-label="Dismiss"
          >✕</button>
        </div>

        <!-- Line 2: context with colored worker name -->
        <div class="mt-0.5 text-muted-foreground/50 overflow-hidden text-ellipsis whitespace-nowrap" style="font-size: 11px;">
          {#if item.workerName && item.workerColor && item.context.includes(item.workerName)}
            {@const parts = item.context.split(item.workerName)}
            {parts[0]}<span style="color: {item.workerColor}; font-weight: 500;">{item.workerName}</span>{parts.slice(1).join(item.workerName)}
          {:else}
            {item.context}
          {/if}
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .attention-item:hover .dismiss-btn {
    opacity: 1 !important;
  }
</style>
