<script lang="ts">
import { useSidebar } from "$lib/components/ui/sidebar/context.svelte.ts";
import { openTaskThread } from "./api.ts";
import { coworkers, kanbanData } from "./store.ts";
import TaskRow from "./TaskRow.svelte";
import { groupTasksByParent } from "./taskGrouping.ts";

const sidebar = useSidebar();

let { channelName = "" } = $props();

/**
 * Filter tasks by channel field.
 * For 'midtown' channel, return all tasks.
 * For topic channels, only return tasks explicitly assigned to that channel.
 *
 * This matches the TUI implementation in src/bin/midtown/cli/chat/ui/board.rs
 * which groups tasks by `task.channel.as_deref().unwrap_or(main_channel)`.
 */
function filterTasksByChannel(tasks, channel) {
	if (channel === "midtown") {
		// Main channel shows only tasks with no channel (or channel='midtown').
		// Tasks assigned to other channels appear there only, not duplicated here.
		// Matches TUI's unwrap_or(main_channel) grouping.
		return tasks.filter((task) => !task.channel || task.channel === "midtown");
	}

	// Topic channels only show tasks explicitly assigned to that channel via the channel field
	return tasks.filter((task) => task.channel === channel);
}

// Derived: tasks for this channel, grouped by parent-child hierarchy.
// Include completed tasks so children of active parents show as filled segments.
const groupedTasks = $derived.by(() => {
	const allTasks = [
		...$kanbanData.inProgress.map((t) => ({ ...t, status: "in_progress" })),
		...$kanbanData.backlog.map((t) => ({ ...t, status: "pending" })),
		...($kanbanData.completedTasks || []).map((t) => ({ ...t, status: "completed" })),
	];
	const filtered = filterTasksByChannel(allTasks, channelName);
	const groups = groupTasksByParent(filtered);
	// Only show groups whose parent is still active (not completed top-level tasks)
	return groups.filter((g) => g.task.status !== "completed");
});

// Map coworker name → coworker object for progress/phase lookup
const cwMap = $derived(new Map($coworkers.map((cw) => [cw.name, cw])));

// Map task_id → { reviewer, reviewPosted } for showing reviewer avatar + glow state
const taskReviewerMap = $derived.by(() => {
	const map = new Map();
	for (const pr of $kanbanData.review) {
		if (pr.task_id != null && pr.reviewer) {
			map.set(String(pr.task_id), { reviewer: pr.reviewer, reviewPosted: pr.review_posted || false });
		}
	}
	return map;
});

function handleTaskClick(task) {
	if (sidebar.isMobile) sidebar.setOpenMobile(false);
	openTaskThread(task, task.channel || channelName);
}
</script>

<div class="flex flex-col gap-0.5 py-1 pb-1.5">
  {#each groupedTasks as { task, children }}
    {@const cw = task.owner ? cwMap.get(task.owner) : null}
    {@const reviewInfo = taskReviewerMap.get(String(task.id))}
    {@const childReviewer = !reviewInfo?.reviewer ? children.find((c) => /review/i.test(c.subject))?.owner : undefined}
    <TaskRow
      {task}
      {cw}
      {children}
      reviewer={reviewInfo?.reviewer ?? childReviewer}
      reviewPosted={reviewInfo?.reviewPosted ?? false}
      onclick={() => handleTaskClick(task)}
    />
  {/each}

  {#if groupedTasks.length === 0}
    <div class="px-3 py-2 text-[0.72rem] text-muted-foreground italic text-center">No active tasks</div>
  {/if}
</div>
