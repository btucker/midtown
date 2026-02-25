<script>
  import { coworkers, maxCoworkers, repoStatus, kanbanData } from './store.js'
  import { openTaskThread } from './api.js'
  import { getSenderColor } from './messageUtils.js'
  import * as Tooltip from '$lib/components/ui/tooltip/index.js'

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

  function avatarLetter(name) {
    return (name || '?')[0].toUpperCase()
  }

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

  function getPrUrl(prNumber) {
    if (!prNumber || !$repoStatus.fullName) return null
    return `https://github.com/${$repoStatus.fullName}/pull/${prNumber}`
  }

  function openTaskDetail(taskId) {
    const allTasks = [...$kanbanData.inProgress, ...$kanbanData.backlog]
    const task = allTasks.find((t) => String(t.id) === String(taskId))
    if (task) {
      openTaskThread(task, task.channel || 'midtown')
    }
  }

  function openPrDetail(prNumber) {
    const url = getPrUrl(prNumber)
    if (url) {
      window.open(url, '_blank', 'noopener')
    }
  }
</script>

{#if activeCoworkers.length > 0}
  <div class="overflow-hidden rounded-md bg-sidebar">
    <div class="px-3 py-2">
      <span class="text-[0.75rem] font-bold tracking-wide text-sidebar-primary">
        Coworkers ({activeCoworkers.length}/{$maxCoworkers})
      </span>
    </div>
    <div class="p-1.5">
      {#each activeCoworkers as cw}
        <div class="flex flex-col gap-0.5 px-1.5 py-1 font-mono text-sm leading-normal">
          <div class="flex items-center gap-1.5">
            <Tooltip.Root>
              <Tooltip.Trigger>
                <span class="relative shrink-0 w-5 h-5 inline-block">
                  <span
                    class="flex items-center justify-center w-5 h-5 rounded text-[0.6rem] font-bold text-white select-none"
                    style="background-color: {getSenderColor(cw.name)}"
                  >{avatarLetter(cw.name)}</span>
                  <span class="absolute -bottom-0.5 -right-0.5 w-2 h-2 rounded-full border-2 border-sidebar"
                    style="background-color: {getHealthColor(cw.health)}"></span>
                </span>
              </Tooltip.Trigger>
              <Tooltip.Content side="top">{cw.name}</Tooltip.Content>
            </Tooltip.Root>
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
            <div class="ml-[26px] flex items-center gap-1.5" style="margin-left: calc(1.25rem + 0.375rem)">
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
