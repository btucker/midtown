<script>
  import { getSenderColor, isDimSender, formatTime, senderChanged, timeChanged } from './messageUtils.js'
  import { renderContent } from './markdown.js'
  import { getApiBase } from './api.js'

  let {
    msg,
    msgs,
    index,
    senderOverrides = undefined,
    dimSenders = undefined,
    gutterWidth = '3.7em',
    gutterMarginRight = '0.5em',
    senderSpacing = '2.2em',
    senderClass = '',
    currentTask = undefined,
    children = undefined,
  } = $props()
</script>

{#if senderChanged(msgs, index)}
  {#if index > 0}
    <div style="height: {senderSpacing}"></div>
  {/if}
  <div class="whitespace-nowrap overflow-hidden text-ellipsis flex items-center gap-[7px] {senderClass}">
    <span
      class="font-mono font-semibold text-[0.93rem]"
      style="color: {getSenderColor(msg.from, senderOverrides)}"
      data-testid="message-sender"
    >{msg.from}</span>
    <span class="text-muted-foreground/50 text-[0.72rem] select-none" data-testid="message-time">
      {formatTime(msg.timestamp)}
    </span>
    {#if currentTask}
      <span class="text-muted-foreground text-[0.78rem]"> — {currentTask}</span>
    {/if}
  </div>
{/if}

{#if children}
  {@render children()}
{:else}
  <div class="flex gap-0 break-words">
    <span
      class="text-muted-foreground/50 flex-shrink-0 text-right select-none text-[0.78rem]"
      style="width: {gutterWidth}; margin-right: {gutterMarginRight}"
    >{timeChanged(msgs, index) ? formatTime(msg.timestamp) : ''}</span>
    <span class="flex-1 min-w-0 {isDimSender(msg.from, dimSenders) ? 'text-muted-foreground' : 'text-foreground'}">
      {@html renderContent(msg.content || '', getApiBase())}
    </span>
  </div>
{/if}
