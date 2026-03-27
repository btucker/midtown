<script lang="ts">
import ArchiveIcon from "@lucide/svelte/icons/archive";
import GripVertical from "@lucide/svelte/icons/grip-vertical";
import { type DndEvent, dragHandle, dragHandleZone } from "svelte-dnd-action";
import { useSidebar } from "$lib/components/ui/sidebar/context.svelte.ts";
import ActivityFeed from "./ActivityFeed.svelte";
import {
	closeThread,
	fetchChannels,
	fetchHistory,
	getApiBase,
	markRead,
	openTaskThread,
	openThread,
	pushNavState,
} from "./api.ts";
import { computeVisibleDmChannels, getDisplayableDmChannels } from "./channelUtils.ts";
import {
	activeChannel,
	activeProject,
	channelOrder,
	channels,
	kanbanData,
	messagesByChannel,
	showArchivedChannels,
	threadForkOwners,
} from "./store.ts";
import type { Channel } from "./types.ts";

const sidebar = useSidebar();

let showCreateInput = false;
let newChannelName = "";
let createError = "";
let isCreating = false;

// React to changes in showArchivedChannels toggle
$: {
	fetchChannels($showArchivedChannels);
}

$: regularChannels = $channels.filter((ch) => !ch.is_dm && !ch.name.startsWith("dm-"));

// Apply user-defined channel order. Channels in the saved order come first
// (in that order), followed by any new channels not yet in the saved order.
$: orderedChannels = (() => {
	const order = $channelOrder;
	if (order.length === 0) return regularChannels;
	const byName = new Map(regularChannels.map((ch) => [ch.name, ch]));
	const ordered = [];
	for (const name of order) {
		const ch = byName.get(name);
		if (ch) {
			ordered.push(ch);
			byName.delete(name);
		}
	}
	// Append channels not in the saved order
	for (const ch of regularChannels) {
		if (byName.has(ch.name)) {
			ordered.push(ch);
		}
	}
	return ordered;
})();

// svelte-dnd-action needs items with `id` fields
type DndChannelItem = Channel & { id: string };
$: dndChannelItems = orderedChannels.map((ch): DndChannelItem => ({ ...ch, id: ch.name }));

function handleDndConsider(e: CustomEvent<DndEvent<DndChannelItem>>) {
	dndChannelItems = e.detail.items;
}

function handleDndFinalize(e: CustomEvent<DndEvent<DndChannelItem>>) {
	dndChannelItems = e.detail.items;
	channelOrder.set(dndChannelItems.map((item) => item.name));
}

$: forkNames = new Set(Object.values($threadForkOwners));
$: dmChannels = getDisplayableDmChannels($channels, forkNames);

// DM section: collapsed by default, shows unread + active + visited DMs when expanded
let dmSectionExpanded = false;
let showAllDms = false;
let visitedDms = new Set<string>();

// Auto-expand DM section when navigating to a DM (e.g., via sidebar DM selection)
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

function selectChannel(channelName: string) {
	// Switch channel immediately for instant UI response (non-blocking).
	sidebar.setOpenMobile(false);

	// Close thread panel when switching channels — thread context is
	// channel-scoped and should not carry over to a different channel.
	// pushState: false because we push our own entry below with the new channel.
	closeThread({ pushState: false });

	activeChannel.set(channelName);
	pushNavState({ channel: channelName });

	// Clear unread count for this channel
	channels.update((channelList) => channelList.map((ch) => (ch.name === channelName ? { ...ch, unread: 0 } : ch)));

	// Always fetch full history on channel switch. Previously this only fetched
	// when the store was empty, but that caused stale/incomplete data when a few
	// WS messages had arrived but the full history was never loaded.
	fetchHistory(channelName);
	markRead("channel", channelName);
}

function handleActivityItemClick(item: { threadId?: string; taskId?: number; channel: string }) {
	if (item.threadId) {
		// Navigate to the thread's channel first if needed
		if ($activeChannel !== item.channel) {
			activeChannel.set(item.channel);
			pushNavState({ channel: item.channel });
			fetchHistory(item.channel);
		}
		const channelMsgs = $messagesByChannel[item.channel] || [];
		const parentMsg = channelMsgs.find((m) => m.id === item.threadId);
		const msg = parentMsg || { id: item.threadId ?? "", from: "", content: "", timestamp: "" };
		openThread(msg, item.channel);
		sidebar.setOpenMobile(false);
	} else if (item.taskId) {
		// Find the task and open its thread
		const allTasks = [...$kanbanData.inProgress, ...$kanbanData.backlog, ...($kanbanData.completedTasks || [])];
		const task = allTasks.find((t) => t.id === item.taskId);
		if (task) {
			openTaskThread(task, item.channel);
			sidebar.setOpenMobile(false);
		} else {
			selectChannel(item.channel);
		}
	} else {
		selectChannel(item.channel);
	}
}

function formatChannelName(name: string) {
	return `#${name}`;
}

function formatDmName(name: string) {
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

function handleKeyDown(event: KeyboardEvent) {
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
  <!-- Activity feed (attention items + tasks) -->
  <ActivityFeed onItemClick={handleActivityItemClick} />

  <!-- Channel header (archive toggle, + button) -->
  <div class="flex items-center justify-between px-3 pt-2 pb-1">
    <div class="section-heading text-xs font-bold text-muted-foreground uppercase tracking-wide">Channels</div>
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

  <!-- Channels (drag-to-reorder) -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    use:dragHandleZone={{ items: dndChannelItems, flipDurationMs: 200, dropTargetStyle: {}, dragDisabled: sidebar.isMobile, type: "channels" }}
    onconsider={handleDndConsider}
    onfinalize={handleDndFinalize}
  >
  {#each dndChannelItems as channel (channel.id)}
    {@const isActive = $activeChannel === channel.name}
    {@const hasUnread = channel.unread > 0 && channel.name !== 'ops'}

    <div class="channel-row mb-0.5 {isActive ? 'channel-tab-active bg-background -mr-3 rounded-l-md relative' : ''}">
      <div class="flex items-center {isActive ? 'text-primary' : 'rounded-md text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-foreground'}">
        {#if !sidebar.isMobile}
        <span
          use:dragHandle
          class="drag-handle flex items-center justify-center w-4 ml-1 cursor-grab text-muted-foreground/40 hover:text-muted-foreground/80 transition-colors duration-150 shrink-0"
          title="Drag to reorder"
        >
          <GripVertical size={12} />
        </span>
        {/if}
        <button
          class="flex items-center justify-between flex-1 min-w-0 px-2 py-2 border-none bg-transparent text-sm font-mono cursor-pointer transition-all duration-150 text-left text-inherit"
          aria-label="Select channel {channel.name}"
          onclick={() => selectChannel(channel.name)}
        >
          <div class="flex-1 overflow-hidden text-ellipsis whitespace-nowrap {hasUnread ? 'font-bold' : ''}">
            {formatChannelName(channel.name)}
          </div>
        </button>
      </div>
    </div>
  {/each}
  </div>

  <!-- DMs section -->
  {#if dmChannels.length > 0}
    <div class="flex items-center px-3 pt-3 pb-1">
      <button
        class="section-heading flex items-center gap-1.5 p-0 border-none bg-transparent cursor-pointer text-xs font-bold text-muted-foreground uppercase tracking-wide hover:text-sidebar-foreground transition-colors duration-150"
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

  :global(.dark) .section-heading {
    color: hsl(var(--sidebar-foreground) / 0.7);
  }

  :global(.dark .channel-tab-active) {
    box-shadow:
      0 -4px 6px -4px rgba(0, 0, 0, 0.3),
      0 4px 6px -4px rgba(0, 0, 0, 0.3);
  }

  .drag-handle {
    opacity: 0;
    transition: opacity 0.15s;
  }

  /* Show drag handle on hover of the parent channel row */
  :global(.channel-row:hover) .drag-handle {
    opacity: 1;
  }

</style>
