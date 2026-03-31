<script lang="ts">
import X from "@lucide/svelte/icons/x";
import { useSidebar } from "$lib/components/ui/sidebar/context.svelte.ts";
import { dismissThread, openThread } from "./api.ts";
import { getChannelThreads, getCompletedTaskThreadIds, getTaskThreadIds } from "./channelUtils.ts";
import { kanbanData, messagesByChannel, threadUnreadCounts, trackedThreads } from "./store.ts";
import type { Message } from "./types.ts";

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
});

function handleClick(thread: { id: string; subject: string }) {
	if (sidebar.isMobile) sidebar.setOpenMobile(false);
	// Try to find the parent message in the channel's message store
	const channelMsgs = $messagesByChannel[channelName] || [];
	const parentMsg = channelMsgs.find((m: Message) => m.id === thread.id);
	// Use the real message if available, otherwise a stub
	const msg: Message = parentMsg || { id: thread.id, from: "", content: thread.subject || "", timestamp: "" };
	openThread(msg, channelName);
}

function handleDismiss(e: MouseEvent, threadId: string) {
	e.stopPropagation();
	dismissThread(threadId);
}
</script>

<div class="thread-list" data-testid="thread-list">
  {#each channelThreads as thread}
    {@const hasUnread = thread.unread > 0}
    <button
      class="thread-row"
      class:unread={hasUnread}
      data-testid="sidebar-thread-row"
      data-thread-id={thread.id}
      title={thread.fullText}
      onclick={() => handleClick(thread)}
    >
      <span class="accent-line" class:accent-unread={hasUnread}></span>
      <div class="thread-content">
        <span class="thread-subject" data-testid="sidebar-thread-subject">{thread.subject}</span>
        {#if hasUnread}
          <span class="unread-badge" data-testid="sidebar-thread-unread-badge">{thread.unread}</span>
        {/if}
      </div>
      <span
        class="dismiss-btn"
        role="button"
        tabindex="-1"
        data-testid="sidebar-thread-dismiss"
        title="Stop tracking this thread"
        onclick={(e) => handleDismiss(e, thread.id)}
      >
        <X size={10} />
      </span>
    </button>
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
    width: 100%;
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

  :global(.dark) .thread-row {
    color: hsl(var(--sidebar-foreground) / 0.7);
  }

  .thread-row:hover {
    background: hsl(var(--sidebar-accent));
  }

  .thread-row.unread {
    color: hsl(var(--sidebar-foreground));
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
