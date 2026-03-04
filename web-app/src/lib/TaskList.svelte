<script>
  import { kanbanData, coworkers } from './store.js'
  import { openTaskThread, selectDm } from './api.js'
  import { getSenderColor } from './messageUtils.js'

  let { channelName = '' } = $props()

  /**
   * Filter tasks by channel field.
   * For 'midtown' channel, return all tasks.
   * For topic channels, only return tasks explicitly assigned to that channel.
   *
   * This matches the TUI implementation in src/bin/midtown/cli/chat/ui/board.rs
   * which groups tasks by `task.channel.as_deref().unwrap_or(main_channel)`.
   */
  function filterTasksByChannel(tasks, channel) {
    if (channel === 'midtown') {
      // Main channel shows only tasks with no channel (or channel='midtown').
      // Tasks assigned to other channels appear there only, not duplicated here.
      // Matches TUI's unwrap_or(main_channel) grouping.
      return tasks.filter(task => !task.channel || task.channel === 'midtown')
    }

    // Topic channels only show tasks explicitly assigned to that channel via the channel field
    return tasks.filter(task => task.channel === channel)
  }

  // Derived: tasks for this channel
  const channelTasks = $derived.by(() => {
    const allTasks = [
      ...$kanbanData.inProgress.map(t => ({ ...t, status: 'in_progress' })),
      ...$kanbanData.backlog.map(t => ({ ...t, status: 'pending' }))
    ]
    return filterTasksByChannel(allTasks, channelName)
  })

  // Map coworker name → coworker object for progress/phase lookup
  const cwMap = $derived(new Map($coworkers.map(cw => [cw.name, cw])))

  // Map task_id → PR reviewer for showing reviewer avatar
  const taskReviewerMap = $derived.by(() => {
    const map = new Map()
    for (const pr of $kanbanData.review) {
      if (pr.task_id != null && pr.reviewer) {
        map.set(String(pr.task_id), pr.reviewer)
      }
    }
    return map
  })

  function statusBarColor(task, cw) {
    if (task.status !== 'in_progress') return 'hsl(var(--muted-foreground) / 0.3)'
    // Use the owner's avatar color for the status bar when in-progress
    if (task.owner) return getSenderColor(task.owner)
    return 'hsl(var(--accent-teal))'
  }

  function handleTaskClick(task) {
    openTaskThread(task, task.channel || channelName)
  }
</script>

<div class="task-list">
  {#each channelTasks as task}
    {@const isActive = task.status === 'in_progress'}
    {@const isBlocked = task.blocked_by?.length > 0}
    {@const cw = task.owner ? cwMap.get(task.owner) : null}
    {@const hasProgress = cw?.progress != null}
    {@const reviewer = taskReviewerMap.get(String(task.id))}
    <button
      class="task-row"
      class:active={isActive}
      class:blocked={isBlocked}
      onclick={() => handleTaskClick(task)}
    >
      <span class="status-bar" style="background: {statusBarColor(task, cw)}"></span>
      <div class="task-content">
        <div class="task-top-line">
          <span class="task-id">!{task.id}</span>
          <span class="task-subject">{task.subject}</span>
          {#if isBlocked}
            <span class="blocked-badge" title="Blocked by !{task.blocked_by[0]}">⧗ !{task.blocked_by[0]}</span>
          {/if}
          {#if task.owner}
            <button
              class="owner-chip"
              style="background-color: {getSenderColor(task.owner)}"
              title="{task.owner}{cw?.phase ? ` · ${cw.phase}` : ''}"
              onclick={(e) => { e.stopPropagation(); selectDm(task.owner) }}
            >{task.owner[0].toUpperCase()}</button>
          {/if}
          {#if reviewer}
            <button
              class="reviewer-chip"
              style="border-color: {getSenderColor(reviewer)}"
              title="{reviewer} · reviewing"
              onclick={(e) => { e.stopPropagation(); selectDm(reviewer) }}
            >{reviewer[0].toUpperCase()}</button>
          {/if}
        </div>
        {#if isActive && hasProgress}
          <div class="progress-row">
            <div class="progress-track">
              <div
                class="progress-fill"
                style="width: {cw.progress}%; background: {getSenderColor(task.owner)}"
              ></div>
            </div>
            <span class="progress-label">{cw.progress}%</span>
          </div>
        {/if}
      </div>
    </button>
  {/each}

  {#if channelTasks.length === 0}
    <div class="empty">No active tasks</div>
  {/if}
</div>

<style>
  .task-list {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 4px 0 6px;
  }

  .task-row {
    display: flex;
    align-items: stretch;
    gap: 6px;
    padding: 5px 8px 5px 0;
    border: none;
    background: transparent;
    cursor: pointer;
    border-radius: 5px;
    transition: background 0.1s;
    text-align: left;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    line-height: 1.3;
    color: hsl(var(--muted-foreground));
  }

  .task-row:hover {
    background: hsl(var(--sidebar-accent));
  }

  .task-row.active {
    color: hsl(var(--sidebar-foreground));
  }

  .status-bar {
    width: 3px;
    border-radius: 2px;
    flex-shrink: 0;
  }

  .task-row.active .status-bar {
  }

  .task-content {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .task-top-line {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .task-id {
    flex-shrink: 0;
    font-weight: 600;
    opacity: 0.6;
  }

  .task-row.active .task-id {
    opacity: 0.8;
  }

  .task-subject {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .blocked-badge {
    flex-shrink: 0;
    font-size: 0.62rem;
    color: hsl(var(--status-amber));
    opacity: 0.85;
  }

  .task-row.blocked {
    opacity: 0.65;
  }

  .owner-chip {
    flex-shrink: 0;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    border: none;
    padding: 0;
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.55rem;
    font-weight: 700;
    font-family: var(--font-sans);
    color: white;
    line-height: 1;
    cursor: pointer;
  }

  .owner-chip:hover {
    opacity: 0.85;
  }

  .reviewer-chip {
    flex-shrink: 0;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    border: 1.5px solid;
    padding: 0;
    margin-left: -4px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.55rem;
    font-weight: 700;
    font-family: var(--font-sans);
    color: hsl(var(--muted-foreground));
    background: hsl(var(--sidebar-background));
    line-height: 1;
    cursor: pointer;
  }

  .reviewer-chip:hover {
    opacity: 0.85;
  }

  .progress-row {
    display: flex;
    align-items: center;
    gap: 6px;
    padding-right: 2px;
  }

  .progress-track {
    flex: 1;
    height: 3px;
    background: hsl(var(--sidebar-accent));
    border-radius: 2px;
    overflow: hidden;
  }

  .progress-fill {
    height: 100%;
    border-radius: 2px;
    transition: width 0.5s ease;
  }

  .progress-label {
    flex-shrink: 0;
    font-size: 0.6rem;
    color: hsl(var(--accent-teal));
    font-variant-numeric: tabular-nums;
  }

  .empty {
    padding: 8px 12px;
    font-size: 0.72rem;
    color: hsl(var(--muted-foreground));
    font-style: italic;
    text-align: center;
  }
</style>
