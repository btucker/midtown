<script>
  import { coworkers, maxCoworkers } from './store.js'
  import { onMount, onDestroy } from 'svelte'

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

  // Braille spinner animation
  const SPINNER_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏']
  let spinnerFrame = $state(0)
  let spinnerInterval

  onMount(() => {
    // Faster spinner animation (100ms per frame instead of default)
    spinnerInterval = setInterval(() => {
      spinnerFrame = (spinnerFrame + 1) % SPINNER_FRAMES.length
    }, 100)
  })

  onDestroy(() => {
    if (spinnerInterval) clearInterval(spinnerInterval)
  })

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

  function getSpinner() {
    return SPINNER_FRAMES[spinnerFrame]
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
        <div class="flex items-center gap-1.5 px-1.5 py-1 font-mono text-sm leading-normal">
          <span class="shrink-0 text-base leading-none text-[#d7af5f]">{getSpinner()}</span>
          <span class="shrink-0 font-medium lowercase" style="color: {getHealthColor(cw.health)}">{cw.name}</span>
          {#if cw.phase}
            <span class="hidden text-[0.75rem] text-[#808080] sm:inline">{cw.phase}</span>
          {/if}
          {#if cw.progress !== undefined && cw.progress !== null}
            <span class="hidden text-[0.7rem] text-[#5fafaf] md:inline">{cw.progress}%</span>
          {/if}
          {#if cw.time_estimate}
            <span class="text-[0.7rem] text-[#5faf5f]">{cw.time_estimate}</span>
          {/if}
        </div>
      {/each}
    </div>
  </div>
{/if}
