<script>
  import { coworkers, maxCoworkers } from './store.js'

  // Filter to only active coworkers (matching TUI logic - skip idle/completed)
  let activeCoworkers = $derived(
    $coworkers.filter((cw) => {
      // If phase is present, filter by phase (skip idle/completed)
      if (cw.phase !== undefined) {
        return cw.phase !== null
      }
      // Otherwise filter by status
      return cw.status !== 'idle' && cw.status !== 'stopped'
    })
  )

  function getHealthColor(health) {
    switch (health?.toLowerCase()) {
      case 'green':
        return '#5faf5f'
      case 'yellow':
        return '#d7af5f'
      case 'red':
        return '#af5f5f'
      default:
        return '#5faf5f' // default to green
    }
  }
</script>

{#if activeCoworkers.length > 0}
  <div class="overflow-hidden rounded-md border-2 border-[#2a2a2a] bg-[#0a0a0a]">
    <div class="border-b border-[#1a1a1a] px-3 py-2">
      <span class="text-[0.75rem] font-bold tracking-wide text-[#5fafaf]">
        Coworkers ({activeCoworkers.length}/{$maxCoworkers})
      </span>
    </div>
    <div class="p-1.5">
      {#each activeCoworkers as cw}
        <div class="flex flex-col gap-0.5 px-1.5 py-1 font-mono text-sm leading-normal">
          <div class="flex items-center gap-1.5">
            <span class="shrink-0 text-base leading-none" style="color: {getHealthColor(cw.health)}">●</span>
            <span class="font-medium lowercase text-[#d0d0d0]">{cw.name}</span>
            {#if cw.task_id}
              <span class="font-semibold text-[#d7af5f]">!{cw.task_id}</span>
            {/if}
            {#if cw.phase}
              <span class="text-[0.75rem] text-[#808080]">{cw.phase}</span>
            {/if}
            {#if cw.pr_number}
              <span class="font-medium text-[#5fafaf]">#{cw.pr_number}</span>
            {/if}
          </div>
          {#if cw.progress !== undefined && cw.progress !== null}
            <div class="ml-6 flex items-center gap-1.5">
              <div class="h-1.5 w-24 overflow-hidden rounded-full bg-[#2a2a2a]">
                <div
                  class="h-full rounded-full bg-[#5fafaf] transition-all duration-300"
                  style="width: {cw.progress}%"
                ></div>
              </div>
              <span class="text-[0.65rem] text-[#808080]">{cw.progress}%</span>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
{/if}
