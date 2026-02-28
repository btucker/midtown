<script>
  import { threadData, agentToolItems } from './store.js'
  import { sendMessage, closeThread, getApiBase } from './api.js'
  import { tick, onMount, onDestroy } from 'svelte'
  import { getSenderColor, isDimSender } from './messageUtils.js'
  import MermaidDiagram from './MermaidDiagram.svelte'
  import { parseSegments, hasMermaid, renderContent } from './markdown.js'
  import MessageRow from './MessageRow.svelte'
  import ThreadActivityDrawer from './ThreadActivityDrawer.svelte'
  import TaskCard from './TaskCard.svelte'
  import { clearMobileTextarea } from './mobileInput.js'

  const THREAD_SENDER_OVERRIDES = {
    midtown: '#585858',
  }
  const THREAD_DIM_SENDERS = ['midtown']

  // Thread panel resize state (desktop only)
  const THREAD_PANEL_WIDTH_KEY = 'thread-panel:width'
  const THREAD_PANEL_DEFAULT_WIDTH = 380
  const THREAD_PANEL_MIN_WIDTH = 280
  const THREAD_PANEL_MAX_WIDTH = 600

  let panelWidth = $state((() => {
    if (typeof localStorage === 'undefined') return THREAD_PANEL_DEFAULT_WIDTH
    const saved = localStorage.getItem(THREAD_PANEL_WIDTH_KEY)
    if (saved) {
      const parsed = parseInt(saved, 10)
      if (!isNaN(parsed) && parsed >= THREAD_PANEL_MIN_WIDTH && parsed <= THREAD_PANEL_MAX_WIDTH) {
        return parsed
      }
    }
    return THREAD_PANEL_DEFAULT_WIDTH
  })())

  let isResizing = $state(false)
  let resizeStartX = 0
  let resizeStartWidth = 0

  function handleResizeMouseDown(e) {
    e.preventDefault()
    isResizing = true
    resizeStartX = e.clientX
    resizeStartWidth = panelWidth
    document.addEventListener('mousemove', handleResizeMouseMove)
    document.addEventListener('mouseup', handleResizeMouseUp)
    document.body.style.cursor = 'ew-resize'
    document.body.style.userSelect = 'none'
  }

  function handleResizeMouseMove(e) {
    // Panel is on the right: drag left (negative delta) = wider
    const delta = resizeStartX - e.clientX
    const newWidth = Math.max(THREAD_PANEL_MIN_WIDTH, Math.min(THREAD_PANEL_MAX_WIDTH, resizeStartWidth + delta))
    panelWidth = newWidth
  }

  function handleResizeMouseUp() {
    isResizing = false
    document.removeEventListener('mousemove', handleResizeMouseMove)
    document.removeEventListener('mouseup', handleResizeMouseUp)
    document.body.style.cursor = ''
    document.body.style.userSelect = ''
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(THREAD_PANEL_WIDTH_KEY, String(panelWidth))
    }
  }

  // Cleanup resize listeners if component is destroyed mid-drag
  $effect(() => {
    return () => {
      document.removeEventListener('mousemove', handleResizeMouseMove)
      document.removeEventListener('mouseup', handleResizeMouseUp)
      document.body.style.cursor = ''
      document.body.style.userSelect = ''
    }
  })

  function isAction(msg) {
    return msg.msg_type === 'action' || msg.content?.startsWith('/me ')
  }

  function isInsight(msg) {
    return msg.msg_type === 'insight' || msg.type === 'insight'
  }

  function getActionContent(msg) {
    return msg.content.replace(/^\/me\s*/, '')
  }

  let replyText = $state('')
  let desktopScrollArea = $state(null)
  let mobileScrollArea = $state(null)
  let desktopTextareaEl = $state(null)
  let mobileTextareaEl = $state(null)
  let isDesktop = $state(typeof window !== 'undefined' && window.matchMedia('(min-width: 1024px)').matches)

  // Optimistic thinking state: true from the moment the user sends a reply until
  // real InProgress tool items arrive (or 30s timeout).
  let thinking = $state(false)
  let thinkingTimeout = null

  // Stable thread identity: changes only when a different thread is opened or closed.
  // Using $derived ensures the clearing effect below re-runs only on actual thread switches,
  // not on every message-array update (which reassigns $threadData but keeps the same id).
  let currentThreadId = $derived($threadData?.parentMessage?.id ?? null)

  // Clear thinking when the thread is closed or switched to a different thread.
  $effect(() => {
    currentThreadId // track dependency — re-runs only when thread identity changes
    thinking = false
    if (thinkingTimeout) {
      clearTimeout(thinkingTimeout)
      thinkingTimeout = null
    }
  })

  // Clear thinking when real InProgress tool items arrive for this thread's channel.
  $effect(() => {
    const channelName = $threadData?.channelName ?? 'midtown'
    const items = $agentToolItems[channelName] || []
    if (items.some((item) => item.status === 'InProgress')) {
      thinking = false
      if (thinkingTimeout) {
        clearTimeout(thinkingTimeout)
        thinkingTimeout = null
      }
    }
  })

  onDestroy(() => {
    if (thinkingTimeout) {
      clearTimeout(thinkingTimeout)
      thinkingTimeout = null
    }
  })

  // Track viewport changes to know which panel is active
  onMount(() => {
    const mql = window.matchMedia('(min-width: 1024px)')
    function onChange(e) { isDesktop = e.matches }
    mql.addEventListener('change', onChange)
    return () => mql.removeEventListener('change', onChange)
  })

  // Derive the active elements based on viewport
  let scrollArea = $derived(isDesktop ? desktopScrollArea : mobileScrollArea)
  let textareaEl = $derived(isDesktop ? desktopTextareaEl : mobileTextareaEl)

  function handleClose() { closeThread() }
  function handleWindowKeydown(event) {
    if (event.key === 'Escape' && !event.defaultPrevented) handleClose()
  }

  function handleSubmit(e) {
    e.preventDefault()
    if (!replyText.trim() || !$threadData) return
    // If no parentMessage (task without message_id), send as top-level channel message
    const parentId = $threadData.parentMessage?.id ?? null
    sendMessage(replyText.trim(), $threadData.channelName, parentId)
    replyText = ''
    clearMobileTextarea(textareaEl, () => { replyText = '' })
    // Optimistic: show the drawer immediately while waiting for tool calls to arrive
    thinking = true
    if (thinkingTimeout) clearTimeout(thinkingTimeout)
    thinkingTimeout = setTimeout(() => {
      thinking = false
      thinkingTimeout = null
    }, 30000)
  }

  function handleTextareaKeyDown(e) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSubmit(e)
    }
  }

  // Auto-scroll when new messages arrive
  $effect(() => {
    if ($threadData?.messages?.length > 0 && scrollArea) {
      tick().then(() => {
        scrollArea.scrollTop = scrollArea.scrollHeight
      })
    }
  })

  // Focus textarea when thread opens
  $effect(() => {
    if ($threadData && textareaEl) {
      tick().then(() => textareaEl.focus())
    }
  })

  function resizeTextarea() {
    if (!textareaEl) return
    textareaEl.style.height = 'auto'
    textareaEl.style.height = textareaEl.scrollHeight + 'px'
  }

  $effect(() => {
    replyText;
    tick().then(() => resizeTextarea())
  })
</script>

<svelte:window onkeydown={handleWindowKeydown} />

{#if $threadData}
  <!-- Desktop: side panel with resize handle -->
  <div
    class="hidden lg:flex flex-col h-full bg-background border-l-2 border-border shrink-0 relative"
    style="width: {panelWidth}px"
    data-testid="thread-panel"
  >
    <!-- Resize handle on left edge -->
    <div
      role="separator"
      aria-label="Resize thread panel"
      onmousedown={handleResizeMouseDown}
      class="absolute inset-y-0 -left-1 w-2 cursor-ew-resize hover:bg-primary/20 transition-colors z-30 hidden md:block"
      class:bg-primary={isResizing}
    >
      <div class="absolute inset-y-0 left-0 w-full flex items-center justify-center transition-opacity {isResizing ? 'opacity-100' : 'opacity-0 hover:opacity-100'}">
        <div class="w-0.5 h-12 bg-primary/60 rounded-full"></div>
      </div>
    </div>

    <!-- Header -->
    <div class="flex items-center justify-between px-[18px] py-4 bg-card border-b-2 border-border shrink-0">
      <h2 class="text-[0.85rem] font-bold text-foreground m-0">Thread</h2>
      <button
        class="w-8 h-8 flex items-center justify-center bg-transparent border border-border rounded-md text-muted-foreground text-[1.3rem] cursor-pointer transition-all duration-150 leading-none hover:bg-accent hover:border-destructive hover:text-destructive ml-2 shrink-0"
        onclick={handleClose}
        aria-label="Close thread"
        data-testid="thread-close-button"
      >&times;</button>
    </div>

    <!-- Scrollable content: task cards + parent message + replies -->
    <div
      class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden text-[1rem] leading-[1.55] px-[18px] pt-[10px] pb-[10px]"
      bind:this={desktopScrollArea}
    >
      <!-- Task cards at top (above parent message) -->
      {#if $threadData.tasks?.length > 0}
        {#each $threadData.tasks as task}
          <TaskCard {task} />
        {/each}
      {/if}

      <!-- Parent message as first item in stream -->
      {#if $threadData.parentMessage}
        <MessageRow
          msg={$threadData.parentMessage}
          msgs={[$threadData.parentMessage]}
          index={0}
          senderOverrides={THREAD_SENDER_OVERRIDES}
          dimSenders={THREAD_DIM_SENDERS}
          senderClass="mt-1"
          channelName={$threadData?.channelName}
        />
      {/if}

      <!-- Separator with reply count -->
      <div class="flex items-center gap-2 py-3 text-muted-foreground/50 text-[0.72rem]">
        <div class="flex-1 h-px bg-border/60"></div>
        <span>{$threadData.messages.length === 0 ? 'no replies yet' : $threadData.messages.length === 1 ? '1 reply' : `${$threadData.messages.length} replies`}</span>
        <div class="flex-1 h-px bg-border/60"></div>
      </div>

      <!-- Thread replies -->
      {#each $threadData.messages as msg, i}
          <MessageRow
            {msg}
            msgs={$threadData.messages}
            index={i}
            senderOverrides={THREAD_SENDER_OVERRIDES}
            dimSenders={THREAD_DIM_SENDERS}
            senderClass="mt-1"
            channelName={$threadData?.channelName}
            class={msg.pending ? 'opacity-60' : ''}
          >
            {#if isAction(msg) && !hasMermaid(msg.content)}
              <div class="flex gap-0 break-words">
                <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">*</span>
                <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">{@html renderContent(getActionContent(msg), getApiBase())}</span>
              </div>
            {:else if isAction(msg) && hasMermaid(msg.content)}
              {#each parseSegments(getActionContent(msg)) as segment, si}
                {#if segment.type === 'mermaid'}
                  <MermaidDiagram code={segment.content} />
                {:else}
                  <div class="flex gap-0 break-words">
                    {#if si === 0}
                      <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">*</span>
                    {:else}
                      <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                    {/if}
                    <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">{@html renderContent(segment.content, getApiBase())}</span>
                  </div>
                {/if}
              {/each}
            {:else if isInsight(msg)}
              <div class="rounded-md border border-insight/40 bg-insight/8 px-3 py-2 my-1 ml-[0.5em]">
                <div class="flex items-center gap-1.5 mb-1.5 text-insight text-[0.72rem] font-bold uppercase tracking-wide" aria-label="Insight">
                  <span aria-hidden="true">★</span>
                  <span>Insight</span>
                </div>
                {#if hasMermaid(msg.content || '')}
                  <div class="flex flex-col gap-2">
                    {#each parseSegments(msg.content || '') as segment}
                      {#if segment.type === 'mermaid'}
                        <MermaidDiagram code={segment.content} />
                      {:else}
                        <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
                      {/if}
                    {/each}
                  </div>
                {:else}
                  <div class="message-text text-foreground">{@html renderContent(msg.content || '', getApiBase())}</div>
                {/if}
              </div>
            {:else if hasMermaid(msg.content)}
              {#each parseSegments(msg.content) as segment}
                {#if segment.type === 'mermaid'}
                  <MermaidDiagram code={segment.content} />
                {:else}
                  <div class="break-words message-text {isDimSender(msg.from, THREAD_DIM_SENDERS) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
                {/if}
              {/each}
            {:else}
              <div class="break-words message-text {isDimSender(msg.from, THREAD_DIM_SENDERS) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(msg.content, getApiBase())}</div>
            {/if}
          </MessageRow>
        {/each}
    </div>

    <!-- Activity drawer: slides up from the input when lead is working -->
    <ThreadActivityDrawer channelName={$threadData?.channelName} {thinking} />

    <!-- Input -->
    <form class="flex gap-2 px-3 pt-2 pb-2 bg-card border-t border-border shrink-0" onsubmit={handleSubmit}>
      <textarea
        data-testid="thread-input"
        bind:this={desktopTextareaEl}
        bind:value={replyText}
        placeholder="Reply in thread..."
        rows="1"
        class="flex-1 py-[10px] px-[14px] border-2 border-input rounded-[14px] bg-background text-foreground text-[0.9rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[6em] overflow-y-auto focus:border-primary placeholder:text-muted-foreground"
        onkeydown={handleTextareaKeyDown}
        oninput={resizeTextarea}
      ></textarea>
      <button
        type="submit"
        disabled={!replyText.trim()}
        data-testid="thread-send-button"
        class="py-[10px] px-[16px] border-none rounded-[18px] bg-primary text-primary-foreground font-bold cursor-pointer transition-all duration-200 text-[0.85rem] disabled:opacity-40 disabled:cursor-not-allowed hover:bg-primary/90"
      >Send</button>
    </form>
  </div>

  <!-- Mobile: slide-in pane (inside board content area) -->
  <div class="lg:hidden absolute inset-0 z-20 bg-background flex flex-col thread-mobile-pane" data-testid="thread-panel-mobile">
    <!-- Mobile header with back button -->
    <!-- No top safe-area padding here: this pane is inside board content, below
         App.svelte's mobile header which already applies pt-safe-offset-* -->
    <div class="flex items-center gap-2 px-3 py-3 bg-card border-b-2 border-border shrink-0">
      <button
        class="w-8 h-8 flex items-center justify-center bg-transparent border border-border rounded-md text-muted-foreground text-[1.1rem] cursor-pointer transition-all duration-150 leading-none hover:text-foreground shrink-0"
        onclick={handleClose}
        aria-label="Back to channel"
        data-testid="thread-back-button"
      >&larr;</button>
      <h2 class="text-[0.85rem] font-bold text-foreground m-0">Thread</h2>
    </div>

    <!-- Mobile messages -->
    <div
      class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden text-[1rem] leading-[1.55] px-[18px] pt-[10px] pb-[10px]"
      bind:this={mobileScrollArea}
    >
      <!-- Task cards at top -->
      {#if $threadData.tasks?.length > 0}
        {#each $threadData.tasks as task}
          <TaskCard {task} />
        {/each}
      {/if}

      <!-- Parent message as first item in stream -->
      {#if $threadData.parentMessage}
        <MessageRow
          msg={$threadData.parentMessage}
          msgs={[$threadData.parentMessage]}
          index={0}
          senderOverrides={THREAD_SENDER_OVERRIDES}
          dimSenders={THREAD_DIM_SENDERS}
          senderClass="mt-1"
          channelName={$threadData?.channelName}
        />
      {/if}

      <!-- Separator with reply count -->
      <div class="flex items-center gap-2 py-3 text-muted-foreground/50 text-[0.72rem]">
        <div class="flex-1 h-px bg-border/60"></div>
        <span>{$threadData.messages.length === 0 ? 'no replies yet' : $threadData.messages.length === 1 ? '1 reply' : `${$threadData.messages.length} replies`}</span>
        <div class="flex-1 h-px bg-border/60"></div>
      </div>

      <!-- Replies -->
      {#each $threadData.messages as msg, i}
          <MessageRow
            {msg}
            msgs={$threadData.messages}
            index={i}
            senderOverrides={THREAD_SENDER_OVERRIDES}
            dimSenders={THREAD_DIM_SENDERS}
            senderClass="mt-1"
            channelName={$threadData?.channelName}
            class={msg.pending ? 'opacity-60' : ''}
          >
            {#if isAction(msg) && !hasMermaid(msg.content)}
              <div class="flex gap-0 break-words">
                <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">*</span>
                <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">{@html renderContent(getActionContent(msg), getApiBase())}</span>
              </div>
            {:else if isAction(msg) && hasMermaid(msg.content)}
              {#each parseSegments(getActionContent(msg)) as segment, si}
                {#if segment.type === 'mermaid'}
                  <MermaidDiagram code={segment.content} />
                {:else}
                  <div class="flex gap-0 break-words">
                    {#if si === 0}
                      <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">*</span>
                    {:else}
                      <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                    {/if}
                    <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, THREAD_SENDER_OVERRIDES, $threadData?.channelName)}">{@html renderContent(segment.content, getApiBase())}</span>
                  </div>
                {/if}
              {/each}
            {:else if isInsight(msg)}
              <div class="rounded-md border border-insight/40 bg-insight/8 px-3 py-2 my-1 ml-[0.5em]">
                <div class="flex items-center gap-1.5 mb-1.5 text-insight text-[0.72rem] font-bold uppercase tracking-wide" aria-label="Insight">
                  <span aria-hidden="true">★</span>
                  <span>Insight</span>
                </div>
                {#if hasMermaid(msg.content || '')}
                  <div class="flex flex-col gap-2">
                    {#each parseSegments(msg.content || '') as segment}
                      {#if segment.type === 'mermaid'}
                        <MermaidDiagram code={segment.content} />
                      {:else}
                        <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
                      {/if}
                    {/each}
                  </div>
                {:else}
                  <div class="message-text text-foreground">{@html renderContent(msg.content || '', getApiBase())}</div>
                {/if}
              </div>
            {:else if hasMermaid(msg.content)}
              {#each parseSegments(msg.content) as segment}
                {#if segment.type === 'mermaid'}
                  <MermaidDiagram code={segment.content} />
                {:else}
                  <div class="break-words message-text {isDimSender(msg.from, THREAD_DIM_SENDERS) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
                {/if}
              {/each}
            {:else}
              <div class="break-words message-text {isDimSender(msg.from, THREAD_DIM_SENDERS) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(msg.content, getApiBase())}</div>
            {/if}
          </MessageRow>
        {/each}
    </div>

    <!-- Activity drawer: slides up from the input when lead is working -->
    <ThreadActivityDrawer channelName={$threadData?.channelName} {thinking} />

    <!-- Mobile input -->
    <form class="flex gap-2 px-3 pt-2 pb-safe-offset-2 bg-card border-t border-border shrink-0" onsubmit={handleSubmit}>
      <textarea
        data-testid="thread-input"
        bind:this={mobileTextareaEl}
        bind:value={replyText}
        placeholder="Reply in thread..."
        rows="1"
        class="flex-1 py-[10px] px-[14px] border-2 border-input rounded-[14px] bg-background text-foreground text-[0.9rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[6em] overflow-y-auto focus:border-primary placeholder:text-muted-foreground"
        onkeydown={handleTextareaKeyDown}
        oninput={resizeTextarea}
      ></textarea>
      <button
        type="submit"
        disabled={!replyText.trim()}
        data-testid="thread-send-button"
        class="py-[10px] px-[16px] border-none rounded-[18px] bg-primary text-primary-foreground font-bold cursor-pointer transition-all duration-200 text-[0.85rem] disabled:opacity-40 disabled:cursor-not-allowed hover:bg-primary/90"
      >Send</button>
    </form>
  </div>
{/if}

<style>
  .thread-mobile-pane {
    animation: thread-slide-in 0.24s ease-out;
  }

  @keyframes thread-slide-in {
    from {
      transform: translateX(100%);
    }
    to {
      transform: translateX(0);
    }
  }
</style>
