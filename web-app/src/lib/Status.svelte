<script>
  import { coworkers, daemonStatus } from './store.js'
  import { fetchStatus } from './api.js'

  function getStatusColor(status) {
    switch (status?.toLowerCase()) {
      case 'running':
      case 'active':
        return '#5faf5f'
      case 'idle':
        return '#d7af5f'
      case 'stopped':
      case 'failed':
        return '#af5f5f'
      default:
        return '#585858'
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
              <div class="badges">
                {#if cw.health}
                  <span
                    class="health-badge"
                    style="background: {cw.health === 'green' ? '#5faf5f' : cw.health === 'yellow' ? '#d7af5f' : '#af5f5f'}"
                  >
                    ●
                  </span>
                {/if}
                <span class="model-badge">{cw.model || 'unknown'}</span>
              </div>
            </div>
            <div class="coworker-details">
              {#if cw.task_id}
                <div class="detail-row">
                  <span class="detail-label">Task:</span>
                  <span class="detail-value">!{cw.task_id}</span>
                </div>
              {/if}
              {#if cw.phase}
                <div class="detail-row">
                  <span class="detail-label">Phase:</span>
                  <span class="detail-value">{cw.phase}</span>
                </div>
              {/if}
              {#if cw.pr_number}
                <div class="detail-row">
                  <span class="detail-label">PR:</span>
                  <span class="detail-value">#{cw.pr_number}</span>
                </div>
              {/if}
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
            <span class="task-id">!{task.id}</span>
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
    color: #5fafaf;
    margin-bottom: 12px;
  }

  .section-header h2 {
    margin-bottom: 0;
  }

  .refresh-btn {
    padding: 6px 12px;
    border: 1px solid #3a3a3a;
    border-radius: 4px;
    background: transparent;
    color: #585858;
    font-size: 0.75rem;
    cursor: pointer;
  }

  .refresh-btn:hover {
    border-color: #5fafaf;
    color: #5fafaf;
  }

  .daemon-status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px;
    background: #262626;
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
    color: #585858;
    font-style: italic;
    padding: 12px;
    background: #262626;
    border-radius: 8px;
  }

  .coworker-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .coworker-card {
    padding: 12px;
    background: #262626;
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

  .badges {
    display: flex;
    gap: 4px;
    align-items: center;
  }

  .health-badge {
    font-size: 1rem;
    width: 20px;
    height: 20px;
    border-radius: 50%;
    display: flex;
    align-items: center;
    justify-content: center;
    color: #1c1c1c;
    font-weight: bold;
  }

  .model-badge {
    font-size: 0.7rem;
    padding: 2px 8px;
    border-radius: 12px;
    background: #3a3a3a;
    color: #a8a8a8;
    text-transform: capitalize;
  }

  .coworker-details {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-top: 8px;
  }

  .detail-row {
    display: flex;
    gap: 8px;
    font-size: 0.8rem;
  }

  .detail-label {
    color: #585858;
    min-width: 50px;
  }

  .detail-value {
    color: #a8a8a8;
    font-family: monospace;
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
    background: #262626;
    border-radius: 4px;
    font-size: 0.85rem;
  }

  .task-id {
    color: #585858;
    min-width: 30px;
  }

  .task-subject {
    flex: 1;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .task-status {
    color: #585858;
    text-transform: capitalize;
  }
</style>
