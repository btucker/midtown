<script lang="ts">
import { openTaskThread } from "./api.ts";
import { getSenderColor } from "./messageUtils.ts";
import { coworkers, kanbanData } from "./store.ts";
import TaskRow from "./TaskRow.svelte";
import type { Task } from "./types.ts";

interface Props {
	mainChannelName?: string;
	onTaskClick?: (task: Task) => void;
}

let { mainChannelName = "midtown", onTaskClick }: Props = $props();

// Today's date string in local timezone (YYYY-MM-DD) for filtering completed tasks
function todayLocalDate(): string {
	const d = new Date();
	return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

// Active tasks: in_progress + backlog, annotated with status
const activeTasks = $derived([
	...$kanbanData.inProgress.map((t) => ({ ...t, status: "in_progress" as const })),
	...$kanbanData.backlog.map((t) => ({ ...t, status: "pending" as const })),
]);

// Sort order: review tasks first (have a reviewer), then in_progress, then pending/completed
function taskSortKey(task: Task): number {
	const reviewerMap = reviewerByTaskId;
	if (reviewerMap.get(String(task.id))) return 0;
	if (task.status === "in_progress") return 1;
	return 2;
}

// Map task_id → { reviewer, reviewPosted } from kanban review data
const reviewerByTaskId = $derived.by(() => {
	const map = new Map<string, { reviewer: string; reviewPosted: boolean }>();
	for (const pr of $kanbanData.review) {
		if (pr.task_id != null && pr.reviewer) {
			map.set(String(pr.task_id), { reviewer: pr.reviewer, reviewPosted: pr.review_posted || false });
		}
	}
	return map;
});

// Sorted active tasks
const sortedActiveTasks = $derived([...activeTasks].sort((a, b) => taskSortKey(a) - taskSortKey(b)));

// Map coworker name → coworker object
const cwMap = $derived(new Map($coworkers.map((cw) => [cw.name, cw])));

// Completed tasks filtered to today
const completedToday = $derived.by(() => {
	const today = todayLocalDate();
	const tasks = $kanbanData.completedTasks || [];
	// If tasks have no date field, fall back to showing all completedTasks
	// (the daemon may include only recent completions)
	return tasks.filter((t) => {
		// Try to filter by completed_at date if available
		const completedAt = (t as Task & { completed_at?: string }).completed_at;
		if (!completedAt) return true; // show if no date field
		return completedAt.startsWith(today);
	});
});

let completedCollapsed = $state(true);

function handleTaskClick(task: Task) {
	if (onTaskClick) {
		onTaskClick(task);
	} else {
		openTaskThread(task, task.channel || mainChannelName);
	}
}

function getChannelLabel(task: Task): string {
	return task.channel && task.channel !== mainChannelName ? task.channel : "";
}
</script>

<div class="flex flex-col gap-0.5 py-1">
  <!-- Section header -->
  <div class="flex items-center gap-1.5 px-3 py-1">
    <span style="font-size: 10px; color: #666; letter-spacing: 0.5px; text-transform: uppercase; font-weight: 600;">TASKS</span>
    <span style="background: #333; color: #aaa; font-size: 9px; padding: 1px 5px; border-radius: 8px; font-weight: 500;">{sortedActiveTasks.length}</span>
  </div>

  <!-- Active tasks -->
  {#each sortedActiveTasks as task}
    {@const cw = task.owner ? cwMap.get(task.owner) ?? null : null}
    {@const reviewInfo = reviewerByTaskId.get(String(task.id))}
    {@const channelLabel = getChannelLabel(task)}
    <div class="flex items-start gap-0 px-3 py-[3px] rounded-[5px] hover:bg-sidebar-accent cursor-pointer" onclick={() => handleTaskClick(task)}>
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

  {#if sortedActiveTasks.length === 0}
    <div class="px-3 py-2 text-[0.72rem] text-muted-foreground italic text-center">No active tasks</div>
  {/if}

  <!-- Completed today divider + list -->
  {#if completedToday.length > 0}
    <div class="flex items-center gap-1.5 px-3 mt-1">
      <div style="flex: 1; height: 1px; background: #2a2a4a;"></div>
      <button
        class="flex items-center gap-1 border-none bg-transparent cursor-pointer p-0"
        style="font-size: 9px; color: #555;"
        onclick={() => { completedCollapsed = !completedCollapsed; }}
      >
        <span>{completedCollapsed ? '▸' : '▾'}</span>
        <span>Completed today</span>
      </button>
      <div style="flex: 1; height: 1px; background: #2a2a4a;"></div>
    </div>

    {#if !completedCollapsed}
      <div style="opacity: 0.5;">
        {#each completedToday as task}
          {@const channelLabel = getChannelLabel(task)}
          <button
            class="w-full flex items-center gap-1.5 px-3 py-[4px] border-none bg-transparent cursor-pointer text-left rounded-[5px] hover:bg-sidebar-accent font-mono text-[0.72rem] text-muted-foreground"
            onclick={() => handleTaskClick(task)}
          >
            <span style="color: hsl(var(--accent-green)); flex-shrink: 0;">✓</span>
            <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">{task.subject}</span>
            {#if channelLabel}
              <span style="font-size: 10px; color: #555; flex-shrink: 0;">#{channelLabel}</span>
            {/if}
          </button>
        {/each}
      </div>
    {/if}
  {/if}
</div>
