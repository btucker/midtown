<script>
  import { coworkers, maxCoworkers, repoStatus, kanbanData, isWideScreen } from './store.js'
  import { onMount, onDestroy } from 'svelte'
  import { detailPanelData } from './store.js'
  import { closeThread } from './api.js'

  // Filter to only active coworkers (matching TUI logic - skip idle/completed).
  // Phase may be missing (freshly spawned / not reporting), null (serialized absent
  // phase from /status), or a string abbreviation.
  let activeCoworkers = $derived(
    $coworkers.filter((cw) => {
      // If phase is present as a non-empty string, filter idle/completed out.
      if (typeof cw.phase === 'string' && cw.phase.length > 0) {
        const phase = cw.phase.toLowerCase()
        return phase !== 'idle' && phase !== 'done'
      }

      // Otherwise filter by status (status is typically present on websocket status
      // updates but not always included in /status payloads).
      const status = (cw.status || '').toLowerCase()
      return status !== 'idle' && status !== 'stopped'
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
        return 'hsl(var(--status-green))'
      case 'yellow':
        return 'hsl(var(--status-amber))'
      case 'red':
        return 'hsl(var(--status-red))'
      default:
        return 'hsl(var(--status-green))'
    }
  }

  function getSpinner() {
    return SPINNER_FRAMES[spinnerFrame]
  }

  function getPrUrl(prNumber) {
    if (!prNumber || !$repoStatus.fullName) return null
    return `https://github.com/${$repoStatus.fullName}/pull/${prNumber}`
  }

  function openTaskDetail(taskId) {
    const allTasks = [...$kanbanData.inProgress, ...$kanbanData.backlog]
    const task = allTasks.find((t) => String(t.id) === String(taskId))
    if (task && $isWideScreen) {
      closeThread()
      detailPanelData.set({ type: 'task', data: task })
    }
  }

  function openPrDetail(prNumber) {
    const url = getPrUrl(prNumber)
    if (!url) return
    const pr = $kanbanData.review.find((p) => String(p.number) === String(prNumber))
    if (pr && $isWideScreen) {
      closeThread()
      detailPanelData.set({
        type: 'pr',
        data: {
          number: pr.number,
          title: pr.title,
          author: pr.author,
          reviewer: pr.reviewer,
          status: pr.status,
          url,
        },
      })
    } else {
      window.open(url, '_blank', 'noopener')
    }
  }
</script>

{#if activeCoworkers.length > 0}
  <div class="overflow-hidden rounded-md border-2 border-sidebar-border bg-sidebar">
    <div class="border-b border-sidebar-border px-3 py-2">
      <span class="text-[0.75rem] font-bold tracking-wide text-sidebar-primary">
        Coworkers ({activeCoworkers.length}/{$maxCoworkers})
      </span>
    </div>
    <div class="p-1.5">
      {#each activeCoworkers as cw}
        <div class="flex flex-col gap-0.5 px-1.5 py-1 font-mono text-sm leading-normal">
          <div class="flex items-center gap-1.5">
            <span class="shrink-0 text-base leading-none text-status-amber">{getSpinner()}</span>
            <span class="shrink-0 font-medium lowercase" style="color: {getHealthColor(cw.health)}">{cw.name}</span>
            {#if cw.phase}
              <span class="hidden text-[0.75rem] text-muted-foreground sm:inline">{cw.phase}</span>
            {/if}
            {#if cw.task_id}
              <button
                class="hidden text-[0.7rem] text-muted-foreground hover:text-primary transition-colors duration-100 cursor-pointer bg-transparent border-none p-0 sm:inline"
                onclick={() => openTaskDetail(cw.task_id)}
                title="View task details"
              >!{cw.task_id}</button>
            {/if}
            {#if cw.pr_number}
              <a
                class="hidden text-[0.7rem] text-muted-foreground hover:text-primary transition-colors duration-100 sm:inline"
                href={getPrUrl(cw.pr_number)}
                target="_blank"
                rel="noopener"
                title="View PR on GitHub"
                onclick={(e) => { e.preventDefault(); openPrDetail(cw.pr_number) }}
              >#{cw.pr_number}</a>
            {/if}
            <span class="flex-1"></span>
            {#if cw.time_estimate}
              <span class="text-[0.7rem] text-primary">{cw.time_estimate}</span>
            {:else if cw.progress !== undefined && cw.progress !== null}
              <span class="text-[0.7rem] text-accent-teal">{cw.progress}%</span>
            {/if}
          </div>
          {#if cw.progress !== undefined && cw.progress !== null}
            <div class="ml-[26px] flex items-center gap-1.5">
              <div class="flex-1 h-1 bg-sidebar-accent rounded-full overflow-hidden">
                <div
                  class="h-full bg-accent-teal rounded-full transition-all duration-500"
                  style="width: {cw.progress}%"
                ></div>
              </div>
            </div>
          {/if}
        </div>
      {/each}
    </div>
  </div>
{/if}
