<script lang="ts">
import { openTaskThread } from "./api.ts";
import {
	computeCompletedTaskItems,
	computeStaleTaskItems,
	computeThreadAttentionItems,
	type LastMessage,
} from "./needsAttention.ts";
import {
	activeProject,
	coworkerMap as coworkerMapStore,
	kanbanData,
	progressTimestamps,
	reviewerByTaskId as reviewerByTaskIdStore,
	threadReadState,
	trackedThreads,
	userSenderName,
} from "./store.ts";
import TaskRow from "./TaskRow.svelte";
import { groupTasksByParent } from "./taskGrouping.ts";
import type { NeedsAttentionItem, Task, TrackedThread } from "./types.ts";

interface Props {
	onItemClick?: (item: { threadId?: string; taskId?: number; channel: string }) => void;
}

let { onItemClick }: Props = $props();

let olderCollapsed = $state(true);

const FIFTEEN_MINUTES_MS = 15 * 60 * 1000;

// ── Attention items (three independent branches) ────────────────────────────
// Each sub-derived reads only the stores it needs, so a kanban update doesn't
// recompute thread attention (and vice versa).

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

// Branch 1: Thread attention — depends on trackedThreads, threadReadState, userSenderName
const threadAttentionItems = $derived(
	computeThreadAttentionItems({
		trackedThreads: $trackedThreads,
		lastMessages: deriveLastMessages($trackedThreads),
		threadReadState: $threadReadState,
		userSender: $userSenderName,
	}),
);

// Branch 2: Completed tasks — depends on kanbanData, coworkerMapStore, threadReadState, activeProject
const completedTaskItems = $derived(
	computeCompletedTaskItems({
		tasks: [...($kanbanData.inProgress ?? []), ...($kanbanData.backlog ?? []), ...($kanbanData.completedTasks ?? [])],
		coworkerMap: $coworkerMapStore,
		threadReadState: $threadReadState,
		mainChannel: $activeProject ?? "midtown",
	}),
);

// Branch 3: Stale tasks — depends on kanbanData, coworkerMapStore, progressTimestamps, activeProject
const staleTaskItems = $derived(
	computeStaleTaskItems({
		tasks: [...($kanbanData.inProgress ?? []), ...($kanbanData.backlog ?? [])],
		coworkerMap: $coworkerMapStore,
		progressTimestamps: $progressTimestamps,
		mainChannel: $activeProject ?? "midtown",
	}),
);

// Combined + sorted — only re-sorts when any branch changes
const attentionItems = $derived(
	[...threadAttentionItems, ...completedTaskItems, ...staleTaskItems].sort((a, b) => b.timestamp - a.timestamp),
);

const typeConfig: Record<string, { accent: string; halo: string; icon: string }> = {
	task_completed: { accent: "hsl(var(--accent-green))", halo: "var(--accent-green)", icon: "✓" },
	thread_waiting: { accent: "hsl(var(--sidebar-ring))", halo: "var(--sidebar-ring)", icon: "↩" },
	mention: { accent: "hsl(var(--status-amber))", halo: "var(--status-amber)", icon: "@" },
	stale_work: { accent: "hsl(var(--status-red))", halo: "var(--status-red)", icon: "⏱" },
};

// ── Tasks ────────────────────────────────────────────────────────────────────

const mainChannel = $derived($activeProject ?? "midtown");

// Shared store-level derived (avoids per-component recomputation)
const reviewerByTaskId = $derived($reviewerByTaskIdStore);

// All tasks including completed, so children of active parents show as filled segments.
// Memoized by input-array references so groupTasksByParent's memo can hit.
const EMPTY_TASKS: Task[] = [];
let _prevInProgress: Task[] | undefined;
let _prevBacklog: Task[] | undefined;
let _prevCompleted: Task[] | undefined;
let _prevAllTasks: Task[] = [];

const allTasks = $derived.by(() => {
	const ip = $kanbanData.inProgress;
	const bl = $kanbanData.backlog;
	const ct = $kanbanData.completedTasks ?? EMPTY_TASKS;
	if (ip === _prevInProgress && bl === _prevBacklog && ct === _prevCompleted) {
		return _prevAllTasks;
	}
	_prevInProgress = ip;
	_prevBacklog = bl;
	_prevCompleted = ct;
	_prevAllTasks = [
		...ip.map((t) => ({ ...t, status: "in_progress" as const })),
		...bl.map((t) => ({ ...t, status: "pending" as const })),
		...ct.map((t) => ({ ...t, status: "completed" as const })),
	];
	return _prevAllTasks;
});

function taskSortKey(task: Task): number {
	if (reviewerByTaskId.get(String(task.id))) return 0;
	if (task.status === "in_progress") return 1;
	return 2;
}

// Group by parent, filter out completed parents, then sort
const groupedActiveTasks = $derived.by(() => {
	const groups = groupTasksByParent(allTasks);
	return groups.filter((g) => g.task.status !== "completed").sort((a, b) => taskSortKey(a.task) - taskSortKey(b.task));
});

const cwMap = $derived($coworkerMapStore);

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
// Exclusion sets are independent deriveds so allThreadsSorted doesn't cascade
// through attentionItems (which also reads kanbanData, causing a double-fire).

// Threads shown as attention items — derived from attentionItems, not kanbanData
const attentionThreadIds = $derived(new Set(attentionItems.filter((i) => i.threadId).map((i) => i.threadId)));

// Threads tied to active tasks — reads kanbanData directly (single cascade path)
const taskThreadIds = $derived(
	new Set([...$kanbanData.inProgress, ...$kanbanData.backlog].map((t) => t.thread_id).filter(Boolean)),
);

const allThreadsSorted = $derived.by(() => {
	const now = Date.now();

	return Object.entries($trackedThreads)
		.filter(([id]) => !attentionThreadIds.has(id) && !taskThreadIds.has(id))
		.map(([id, t]) => {
			const lastActivityMs = new Date(t.lastActivity).getTime();
			const isRecent = now - lastActivityMs < FIFTEEN_MINUTES_MS;
			const lastRead = $threadReadState[id];
			const isUnread = !lastRead || new Date(lastRead).getTime() < lastActivityMs;
			return { id, thread: t, lastActivityMs, isRecent, isUnread };
		})
		.sort((a, b) => b.lastActivityMs - a.lastActivityMs);
});

// Visible threads: recent (< 15 min) OR unread — always shown
const visibleThreads = $derived(allThreadsSorted.filter((t) => t.isRecent || t.isUnread));
// Older threads: not recent AND read — collapsed
const olderThreads = $derived(allThreadsSorted.filter((t) => !t.isRecent && !t.isUnread));

function handleThreadClick(threadId: string, channel: string) {
	onItemClick?.({ threadId, channel });
}

function handleAttentionClick(item: NeedsAttentionItem) {
	onItemClick?.({ threadId: item.threadId, taskId: item.taskId, channel: item.channel });
}
</script>

<div class="activity-feed flex flex-col gap-0.5">

  <!-- ── Attention items ──────────────────────────────────────────────── -->
  {#if attentionItems.length > 0}
    <div class="px-3 pt-1 pb-0.5">
      <span class="section-heading text-xs font-bold text-muted-foreground uppercase tracking-wide">Needs attention</span>
    </div>
    {#each attentionItems as item (item.id)}
      {@const config = typeConfig[item.type] ?? typeConfig.thread_waiting}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="mx-2 px-2.5 py-1.5 mb-0.5 cursor-pointer rounded-lg bg-background shadow-sm hover:shadow transition-shadow duration-150"
        onclick={() => handleAttentionClick(item)}
      >
        <!-- Line 1: icon + title + #channel pill -->
        <div class="flex items-center gap-1.5 text-xs">
          <span class="shrink-0" style="color: {config.accent}; font-size: 12px; line-height: 1; font-weight: 600;">{config.icon}</span>
          <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap text-sidebar-foreground font-medium">
            {item.title}
          </span>
          {#if item.channel}
            <span class="shrink-0 rounded px-1 py-px text-[9px] font-mono text-muted-foreground bg-sidebar-accent">#{item.channel}</span>
          {/if}
        </div>

        <!-- Line 2: context with colored worker name -->
        <div class="mt-0.5 text-[10px] text-muted-foreground/70 overflow-hidden text-ellipsis whitespace-nowrap">
          {#if item.workerName && item.workerColor && item.context.includes(item.workerName)}
            {@const parts = item.context.split(item.workerName)}
            {parts[0]}<span style="color: {item.workerColor}; font-weight: 500;">{item.workerName}</span>{parts.slice(1).join(item.workerName)}
          {:else}
            {item.context}
          {/if}
        </div>
      </div>
    {/each}
  {/if}

  <!-- ── Active tasks ─────────────────────────────────────────────────── -->
  {#if groupedActiveTasks.length > 0}
    {#each groupedActiveTasks as { task, children } (task.id)}
      {@const cw = task.owner ? cwMap.get(task.owner) ?? null : null}
      {@const reviewInfo = reviewerByTaskId.get(String(task.id))}
      {@const childReviewer = !reviewInfo?.reviewer ? children.find((c) => /review/i.test(c.subject))?.owner : undefined}
      {@const channelLabel = getChannelLabel(task)}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="px-3 py-[3px] rounded-[5px] hover:bg-sidebar-accent cursor-pointer"
        onclick={() => handleTaskClick(task)}
      >
        <TaskRow
          {task}
          {cw}
          {children}
          reviewer={reviewInfo?.reviewer ?? childReviewer}
          reviewPosted={reviewInfo?.reviewPosted ?? false}
          variant="row"
          onclick={() => handleTaskClick(task)}
          channelLabel={channelLabel}
        />
      </div>
    {/each}
  {/if}

  <!-- ── Visible threads (recent or unread) ─────────────────────────── -->
  {#each visibleThreads as { id, thread, isUnread } (id)}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="flex items-center gap-1.5 px-3 py-[5px] text-xs rounded-[5px] hover:bg-sidebar-accent cursor-pointer text-sidebar-foreground/80"
      onclick={() => handleThreadClick(id, thread.channelName)}
    >
      {#if isUnread}
        <span class="shrink-0 w-1.5 h-1.5 rounded-full bg-sidebar-ring"></span>
      {/if}
      <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap {isUnread ? 'text-sidebar-foreground font-medium' : ''}">
        {thread.subject}
      </span>
      <span class="shrink-0 rounded px-1 py-px text-[9px] font-mono text-muted-foreground bg-sidebar-accent">#{thread.channelName}</span>
    </div>
  {/each}

  <!-- ── Older read threads (collapsed) ───────────────────────────────── -->
  {#if olderThreads.length > 0}
    <button
      class="flex items-center gap-1.5 mx-3 mt-1 mb-0.5 px-2 py-1 border-none rounded cursor-pointer text-[10px] text-muted-foreground bg-transparent transition-colors duration-100 hover:bg-sidebar-accent"
      onclick={() => { olderCollapsed = !olderCollapsed; }}
    >
      <span class="text-[8px]">{olderCollapsed ? '▸' : '▾'}</span>
      <span>{olderThreads.length} older threads</span>
    </button>

    {#if !olderCollapsed}
      {#each olderThreads as { id, thread } (id)}
        <!-- svelte-ignore a11y_click_events_have_key_events -->
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="flex items-center gap-1.5 px-3 py-[5px] text-xs rounded-[5px] hover:bg-sidebar-accent cursor-pointer text-sidebar-foreground/80"
          onclick={() => handleThreadClick(id, thread.channelName)}
        >
          <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">
            {thread.subject}
          </span>
          <span class="shrink-0 rounded px-1 py-px text-[9px] font-mono text-muted-foreground bg-sidebar-accent">#{thread.channelName}</span>
        </div>
      {/each}
    {/if}
  {/if}
</div>

