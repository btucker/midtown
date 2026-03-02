<script>
  import { getSenderColor, isDimSender, formatTime, formatTimeCompact, senderChanged, timeChanged } from './messageUtils.js'
  import { renderContent } from './markdown.js'
  import { getApiBase } from './api.js'

  const AVATAR_SIZE = '2.4rem'
  const AVATAR_GAP = '0.5rem'

  let {
    msg,
    msgs,
    index,
    senderOverrides = undefined,
    dimSenders = undefined,
    senderSpacing = '1.5em',
    senderClass = '',
    currentTask = undefined,
    channelName = undefined,
    class: extraClass = '',
    children = undefined,
  } = $props()

  const TASK_DIVIDER_RE = /^─── Task !.+───$/

  function isTaskDivider(msg) {
    return msg.from === 'midtown' && TASK_DIVIDER_RE.test((msg.content || '').trim())
  }

  // renderContent() wraps output in block-level <p> tags via marked.parse().
  // Strip the outer <p>...</p> so the label can sit inline within the divider flex row.
  function renderInline(text) {
    return renderContent(text, getApiBase()).replace(/^<p>/, '').replace(/<\/p>\s*$/, '')
  }

  function avatarLetter(name) {
    return (name || '?')[0].toUpperCase()
  }
</script>

{#if isTaskDivider(msg)}
  <!-- Task divider: centered HR with task link -->
  <div class="flex items-center gap-2 py-3 text-muted-foreground/50 text-[0.72rem] select-none">
    <div class="flex-1 h-px bg-border/60"></div>
    <span>{@html renderInline(msg.content.replace(/^───\s*/, '').replace(/\s*───$/, ''))}</span>
    <div class="flex-1 h-px bg-border/60"></div>
  </div>
{:else if senderChanged(msgs, index)}
  <div class="flex items-start gap-[0.5rem] pt-[3px] {senderClass} {extraClass}" style={index > 0 ? `margin-top: ${senderSpacing}` : ''}>
    <!-- Avatar -->
    <div
      class="flex-shrink-0 rounded-md flex items-center justify-center text-white font-bold text-[1rem] select-none mt-[0.15rem]"
      style="width: {AVATAR_SIZE}; height: {AVATAR_SIZE}; background-color: {getSenderColor(msg.from, senderOverrides, channelName)}"
    >{avatarLetter(msg.from)}</div>
    <!-- Header + content -->
    <div class="flex-1 min-w-0">
      <div class="whitespace-nowrap overflow-hidden text-ellipsis flex items-baseline gap-3">
        <span
          class="font-mono font-semibold text-[1rem] text-foreground"
          data-testid="message-sender"
        >{msg.from}</span>
        <span class="text-muted-foreground/70 text-[0.7rem] select-none" data-testid="message-time">
          {formatTime(msg.timestamp)}
        </span>
        {#if currentTask}
          <span class="text-muted-foreground text-[0.7rem]"> — {currentTask}</span>
        {/if}
      </div>
      {#if children}
        {@render children()}
      {:else}
        <div class="break-words {isDimSender(msg.from, dimSenders) ? 'text-muted-foreground' : 'text-foreground'}">
          {@html renderContent(msg.content || '', getApiBase())}
        </div>
      {/if}
    </div>
  </div>
{:else}
  <!-- Continuation: gutter sits in the avatar column, text aligns under username -->
  <div class="flex gap-[0.5rem] mt-[0.5em] {extraClass}">
    <span
      class="text-muted-foreground/70 flex-shrink-0 text-right select-none text-[0.7rem] leading-[1.55rem]"
      style="width: {AVATAR_SIZE}"
    >{timeChanged(msgs, index) ? formatTimeCompact(msg.timestamp) : ''}</span>
    <div class="flex-1 min-w-0">
      {#if children}
        {@render children()}
      {:else}
        <div class="break-words {isDimSender(msg.from, dimSenders) ? 'text-muted-foreground' : 'text-foreground'}">
          {@html renderContent(msg.content || '', getApiBase())}
        </div>
      {/if}
    </div>
  </div>
{/if}
