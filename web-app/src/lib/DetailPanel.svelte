<script>
  let { panelData = null, onClose = () => {} } = $props()

  function handleClose() {
    onClose()
  }

  function handleKeydown(event) {
    if (event.key === 'Escape') {
      handleClose()
    }
  }

  function formatDate(timestamp) {
    if (!timestamp) return 'Unknown'
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

  function getPrStatusColor(status) {
    switch (status?.toLowerCase()) {
      case 'success':
      case 'approved':
        return '#5faf5f'
      case 'pending':
      case 'in_progress':
        return '#d7af5f'
      case 'failure':
      case 'rejected':
        return '#af5f5f'
      default:
        return '#585858'
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

{#if panelData}
  <div class="hidden lg:flex flex-col h-full bg-[#0f0f0f] border-l-2 border-[#2a2a2a] [grid-area:detail]">
    <div class="flex items-center justify-between px-[18px] py-4 bg-[#1a1a1a] border-b-2 border-[#2a2a2a] shrink-0">
      <h2 class="text-base font-bold text-[#d0d0d0]">
        {#if panelData.type === 'task'}
          Task !{panelData.data.id}
        {:else if panelData.type === 'pr'}
          PR #{panelData.data.number}
        {:else if panelData.type === 'coworker'}
          {panelData.data.name}
        {/if}
      </h2>
      <button
        class="w-8 h-8 flex items-center justify-center bg-transparent border border-[#2a2a2a] rounded-md text-[#808080] text-[1.3rem] cursor-pointer transition-all duration-150 leading-none hover:bg-[#1a1a1a] hover:border-[#af5f5f] hover:text-[#ff5f5f]"
        onclick={handleClose}
        aria-label="Close"
      >
        &times;
      </button>
    </div>

    <div class="flex-1 overflow-y-auto p-4">
      {#if panelData.type === 'task'}
        <!-- Task detail -->
        <div class="flex flex-col gap-4">
          <div class="flex flex-col gap-1.5">
            <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Subject</span>
            <span class="text-[0.9rem] text-[#d0d0d0]">{panelData.data.subject}</span>
          </div>
          {#if panelData.data.description}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Description</span>
              <div class="text-[0.9rem] text-[#d0d0d0] whitespace-pre-wrap leading-relaxed p-2.5 bg-[#1a1a1a] rounded-md border border-[#2a2a2a]">
                {panelData.data.description}
              </div>
            </div>
          {/if}
          <div class="flex flex-col gap-1.5">
            <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Status</span>
            <span
              class="inline-block px-2.5 py-1 rounded-xl text-[0.75rem] font-semibold text-[#0f0f0f] capitalize"
              style="background: {getStatusColor(panelData.data.status)}"
            >
              {panelData.data.status || 'Unknown'}
            </span>
          </div>
          {#if panelData.data.owner}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Owner</span>
              <span class="text-[0.9rem] text-[#d0d0d0]">{panelData.data.owner}</span>
            </div>
          {/if}
          {#if panelData.data.blocked_by && panelData.data.blocked_by.length > 0}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Blocked by</span>
              <div class="flex flex-wrap">
                {#each panelData.data.blocked_by as blocker}
                  <span class="inline-block px-2 py-[3px] mr-1.5 mb-1 bg-[#2a2a2a] border border-[#3a3a3a] rounded text-[0.8rem] text-[#af5f5f]">
                    !{blocker}
                  </span>
                {/each}
              </div>
            </div>
          {/if}
        </div>
      {:else if panelData.type === 'pr'}
        <!-- PR detail -->
        <div class="flex flex-col gap-4">
          <div class="flex flex-col gap-1.5">
            <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Title</span>
            <span class="text-[0.9rem] text-[#d0d0d0]">{panelData.data.title}</span>
          </div>
          {#if panelData.data.author}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Author</span>
              <span class="text-[0.9rem] text-[#d0d0d0]">{panelData.data.author}</span>
            </div>
          {/if}
          {#if panelData.data.reviewer}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Reviewer</span>
              <span class="text-[0.9rem] text-[#d0d0d0]">{panelData.data.reviewer}</span>
            </div>
          {/if}
          {#if panelData.data.status}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">CI Status</span>
              <span
                class="inline-block px-2.5 py-1 rounded-xl text-[0.75rem] font-semibold text-[#0f0f0f] capitalize"
                style="background: {getPrStatusColor(panelData.data.status)}"
              >
                {panelData.data.status}
              </span>
            </div>
          {/if}
          {#if panelData.data.url}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">GitHub</span>
              <a
                href={panelData.data.url}
                target="_blank"
                rel="noopener"
                class="text-[#5fafaf] no-underline transition-colors duration-150 hover:text-[#87d7d7] hover:underline"
              >
                View on GitHub &rarr;
              </a>
            </div>
          {/if}
        </div>
      {:else if panelData.type === 'coworker'}
        <!-- Coworker detail -->
        <div class="flex flex-col gap-4">
          <div class="flex flex-col gap-1.5">
            <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Name</span>
            <span class="text-[0.9rem] text-[#d0d0d0]">{panelData.data.name}</span>
          </div>
          {#if panelData.data.status}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Status</span>
              <span
                class="inline-block px-2.5 py-1 rounded-xl text-[0.75rem] font-semibold text-[#0f0f0f] capitalize"
                style="background: {getStatusColor(panelData.data.status)}"
              >
                {panelData.data.status}
              </span>
            </div>
          {/if}
          {#if panelData.data.current_task}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Current Task</span>
              <span class="text-[0.9rem] text-[#d0d0d0]">{panelData.data.current_task}</span>
            </div>
          {/if}
          {#if panelData.data.model}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Model</span>
              <span class="inline-block px-2.5 py-1 rounded-xl text-[0.75rem] font-semibold bg-[#2a2a2a] text-[#a8a8a8] capitalize">
                {panelData.data.model}
              </span>
            </div>
          {/if}
          {#if panelData.data.started_at}
            <div class="flex flex-col gap-1.5">
              <span class="text-[0.75rem] text-[#606060] font-semibold uppercase tracking-wide">Started</span>
              <span class="text-[0.9rem] text-[#d0d0d0]">{formatDate(panelData.data.started_at)}</span>
            </div>
          {/if}
        </div>
      {/if}
    </div>
  </div>
{/if}
