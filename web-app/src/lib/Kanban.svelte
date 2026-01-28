<script>
  import { kanbanData } from './store.js'

  const repoUrl = 'https://github.com/btucker/midtown'

  let expanded = $state(false)
  let selectedItem = $state(null)

  function toggleExpand() {
    expanded = !expanded
  }

  function selectTask(task) {
    selectedItem = { type: 'task', data: task }
  }

  function selectPr(pr) {
    selectedItem = { type: 'pr', data: pr }
  }

  function closeModal() {
    selectedItem = null
  }

  function handleKeydown(event) {
    if (event.key === 'Escape' && selectedItem) {
      closeModal()
    }
  }

  function formatRelativeTime(timestamp) {
    if (!timestamp) return ''
    try {
      const date = new Date(timestamp)
      const now = new Date()
      const diffMs = now - date
      const diffMins = Math.floor(diffMs / 60000)

      if (diffMins < 1) return '<1m'
      if (diffMins < 60) return `${diffMins}m`
      const diffHours = Math.floor(diffMins / 60)
      if (diffHours < 24) return `${diffHours}h`
      const diffDays = Math.floor(diffHours / 24)
      return `${diffDays}d`
    } catch {
      return ''
    }
  }

  function ciDot(status) {
    switch (status) {
      case 'approved':
        return { color: '#4ade80' }
      case 'changes requested':
        return { color: '#e94560' }
      case 'awaiting review':
        return { color: '#fbbf24' }
      default:
        return { color: '#666' }
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="kanban-wrapper" class:expanded>
  <div class="kanban-header">
    <span class="kanban-title">Kanban</span>
    <button class="expand-toggle" onclick={toggleExpand} aria-label={expanded ? 'Collapse' : 'Expand'}>
      <span class="chevron" class:expanded>▲</span>
    </button>
  </div>

  <div class="kanban">
    <div class="kanban-column">
      <h3 class="column-title backlog">
        Backlog <span class="count">({$kanbanData.backlog.length})</span>
      </h3>
      <div class="column-items">
        {#each $kanbanData.backlog as task}
          <button class="kanban-card clickable" onclick={() => selectTask(task)}>
            <span class="task-id">#{task.id}</span>
            <span class="task-subject">{task.subject}</span>
          </button>
        {/each}
        {#if $kanbanData.backlog.length === 0}
          <div class="empty">No tasks</div>
        {/if}
      </div>
    </div>

    <div class="kanban-column">
      <h3 class="column-title in-progress">
        In Progress <span class="count">({$kanbanData.inProgress.length})</span>
      </h3>
      <div class="column-items">
        {#each $kanbanData.inProgress as task}
          <button class="kanban-card clickable" onclick={() => selectTask(task)}>
            <div class="card-line">
              <span class="task-id">#{task.id}</span>
              <span class="task-subject">{task.subject}</span>
            </div>
            {#if task.owner}
              <div class="card-detail">{task.owner}</div>
            {/if}
          </button>
        {/each}
        {#if $kanbanData.inProgress.length === 0}
          <div class="empty">No tasks</div>
        {/if}
      </div>
    </div>

    <div class="kanban-column">
      <h3 class="column-title review">
        Review <span class="count">({$kanbanData.review.length})</span>
      </h3>
      <div class="column-items">
        {#each $kanbanData.review as pr}
          {@const dot = ciDot(pr.status)}
          <button class="kanban-card clickable" onclick={() => selectPr(pr)}>
            <div class="card-line">
              <span class="ci-dot" style="color: {dot.color}">{'\u25CF'}</span>
              <a
                href="{repoUrl}/pull/{pr.number}"
                class="pr-link"
                target="_blank"
                rel="noopener"
                onclick={(e) => e.stopPropagation()}
              >PR#{pr.number}</a>
              <span class="pr-title">{pr.title}</span>
            </div>
            <div class="card-detail">by: {pr.author}{pr.created_at ? ` (${formatRelativeTime(pr.created_at)})` : ''}</div>
            {#if pr.reviewer}
              <div class="card-detail">rev: {pr.reviewer}{pr.reviewer_assigned_at ? ` (${formatRelativeTime(pr.reviewer_assigned_at)})` : ''}</div>
            {/if}
          </button>
        {/each}
        {#if $kanbanData.review.length === 0}
          <div class="empty">No PRs</div>
        {/if}
      </div>
    </div>

    <div class="kanban-column">
      <h3 class="column-title done">
        Done <span class="count">({$kanbanData.done.length})</span>
      </h3>
      <div class="column-items">
        {#each $kanbanData.done as pr}
          <button class="kanban-card clickable" onclick={() => selectPr(pr)}>
            <div class="pr-number-line">
              <a
                href="{repoUrl}/pull/{pr.number}"
                class="pr-link"
                target="_blank"
                rel="noopener"
                onclick={(e) => e.stopPropagation()}
              >PR#{pr.number}</a>
            </div>
            <span class="pr-title">{pr.title}</span>
          </button>
        {/each}
        {#if $kanbanData.done.length === 0}
          <div class="empty">No merged PRs</div>
        {/if}
      </div>
    </div>
  </div>
</div>

<!-- Modal for full details -->
{#if selectedItem}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_interactive_supports_focus -->
  <div class="modal-overlay" onclick={closeModal} role="dialog" aria-modal="true" tabindex="-1">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
    <div class="modal-content" role="document" onclick={(e) => e.stopPropagation()}>
      <button class="modal-close" onclick={closeModal} aria-label="Close">×</button>

      {#if selectedItem.type === 'task'}
        <div class="modal-header">
          <span class="modal-id">#{selectedItem.data.id}</span>
          <span class="modal-status">{selectedItem.data.status}</span>
        </div>
        <h4 class="modal-title">{selectedItem.data.subject}</h4>
        {#if selectedItem.data.description}
          <p class="modal-description">{selectedItem.data.description}</p>
        {:else}
          <p class="modal-description empty">No description</p>
        {/if}
        {#if selectedItem.data.owner}
          <div class="modal-meta">
            <span class="meta-label">Owner:</span>
            <span class="meta-value">{selectedItem.data.owner}</span>
          </div>
        {/if}
      {:else if selectedItem.type === 'pr'}
        <div class="modal-header">
          <span class="modal-id">PR#{selectedItem.data.number}</span>
          <span class="modal-status">{selectedItem.data.status}</span>
        </div>
        <h4 class="modal-title">{selectedItem.data.title}</h4>
        <div class="modal-meta">
          <span class="meta-label">Author:</span>
          <span class="meta-value">{selectedItem.data.author}</span>
        </div>
        {#if selectedItem.data.reviewer}
          <div class="modal-meta">
            <span class="meta-label">Reviewer:</span>
            <span class="meta-value">{selectedItem.data.reviewer}</span>
          </div>
        {/if}
      {/if}
    </div>
  </div>
{/if}

<style>
  .kanban-wrapper {
    border-bottom: 1px solid #0f3460;
    flex-shrink: 0;
    max-height: 15vh;
    overflow: hidden;
    transition: max-height 0.3s ease-in-out;
  }

  .kanban-wrapper.expanded {
    max-height: 50vh;
  }

  .kanban-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 4px 8px;
    background: #16213e;
  }

  .kanban-title {
    font-size: 0.7rem;
    color: #00d9ff;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .expand-toggle {
    background: transparent;
    border: none;
    color: #666;
    cursor: pointer;
    padding: 2px 6px;
    font-size: 0.65rem;
    transition: color 0.2s;
  }

  .expand-toggle:hover {
    color: #00d9ff;
  }

  .chevron {
    display: inline-block;
    transition: transform 0.3s ease-in-out;
  }

  .chevron.expanded {
    transform: rotate(180deg);
  }

  .kanban {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
    padding: 8px;
    overflow-x: auto;
    overflow-y: auto;
    height: calc(100% - 28px);
  }

  @media (max-width: 600px) {
    .kanban {
      grid-template-columns: repeat(2, 1fr);
    }
  }

  .kanban-column {
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
  }

  .column-title {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    padding: 4px 8px;
    margin: 0;
    border-radius: 4px 4px 0 0;
    white-space: nowrap;
  }

  .column-title .count {
    opacity: 0.6;
    font-weight: normal;
  }

  .column-title.backlog {
    color: #60a5fa;
    border-bottom: 2px solid #60a5fa;
  }
  .column-title.in-progress {
    color: #fbbf24;
    border-bottom: 2px solid #fbbf24;
  }
  .column-title.review {
    color: #c084fc;
    border-bottom: 2px solid #c084fc;
  }
  .column-title.done {
    color: #4ade80;
    border-bottom: 2px solid #4ade80;
  }

  .column-items {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0;
  }

  .kanban-card {
    display: block;
    width: 100%;
    padding: 4px 8px;
    font-size: 0.75rem;
    border-radius: 4px;
    margin-bottom: 2px;
    background: #16213e;
    overflow: hidden;
    border: 1px solid transparent;
    text-align: left;
    font-family: inherit;
    color: inherit;
  }

  .clickable {
    cursor: pointer;
    transition: background-color 0.15s, border-color 0.15s;
  }

  .clickable:hover {
    background: #1a2744;
    border-color: #0f3460;
  }

  .clickable:focus {
    outline: none;
    border-color: #00d9ff;
  }

  .card-line {
    display: flex;
    gap: 4px;
    align-items: baseline;
    overflow: hidden;
  }

  .card-detail {
    padding-left: 12px;
    font-size: 0.65rem;
    color: #888;
  }

  .task-id {
    color: #888;
    flex-shrink: 0;
  }

  .task-subject,
  .pr-title {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    line-height: 1.3;
  }

  .pr-number-line {
    margin-bottom: 2px;
  }

  .ci-dot {
    flex-shrink: 0;
    font-size: 0.6rem;
  }

  .pr-link {
    color: #c084fc;
    text-decoration: none;
    flex-shrink: 0;
  }

  .pr-link:hover {
    text-decoration: underline;
  }

  .empty {
    color: #444;
    font-size: 0.7rem;
    padding: 8px;
    font-style: italic;
  }

  /* Modal styles */
  .modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 16px;
  }

  .modal-content {
    background: #16213e;
    border-radius: 8px;
    padding: 16px;
    max-width: 400px;
    width: 100%;
    max-height: 80vh;
    overflow-y: auto;
    position: relative;
    border: 1px solid #0f3460;
  }

  .modal-close {
    position: absolute;
    top: 8px;
    right: 8px;
    background: transparent;
    border: none;
    color: #666;
    font-size: 1.5rem;
    cursor: pointer;
    padding: 4px 8px;
    line-height: 1;
  }

  .modal-close:hover {
    color: #00d9ff;
  }

  .modal-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }

  .modal-id {
    color: #00d9ff;
    font-family: ui-monospace, monospace;
    font-size: 0.85rem;
  }

  .modal-status {
    font-size: 0.7rem;
    padding: 2px 8px;
    border-radius: 12px;
    background: #0f3460;
    color: #888;
    text-transform: capitalize;
  }

  .modal-title {
    color: #eee;
    font-size: 1rem;
    font-weight: 600;
    margin: 0 0 12px 0;
    line-height: 1.4;
  }

  .modal-description {
    color: #aaa;
    font-size: 0.85rem;
    line-height: 1.5;
    margin: 0 0 12px 0;
    white-space: pre-wrap;
  }

  .modal-description.empty {
    color: #666;
    font-style: italic;
  }

  .modal-meta {
    display: flex;
    gap: 8px;
    font-size: 0.8rem;
    margin-bottom: 4px;
  }

  .meta-label {
    color: #666;
  }

  .meta-value {
    color: #ccc;
  }
</style>
