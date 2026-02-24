<script>
  import { messagesByChannel } from './store.js'
  import { AVENUE_COLORS as BASE_AVENUE_COLORS } from './avenue-colors.js'
  import { tick } from 'svelte'
  import { fetchHistory } from './api.js'

  // Read directly from the ops channel (daemon system messages are routed there)
  let opsMessages = $derived(
    ($messagesByChannel['ops'] || []).slice(-100)
  )

  let scrollEl = $state(null)
  let autoScroll = $state(true)
  let collapsed = $state(false)

  function formatTime(timestamp) {
    try {
      const date = new Date(timestamp)
      return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
    } catch {
      return ''
    }
  }

  function getSenderLabel(msg) {
    return msg.from || '?'
  }

  function getContent(msg) {
    if (msg.msg_type === 'action' || msg.content?.startsWith('/me ')) {
      return msg.content.replace(/^\/me\s*/, '')
    }
    return msg.content
  }

  // Sender color palette — matches Channel.svelte with ops-specific overrides
  const AVENUE_COLORS = { ...BASE_AVENUE_COLORS, midtown: '#585858' }

  function getSenderColor(name) {
    return AVENUE_COLORS[name?.toLowerCase()] || '#808080'
  }

  // Pre-populate ops history on mount so the sidebar shows existing messages
  $effect(() => {
    fetchHistory('ops')
  })

  $effect(() => {
    if (opsMessages.length > 0 && autoScroll && scrollEl) {
      tick().then(() => {
        scrollEl.scrollTop = scrollEl.scrollHeight
      })
    }
  })

  function handleScroll() {
    if (!scrollEl) return
    const { scrollTop, scrollHeight, clientHeight } = scrollEl
    autoScroll = scrollHeight - scrollTop - clientHeight < 30
  }
</script>

<div class="overflow-hidden rounded-md border-2 border-[#2a2a2a] bg-[#0a0a0a]">
  <!-- Header with collapse toggle -->
  <button
    class="flex w-full items-center justify-between border-b border-[#1a1a1a] px-3 py-2 text-left bg-transparent cursor-pointer hover:bg-[#111]"
    onclick={() => collapsed = !collapsed}
    aria-label="Toggle Midtown Ops"
  >
    <span class="text-[0.7rem] font-bold tracking-wide text-[#606060]">MIDTOWN OPS</span>
    <span class="text-[0.6rem] text-[#3a3a3a]">{collapsed ? '▶' : '▼'}</span>
  </button>

  {#if !collapsed}
    <div
      class="h-[120px] overflow-y-auto overflow-x-hidden font-mono text-[0.7rem] leading-[1.4] px-2 py-1.5"
      bind:this={scrollEl}
      onscroll={handleScroll}
    >
      {#if opsMessages.length === 0}
        <div class="text-[#3a3a3a] text-center py-4">No ops messages</div>
      {:else}
        {#each opsMessages as msg}
          <div class="flex gap-1 break-words min-w-0">
            <span class="text-[#333] flex-shrink-0 w-[3.2em] text-right">{formatTime(msg.timestamp)}</span>
            {#if msg.msg_type === 'action' || msg.content?.startsWith('/me ')}
              <!-- Action: "* name content" -->
              <span class="flex-shrink-0" style="color: {getSenderColor(msg.from)}">*</span>
              <span class="flex-shrink-0 font-medium" style="color: {getSenderColor(msg.from)}">{getSenderLabel(msg)}</span>
              <span class="flex-1 min-w-0 text-[#5a5a5a]">{getContent(msg)}</span>
            {:else}
              <!-- System: "source message" -->
              <span class="flex-shrink-0 font-medium" style="color: {getSenderColor(msg.from)}">{getSenderLabel(msg)}</span>
              <span class="flex-1 min-w-0 text-[#4a4a4a]">{msg.content}</span>
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  {/if}
</div>
