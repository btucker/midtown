<script>
  import { coworkers, daemonStatus } from './store.js'
  import { fetchStatus } from './api.js'

  function getStatusColor(status) {
    switch (status?.toLowerCase()) {
      case 'running':
      case 'active':
        return '#4ade80'
      case 'idle':
        return '#fbbf24'
      case 'stopped':
      case 'failed':
        return '#e94560'
      default:
        return '#666'
    }
  }

  function formatDate(timestamp) {
    try {
      const date = new Date(timestamp)
      return date.toLocaleString([], {
        month: 'short',
        day: 'numeric',
        hour: '2-digit',
        minute: '2-digit',
      })
    } catch {
      return 'Unknown'
    }
  }

  async function refresh() {
    await fetchStatus()
  }
</script>

<div class="status-container">
  <div class="section">
    <div class="section-header">
      <h2>Daemon</h2>
      <button class="refresh-btn" onclick={refresh}>Refresh</button>
    </div>
    <div class="daemon-status">
      <span
        class="status-dot"
        style="background: {getStatusColor($daemonStatus?.daemon)}"
      ></span>
      <span class="status-text">{$daemonStatus?.daemon || 'Unknown'}</span>
    </div>
  </div>

  <div class="section">
    <h2>Coworkers ({$coworkers.length})</h2>
    {#if $coworkers.length === 0}
      <p class="empty">No active coworkers</p>
    {:else}
      <div class="coworker-list">
        {#each $coworkers as cw}
          <div class="coworker-card">
            <div class="coworker-header">
              <span class="coworker-name">{cw.name}</span>
              <span
                class="status-badge"
                style="background: {getStatusColor(cw.status)}"
              >
                {cw.status}
              </span>
            </div>
            {#if cw.current_task}
              <div class="current-task">
                <span class="task-label">Working on:</span>
                <span class="task-text">{cw.current_task}</span>
              </div>
            {/if}
            <div class="started-at">
              Started: {formatDate(cw.started_at)}
            </div>
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="section">
    <h2>Tasks</h2>
    {#if !$daemonStatus?.tasks || $daemonStatus.tasks.length === 0}
      <p class="empty">No tasks</p>
    {:else}
      <div class="task-list">
        {#each $daemonStatus.tasks as task}
          <div class="task-item">
            <span class="task-id">#{task.id}</span>
            <span class="task-subject">{task.subject}</span>
            <span class="task-status">{task.status}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .status-container {
    padding: 16px;
    overflow-y: auto;
    height: 100%;
  }

  .section {
    margin-bottom: 24px;
  }

  .section-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
  }

  h2 {
    font-size: 1rem;
    color: #00d9ff;
    margin-bottom: 12px;
  }

  .section-header h2 {
    margin-bottom: 0;
  }

  .refresh-btn {
    padding: 6px 12px;
    border: 1px solid #0f3460;
    border-radius: 4px;
    background: transparent;
    color: #888;
    font-size: 0.75rem;
    cursor: pointer;
  }

  .refresh-btn:hover {
    border-color: #00d9ff;
    color: #00d9ff;
  }

  .daemon-status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px;
    background: #16213e;
    border-radius: 8px;
  }

  .status-dot {
    width: 10px;
    height: 10px;
    border-radius: 50%;
  }

  .status-text {
    text-transform: capitalize;
  }

  .empty {
    color: #666;
    font-style: italic;
    padding: 12px;
    background: #16213e;
    border-radius: 8px;
  }

  .coworker-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .coworker-card {
    padding: 12px;
    background: #16213e;
    border-radius: 8px;
  }

  .coworker-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
  }

  .coworker-name {
    font-weight: 600;
    text-transform: capitalize;
  }

  .status-badge {
    font-size: 0.7rem;
    padding: 2px 8px;
    border-radius: 12px;
    color: #1a1a2e;
    text-transform: capitalize;
  }

  .current-task {
    font-size: 0.85rem;
    margin-bottom: 4px;
  }

  .task-label {
    color: #888;
  }

  .task-text {
    color: #ccc;
  }

  .started-at {
    font-size: 0.75rem;
    color: #666;
  }

  .task-list {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .task-item {
    display: flex;
    gap: 8px;
    padding: 8px 12px;
    background: #16213e;
    border-radius: 4px;
    font-size: 0.85rem;
  }

  .task-id {
    color: #888;
    min-width: 30px;
  }

  .task-subject {
    flex: 1;
  }

  .task-status {
    color: #666;
    text-transform: capitalize;
  }
</style>
