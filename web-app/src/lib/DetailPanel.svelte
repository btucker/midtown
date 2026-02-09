<script>
  let { panelData = null, onClose = () => {} } = $props()

  function handleClose() {
    onClose()
  }

  function handleKeydown(event) {
    if (event.key === 'Escape') {
      handleClose()
    }
  }

  function formatDate(timestamp) {
    if (!timestamp) return 'Unknown'
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

  function getPrStatusColor(status) {
    switch (status?.toLowerCase()) {
      case 'success':
      case 'approved':
        return '#5faf5f'
      case 'pending':
      case 'in_progress':
        return '#d7af5f'
      case 'failure':
      case 'rejected':
        return '#af5f5f'
      default:
        return '#585858'
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

{#if panelData}
  <div class="detail-panel">
    <div class="panel-header">
      <h2 class="panel-title">
        {#if panelData.type === 'task'}
          Task !{panelData.data.id}
        {:else if panelData.type === 'pr'}
          PR #{panelData.data.number}
        {:else if panelData.type === 'coworker'}
          {panelData.data.name}
        {/if}
      </h2>
      <button class="close-btn" onclick={handleClose} aria-label="Close">
        ✕
      </button>
    </div>

    <div class="panel-content">
      {#if panelData.type === 'task'}
        <!-- Task detail -->
        <div class="detail-section">
          <div class="detail-field">
            <span class="field-label">Subject</span>
            <span class="field-value">{panelData.data.subject}</span>
          </div>
          {#if panelData.data.description}
            <div class="detail-field">
              <span class="field-label">Description</span>
              <div class="field-value description">{panelData.data.description}</div>
            </div>
          {/if}
          <div class="detail-field">
            <span class="field-label">Status</span>
            <span class="field-value status-badge" style="background: {getStatusColor(panelData.data.status)}">
              {panelData.data.status || 'Unknown'}
            </span>
          </div>
          {#if panelData.data.owner}
            <div class="detail-field">
              <span class="field-label">Owner</span>
              <span class="field-value">{panelData.data.owner}</span>
            </div>
          {/if}
          {#if panelData.data.blocked_by && panelData.data.blocked_by.length > 0}
            <div class="detail-field">
              <span class="field-label">Blocked by</span>
              <div class="field-value">
                {#each panelData.data.blocked_by as blocker}
                  <span class="blocker-tag">!{blocker}</span>
                {/each}
              </div>
            </div>
          {/if}
        </div>
      {:else if panelData.type === 'pr'}
        <!-- PR detail -->
        <div class="detail-section">
          <div class="detail-field">
            <span class="field-label">Title</span>
            <span class="field-value">{panelData.data.title}</span>
          </div>
          {#if panelData.data.author}
            <div class="detail-field">
              <span class="field-label">Author</span>
              <span class="field-value">{panelData.data.author}</span>
            </div>
          {/if}
          {#if panelData.data.reviewer}
            <div class="detail-field">
              <span class="field-label">Reviewer</span>
              <span class="field-value">{panelData.data.reviewer}</span>
            </div>
          {/if}
          {#if panelData.data.status}
            <div class="detail-field">
              <span class="field-label">CI Status</span>
              <span class="field-value status-badge" style="background: {getPrStatusColor(panelData.data.status)}">
                {panelData.data.status}
              </span>
            </div>
          {/if}
          {#if panelData.data.url}
            <div class="detail-field">
              <span class="field-label">GitHub</span>
              <a href={panelData.data.url} target="_blank" rel="noopener" class="field-value link">
                View on GitHub →
              </a>
            </div>
          {/if}
        </div>
      {:else if panelData.type === 'coworker'}
        <!-- Coworker detail -->
        <div class="detail-section">
          <div class="detail-field">
            <span class="field-label">Name</span>
            <span class="field-value">{panelData.data.name}</span>
          </div>
          {#if panelData.data.status}
            <div class="detail-field">
              <span class="field-label">Status</span>
              <span class="field-value status-badge" style="background: {getStatusColor(panelData.data.status)}">
                {panelData.data.status}
              </span>
            </div>
          {/if}
          {#if panelData.data.current_task}
            <div class="detail-field">
              <span class="field-label">Current Task</span>
              <span class="field-value">{panelData.data.current_task}</span>
            </div>
          {/if}
          {#if panelData.data.model}
            <div class="detail-field">
              <span class="field-label">Model</span>
              <span class="field-value model-badge">{panelData.data.model}</span>
            </div>
          {/if}
          {#if panelData.data.started_at}
            <div class="detail-field">
              <span class="field-label">Started</span>
              <span class="field-value">{formatDate(panelData.data.started_at)}</span>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .detail-panel {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: #0f0f0f;
    border-left: 2px solid #2a2a2a;
    grid-area: detail;
  }

  .panel-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 18px;
    background: #1a1a1a;
    border-bottom: 2px solid #2a2a2a;
    flex-shrink: 0;
  }

  .panel-title {
    font-size: 1rem;
    font-weight: 700;
    color: #d0d0d0;
  }

  .close-btn {
    width: 32px;
    height: 32px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: 1px solid #2a2a2a;
    border-radius: 6px;
    color: #808080;
    font-size: 1.3rem;
    cursor: pointer;
    transition: all 0.15s;
    line-height: 1;
  }

  .close-btn:hover {
    background: #1a1a1a;
    border-color: #af5f5f;
    color: #ff5f5f;
  }

  .panel-content {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
  }

  .detail-section {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .detail-field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .field-label {
    font-size: 0.75rem;
    color: #606060;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .field-value {
    font-size: 0.9rem;
    color: #d0d0d0;
  }

  .field-value.description {
    white-space: pre-wrap;
    line-height: 1.5;
    padding: 10px;
    background: #1a1a1a;
    border-radius: 6px;
    border: 1px solid #2a2a2a;
  }

  .field-value.link {
    color: #5fafaf;
    text-decoration: none;
    transition: color 0.15s;
  }

  .field-value.link:hover {
    color: #87d7d7;
    text-decoration: underline;
  }

  .status-badge {
    display: inline-block;
    padding: 4px 10px;
    border-radius: 12px;
    font-size: 0.75rem;
    font-weight: 600;
    color: #0f0f0f;
    text-transform: capitalize;
  }

  .model-badge {
    display: inline-block;
    padding: 4px 10px;
    border-radius: 12px;
    font-size: 0.75rem;
    font-weight: 600;
    background: #2a2a2a;
    color: #a8a8a8;
    text-transform: capitalize;
  }

  .blocker-tag {
    display: inline-block;
    padding: 3px 8px;
    margin-right: 6px;
    margin-bottom: 4px;
    background: #2a2a2a;
    border: 1px solid #3a3a3a;
    border-radius: 4px;
    font-size: 0.8rem;
    color: #af5f5f;
  }

  /* Hide on mobile and medium screens */
  @media (max-width: 1024px) {
    .detail-panel {
      display: none;
    }
  }
</style>
