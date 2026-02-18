<script>
  import { threadData } from './store.js'
  import { sendMessage, closeThread } from './api.js'
  import { renderContent } from './markdown.js'
  import { tick } from 'svelte'

  // Avenue colors (same as Channel.svelte)
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
  }
  const DIM_SENDERS = new Set(['daemon', 'midtown', 'github', 'system'])

  function getSenderColor(name) {
    return AVENUE_COLORS[name?.toLowerCase()] || '#d0d0d0'
  }
  function isDimSender(sender) {
    return DIM_SENDERS.has(sender?.toLowerCase())
  }
  function formatTime(timestamp) {
    try {
      const date = new Date(timestamp)
      return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
    } catch { return '' }
  }

  let replyText = $state('')
  let scrollArea = $state(null)
  let textareaEl = $state(null)

  function handleClose() { closeThread() }
  function handleKeydown(event) {
    if (event.key === 'Escape') handleClose()
  }

  function handleSubmit(e) {
    e.preventDefault()
    if (!replyText.trim() || !$threadData) return
    sendMessage(replyText.trim(), $threadData.channelName, $threadData.parentMessage.id)
    replyText = ''
  }

  function handleKeyDown(e) {
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

<svelte:window onkeydown={handleKeydown} />

{#if $threadData}
  <div class="hidden lg:flex flex-col h-full bg-[#0f0f0f] border-l-2 border-[#2a2a2a] w-[380px] shrink-0">
    <!-- Header -->
    <div class="flex items-center justify-between px-[18px] py-4 bg-[#1a1a1a] border-b-2 border-[#2a2a2a] shrink-0">
      <div class="flex-1 min-w-0">
        <h2 class="text-[0.85rem] font-bold text-[#d0d0d0] m-0">Thread</h2>
        <p class="text-[0.75rem] text-[#808080] m-0 mt-0.5 truncate">
          <span style="color: {getSenderColor($threadData.parentMessage.from)}">{$threadData.parentMessage.from}</span>:
          {$threadData.parentMessage.content?.slice(0, 60)}{$threadData.parentMessage.content?.length > 60 ? '...' : ''}
        </p>
      </div>
      <button
        class="w-8 h-8 flex items-center justify-center bg-transparent border border-[#2a2a2a] rounded-md text-[#808080] text-[1.3rem] cursor-pointer transition-all duration-150 leading-none hover:bg-[#1a1a1a] hover:border-[#af5f5f] hover:text-[#ff5f5f] ml-2 shrink-0"
        onclick={handleClose}
        aria-label="Close thread"
      >&times;</button>
    </div>

    <!-- Messages -->
    <div
      class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden font-[SF_Mono,Menlo,Consolas,Monaco,'Courier_New',monospace] text-[0.82rem] leading-[1.55] px-[14px] pt-[10px] pb-[10px]"
      bind:this={scrollArea}
    >
      <!-- Parent message (highlighted) -->
      <div class="pb-2 mb-2 border-b border-[#2a2a2a]">
        <div class="font-bold text-[0.82rem]" style="color: {getSenderColor($threadData.parentMessage.from)}">
          {$threadData.parentMessage.from}
        </div>
        <div class="text-[#d0d0d0] break-words">{@html renderContent($threadData.parentMessage.content || '')}</div>
        <div class="text-[#4a4a4a] text-[0.75rem] mt-1">{formatTime($threadData.parentMessage.timestamp)}</div>
      </div>

      <!-- Thread replies -->
      {#if $threadData.messages.length === 0}
        <div class="text-center text-[#606060] py-4 text-[0.82rem]">No replies yet</div>
      {:else}
        {#each $threadData.messages as msg, i}
          {#if i === 0 || $threadData.messages[i - 1].from !== msg.from}
            {#if i > 0}<div class="h-[0.5em]"></div>{/if}
            <div class="font-bold text-[0.82rem]" style="color: {getSenderColor(msg.from)}">{msg.from}</div>
          {/if}
          <div class="flex gap-0 break-words">
            <span class="text-[#4a4a4a] flex-shrink-0 w-[3.2em] text-right mr-[0.4em] select-none text-[0.78rem]">{formatTime(msg.timestamp)}</span>
            <span class="flex-1 min-w-0 {isDimSender(msg.from) ? 'text-[#606060]' : 'text-[#d0d0d0]'}">{@html renderContent(msg.content || '')}</span>
          </div>
        {/each}
      {/if}
    </div>

    <!-- Input -->
    <form class="flex gap-2 px-3 pt-2 pb-2 bg-card border-t border-border shrink-0" onsubmit={handleSubmit}>
      <textarea
        bind:this={textareaEl}
        bind:value={replyText}
        placeholder="Reply in thread..."
        rows="1"
        class="flex-1 py-[10px] px-[14px] border-2 border-[#2a2a2a] rounded-[14px] bg-[#0f0f0f] text-[#d0d0d0] text-[0.9rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[6em] overflow-y-auto focus:border-[#5faf5f] placeholder:text-[#606060]"
        onkeydown={handleKeyDown}
        oninput={resizeTextarea}
      ></textarea>
      <button
        type="submit"
        disabled={!replyText.trim()}
        class="py-[10px] px-[16px] border-none rounded-[18px] bg-[#5faf5f] text-[#0a0a0a] font-bold cursor-pointer transition-all duration-200 text-[0.85rem] disabled:opacity-40 disabled:cursor-not-allowed hover:bg-[#6fc57f]"
      >Send</button>
    </form>
  </div>
{/if}
