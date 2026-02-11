<script>
  import { coworkers, maxCoworkers } from './store.js'

  // Filter to only active coworkers (matching TUI logic - skip idle/completed)
  let activeCoworkers = $derived(
    $coworkers.filter((cw) => {
      // If phase is present, filter by phase (skip idle/completed)
      if (cw.phase !== undefined) {
        return cw.phase !== null
      }
      // Otherwise filter by status
      return cw.status !== 'idle' && cw.status !== 'stopped'
    })
  )

  function getHealthColor(health) {
    switch (health?.toLowerCase()) {
      case 'green':
        return '#5faf5f'
      case 'yellow':
        return '#d7af5f'
      case 'red':
        return '#af5f5f'
      default:
        return '#5faf5f' // default to green
    }
  }
</script>

{#if activeCoworkers.length > 0}
  <div class="coworker-status">
    <div class="status-header">
      <span class="header-title">Coworkers ({activeCoworkers.length}/{$maxCoworkers})</span>
    </div>
    <div class="status-list">
      {#each activeCoworkers as cw}
        <div class="coworker-line">
          <span class="health-dot" style="color: {getHealthColor(cw.health)}">●</span>
          <span class="coworker-name">{cw.name}</span>
          {#if cw.task_id}
            <span class="task-id">!{cw.task_id}</span>
          {/if}
          {#if cw.phase}
            <span class="phase">{cw.phase}</span>
          {/if}
          {#if cw.pr_number}
            <span class="pr-number">#{cw.pr_number}</span>
          {/if}
        </div>
      {/each}
    </div>
  </div>
{/if}

<style>
  .coworker-status {
    border: 2px solid #2a2a2a;
    border-radius: 6px;
    background: #0a0a0a;
    overflow: hidden;
  }

  .status-header {
    padding: 8px 12px;
    border-bottom: 1px solid #1a1a1a;
  }

  .header-title {
    font-size: 0.75rem;
    font-weight: 700;
    color: #5fafaf;
    letter-spacing: 0.02em;
  }

  .status-list {
    padding: 6px;
  }

  .coworker-line {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 6px;
    font-size: 0.8rem;
    font-family: 'SF Mono', 'Menlo', 'Consolas', 'Monaco', 'Courier New', monospace;
    line-height: 1.4;
  }

  .health-dot {
    font-size: 0.9rem;
    line-height: 1;
    flex-shrink: 0;
  }

  .coworker-name {
    color: #d0d0d0;
    font-weight: 500;
    text-transform: lowercase;
  }

  .task-id {
    color: #d7af5f;
    font-weight: 600;
  }

  .phase {
    color: #808080;
    font-size: 0.75rem;
  }

  .pr-number {
    color: #5fafaf;
    font-weight: 500;
  }
</style>
