<script>
  import { channels, activeChannel, kanbanData, activeProject, messagesByChannel } from './store.js'
  import { fetchHistory, fetchChannels } from './api.js'

  let showCreateInput = false
  let newChannelName = ''
  let createError = ''
  let isCreating = false

  // Match channel name as a whole word in task text (avoids "auth" matching "authentication")
  function matchesChannel(text, channelName) {
    if (!text) return false
    const pattern = new RegExp(`\\b${channelName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`, 'i')
    return pattern.test(text)
  }

  // Get CI status for a channel based on its PRs
  function getChannelCiStatus(channelName, kanban) {
    const channelPrs = kanban.review.filter((pr) => matchesChannel(pr.task_name, channelName))
    if (channelPrs.length === 0) return null

    // Check if any PR has failing CI
    if (channelPrs.some((pr) => pr.status === 'ci_failed')) return 'failed'
    if (channelPrs.some((pr) => pr.status === 'ci_pending')) return 'pending'
    if (channelPrs.every((pr) => pr.status === 'ci_passed' || pr.status === 'approved')) return 'passed'
    return null
  }

  // Get task count for a channel
  function getTaskCount(channelName, kanban) {
    if (channelName === 'midtown') {
      // Main channel shows all tasks
      return {
        inProgress: kanban.inProgress.length,
        pending: kanban.backlog.length,
        review: kanban.review.length,
      }
    }
    // Topic channels filter by channel name as whole word in task description
    const filter = (list) => list.filter((item) =>
      matchesChannel(item.title || item.task_name || '', channelName)
    )
    return {
      inProgress: filter(kanban.inProgress).length,
      pending: filter(kanban.backlog).length,
      review: filter(kanban.review).length,
    }
  }

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
      const project = $activeProject || 'default'
      const response = await fetch(`/api/${project}/channels/create`, {
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
</script>

<div class="channel-list">
  <div class="channel-list-header-row">
    <div class="channel-list-header">Channels</div>
    <button class="create-channel-btn" onclick={toggleCreateInput} title="Create new channel">
      +
    </button>
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
    {@const counts = getTaskCount(channel.name, $kanbanData)}
    {@const ciStatus = getChannelCiStatus(channel.name, $kanbanData)}
    {@const totalTasks = counts.inProgress + counts.pending + counts.review}
    {@const isActive = $activeChannel === channel.name}

    <button
      class="channel-item"
      class:active={isActive}
      onclick={() => selectChannel(channel.name)}
    >
      <div class="channel-name">
        {formatChannelName(channel.name)}
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

    {#if counts.inProgress > 0 || counts.pending > 0}
      <div class="channel-tasks">
        {#if counts.inProgress > 0}
          <div class="task-indicator">● {counts.inProgress} in progress</div>
        {/if}
        {#if counts.pending > 0}
          <div class="task-indicator">○ {counts.pending} pending</div>
        {/if}
      </div>
    {/if}
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

  .create-channel-btn:hover {
    background: #2a2a2a;
    color: #d0d0d0;
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

  .channel-item {
    display: flex;
    align-items: center;
    justify-content: space-between;
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

  .channel-tasks {
    margin-left: 24px;
    padding-left: 12px;
    border-left: 2px solid #2a2a2a;
  }

  .task-indicator {
    font-size: 0.75rem;
    color: #585858;
    padding: 2px 0;
  }
</style>
