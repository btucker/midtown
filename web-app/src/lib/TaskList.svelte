<script>
  import { kanbanData } from './store.js'

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
    <div
      class="flex items-baseline gap-1.5 px-2 py-[3px] text-[0.75rem] leading-snug cursor-pointer transition-colors duration-100 rounded hover:bg-sidebar-accent"
    >
      <span class="shrink-0 text-[0.6rem] {getStatusColor(task.status)}">{getStatusMarker(task.status)}</span>
      {#if task.blocked_by?.length > 0}
        <span class="shrink-0 text-[0.65rem] text-[#505050]">↳!{task.blocked_by[0]}</span>
      {/if}
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
