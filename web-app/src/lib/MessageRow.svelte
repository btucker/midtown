<script>
  import { getSenderColor, isDimSender, formatTime, formatTimeCompact, senderChanged, timeChanged, getPermalinkUrl } from './messageUtils.js'
  import { renderContent } from './markdown.js'
  import { getApiBase } from './api.js'
  import { activeProject } from './store.js'

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
    threadParentId = undefined,
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

  let permalinkUrl = $derived(
    channelName && msg?.id
      ? getPermalinkUrl($activeProject, channelName, msg.id, threadParentId)
      : ''
  )

  let copiedTooltip = $state(false)
  let tooltipTimeout = null

  function handleTimestampClick(e) {
    if (!permalinkUrl) return
    e.preventDefault()
    e.stopPropagation()
    const fullUrl = window.location.origin + permalinkUrl
    navigator.clipboard.writeText(fullUrl).then(() => {
      copiedTooltip = true
      if (tooltipTimeout) clearTimeout(tooltipTimeout)
      tooltipTimeout = setTimeout(() => { copiedTooltip = false }, 1500)
    })
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
  <div class="flex items-start gap-[0.5rem] pt-[3px] {senderClass} {extraClass}" data-msg-id={msg.id} style={index > 0 ? `margin-top: ${senderSpacing}` : ''}>
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
        {#if permalinkUrl}
          <a
            href={permalinkUrl}
            class="timestamp-link text-muted-foreground/70 text-[0.7rem] select-none no-underline hover:text-muted-foreground relative"
            data-testid="message-time"
            onclick={handleTimestampClick}
          >
            {formatTime(msg.timestamp)}
            {#if copiedTooltip}
              <span class="copied-tooltip">Link copied!</span>
            {/if}
          </a>
        {:else}
          <span class="text-muted-foreground/70 text-[0.7rem] select-none" data-testid="message-time">
            {formatTime(msg.timestamp)}
          </span>
        {/if}
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
  <div class="flex gap-[0.5rem] mt-[0.5em] {extraClass}" data-msg-id={msg.id}>
    {#if timeChanged(msgs, index) && permalinkUrl}
      <a
        href={permalinkUrl}
        class="timestamp-link text-muted-foreground/70 flex-shrink-0 text-right select-none text-[0.7rem] leading-[1.55rem] no-underline hover:text-muted-foreground relative"
        style="width: {AVATAR_SIZE}"
        onclick={handleTimestampClick}
      >
        {formatTimeCompact(msg.timestamp)}
        {#if copiedTooltip}
          <span class="copied-tooltip">Link copied!</span>
        {/if}
      </a>
    {:else}
      <span
        class="text-muted-foreground/70 flex-shrink-0 text-right select-none text-[0.7rem] leading-[1.55rem]"
        style="width: {AVATAR_SIZE}"
      >{timeChanged(msgs, index) ? formatTimeCompact(msg.timestamp) : ''}</span>
    {/if}
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

<style>
  .timestamp-link {
    cursor: pointer;
    text-decoration: none;
  }

  .timestamp-link:hover {
    text-decoration: underline;
  }

  .copied-tooltip {
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    bottom: calc(100% + 4px);
    background: hsl(var(--card));
    color: hsl(var(--foreground));
    border: 1px solid hsl(var(--border));
    font-size: 0.65rem;
    padding: 2px 8px;
    border-radius: 4px;
    white-space: nowrap;
    pointer-events: none;
    animation: tooltip-fade 1.5s ease-out forwards;
    z-index: 50;
    box-shadow: 0 2px 6px rgba(0, 0, 0, 0.15);
  }

  @keyframes tooltip-fade {
    0% { opacity: 1; }
    70% { opacity: 1; }
    100% { opacity: 0; }
  }
</style>
