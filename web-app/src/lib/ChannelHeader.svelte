<script>
  import { activeChannel, kanbanData } from './store.js'
  import Kanban from './Kanban.svelte'

  let kanbanExpanded = $state(true)

  function toggleKanban() {
    kanbanExpanded = !kanbanExpanded
  }

  // Match channel name as a whole word in task text (avoids "auth" matching "authentication")
  function matchesChannel(text, channelName) {
    if (!text) return false
    const pattern = new RegExp(`\\b${channelName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`, 'i')
    return pattern.test(text)
  }

  // Get task count for the active channel
  function getChannelTaskCount(channelName, kanban) {
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

  // Get active PRs for this channel
  function getChannelPrs(channelName, kanban) {
    if (channelName === 'midtown') {
      return kanban.review
    }
    return kanban.review.filter((pr) => matchesChannel(pr.task_name, channelName))
  }

  let channelCounts = $derived(getChannelTaskCount($activeChannel, $kanbanData))
  let channelPrs = $derived(getChannelPrs($activeChannel, $kanbanData))
  let totalTasks = $derived(channelCounts.inProgress + channelCounts.pending + channelCounts.review)
</script>

<div class="channel-header">
  <div class="header-top">
    <div class="channel-info">
      <div class="channel-title">
        <span class="channel-hash">#</span>
        <span class="channel-name">{$activeChannel}</span>
      </div>
      {#if totalTasks > 0 || channelPrs.length > 0}
        <div class="channel-stats">
          {#if channelPrs.length > 0}
            <span class="stat-badge pr-badge" title="{channelPrs.length} active PR{channelPrs.length === 1 ? '' : 's'}">
              {channelPrs.length} PR{channelPrs.length === 1 ? '' : 's'}
            </span>
          {/if}
          {#if channelCounts.inProgress > 0}
            <span class="stat-badge in-progress-badge" title="{channelCounts.inProgress} in progress">
              {channelCounts.inProgress} in progress
            </span>
          {/if}
          {#if channelCounts.pending > 0}
            <span class="stat-badge pending-badge" title="{channelCounts.pending} pending">
              {channelCounts.pending} pending
            </span>
          {/if}
        </div>
      {/if}
    </div>
    <button class="kanban-toggle" onclick={toggleKanban} title={kanbanExpanded ? 'Hide kanban' : 'Show kanban'}>
      {kanbanExpanded ? '\u25BC' : '\u25B6'}
    </button>
  </div>
  {#if kanbanExpanded}
    <div class="kanban-strip">
      <Kanban />
    </div>
  {/if}
</div>

<style>
  .channel-header {
    background: #1a1a1a;
    border-bottom: 2px solid #2a2a2a;
    flex-shrink: 0;
  }

  .header-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
  }

  .channel-info {
    display: flex;
    align-items: center;
    gap: 12px;
    flex: 1;
    min-width: 0;
  }

  .channel-title {
    display: flex;
    align-items: baseline;
    gap: 4px;
    flex-shrink: 0;
  }

  .channel-hash {
    font-size: 1.2rem;
    color: #606060;
    font-weight: 700;
  }

  .channel-name {
    font-size: 1.1rem;
    font-weight: 700;
    color: #d0d0d0;
  }

  .channel-stats {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }

  .stat-badge {
    font-size: 0.75rem;
    padding: 3px 8px;
    border-radius: 12px;
    font-weight: 600;
    white-space: nowrap;
  }

  .pr-badge {
    background: #2a3a5a;
    color: #5f87af;
  }

  .in-progress-badge {
    background: #2a3a2a;
    color: #5faf5f;
  }

  .pending-badge {
    background: #3a3a2a;
    color: #af5faf;
  }

  .kanban-toggle {
    background: transparent;
    border: 1px solid #2a2a2a;
    color: #606060;
    font-size: 0.75rem;
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
    transition: all 0.15s;
    flex-shrink: 0;
  }

  .kanban-toggle:hover {
    background: #252525;
    border-color: #5faf5f;
    color: #5faf5f;
  }

  .kanban-strip {
    border-top: 1px solid #2a2a2a;
  }

  /* Hide on mobile */
  @media (max-width: 768px) {
    .channel-header {
      display: none;
    }
  }
</style>
