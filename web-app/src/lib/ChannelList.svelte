<script>
import ArchiveIcon from "@lucide/svelte/icons/archive";
import Check from "@lucide/svelte/icons/check";
import X from "@lucide/svelte/icons/x";
import { SvelteSet } from "svelte/reactivity";
import { useSidebar } from "$lib/components/ui/sidebar/context.svelte.ts";
import {
	closeThread,
	dismissThread,
	fetchChannels,
	fetchHistory,
	getApiBase,
	openThread,
	pushNavState,
} from "./api.ts";
import {
	computeExpandedAfterChannelNameClick,
	computeVisibleDmChannels,
	getAllCompletedThreads,
	getChannelCiStatus,
	getChannelHasActiveTasks,
	getChannelHasTrackedThreads,
	getChannelTaskCount,
	getChannelTasks,
	getChannelThreads,
	getCompletedTaskThreadIds,
	getTaskThreadIds,
} from "./channelUtils.ts";
import { getSenderColor } from "./messageUtils.ts";
import {
	activeChannel,
	activeProject,
	channels,
	coworkers,
	dismissedThreads,
	kanbanData,
	messagesByChannel,
	showArchivedChannels,
	threadData,
	threadUnreadCounts,
	trackedThreads,
} from "./store.ts";
import TaskList from "./TaskList.svelte";
import ThreadList from "./ThreadList.svelte";

const sidebar = useSidebar();

// Build a map of coworker name → coworker object for quick lookup
$: coworkerMap = new Map($coworkers.map((cw) => [cw.name, cw]));

// Thread IDs that are already represented by active tasks (for dedup)
$: taskThreadIds = getTaskThreadIds($kanbanData);
// Thread IDs from completed tasks (for visual indicator)
$: completedTaskThreadIds = getCompletedTaskThreadIds($kanbanData);

let showCreateInput = false;
let newChannelName = "";
let createError = "";
let isCreating = false;

// React to changes in showArchivedChannels toggle
$: {
	fetchChannels($showArchivedChannels);
}

$: regularChannels = $channels.filter((ch) => !ch.is_dm && !ch.name.startsWith("dm-"));
$: dmChannels = $channels.filter((ch) => ch.is_dm || ch.name.startsWith("dm-"));

// Track which channels have their task lists expanded (default: collapsed)
// Using SvelteSet for reactivity — plain Set mutations don't trigger re-renders in Svelte 5
let expandedChannels = new SvelteSet();

// Auto-expand the active channel when it gains tasks or tracked threads
$: if (
	$activeChannel &&
	!expandedChannels.has($activeChannel) &&
	(getChannelHasActiveTasks($activeChannel, $kanbanData) ||
		getChannelHasTrackedThreads($activeChannel, $trackedThreads, taskThreadIds, completedTaskThreadIds))
) {
	expandedChannels.add($activeChannel);
}

// Auto-expand the channel when a thread is opened (e.g. from the message area)
$: if ($threadData?.channelName && !expandedChannels.has($threadData.channelName)) {
	expandedChannels.add($threadData.channelName);
}

// DM section: collapsed by default, shows unread + active + visited DMs when expanded
let dmSectionExpanded = false;
let showAllDms = false;
let visitedDms = new SvelteSet();

// Auto-expand DM section when navigating to a DM (e.g., via CoworkerStatus click)
// and track the DM as visited so it remains visible after collapse/re-expand
$: if ($activeChannel && dmChannels.some((ch) => ch.name === $activeChannel)) {
	dmSectionExpanded = true;
	visitedDms.add($activeChannel);
}

$: unreadDmCount = dmChannels.filter((ch) => ch.unread > 0).length;
$: visibleDmChannels = computeVisibleDmChannels(dmChannels, {
	expanded: dmSectionExpanded,
	showAll: showAllDms,
	activeChannel: $activeChannel,
	visitedDms,
});
// Base visible count: what visibleDmChannels.length would be with showAll=false.
// Used for the "show less" guard — only show the button when collapsing would
// actually hide channels (i.e., total DMs > filtered DMs).
$: baseDmVisibleCount = computeVisibleDmChannels(dmChannels, {
	expanded: true,
	showAll: false,
	activeChannel: $activeChannel,
	visitedDms,
}).length;

// Completed threads across all channels (for "Needs Attention" section)
$: completedThreads = getAllCompletedThreads(
	$trackedThreads,
	$threadUnreadCounts,
	taskThreadIds,
	completedTaskThreadIds,
);

function selectChannel(channelName) {
	// Switch channel immediately for instant UI response (non-blocking).
	// Previously this was async and awaited fetchHistory, which blocked the UI
	// until the network request completed (~100-500ms), making channel switching
	// feel sluggish on desktop. Now the channel switches instantly and messages
	// appear when the fetch completes.

	// Close mobile sidebar overlay when navigating to a channel
	sidebar.setOpenMobile(false);

	// Close thread panel when switching channels — thread context is
	// channel-scoped and should not carry over to a different channel.
	// pushState: false because we push our own entry below with the new channel.
	closeThread({ pushState: false });

	// Compute and apply the new expanded state for this channel.
	// computeExpandedAfterChannelNameClick handles two cases:
	//   - Switching to a new channel: auto-expand if it has active tasks
	//   - Re-clicking the already-active channel: toggle expand/collapse
	const next = computeExpandedAfterChannelNameClick(channelName, expandedChannels, $activeChannel, $kanbanData, {
		trackedThreads: $trackedThreads,
		taskThreadIds,
		completedTaskThreadIds,
	});
	if (next.has(channelName)) {
		expandedChannels.add(channelName);
	} else {
		expandedChannels.delete(channelName);
	}

	activeChannel.set(channelName);
	pushNavState({ channel: channelName });

	// Clear unread count for this channel
	channels.update((channelList) => channelList.map((ch) => (ch.name === channelName ? { ...ch, unread: 0 } : ch)));

	// Always fetch full history on channel switch. Previously this only fetched
	// when the store was empty, but that caused stale/incomplete data when a few
	// WS messages had arrived but the full history was never loaded.
	fetchHistory(channelName);
}

function handleCompletedThreadClick(thread) {
	sidebar.setOpenMobile(false);
	// Close any existing thread panel before switching channels to avoid
	// stale thread state and double browser-history entries (matches selectChannel behavior).
	closeThread({ pushState: false });
	// Switch to the thread's channel so the thread panel has proper context
	if ($activeChannel !== thread.channelName) {
		activeChannel.set(thread.channelName);
		pushNavState({ channel: thread.channelName });
		fetchHistory(thread.channelName);
	}
	// Find the parent message in the channel's message store, or create a stub
	const channelMsgs = $messagesByChannel[thread.channelName] || [];
	const parentMsg = channelMsgs.find((m) => m.id === thread.id);
	const msg = parentMsg || { id: thread.id, from: "", content: thread.subject || "" };
	openThread(msg, thread.channelName);
}

function handleCompletedThreadDismiss(e, threadId) {
	e.stopPropagation();
	dismissThread(threadId);
}

function formatChannelName(name) {
	return `#${name}`;
}

function formatDmName(name) {
	return `@${name.replace(/^dm-/, "")}`;
}

function toggleCreateInput() {
	showCreateInput = !showCreateInput;
	if (showCreateInput) {
		newChannelName = "";
		createError = "";
	}
}

async function createChannel() {
	createError = "";
	const name = newChannelName.trim();

	if (!name) {
		createError = "Channel name cannot be empty";
		return;
	}

	if (!/^[a-zA-Z0-9_-]+$/.test(name)) {
		createError = "Only alphanumeric characters, hyphens, and underscores allowed";
		return;
	}

	if (name.toLowerCase() === "midtown") {
		createError = 'Channel name "midtown" is reserved';
		return;
	}

	isCreating = true;
	try {
		const response = await fetch(`${getApiBase()}/channels/create`, {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ name }),
		});

		if (!response.ok) {
			const errorData = await response.json();
			createError = errorData.error || "Failed to create channel";
			return;
		}

		// Success - refresh channel list
		await fetchChannels();

		// Switch to the new channel and close the input
		activeChannel.set(name);
		showCreateInput = false;
		newChannelName = "";
	} catch (error) {
		createError = `Network error: ${error.message}`;
	} finally {
		isCreating = false;
	}
}

function handleKeyDown(event) {
	if (event.key === "Enter") {
		createChannel();
	} else if (event.key === "Escape") {
		showCreateInput = false;
		newChannelName = "";
		createError = "";
	}
}
</script>

<div class="flex flex-col gap-1 p-3 overflow-y-auto">
  <div class="flex items-center justify-between px-3 pt-2 pb-1">
    <div class="text-xs font-bold text-muted-foreground uppercase tracking-wide">Channels</div>
    <div class="flex gap-1">
      <button
        class="w-6 h-6 p-0 border-none rounded bg-transparent text-muted-foreground text-sm leading-none cursor-pointer transition-all duration-150 flex items-center justify-center hover:bg-sidebar-accent hover:text-sidebar-foreground"
        aria-label="Toggle archived channels"
        class:bg-sidebar-accent={$showArchivedChannels}
        class:text-sidebar-primary={$showArchivedChannels}
        onclick={() => showArchivedChannels.update(v => !v)}
        title={$showArchivedChannels ? "Hide archived channels" : "Show archived channels"}
      >
        <ArchiveIcon size={14} />
      </button>
      <button
        class="w-6 h-6 p-0 border-none rounded bg-transparent text-muted-foreground text-xl leading-none cursor-pointer transition-all duration-150 flex items-center justify-center hover:bg-sidebar-accent hover:text-sidebar-foreground"
        onclick={toggleCreateInput}
        title="Create new channel"
      >
        +
      </button>
    </div>
  </div>

  {#if showCreateInput}
    <div class="px-3 py-2 mb-2 bg-sidebar-accent rounded-md">
      <!-- svelte-ignore a11y_autofocus -->
      <input
        type="text"
        class="w-full px-2 py-1.5 border border-sidebar-border rounded bg-sidebar text-sidebar-foreground text-sm font-mono outline-none focus:border-primary disabled:opacity-50"
        placeholder="channel-name"
        bind:value={newChannelName}
        onkeydown={handleKeyDown}
        disabled={isCreating}
        autofocus
      />
      {#if createError}
        <div class="mt-1 text-xs text-destructive">{createError}</div>
      {/if}
      <div class="flex gap-1.5 mt-2">
        <button
          class="flex-1 px-3 py-1.5 border-none rounded text-xs font-medium cursor-pointer transition-all duration-150 bg-primary text-primary-foreground hover:bg-primary/90 disabled:opacity-50 disabled:cursor-not-allowed"
          onclick={createChannel}
          disabled={isCreating || !newChannelName.trim()}
        >
          {isCreating ? 'Creating...' : 'Create'}
        </button>
        <button
          class="flex-1 px-3 py-1.5 border-none rounded text-xs font-medium cursor-pointer transition-all duration-150 bg-sidebar-accent text-sidebar-foreground hover:bg-sidebar-accent/80 disabled:opacity-50"
          onclick={toggleCreateInput}
          disabled={isCreating}
        >
          Cancel
        </button>
      </div>
    </div>
  {/if}

  {#if completedThreads.length > 0}
    <div class="px-3 pt-2 pb-1">
      <div class="text-xs font-bold text-muted-foreground uppercase tracking-wide">Needs Attention</div>
    </div>
    <div class="needs-attention-list">
      {#each completedThreads as thread}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="completed-thread-row"
          role="button"
          tabindex="0"
          title={thread.fullText}
          data-testid="needs-attention-thread"
          onclick={() => handleCompletedThreadClick(thread)}
          onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') handleCompletedThreadClick(thread) }}
        >
          <span class="completed-check-icon"><Check size={10} /></span>
          <div class="completed-thread-content">
            <span class="completed-thread-subject">{thread.subject}</span>
            <span class="completed-thread-channel">#{thread.channelName}</span>
          </div>
          <button
            class="completed-dismiss-btn"
            title="Dismiss"
            data-testid="needs-attention-dismiss"
            onclick={(e) => handleCompletedThreadDismiss(e, thread.id)}
          >
            <X size={10} />
          </button>
        </div>
      {/each}
    </div>
  {/if}

  {#each regularChannels as channel}
    {@const counts = getChannelTaskCount(channel.name, $kanbanData)}
    {@const ciStatus = getChannelCiStatus(channel.name, $kanbanData)}
    {@const isActive = $activeChannel === channel.name}
    {@const isExpanded = expandedChannels.has(channel.name)}
    {@const hasActiveTasks = counts.inProgress > 0 || counts.pending > 0}
    {@const hasTrackedThreads = getChannelHasTrackedThreads(channel.name, $trackedThreads, taskThreadIds, completedTaskThreadIds)}
    {@const hasUnread = channel.unread > 0 && channel.name !== 'ops'}

    <div class="mb-0.5 {isActive ? 'channel-tab-active bg-background -mr-3 rounded-l-md relative' : ''}">
      <div class="flex items-center {isActive ? 'text-primary' : 'rounded-md text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-foreground'}">
        <button
          class="flex items-center justify-between flex-1 min-w-0 px-3 py-2 border-none bg-transparent text-sm font-mono cursor-pointer transition-all duration-150 text-left text-inherit"
          aria-label="Select channel {channel.name}"
          onclick={() => selectChannel(channel.name)}
        >
          <div class="flex-1 overflow-hidden text-ellipsis whitespace-nowrap {hasUnread ? 'font-bold' : ''}">
            {formatChannelName(channel.name)}
          </div>
          <div class="flex items-center gap-1.5">
            {#if !isExpanded && (hasActiveTasks || hasTrackedThreads)}
              {@const tasks = hasActiveTasks ? getChannelTasks(channel.name, $kanbanData) : []}
              {@const threads = hasTrackedThreads ? getChannelThreads(channel.name, $trackedThreads, $threadUnreadCounts, taskThreadIds, completedTaskThreadIds).threads : []}
              {@const unreadThreads = threads.filter(t => t.unread > 0)}
              <div class="flex items-center gap-[3px]">
                {#each tasks as task}
                  {@const cw = task.owner ? coworkerMap.get(task.owner) : null}
                  {@const pipColor = task.owner ? getSenderColor(task.owner) : null}
                  {@const tipParts = [`!${task.id} ${task.subject}`, task.owner ? `${task.owner}${cw?.phase ? ` · ${cw.phase}` : ''}` : null, cw?.progress != null ? `${cw.progress}% done` : null].filter(Boolean)}
                  <span
                    class="task-pip {task.status === 'in_progress' ? 'task-pip-active' : 'task-pip-pending'}"
                    style={pipColor ? `background: ${pipColor}` : ''}
                    title={tipParts.join('\n')}
                  ></span>
                {/each}
                {#each unreadThreads as thread}
                  <span
                    class="thread-pip"
                    data-testid="sidebar-thread-pip"
                    title={thread.subject}
                  ></span>
                {/each}
              </div>
            {/if}
            {#if ciStatus === 'passed'}
              <span class="text-[0.7rem]" title="CI passing">🟢</span>
            {:else if ciStatus === 'failed'}
              <span class="text-[0.7rem]" title="CI failing">🔴</span>
            {:else if ciStatus === 'pending'}
              <span class="text-[0.7rem]" title="CI pending">🟡</span>
            {/if}
          </div>
        </button>
      </div>

      {#if isExpanded && (hasActiveTasks || hasTrackedThreads)}
        <div class={!isActive ? 'expanded-group' : 'mr-3'}>
          {#if hasActiveTasks}
            <div class="px-3 py-1 pb-2">
              <TaskList channelName={channel.name} />
            </div>
          {/if}
          {#if hasTrackedThreads}
            <div class="px-3 py-0 pb-1">
              <ThreadList channelName={channel.name} />
            </div>
          {/if}
        </div>
      {/if}
    </div>
  {/each}

  {#if dmChannels.length > 0}
    <div class="flex items-center px-3 pt-3 pb-1">
      <button
        class="flex items-center gap-1.5 p-0 border-none bg-transparent cursor-pointer text-xs font-bold text-muted-foreground uppercase tracking-wide hover:text-sidebar-foreground transition-colors duration-150"
        onclick={() => { dmSectionExpanded = !dmSectionExpanded; if (!dmSectionExpanded) showAllDms = false }}
        aria-label={dmSectionExpanded ? 'Collapse direct messages' : 'Expand direct messages'}
      >
        <span class="text-[0.55rem] leading-none">{dmSectionExpanded ? '▼' : '▶'}</span>
        Direct Messages
        {#if !dmSectionExpanded && unreadDmCount > 0}
          <span class="ml-1 px-1.5 py-0.5 rounded-full bg-primary text-primary-foreground text-[0.6rem] font-bold leading-none">{unreadDmCount}</span>
        {/if}
      </button>
    </div>
    {#if dmSectionExpanded}
      {#each visibleDmChannels as channel}
        {@const isActive = $activeChannel === channel.name}
        {@const hasUnread = channel.unread > 0}
        <div class="mb-0.5 {isActive ? 'channel-tab-active bg-background -mr-3 rounded-l-md relative' : ''}">
          <div class="flex items-center {isActive ? 'text-primary' : 'rounded-md text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-foreground'}">
            <button
              class="flex items-center flex-1 min-w-0 px-3 py-2 border-none bg-transparent text-sm font-mono cursor-pointer transition-all duration-150 text-left text-inherit"
              aria-label="Open DM with {channel.name.replace(/^dm-/, '')}"
              onclick={() => selectChannel(channel.name)}
            >
              <div class="flex-1 overflow-hidden text-ellipsis whitespace-nowrap {hasUnread ? 'font-bold' : ''}">
                {formatDmName(channel.name)}
              </div>
            </button>
          </div>
        </div>
      {/each}
      {#if !showAllDms && dmChannels.length > visibleDmChannels.length}
        <button
          class="ml-2 px-1 py-1 border-none bg-transparent text-xs text-muted-foreground cursor-pointer hover:text-sidebar-foreground transition-colors duration-150"
          onclick={() => showAllDms = true}
        >
          show all ({dmChannels.length})
        </button>
      {:else if showAllDms && dmChannels.length > baseDmVisibleCount}
        <button
          class="ml-2 px-1 py-1 border-none bg-transparent text-xs text-muted-foreground cursor-pointer hover:text-sidebar-foreground transition-colors duration-150"
          onclick={() => showAllDms = false}
        >
          show less
        </button>
      {/if}
    {/if}
  {/if}
</div>

<style>
  /* Tab effect: the active channel extends flush to the sidebar's right edge,
     with its bg-background covering the sidebar's inset shadow.
     Subtle shadows on the top and bottom edges simulate the sidebar's
     depth shadow wrapping around the tab. */
  :global(.channel-tab-active) {
    box-shadow:
      0 -4px 6px -4px rgba(0, 0, 0, 0.1),
      0 4px 6px -4px rgba(0, 0, 0, 0.1);
  }

  :global(.dark .channel-tab-active) {
    box-shadow:
      0 -4px 6px -4px rgba(0, 0, 0, 0.3),
      0 4px 6px -4px rgba(0, 0, 0, 0.3);
  }

  .expanded-group {
    background: hsl(var(--muted-foreground) / 0.06);
    border-radius: 6px;
    margin: 0 4px;
  }

  .task-pip {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    flex-shrink: 0;
  }

  .task-pip-active {
    background: hsl(var(--accent-teal));
    box-shadow: 0 0 4px currentColor;
    opacity: 0.9;
  }

  .task-pip-pending {
    background: hsl(var(--muted-foreground) / 0.35);
    opacity: 0.6;
  }

  .thread-pip {
    width: 4px;
    height: 4px;
    border-radius: 1px;
    flex-shrink: 0;
    background: hsl(var(--accent-teal));
    opacity: 0.8;
  }

  /* Needs Attention section */
  .needs-attention-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 2px 8px 8px;
  }

  .completed-thread-row {
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
    color: hsl(var(--accent-green, 145 40% 38%) / 0.8);
  }

  .completed-thread-row:hover {
    background: hsl(var(--sidebar-accent));
  }

  .completed-check-icon {
    width: 14px;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    color: hsl(var(--accent-green, 145 40% 38%));
  }

  .completed-thread-content {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .completed-thread-subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .completed-thread-channel {
    flex-shrink: 0;
    font-size: 0.6rem;
    color: hsl(var(--muted-foreground) / 0.5);
  }

  .completed-dismiss-btn {
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

  .completed-thread-row:hover .completed-dismiss-btn {
    opacity: 1;
  }

  .completed-dismiss-btn:hover {
    color: hsl(var(--muted-foreground));
    background: hsl(var(--sidebar-accent));
  }

</style>
