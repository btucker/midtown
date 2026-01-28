<script>
  import { kanbanData } from './store.js'

  const repoUrl = 'https://github.com/btucker/midtown'

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

<div class="kanban">
  <div class="kanban-column">
    <h3 class="column-title backlog">
      Backlog <span class="count">({$kanbanData.backlog.length})</span>
    </h3>
    <div class="column-items">
      {#each $kanbanData.backlog as task}
        <div class="kanban-card">
          <span class="task-id">#{task.id}</span>
          <span class="task-subject">{task.subject}</span>
        </div>
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
        <div class="kanban-card">
          <div class="card-line">
            <span class="task-id">#{task.id}</span>
            <span class="task-subject">{task.subject}</span>
          </div>
          {#if task.owner}
            <div class="card-detail">{task.owner}</div>
          {/if}
        </div>
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
        <div class="kanban-card">
          <div class="card-line">
            <span class="ci-dot" style="color: {dot.color}">{'\u25CF'}</span>
            <a
              href="{repoUrl}/pull/{pr.number}"
              class="pr-link"
              target="_blank"
              rel="noopener"
            >PR#{pr.number}</a>
            <span class="pr-title">{pr.title}</span>
          </div>
          <div class="card-detail">{pr.author}</div>
        </div>
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
        <div class="kanban-card">
          <a
            href="{repoUrl}/pull/{pr.number}"
            class="pr-link"
            target="_blank"
            rel="noopener"
          >PR#{pr.number}</a>
          <span class="pr-title">{pr.title}</span>
        </div>
      {/each}
      {#if $kanbanData.done.length === 0}
        <div class="empty">No merged PRs</div>
      {/if}
    </div>
  </div>
</div>

<style>
  .kanban {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 8px;
    padding: 8px;
    overflow-x: auto;
    min-height: 120px;
    max-height: 35vh;
    border-bottom: 1px solid #0f3460;
    flex-shrink: 0;
  }

  @media (max-width: 600px) {
    .kanban {
      grid-template-columns: repeat(2, 1fr);
      max-height: 50vh;
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
    padding: 4px 8px;
    font-size: 0.75rem;
    border-radius: 4px;
    margin-bottom: 2px;
    background: #16213e;
    overflow: hidden;
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
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
</style>
