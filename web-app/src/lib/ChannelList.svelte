<script>
  import { SvelteSet } from 'svelte/reactivity'
  import { channels, activeChannel, kanbanData, activeProject, messagesByChannel, showArchivedChannels } from './store.js'
  import { fetchHistory, fetchChannels, getApiBase } from './api.js'
  import { getChannelTaskCount, getChannelCiStatus } from './channelUtils.js'
  import TaskList from './TaskList.svelte'

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

  async function selectChannel(channelName) {
    activeChannel.set(channelName)

    // Clear unread count for this channel
    channels.update((channelList) =>
      channelList.map((ch) => (ch.name === channelName ? { ...ch, unread: 0 } : ch))
    )

    // Load messages for this channel if we haven't fetched them yet
    const currentMessages = $messagesByChannel[channelName]
    if (!currentMessages || currentMessages.length === 0) {
      await fetchHistory(channelName)
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

<div class="channel-list">
  <div class="channel-list-header-row">
    <div class="channel-list-header">Channels</div>
    <div class="header-buttons">
      <button
        class="toggle-archived-btn"
        class:active={$showArchivedChannels}
        onclick={() => showArchivedChannels.update(v => !v)}
        title={$showArchivedChannels ? "Hide archived channels" : "Show archived channels"}
      >
        📦
      </button>
      <button class="create-channel-btn" onclick={toggleCreateInput} title="Create new channel">
        +
      </button>
    </div>
  </div>

  {#if showCreateInput}
    <div class="create-channel-form">
      <input
        type="text"
        class="channel-name-input"
        placeholder="channel-name"
        bind:value={newChannelName}
        onkeydown={handleKeyDown}
        disabled={isCreating}
        autofocus
      />
      {#if createError}
        <div class="create-error">{createError}</div>
      {/if}
      <div class="create-actions">
        <button
          class="create-btn"
          onclick={createChannel}
          disabled={isCreating || !newChannelName.trim()}
        >
          {isCreating ? 'Creating...' : 'Create'}
        </button>
        <button class="cancel-btn" onclick={toggleCreateInput} disabled={isCreating}>
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

    <div class="channel-group">
      <button
        class="channel-item"
        class:active={isActive}
        onclick={() => selectChannel(channel.name)}
      >
        <div class="channel-left">
          {#if hasActiveTasks}
            <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
            <span
              class="expand-btn"
              onclick={(e) => toggleChannelTasks(channel.name, e)}
              title={isExpanded ? 'Collapse tasks' : 'Expand tasks'}
              role="button"
              tabindex="0"
            >
              {isExpanded ? '▼' : '▶'}
            </span>
          {/if}
          <div class="channel-name">
            {formatChannelName(channel.name)}
          </div>
        </div>
        <div class="channel-badges">
          {#if channel.unread > 0}
            <span class="unread-count" title="{channel.unread} unread messages">{channel.unread}</span>
          {/if}
          {#if totalTasks > 0}
            <span class="task-count">{totalTasks}</span>
          {/if}
          {#if ciStatus === 'passed'}
            <span class="ci-badge ci-passed" title="CI passing">🟢</span>
          {:else if ciStatus === 'failed'}
            <span class="ci-badge ci-failed" title="CI failing">🔴</span>
          {:else if ciStatus === 'pending'}
            <span class="ci-badge ci-pending" title="CI pending">🟡</span>
          {/if}
        </div>
      </button>

      {#if isExpanded && hasActiveTasks}
        <div class="channel-task-list">
          <TaskList channelName={channel.name} />
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .channel-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px;
    background: #1c1c1c;
    border-right: 1px solid #3a3a3a;
    overflow-y: auto;
  }

  .channel-list-header-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px 4px;
  }

  .channel-list-header {
    font-size: 0.75rem;
    font-weight: 700;
    color: #585858;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .header-buttons {
    display: flex;
    gap: 4px;
  }

  .toggle-archived-btn,
  .create-channel-btn {
    width: 24px;
    height: 24px;
    padding: 0;
    border: none;
    border-radius: 4px;
    background: transparent;
    color: #585858;
    font-size: 1.25rem;
    line-height: 1;
    cursor: pointer;
    transition: all 0.15s;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .toggle-archived-btn {
    font-size: 0.875rem;
  }

  .toggle-archived-btn:hover,
  .create-channel-btn:hover {
    background: #2a2a2a;
    color: #d0d0d0;
  }

  .toggle-archived-btn.active {
    background: #3a3a3a;
    color: #5fafaf;
  }

  .create-channel-form {
    padding: 8px 12px;
    margin-bottom: 8px;
    background: #242424;
    border-radius: 6px;
  }

  .channel-name-input {
    width: 100%;
    padding: 6px 8px;
    border: 1px solid #3a3a3a;
    border-radius: 4px;
    background: #1c1c1c;
    color: #d0d0d0;
    font-size: 0.875rem;
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
    outline: none;
  }

  .channel-name-input:focus {
    border-color: #5fafaf;
  }

  .channel-name-input:disabled {
    opacity: 0.5;
  }

  .create-error {
    margin-top: 4px;
    font-size: 0.75rem;
    color: #ff6b6b;
  }

  .create-actions {
    display: flex;
    gap: 6px;
    margin-top: 8px;
  }

  .create-btn,
  .cancel-btn {
    flex: 1;
    padding: 6px 12px;
    border: none;
    border-radius: 4px;
    font-size: 0.75rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.15s;
  }

  .create-btn {
    background: #5fafaf;
    color: #1c1c1c;
  }

  .create-btn:hover:not(:disabled) {
    background: #6fc5c5;
  }

  .create-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .cancel-btn {
    background: #3a3a3a;
    color: #d0d0d0;
  }

  .cancel-btn:hover:not(:disabled) {
    background: #4a4a4a;
  }

  .channel-group {
    margin-bottom: 2px;
  }

  .channel-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 8px 12px;
    border: none;
    border-radius: 6px;
    background: transparent;
    color: #a8a8a8;
    font-size: 0.875rem;
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
    cursor: pointer;
    transition: all 0.15s;
    text-align: left;
  }

  .channel-left {
    display: flex;
    align-items: center;
    gap: 6px;
    flex: 1;
    min-width: 0;
  }

  .expand-btn {
    width: 18px;
    height: 18px;
    flex-shrink: 0;
    padding: 0;
    border: none;
    background: transparent;
    color: #606060;
    font-size: 0.65rem;
    line-height: 1;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: color 0.15s;
  }

  .expand-btn:hover {
    color: #a0a0a0;
  }

  .channel-item:hover:not(.active) {
    background: #262626;
    color: #d0d0d0;
  }

  .channel-item.active {
    background: #303030;
    color: #5fafaf;
  }

  .channel-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .channel-badges {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .unread-count {
    font-size: 0.75rem;
    padding: 2px 6px;
    border-radius: 10px;
    background: #ff6b6b;
    color: #ffffff;
    min-width: 1.5em;
    text-align: center;
    font-weight: 600;
  }

  .task-count {
    font-size: 0.75rem;
    padding: 2px 6px;
    border-radius: 10px;
    background: #3a3a3a;
    color: #d0d0d0;
    min-width: 1.5em;
    text-align: center;
  }

  .channel-item.active .task-count {
    background: #5fafaf;
    color: #1c1c1c;
  }

  .ci-badge {
    font-size: 0.7rem;
  }

  .channel-task-list {
    margin-left: 24px;
    padding: 4px 0 8px 12px;
    border-left: 2px solid #2a2a2a;
  }
</style>
