<script>
  import { messagesByChannel } from './store.js'
  import { tick } from 'svelte'
  import { fetchHistory } from './api.js'
  import { getSenderColor, formatTime } from './messageUtils.js'

  const OPS_SENDER_OVERRIDES = {
    midtown: '#585858',
  }

  // Read directly from the ops channel (daemon system messages are routed there)
  let opsMessages = $derived(
    ($messagesByChannel['ops'] || []).slice(-100)
  )

  let scrollEl = $state(null)
  let autoScroll = $state(true)
  let collapsed = $state(false)

  function getSenderLabel(msg) {
    return msg.from || '?'
  }

  function getContent(msg) {
    if (msg.msg_type === 'action' || msg.content?.startsWith('/me ')) {
      return msg.content.replace(/^\/me\s*/, '')
    }
    return msg.content
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

<div class="overflow-hidden rounded-md border-2 border-sidebar-border bg-sidebar">
  <!-- Header with collapse toggle -->
  <button
    class="flex w-full items-center justify-between border-b border-sidebar-border px-3 py-2 text-left bg-transparent cursor-pointer hover:bg-sidebar-accent"
    onclick={() => collapsed = !collapsed}
    aria-label="Toggle Midtown Ops"
  >
    <span class="text-[0.7rem] font-bold tracking-wide text-muted-foreground">MIDTOWN OPS</span>
    <span class="text-[0.6rem] text-muted-foreground/40">{collapsed ? '▶' : '▼'}</span>
  </button>

  {#if !collapsed}
    <div
      class="h-[120px] overflow-y-auto overflow-x-hidden font-mono text-[0.7rem] leading-[1.4] px-2 py-1.5"
      bind:this={scrollEl}
      onscroll={handleScroll}
    >
      {#if opsMessages.length === 0}
        <div class="text-muted-foreground text-center py-4">No ops messages</div>
      {:else}
        {#each opsMessages as msg}
          <div class="flex gap-1 break-words min-w-0">
            <span class="text-muted-foreground/60 flex-shrink-0 w-[3.2em] text-right">{formatTime(msg.timestamp)}</span>
            {#if msg.msg_type === 'action' || msg.content?.startsWith('/me ')}
              <!-- Action: "* name content" -->
              <span class="flex-shrink-0" style="color: {getSenderColor(msg.from, OPS_SENDER_OVERRIDES)}">*</span>
              <span class="flex-shrink-0 font-medium" style="color: {getSenderColor(msg.from, OPS_SENDER_OVERRIDES)}">{getSenderLabel(msg)}</span>
              <span class="flex-1 min-w-0 text-muted-foreground/80">{getContent(msg)}</span>
            {:else}
              <!-- System: "source message" -->
              <span class="flex-shrink-0 font-medium" style="color: {getSenderColor(msg.from, OPS_SENDER_OVERRIDES)}">{getSenderLabel(msg)}</span>
              <span class="flex-1 min-w-0 text-muted-foreground/60">{msg.content}</span>
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  {/if}
</div>
