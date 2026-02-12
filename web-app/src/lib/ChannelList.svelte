<script>
  import { channels, activeChannel, kanbanData, activeProject, messagesByChannel, detailPanelData } from './store.js'
  import { fetchHistory, fetchChannels, getApiBase } from './api.js'
  import { getChannelTaskCount, getChannelCiStatus, matchesChannel } from './channelUtils.js'

  let showCreateInput = false
  let newChannelName = ''
  let createError = ''
  let isCreating = false

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

  // Track expanded task lists per channel
  let expandedTaskLists = $state(new Set())

  function toggleTaskList(channelName, event) {
    event.stopPropagation() // Don't trigger channel selection
    if (expandedTaskLists.has(channelName)) {
      expandedTaskLists.delete(channelName)
    } else {
      expandedTaskLists.add(channelName)
    }
    expandedTaskLists = new Set(expandedTaskLists) // Trigger reactivity
  }

  // Compute task indentation based on blocked_by dependencies
  function computeTaskIndentation(tasks) {
    const indentation = new Map()
    const taskMap = new Map(tasks.map((t) => [t.id.toString(), t]))
    const processed = new Set()

    function computeIndentRecursive(taskId) {
      if (indentation.has(taskId)) {
        return indentation.get(taskId)
      }
      if (processed.has(taskId)) {
        indentation.set(taskId, 0)
        return 0
      }
      processed.add(taskId)

      const task = taskMap.get(taskId)
      if (!task) {
        indentation.set(taskId, 0)
        return 0
      }

      if (!task.blocked_by || task.blocked_by.length === 0) {
        indentation.set(taskId, 0)
        return 0
      }

      // Find first unresolved dependency in the current task list
      for (const blockerId of task.blocked_by) {
        const blockerIdStr = blockerId.toString()
        if (taskMap.has(blockerIdStr)) {
          const blockerLevel = computeIndentRecursive(blockerIdStr)
          const level = blockerLevel + 1
          indentation.set(taskId, level)
          return level
        }
      }

      indentation.set(taskId, 0)
      return 0
    }

    for (const task of tasks) {
      computeIndentRecursive(task.id.toString())
    }

    return indentation
  }

  // Get tasks for a specific channel
  function getChannelTasks(channelName, kanban) {
    const allTasks = [...kanban.backlog, ...kanban.inProgress]

    if (channelName === 'midtown') {
      return allTasks
    }

    // Filter by channel field OR channel name in subject/description
    return allTasks.filter(
      (task) =>
        task.channel === channelName ||
        matchesChannel(task.subject || '', channelName) ||
        matchesChannel(task.description || '', channelName)
    )
  }

  function openTaskDetail(task, event) {
    event.stopPropagation()
    detailPanelData.set({ type: 'task', data: task })
  }

  function getStatusMarker(status) {
    return status === 'in_progress' ? '●' : '○'
  }

  function getStatusColor(status) {
    return status === 'in_progress' ? '#fbbf24' : '#606060'
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
    {@const counts = getChannelTaskCount(channel.name, $kanbanData)}
    {@const ciStatus = getChannelCiStatus(channel.name, $kanbanData)}
    {@const totalTasks = counts.inProgress + counts.pending + counts.review}
    {@const isActive = $activeChannel === channel.name}
    {@const tasks = getChannelTasks(channel.name, $kanbanData)}
    {@const isExpanded = expandedTaskLists.has(channel.name)}

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
          <button
            class="task-count"
            class:expanded={isExpanded}
            onclick={(e) => toggleTaskList(channel.name, e)}
            title="Click to {isExpanded ? 'collapse' : 'expand'} tasks"
          >
            {totalTasks}
          </button>
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

    <!-- Expanded task list (collapsed by default) -->
    {#if isExpanded && tasks.length > 0}
      {@const indentation = computeTaskIndentation(tasks)}
      <div class="task-list">
        {#each tasks as task}
          {@const indentLevel = indentation.get(task.id.toString()) || 0}
          {@const marker = getStatusMarker(task.status)}
          {@const color = getStatusColor(task.status)}

          <button
            class="task-item"
            style="padding-left: {12 + indentLevel * 12}px"
            onclick={(e) => openTaskDetail(task, e)}
          >
            <span class="status-marker" style="color: {color}">{marker}</span>
            <span class="task-id">!{task.id}</span>
            <span class="task-subject">{task.subject}</span>
            {#if task.owner}
              <span class="task-owner">[{task.owner}]</span>
            {/if}
          </button>
        {/each}
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
    border: none;
    cursor: pointer;
    transition: all 0.15s;
    font-family: inherit;
  }

  .task-count:hover {
    background: #4a4a4a;
  }

  .task-count.expanded {
    background: #5fafaf;
    color: #1c1c1c;
  }

  .channel-item.active .task-count {
    background: #5fafaf;
    color: #1c1c1c;
  }

  .ci-badge {
    font-size: 0.7rem;
  }

  .task-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    margin-left: 12px;
    padding: 4px 0;
    border-left: 2px solid #2a2a2a;
  }

  .task-item {
    display: flex;
    align-items: baseline;
    gap: 5px;
    width: 100%;
    padding: 4px 12px;
    border: none;
    border-radius: 3px;
    background: transparent;
    color: #a0a0a0;
    font-size: 0.75rem;
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
    cursor: pointer;
    transition: all 0.1s;
    text-align: left;
  }

  .task-item:hover {
    background: #262626;
    color: #d0d0d0;
  }

  .status-marker {
    flex-shrink: 0;
    font-size: 0.7rem;
  }

  .task-id {
    flex-shrink: 0;
    color: #606060;
  }

  .task-subject {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .task-owner {
    flex-shrink: 0;
    font-size: 0.7rem;
    color: #606060;
  }
</style>
