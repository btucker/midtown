<script>
  import { coworkers, daemonStatus } from './store.js'
  import { fetchStatus } from './api.js'

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
          <div class="p-3 bg-[#262626] rounded-lg">
            <div class="flex justify-between items-center mb-2">
              <span class="font-semibold capitalize">{cw.name}</span>
              <div class="flex gap-1 items-center">
                {#if cw.health}
                  <span
                    class="text-base w-5 h-5 rounded-full flex items-center justify-center text-[#1c1c1c] font-bold"
                    style="background: {cw.health === 'green' ? '#5faf5f' : cw.health === 'yellow' ? '#d7af5f' : '#af5f5f'}"
                  >
                    &bull;
                  </span>
                {/if}
                <span class="text-[0.7rem] px-2 py-0.5 rounded-xl bg-[#3a3a3a] text-[#a8a8a8] capitalize">
                  {cw.model || 'unknown'}
                </span>
              </div>
            </div>
            <div class="flex flex-col gap-1 mt-2">
              {#if cw.task_id}
                <div class="flex gap-2 text-[0.8rem]">
                  <span class="text-[#585858] min-w-[50px]">Task:</span>
                  <span class="text-[#a8a8a8] font-mono">!{cw.task_id}</span>
                </div>
              {/if}
              {#if cw.phase}
                <div class="flex gap-2 text-[0.8rem]">
                  <span class="text-[#585858] min-w-[50px]">Phase:</span>
                  <span class="text-[#a8a8a8] font-mono">{cw.phase}</span>
                </div>
              {/if}
              {#if cw.pr_number}
                <div class="flex gap-2 text-[0.8rem]">
                  <span class="text-[#585858] min-w-[50px]">PR:</span>
                  <span class="text-[#a8a8a8] font-mono">#{cw.pr_number}</span>
                </div>
              {/if}
              {#if cw.progress != null}
                <div class="flex gap-2 text-[0.8rem] items-center">
                  <span class="text-[#585858] min-w-[50px]">Progress:</span>
                  <div class="flex-1 flex items-center gap-2">
                    <div class="flex-1 h-1.5 bg-[#3a3a3a] rounded-full overflow-hidden">
                      <div
                        class="h-full bg-[#5fafaf] rounded-full transition-all duration-300"
                        style="width: {cw.progress}%"
                      ></div>
                    </div>
                    <span class="text-[#a8a8a8] font-mono text-[0.7rem] min-w-[32px] text-right">{cw.progress}%</span>
                  </div>
                </div>
              {/if}
            </div>
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
          <div class="flex gap-2 px-3 py-2 bg-[#262626] rounded text-[0.85rem]">
            <span class="text-[#585858] min-w-[30px]">!{task.id}</span>
            <span class="flex-1 line-clamp-2 overflow-hidden">{task.subject}</span>
            <span class="text-[#585858] capitalize">{task.status}</span>
          </div>
        {/each}
      </div>
    {/if}
  </div>
</div>
