<script lang="ts">
import { openTaskThread } from "./api.ts";
import { getSenderColor } from "./messageUtils.ts";
import { computeAttentionItems, type LastMessage } from "./needsAttention.ts";
import {
	activeProject,
	channelReadState,
	coworkers,
	kanbanData,
	progressTimestamps,
	threadReadState,
	trackedThreads,
	userSenderName,
} from "./store.ts";
import TaskRow from "./TaskRow.svelte";
import type { NeedsAttentionItem, Task, TrackedThread } from "./types.ts";

interface Props {
	onItemClick?: (item: { threadId?: string; taskId?: number; channel: string }) => void;
}

let { onItemClick }: Props = $props();

let olderCollapsed = $state(true);

const FIFTEEN_MINUTES_MS = 15 * 60 * 1000;

// ── Attention items ──────────────────────────────────────────────────────────

function deriveLastMessages(threads: Record<string, TrackedThread>): Record<string, LastMessage> {
	const result: Record<string, LastMessage> = {};
	for (const [id, t] of Object.entries(threads)) {
		if (t.lastReplySender) {
			result[id] = {
				sender: t.lastReplySender,
				content: t.fullText || t.subject,
				timestamp: t.lastActivity,
			};
		}
	}
	return result;
}

const attentionItems = $derived(
	computeAttentionItems({
		trackedThreads: $trackedThreads,
		lastMessages: deriveLastMessages($trackedThreads),
		coworkers: $coworkers,
		tasks: [...($kanbanData.inProgress ?? []), ...($kanbanData.backlog ?? []), ...($kanbanData.completedTasks ?? [])],
		progressTimestamps: $progressTimestamps,
		threadReadState: $threadReadState,
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

// ── Tasks ────────────────────────────────────────────────────────────────────

const mainChannel = $derived($activeProject ?? "midtown");

// Map task_id → { reviewer, reviewPosted } from kanban review data
const reviewerByTaskId = $derived.by(() => {
	const map = new Map<string, { reviewer: string; reviewPosted: boolean }>();
	for (const pr of $kanbanData.review) {
		if (pr.task_id != null && pr.reviewer) {
			map.set(String(pr.task_id), {
				reviewer: pr.reviewer,
				reviewPosted: pr.review_posted || false,
			});
		}
	}
	return map;
});

const activeTasks = $derived([
	...$kanbanData.inProgress.map((t) => ({ ...t, status: "in_progress" as const })),
	...$kanbanData.backlog.map((t) => ({ ...t, status: "pending" as const })),
]);

function taskSortKey(task: Task): number {
	if (reviewerByTaskId.get(String(task.id))) return 0;
	if (task.status === "in_progress") return 1;
	return 2;
}

const sortedActiveTasks = $derived([...activeTasks].sort((a, b) => taskSortKey(a) - taskSortKey(b)));

const cwMap = $derived(new Map($coworkers.map((cw) => [cw.name, cw])));

function getChannelLabel(task: Task): string {
	return task.channel && task.channel !== mainChannel ? task.channel : "";
}

function handleTaskClick(task: Task) {
	if (onItemClick) {
		onItemClick({ taskId: task.id, channel: task.channel || mainChannel });
	} else {
		openTaskThread(task, task.channel || mainChannel);
	}
}

// ── Threads ──────────────────────────────────────────────────────────────────

const allThreadsSorted = $derived.by(() => {
	const now = Date.now();
	// Exclude threads that are already shown as attention items
	const attentionThreadIds = new Set(attentionItems.filter((i) => i.threadId).map((i) => i.threadId));
	// Exclude threads tied to active tasks
	const taskThreadIds = new Set(
		[...$kanbanData.inProgress, ...$kanbanData.backlog].map((t) => t.thread_id).filter(Boolean),
	);

	return Object.entries($trackedThreads)
		.filter(([id]) => !attentionThreadIds.has(id) && !taskThreadIds.has(id))
		.map(([id, t]) => {
			const lastActivityMs = new Date(t.lastActivity).getTime();
			const isRecent = now - lastActivityMs < FIFTEEN_MINUTES_MS;
			const lastRead = $threadReadState[id];
			const isUnread = !lastRead || new Date(lastRead) < new Date(t.lastActivity);
			return { id, thread: t, lastActivityMs, isRecent, isUnread };
		})
		.sort((a, b) => b.lastActivityMs - a.lastActivityMs);
});

const recentThreads = $derived(allThreadsSorted.filter((t) => t.isRecent));
const olderThreads = $derived(allThreadsSorted.filter((t) => !t.isRecent));
const olderUnreadCount = $derived(olderThreads.filter((t) => t.isUnread).length);

function handleThreadClick(threadId: string, channel: string) {
	onItemClick?.({ threadId, channel });
}

function handleAttentionClick(item: NeedsAttentionItem) {
	onItemClick?.({ threadId: item.threadId, taskId: item.taskId, channel: item.channel });
}
</script>

<div class="flex flex-col gap-0.5">
  <!-- Section header -->
  <div class="flex items-center gap-1.5 px-3 py-1">
    <span
      style="font-size: 10px; color: #666; letter-spacing: 0.5px; text-transform: uppercase; font-weight: 600;"
    >ACTIVITY</span>
  </div>

  <!-- ── Attention items ──────────────────────────────────────────────── -->
  {#each attentionItems as item (item.id)}
    {@const config = typeConfig[item.type] ?? typeConfig.thread_waiting}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="mx-2 cursor-pointer"
      style="background: {config.bg}; border: 1px solid {config.border}; padding: 8px; border-radius: 6px; margin-bottom: 3px;"
      onclick={() => handleAttentionClick(item)}
    >
      <!-- Line 1: icon + title + #channel -->
      <div class="flex items-center gap-1.5" style="font-size: 12px;">
        <span class="shrink-0 text-muted-foreground/70" style="font-size: 11px; line-height: 1;">{config.icon}</span>
        <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-sidebar-foreground">
          {item.title}
        </span>
        {#if item.channel}
          <span class="shrink-0" style="font-size: 10px; color: #555;">#{item.channel}</span>
        {/if}
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

  <!-- ── Active tasks ─────────────────────────────────────────────────── -->
  {#each sortedActiveTasks as task (task.id)}
    {@const cw = task.owner ? cwMap.get(task.owner) ?? null : null}
    {@const reviewInfo = reviewerByTaskId.get(String(task.id))}
    {@const channelLabel = getChannelLabel(task)}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="flex items-start gap-0 px-3 py-[3px] rounded-[5px] hover:bg-sidebar-accent cursor-pointer"
      onclick={() => handleTaskClick(task)}
    >
      <div class="flex-1 min-w-0">
        <TaskRow
          {task}
          {cw}
          reviewer={reviewInfo?.reviewer ?? null}
          reviewPosted={reviewInfo?.reviewPosted ?? false}
          variant="row"
          onclick={() => handleTaskClick(task)}
        />
      </div>
      {#if channelLabel}
        <span
          class="shrink-0 mt-[5px] ml-1"
          style="font-size: 10px; color: #555;"
          title={channelLabel}
        >#{channelLabel}</span>
      {/if}
    </div>
  {/each}

  <!-- ── Recent threads (< 15 min) ───────────────────────────────────── -->
  {#each recentThreads as { id, thread, isUnread } (id)}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="flex items-center gap-1.5 px-3 py-[3px] rounded-[5px] hover:bg-sidebar-accent cursor-pointer"
      style="font-size: 12px; color: #bbb;"
      onclick={() => handleThreadClick(id, thread.channelName)}
    >
      <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
        {thread.subject}
      </span>
      {#if isUnread}
        <span class="shrink-0 w-1.5 h-1.5 rounded-full" style="background: #3b82f6;"></span>
      {/if}
      <span class="shrink-0" style="font-size: 10px; color: #555;">#{thread.channelName}</span>
    </div>
  {/each}

  <!-- ── Older threads (collapsed) ────────────────────────────────────── -->
  {#if olderThreads.length > 0}
    <div class="flex items-center gap-1.5 px-3 mt-1">
      <div style="flex: 1; height: 1px; background: #2a2a4a;"></div>
      <button
        class="flex items-center gap-1 border-none bg-transparent cursor-pointer p-0"
        style="font-size: 9px; color: #555;"
        onclick={() => { olderCollapsed = !olderCollapsed; }}
      >
        <span>{olderCollapsed ? '▸' : '▾'}</span>
        <span>{olderThreads.length} older threads{olderUnreadCount > 0 ? ` · ${olderUnreadCount} unread` : ''}</span>
      </button>
      <div style="flex: 1; height: 1px; background: #2a2a4a;"></div>
    </div>

    {#if !olderCollapsed}
      {#each olderThreads as { id, thread, isUnread } (id)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="flex items-center gap-1.5 px-3 py-[3px] rounded-[5px] hover:bg-sidebar-accent cursor-pointer"
          style="font-size: 12px; color: #bbb;"
          onclick={() => handleThreadClick(id, thread.channelName)}
        >
          <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
            {thread.subject}
          </span>
          {#if isUnread}
            <span class="shrink-0 w-1.5 h-1.5 rounded-full" style="background: #3b82f6;"></span>
          {/if}
          <span class="shrink-0" style="font-size: 10px; color: #555;">#{thread.channelName}</span>
        </div>
      {/each}
    {/if}
  {/if}
</div>
