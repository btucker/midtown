<script>
import Check from "@lucide/svelte/icons/check";
import X from "@lucide/svelte/icons/x";
import { dismissThread, openThread } from "./api.js";
import { useSidebar } from "$lib/components/ui/sidebar/context.svelte.js";
import { getChannelThreads, getCompletedTaskThreadIds, getTaskThreadIds } from "./channelUtils.js";
import { dismissedThreads, kanbanData, messagesByChannel, threadUnreadCounts, trackedThreads } from "./store.js";

const sidebar = useSidebar();

let { channelName = "" } = $props();

// Build the set of thread IDs already represented by active tasks
const taskThreadIds = $derived(getTaskThreadIds($kanbanData));
// Build the set of thread IDs from completed tasks
const completedTaskThreadIds = $derived(getCompletedTaskThreadIds($kanbanData));

// Threads for this channel, filtered and sorted (pure derivation — no side effects)
const threadData = $derived(
	getChannelThreads(channelName, $trackedThreads, $threadUnreadCounts, taskThreadIds, completedTaskThreadIds),
);
const channelThreads = $derived(threadData.threads);

// Side effect: permanently remove task-backed threads from stores
$effect(() => {
	const ids = threadData.toClean;
	if (ids.length === 0) return;
	trackedThreads.update((t) => {
		const next = { ...t };
		for (const id of ids) delete next[id];
		return next;
	});
	dismissedThreads.update((s) => new Set([...s, ...ids]));
});

function handleClick(thread) {
	if (sidebar.isMobile) sidebar.setOpenMobile(false);
	// Try to find the parent message in the channel's message store
	const channelMsgs = $messagesByChannel[channelName] || [];
	const parentMsg = channelMsgs.find((m) => m.id === thread.id);
	// Use the real message if available, otherwise a stub
	const msg = parentMsg || { id: thread.id, from: "", content: thread.subject || "" };
	openThread(msg, channelName);
}

function handleDismiss(e, threadId) {
	e.stopPropagation();
	dismissThread(threadId);
}
</script>

<div class="thread-list" data-testid="thread-list">
  {#each channelThreads as thread}
    {@const hasUnread = thread.unread > 0}
    {@const isCompleted = thread.completed}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="thread-row"
      class:unread={hasUnread}
      class:completed={isCompleted}
      role="button"
      tabindex="0"
      data-testid="sidebar-thread-row"
      data-thread-id={thread.id}
      title={thread.fullText}
      onclick={() => handleClick(thread)}
      onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') handleClick(thread) }}
    >
      {#if isCompleted}
        <span class="completed-check" title="Task completed"><Check size={10} /></span>
      {:else}
        <span class="accent-line" class:accent-unread={hasUnread}></span>
      {/if}
      <div class="thread-content">
        <span class="thread-subject" data-testid="sidebar-thread-subject">{thread.subject}</span>
        {#if hasUnread}
          <span class="unread-badge" data-testid="sidebar-thread-unread-badge">{thread.unread}</span>
        {/if}
      </div>
      <button
        class="dismiss-btn"
        data-testid="sidebar-thread-dismiss"
        title="Stop tracking this thread"
        onclick={(e) => handleDismiss(e, thread.id)}
      >
        <X size={10} />
      </button>
    </div>
  {/each}
</div>

<style>
  .thread-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 2px 0 4px;
  }

  .thread-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 6px 3px 0;
    border: none;
    background: transparent;
    cursor: pointer;
    border-radius: 4px;
    transition: background 0.1s;
    text-align: left;
    font-family: var(--font-mono);
    font-size: 0.68rem;
    line-height: 1.3;
    color: hsl(var(--muted-foreground) / 0.7);
    position: relative;
  }

  .thread-row:hover {
    background: hsl(var(--sidebar-accent));
  }

  .thread-row.unread {
    color: hsl(var(--sidebar-foreground));
  }

  .thread-row.completed {
    color: hsl(var(--accent-green, 142 71% 45%) / 0.8);
  }

  .accent-line {
    width: 2px;
    align-self: stretch;
    border-radius: 1px;
    flex-shrink: 0;
    background: hsl(var(--muted-foreground) / 0.2);
  }

  .accent-unread {
    background: hsl(var(--accent-teal));
  }

  .completed-check {
    width: 2px;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: hsl(var(--accent-green, 142 71% 45%));
  }

  .thread-content {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .thread-subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .unread-badge {
    flex-shrink: 0;
    min-width: 14px;
    height: 14px;
    padding: 0 3px;
    border-radius: 7px;
    background: hsl(var(--accent-teal));
    color: white;
    font-size: 0.55rem;
    font-weight: 700;
    display: flex;
    align-items: center;
    justify-content: center;
    line-height: 1;
  }

  .dismiss-btn {
    flex-shrink: 0;
    width: 16px;
    height: 16px;
    padding: 0;
    border: none;
    background: transparent;
    border-radius: 3px;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    color: hsl(var(--muted-foreground) / 0.4);
    opacity: 0;
    transition: opacity 0.1s, color 0.1s, background 0.1s;
  }

  .thread-row:hover .dismiss-btn {
    opacity: 1;
  }

  .dismiss-btn:hover {
    color: hsl(var(--muted-foreground));
    background: hsl(var(--sidebar-accent));
  }
</style>
