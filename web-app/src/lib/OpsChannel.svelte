<script>
  import { messagesByChannel } from './store.js'
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

  // Sender color palette — matches Channel.svelte
  const AVENUE_COLORS = {
    lexington: '#5fafaf',
    park: '#5faf5f',
    madison: '#ff5f5f',
    broadway: '#af5faf',
    amsterdam: '#5f87af',
    columbus: '#af5f5f',
    riverside: '#87d7d7',
    york: '#87d787',
    pleasant: '#d7afd7',
    vernon: '#87afd7',
    bleecker: '#d7875f',
    houston: '#ff87d7',
    canal: '#87d7ff',
    spring: '#afff87',
    prince: '#d7afff',
    mercer: '#ffaf87',
    lead: '#d7d787',
    github: '#585858',
    system: '#585858',
    midtown: '#585858',
    daemon: '#585858',
  }

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
              <span class="flex-shrink-0" style="color: {getSenderColor(msg.from)}">*</span>
              <span class="flex-shrink-0 font-medium" style="color: {getSenderColor(msg.from)}">{getSenderLabel(msg)}</span>
              <span class="flex-1 min-w-0 text-muted-foreground/80">{getContent(msg)}</span>
            {:else}
              <!-- System: "source message" -->
              <span class="flex-shrink-0 font-medium" style="color: {getSenderColor(msg.from)}">{getSenderLabel(msg)}</span>
              <span class="flex-1 min-w-0 text-muted-foreground/60">{msg.content}</span>
            {/if}
          </div>
        {/each}
      {/if}
    </div>
  {/if}
</div>
