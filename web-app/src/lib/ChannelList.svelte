<script>
  import { channels, activeChannel, kanbanData } from './store.js'

  // Get CI status for a channel based on its PRs
  function getChannelCiStatus(channelName, kanban) {
    const channelPrs = kanban.review.filter((pr) => pr.task_name?.toLowerCase().includes(channelName.toLowerCase()))
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
    // Topic channels filter by channel name in task description
    const filter = (list) => list.filter((item) =>
      (item.title || item.task_name || '').toLowerCase().includes(channelName.toLowerCase())
    )
    return {
      inProgress: filter(kanban.inProgress).length,
      pending: filter(kanban.backlog).length,
      review: filter(kanban.review).length,
    }
  }

  function selectChannel(channelName) {
    activeChannel.set(channelName)
  }

  function formatChannelName(name) {
    return `#${name}`
  }
</script>

<div class="channel-list">
  <div class="channel-list-header">Channels</div>
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

  .channel-list-header {
    font-size: 0.75rem;
    font-weight: 700;
    color: #585858;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 8px 12px 4px;
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
