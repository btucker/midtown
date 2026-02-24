<script>
  import { kanbanData } from './store.js'

  let { channelName = '' } = $props()

  /**
   * Compute indentation level for each task based on dependency structure.
   * Returns a Map of task ID to indentation level (0 = no indent, 1+ = nested)
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
   * Filter tasks by channel field.
   * For 'midtown' channel, return all tasks.
   * For topic channels, only return tasks explicitly assigned to that channel.
   *
   * This matches the TUI implementation in src/bin/midtown/cli/chat/ui/board.rs
   * which groups tasks by `task.channel.as_deref().unwrap_or(main_channel)`.
   */
  function filterTasksByChannel(tasks, channel) {
    if (channel === 'midtown') {
      // Main channel shows all tasks, including those with no explicit channel assignment
      return tasks
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

  const indentMap = $derived(computeTaskIndentation(channelTasks))

  // Status marker for each task
  function getStatusMarker(status) {
    return status === 'in_progress' ? '\u2022' : '\u25CB'
  }

  function getStatusColor(status) {
    return status === 'in_progress' ? 'text-primary' : 'text-muted-foreground'
  }
</script>

<div class="flex flex-col gap-px py-1">
  {#each channelTasks as task}
    {@const indentLevel = indentMap.get(task.id) || 0}
    {@const indentStyle = `padding-left: ${indentLevel * 16}px`}

    <div
      class="flex items-baseline gap-1.5 px-2 py-[3px] text-[0.75rem] leading-snug cursor-pointer transition-colors duration-100 rounded hover:bg-sidebar-accent"
      style={indentStyle}
    >
      <span class="shrink-0 text-[0.6rem] {getStatusColor(task.status)}">{getStatusMarker(task.status)}</span>
      <span class="shrink-0 text-muted-foreground font-medium">!{task.id}</span>
      <span class="flex-1 text-muted-foreground truncate {task.status === 'in_progress' ? 'text-foreground' : ''}">{task.subject}</span>
      {#if task.owner}
        <span class="shrink-0 text-[0.7rem] text-muted-foreground">[{task.owner}]</span>
      {/if}
    </div>
  {/each}

  {#if channelTasks.length === 0}
    <div class="py-2 text-[0.75rem] text-muted-foreground italic text-center">No active tasks</div>
  {/if}
</div>
