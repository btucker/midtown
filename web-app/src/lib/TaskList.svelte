<script>
  import { kanbanData } from './store.js'
  import { matchesChannel } from './channelUtils.js'

  let { channelName = '' } = $props()

  /**
   * Compute indentation level for each task based on dependency structure.
   * Returns a Map of task ID → indentation level (0 = no indent, 1+ = nested)
   *
   * This mirrors the TUI implementation in src/bin/midtown/cli/chat/ui/board.rs
   */
  function computeTaskIndentation(tasks) {
    const indentation = new Map()
    const processed = new Set()
    const taskMap = new Map(tasks.map(t => [t.id, t]))

    function computeIndentationRecursive(taskId) {
      // If already computed, return cached value
      if (indentation.has(taskId)) {
        return indentation.get(taskId)
      }

      // Guard against cycles
      if (processed.has(taskId)) {
        return 0
      }
      processed.add(taskId)

      const task = taskMap.get(taskId)
      if (!task) {
        indentation.set(taskId, 0)
        return 0
      }

      // If no dependencies, no indentation
      if (!task.blocked_by || task.blocked_by.length === 0) {
        indentation.set(taskId, 0)
        return 0
      }

      // Find the first unresolved dependency in the current task list
      const firstBlocker = task.blocked_by.find(blockerId => taskMap.has(blockerId))

      const level = firstBlocker
        ? computeIndentationRecursive(firstBlocker) + 1
        : 0

      indentation.set(taskId, level)
      return level
    }

    // Process all tasks
    for (const task of tasks) {
      computeIndentationRecursive(task.id)
    }

    return indentation
  }

  /**
   * Filter tasks by channel name (whole word match in subject/description).
   * For 'midtown' channel, return all tasks.
   */
  function filterTasksByChannel(tasks, channel) {
    if (channel === 'midtown') {
      return tasks
    }

    return tasks.filter(task =>
      matchesChannel(task.subject, channel) ||
      matchesChannel(task.description, channel)
    )
  }

  // Derived: tasks for this channel
  const channelTasks = $derived.by(() => {
    const allTasks = [
      ...$kanbanData.inProgress.map(t => ({ ...t, status: 'in_progress' })),
      ...$kanbanData.backlog.map(t => ({ ...t, status: 'pending' }))
    ]
    return filterTasksByChannel(allTasks, channelName)
  })

  const indentMap = $derived(computeTaskIndentation(channelTasks))

  // Status marker for each task
  function getStatusMarker(status) {
    return status === 'in_progress' ? '●' : '○'
  }

  function getStatusColor(status) {
    return status === 'in_progress' ? 'status-in-progress' : 'status-pending'
  }
</script>

<div class="task-list">
  {#each channelTasks as task}
    {@const indentLevel = indentMap.get(task.id) || 0}
    {@const indentStyle = `padding-left: ${indentLevel * 16}px`}

    <div class="task-item {getStatusColor(task.status)}" style={indentStyle}>
      <span class="status-marker">{getStatusMarker(task.status)}</span>
      <span class="task-id">!{task.id}</span>
      <span class="task-subject">{task.subject}</span>
      {#if task.owner}
        <span class="task-owner">[{task.owner}]</span>
      {/if}
    </div>
  {/each}

  {#if channelTasks.length === 0}
    <div class="empty-state">No active tasks</div>
  {/if}
</div>

<style>
  .task-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
    padding: 4px 0;
  }

  .task-item {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 3px 8px;
    font-size: 0.75rem;
    line-height: 1.4;
    cursor: pointer;
    transition: background 0.1s;
    border-radius: 3px;
  }

  .task-item:hover {
    background: #1a1a1a;
  }

  .status-marker {
    flex-shrink: 0;
    font-size: 0.6rem;
  }

  .status-in-progress .status-marker {
    color: #4ade80;
  }

  .status-pending .status-marker {
    color: #606060;
  }

  .task-id {
    flex-shrink: 0;
    color: #606060;
    font-weight: 500;
  }

  .task-subject {
    flex: 1;
    color: #a8a8a8;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .status-in-progress .task-subject {
    color: #d0d0d0;
  }

  .task-owner {
    flex-shrink: 0;
    font-size: 0.7rem;
    color: #606060;
  }

  .empty-state {
    padding: 8px;
    font-size: 0.75rem;
    color: #444;
    font-style: italic;
    text-align: center;
  }
</style>
