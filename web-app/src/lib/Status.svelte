<script>
  import { coworkers, daemonStatus } from './store.js'
  import { fetchStatus } from './api.js'
  import { onMount, onDestroy } from 'svelte'

  // Braille spinner animation
  const SPINNER_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
  let spinnerFrame = $state(0)
  let spinnerInterval

  onMount(() => {
    spinnerInterval = setInterval(() => {
      spinnerFrame = (spinnerFrame + 1) % SPINNER_FRAMES.length
    }, 100)
  })

  onDestroy(() => {
    if (spinnerInterval) clearInterval(spinnerInterval)
  })

  function getSpinner() {
    return SPINNER_FRAMES[spinnerFrame]
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

  function getHealthColor(health) {
    switch (health?.toLowerCase()) {
      case 'green':
        return '#5faf5f'
      case 'yellow':
        return '#d7af5f'
      case 'red':
        return '#af5f5f'
      default:
        return '#5faf5f'
    }
  }

  async function refresh() {
    await fetchStatus()
  }
</script>

<div class="p-4 overflow-y-auto h-full">
  <div class="mb-6">
    <div class="flex justify-between items-center mb-3">
      <h2 class="text-base text-[#5fafaf] mb-0">Daemon</h2>
      <button
        class="px-3 py-1.5 border border-[#3a3a3a] rounded bg-transparent text-[#585858] text-xs cursor-pointer hover:border-[#5fafaf] hover:text-[#5fafaf]"
        onclick={refresh}
      >
        Refresh
      </button>
    </div>
    <div class="flex items-center gap-2 p-3 bg-[#262626] rounded-lg">
      <span
        class="w-2.5 h-2.5 rounded-full"
        style="background: {getStatusColor($daemonStatus?.daemon)}"
      ></span>
      <span class="capitalize">{$daemonStatus?.daemon || 'Unknown'}</span>
    </div>
  </div>

  <div class="mb-6">
    <h2 class="text-base text-[#5fafaf] mb-3">Coworkers ({$coworkers.length})</h2>
    {#if $coworkers.length === 0}
      <p class="text-[#585858] italic p-3 bg-[#262626] rounded-lg">No active coworkers</p>
    {:else}
      <div class="flex flex-col gap-2">
        {#each $coworkers as cw}
          <div class="flex items-center gap-2 p-2 bg-[#262626] rounded-lg font-mono text-sm">
            <span class="text-base text-[#d7af5f]">{getSpinner()}</span>
            <span class="font-medium lowercase" style="color: {getHealthColor(cw.health)}">{cw.name}</span>
            {#if cw.phase}
              <span class="hidden text-[0.75rem] text-[#808080] sm:inline">{cw.phase}</span>
            {/if}
            {#if cw.progress != null}
              <span class="hidden text-[0.7rem] text-[#5fafaf] md:inline">{cw.progress}%</span>
            {/if}
            {#if cw.time_estimate}
              <span class="text-[0.7rem] text-[#5faf5f]">{cw.time_estimate}</span>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="mb-6">
    <h2 class="text-base text-[#5fafaf] mb-3">Tasks</h2>
    {#if !$daemonStatus?.tasks || $daemonStatus.tasks.length === 0}
      <p class="text-[#585858] italic p-3 bg-[#262626] rounded-lg">No tasks</p>
    {:else}
      <div class="flex flex-col gap-1">
        {#each $daemonStatus.tasks as task}
          {@const coworker = $coworkers.find(cw => cw.task_id === Number(task.id))}
          {@const progress = coworker?.progress}
          <div class="flex flex-col gap-1.5 px-3 py-2 bg-[#262626] rounded text-[0.85rem]">
            <div class="flex gap-2">
              <span class="text-[#585858] min-w-[30px]">!{task.id}</span>
              <span class="flex-1 line-clamp-2 overflow-hidden">{task.subject}</span>
              <span class="text-[#585858] capitalize">{task.status}</span>
            </div>
            {#if progress != null}
              <div class="flex items-center gap-2 ml-[38px]">
                <div class="flex-1 h-1.5 bg-[#3a3a3a] rounded-full overflow-hidden">
                  <div
                    class="h-full bg-[#5fafaf] rounded-full transition-all duration-300"
                    style="width: {progress}%"
                  ></div>
                </div>
                <span class="text-[#a8a8a8] font-mono text-[0.65rem] min-w-[32px] text-right">{progress}%</span>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
