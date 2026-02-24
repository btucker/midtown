<script>
  import { threadData } from './store.js'
  import { sendMessage, closeThread } from './api.js'
  import { renderContent } from './markdown.js'
  import { tick, onMount } from 'svelte'
  import { getSenderColor, isDimSender, formatTime, senderChanged, timeChanged } from './messageUtils.js'

  let replyText = $state('')
  let desktopScrollArea = $state(null)
  let mobileScrollArea = $state(null)
  let desktopTextareaEl = $state(null)
  let mobileTextareaEl = $state(null)
  let isDesktop = $state(typeof window !== 'undefined' && window.matchMedia('(min-width: 1024px)').matches)

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
    sendMessage(replyText.trim(), $threadData.channelName, $threadData.parentMessage.id)
    replyText = ''
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
  <!-- Desktop: side panel -->
  <div class="hidden lg:flex flex-col h-full bg-background border-l-2 border-border w-[380px] shrink-0">
    <!-- Header -->
    <div class="flex items-center justify-between px-[18px] py-4 bg-card border-b-2 border-border shrink-0">
      <div class="flex-1 min-w-0">
        <h2 class="text-[0.85rem] font-bold text-foreground m-0">Thread</h2>
        <p class="text-[0.75rem] text-muted-foreground m-0 mt-0.5 break-words">
          <span style="color: {getSenderColor($threadData.parentMessage.from)}">{$threadData.parentMessage.from}</span>:
          {$threadData.parentMessage.content || ''}
        </p>
      </div>
      <button
        class="w-8 h-8 flex items-center justify-center bg-transparent border border-border rounded-md text-muted-foreground text-[1.3rem] cursor-pointer transition-all duration-150 leading-none hover:bg-accent hover:border-destructive hover:text-destructive ml-2 shrink-0 self-start mt-1"
        onclick={handleClose}
        aria-label="Close thread"
      >&times;</button>
    </div>

    <!-- Messages -->
    <div
      class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden font-[SF_Mono,Menlo,Consolas,Monaco,'Courier_New',monospace] text-[1rem] leading-[1.55] px-[14px] pt-[10px] pb-[10px]"
      bind:this={desktopScrollArea}
    >
      <!-- Parent message (highlighted) -->
      <div class="pb-2 mb-2 border-b border-border">
        <div class="font-bold text-[0.82rem]" style="color: {getSenderColor($threadData.parentMessage.from)}">
          {$threadData.parentMessage.from}
        </div>
        <div class="text-foreground break-words">{@html renderContent($threadData.parentMessage.content || '')}</div>
        <div class="text-muted-foreground text-[0.75rem] mt-1">{formatTime($threadData.parentMessage.timestamp)}</div>
      </div>

      <!-- Thread replies -->
      {#if $threadData.messages.length === 0}
        <div class="text-center text-muted-foreground py-4 text-[1rem]">No replies yet</div>
      {:else}
        {#each $threadData.messages as msg, i}
          {#if i === 0 || $threadData.messages[i - 1].from !== msg.from}
            {#if i > 0}<div class="h-[0.5em]"></div>{/if}
            <div class="font-bold text-[0.82rem]" style="color: {getSenderColor(msg.from)}">{msg.from}</div>
          {/if}
          <div class="flex gap-0 break-words">
            <span class="text-muted-foreground flex-shrink-0 w-[3.2em] text-right mr-[0.4em] select-none text-[0.78rem]">{formatTime(msg.timestamp)}</span>
            <span class="flex-1 min-w-0 {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(msg.content || '')}</span>
          </div>
        {/each}
      {/if}
    </div>

    <!-- Input -->
    <form class="flex gap-2 px-3 pt-2 pb-2 bg-card border-t border-border shrink-0" onsubmit={handleSubmit}>
      <textarea
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
        class="py-[10px] px-[16px] border-none rounded-[18px] bg-primary text-primary-foreground font-bold cursor-pointer transition-all duration-200 text-[0.85rem] disabled:opacity-40 disabled:cursor-not-allowed hover:bg-primary/90"
      >Send</button>
    </form>
  </div>

  <!-- Mobile: slide-in pane (inside board content area) -->
  <div class="lg:hidden absolute inset-0 z-20 bg-background flex flex-col thread-mobile-pane">
    <!-- Mobile header with back button -->
    <!-- No top safe-area padding here: this pane is inside board content, below
         App.svelte's mobile header which already applies pt-safe-offset-* -->
    <div class="flex items-center gap-2 px-3 py-3 bg-card border-b-2 border-border shrink-0">
      <button
        class="w-8 h-8 flex items-center justify-center bg-transparent border border-border rounded-md text-muted-foreground text-[1.1rem] cursor-pointer transition-all duration-150 leading-none hover:text-foreground shrink-0"
        onclick={handleClose}
        aria-label="Back to channel"
      >&larr;</button>
      <div class="flex-1 min-w-0">
        <h2 class="text-[0.85rem] font-bold text-foreground m-0">Thread</h2>
        <p class="text-[0.75rem] text-muted-foreground m-0 mt-0.5 break-words">
          <span style="color: {getSenderColor($threadData.parentMessage.from)}">{$threadData.parentMessage.from}</span>:
          {$threadData.parentMessage.content || ''}
        </p>
      </div>
    </div>

    <!-- Mobile messages -->
    <div
      class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden font-[SF_Mono,Menlo,Consolas,Monaco,'Courier_New',monospace] text-[1rem] leading-[1.55] px-[14px] pt-[10px] pb-[10px]"
      bind:this={mobileScrollArea}
    >
      <!-- Parent message -->
      <div class="pb-2 mb-2 border-b border-border">
        <div class="font-bold text-[0.82rem]" style="color: {getSenderColor($threadData.parentMessage.from)}">
          {$threadData.parentMessage.from}
        </div>
        <div class="text-foreground break-words">{@html renderContent($threadData.parentMessage.content || '')}</div>
        <div class="text-muted-foreground text-[0.75rem] mt-1">{formatTime($threadData.parentMessage.timestamp)}</div>
      </div>

      <!-- Replies -->
      {#if $threadData.messages.length === 0}
        <div class="text-center text-muted-foreground py-4 text-[1rem]">No replies yet</div>
      {:else}
        {#each $threadData.messages as msg, i}
          {#if i === 0 || $threadData.messages[i - 1].from !== msg.from}
            {#if i > 0}<div class="h-[0.5em]"></div>{/if}
            <div class="font-bold text-[0.82rem]" style="color: {getSenderColor(msg.from)}">{msg.from}</div>
          {/if}
          <div class="flex gap-0 break-words">
            <span class="text-muted-foreground flex-shrink-0 w-[3.2em] text-right mr-[0.4em] select-none text-[0.78rem]">{formatTime(msg.timestamp)}</span>
            <span class="flex-1 min-w-0 {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(msg.content || '')}</span>
          </div>
        {/each}
      {/if}
    </div>

    <!-- Mobile input -->
    <form class="flex gap-2 px-3 pt-2 pb-safe-offset-2 bg-card border-t border-border shrink-0" onsubmit={handleSubmit}>
      <textarea
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
