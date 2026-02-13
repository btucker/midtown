<script>
  import { SvelteSet } from 'svelte/reactivity'
  import { channels, activeChannel, kanbanData, activeProject, messagesByChannel, showArchivedChannels } from './store.js'
  import { fetchHistory, fetchChannels, getApiBase } from './api.js'
  import { getChannelTaskCount, getChannelCiStatus } from './channelUtils.js'
  import TaskList from './TaskList.svelte'
  import ArchiveIcon from '@lucide/svelte/icons/archive'

  let showCreateInput = false
  let newChannelName = ''
  let createError = ''
  let isCreating = false

  // React to changes in showArchivedChannels toggle
  $: {
    fetchChannels($showArchivedChannels)
  }

  // Track which channels have their task lists expanded (default: collapsed)
  // Using SvelteSet for reactivity — plain Set mutations don't trigger re-renders in Svelte 5
  let expandedChannels = new SvelteSet()

  function selectChannel(channelName) {
    // Switch channel immediately for instant UI response (non-blocking).
    // Previously this was async and awaited fetchHistory, which blocked the UI
    // until the network request completed (~100-500ms), making channel switching
    // feel sluggish on desktop. Now the channel switches instantly and messages
    // appear when the fetch completes.
    activeChannel.set(channelName)

    // Clear unread count for this channel
    channels.update((channelList) =>
      channelList.map((ch) => (ch.name === channelName ? { ...ch, unread: 0 } : ch))
    )

    // Load messages for this channel if we haven't fetched them yet (non-blocking)
    const currentMessages = $messagesByChannel[channelName]
    if (!currentMessages || currentMessages.length === 0) {
      fetchHistory(channelName) // Fire-and-forget - Channel.svelte will show empty state briefly
    }
  }

  function formatChannelName(name) {
    return `#${name}`
  }

  function toggleCreateInput() {
    showCreateInput = !showCreateInput
    if (showCreateInput) {
      newChannelName = ''
      createError = ''
    }
  }

  async function createChannel() {
    createError = ''
    const name = newChannelName.trim()

    if (!name) {
      createError = 'Channel name cannot be empty'
      return
    }

    if (!/^[a-zA-Z0-9_-]+$/.test(name)) {
      createError = 'Only alphanumeric characters, hyphens, and underscores allowed'
      return
    }

    if (name.toLowerCase() === 'midtown') {
      createError = 'Channel name "midtown" is reserved'
      return
    }

    isCreating = true
    try {
      const response = await fetch(`${getApiBase()}/channels/create`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name }),
      })

      if (!response.ok) {
        const errorData = await response.json()
        createError = errorData.error || 'Failed to create channel'
        return
      }

      // Success - refresh channel list
      await fetchChannels()

      // Switch to the new channel and close the input
      activeChannel.set(name)
      showCreateInput = false
      newChannelName = ''
    } catch (error) {
      createError = 'Network error: ' + error.message
    } finally {
      isCreating = false
    }
  }

  function handleKeyDown(event) {
    if (event.key === 'Enter') {
      createChannel()
    } else if (event.key === 'Escape') {
      showCreateInput = false
      newChannelName = ''
      createError = ''
    }
  }

  function toggleChannelTasks(channelName, event) {
    // Stop event from bubbling to channel selection
    event.stopPropagation()

    if (expandedChannels.has(channelName)) {
      expandedChannels.delete(channelName)
    } else {
      expandedChannels.add(channelName)
    }
  }
</script>

<div class="flex flex-col gap-1 p-3 bg-[#1c1c1c] border-r border-[#3a3a3a] overflow-y-auto">
  <div class="flex items-center justify-between px-3 pt-2 pb-1">
    <div class="text-xs font-bold text-[#585858] uppercase tracking-wide">Channels</div>
    <div class="flex gap-1">
      <button
        class="w-6 h-6 p-0 border-none rounded bg-transparent text-[#585858] text-sm leading-none cursor-pointer transition-all duration-150 flex items-center justify-center hover:bg-[#2a2a2a] hover:text-[#d0d0d0]"
        aria-label="Toggle archived channels"
        class:bg-[#3a3a3a]={$showArchivedChannels}
        class:text-[#5fafaf]={$showArchivedChannels}
        onclick={() => showArchivedChannels.update(v => !v)}
        title={$showArchivedChannels ? "Hide archived channels" : "Show archived channels"}
      >
        <ArchiveIcon size={14} />
      </button>
      <button
        class="w-6 h-6 p-0 border-none rounded bg-transparent text-[#585858] text-xl leading-none cursor-pointer transition-all duration-150 flex items-center justify-center hover:bg-[#2a2a2a] hover:text-[#d0d0d0]"
        onclick={toggleCreateInput}
        title="Create new channel"
      >
        +
      </button>
    </div>
  </div>

  {#if showCreateInput}
    <div class="px-3 py-2 mb-2 bg-[#242424] rounded-md">
      <input
        type="text"
        class="w-full px-2 py-1.5 border border-[#3a3a3a] rounded bg-[#1c1c1c] text-[#d0d0d0] text-sm font-mono outline-none focus:border-[#5fafaf] disabled:opacity-50"
        placeholder="channel-name"
        bind:value={newChannelName}
        onkeydown={handleKeyDown}
        disabled={isCreating}
        autofocus
      />
      {#if createError}
        <div class="mt-1 text-xs text-[#ff6b6b]">{createError}</div>
      {/if}
      <div class="flex gap-1.5 mt-2">
        <button
          class="flex-1 px-3 py-1.5 border-none rounded text-xs font-medium cursor-pointer transition-all duration-150 bg-[#5fafaf] text-[#1c1c1c] hover:bg-[#6fc5c5] disabled:opacity-50 disabled:cursor-not-allowed"
          onclick={createChannel}
          disabled={isCreating || !newChannelName.trim()}
        >
          {isCreating ? 'Creating...' : 'Create'}
        </button>
        <button
          class="flex-1 px-3 py-1.5 border-none rounded text-xs font-medium cursor-pointer transition-all duration-150 bg-[#3a3a3a] text-[#d0d0d0] hover:bg-[#4a4a4a] disabled:opacity-50"
          onclick={toggleCreateInput}
          disabled={isCreating}
        >
          Cancel
        </button>
      </div>
    </div>
  {/if}

  {#each $channels as channel}
    {@const counts = getChannelTaskCount(channel.name, $kanbanData)}
    {@const ciStatus = getChannelCiStatus(channel.name, $kanbanData)}
    {@const totalTasks = counts.inProgress + counts.pending + counts.review}
    {@const isActive = $activeChannel === channel.name}
    {@const isExpanded = expandedChannels.has(channel.name)}
    {@const hasActiveTasks = counts.inProgress > 0 || counts.pending > 0}

    <div class="mb-0.5">
      <button
        class="flex items-center justify-between w-full px-3 py-2 border-none rounded-md text-sm font-mono cursor-pointer transition-all duration-150 text-left {isActive ? 'bg-accent text-primary' : 'bg-transparent text-muted-foreground hover:bg-[#262626] hover:text-foreground'}"
        aria-label="Select channel {channel.name}"
        onclick={() => selectChannel(channel.name)}
      >
        <div class="flex items-center gap-1.5 flex-1 min-w-0">
          {#if hasActiveTasks}
            <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
            <span
              class="w-[18px] h-[18px] shrink-0 p-0 border-none bg-transparent text-[#606060] text-[0.65rem] leading-none cursor-pointer flex items-center justify-center transition-colors duration-150 hover:text-[#a0a0a0]"
              onclick={(e) => toggleChannelTasks(channel.name, e)}
              title={isExpanded ? 'Collapse tasks' : 'Expand tasks'}
              role="button"
              tabindex="0"
            >
              {isExpanded ? '▼' : '▶'}
            </span>
          {/if}
          <div class="flex-1 overflow-hidden text-ellipsis whitespace-nowrap">
            {formatChannelName(channel.name)}
          </div>
        </div>
        <div class="flex items-center gap-1.5">
          {#if channel.unread > 0}
            <span class="text-xs px-1.5 py-0.5 rounded-[10px] bg-[#ff6b6b] text-white min-w-[1.5em] text-center font-semibold" title="{channel.unread} unread messages">{channel.unread}</span>
          {/if}
          {#if totalTasks > 0}
            <span
              class="text-xs px-1.5 py-0.5 rounded-[10px] min-w-[1.5em] text-center"
              class:bg-[#3a3a3a]={!isActive}
              class:text-[#d0d0d0]={!isActive}
              class:bg-[#5fafaf]={isActive}
              class:text-[#1c1c1c]={isActive}
            >{totalTasks}</span>
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

      {#if isExpanded && hasActiveTasks}
        <div class="ml-6 py-1 pb-2 pl-3 border-l-2 border-[#2a2a2a]">
          <TaskList channelName={channel.name} />
        </div>
      {/if}
    </div>
  {/each}
</div>
