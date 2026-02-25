<script>
  /**
   * ThreadActivityDrawer — terminal-styled slide-up drawer above the thread reply input.
   *
   * Props:
   *   channelName — the channel whose tool items to display (e.g. "web")
   *   thinking    — true when the user just sent a message and we're waiting for tool calls
   */
  import { agentToolItems } from './store.js'
  import { onDestroy } from 'svelte'
  import { slide } from 'svelte/transition'

  let { channelName, thinking = false } = $props()

  const AGE_OUT_MS = 3000
  const MAX_VISIBLE = 10

  // Map<item_id, completedAtMs> — tracks when each completed item finished
  let agedOut = $state(new Map())

  // Reset age-out map when channelName changes (switching threads)
  $effect(() => {
    channelName // track dependency
    agedOut = new Map()
  })

  // Build merged view: fold ToolResults into their ToolCalls
  let merged = $derived.by(() => {
    const items = $agentToolItems[channelName ?? 'midtown'] || []
    const resultStatus = {}
    for (const item of items) {
      for (const part of item.content) {
        if (part.ToolResult) {
          resultStatus[part.ToolResult.call_id] = part.ToolResult.is_error ? 'error' : 'ok'
        }
      }
    }
    const out = []
    for (const item of items) {
      if (item.content.some((p) => p.ToolCall)) {
        const callId = item.content.find((p) => p.ToolCall)?.ToolCall?.call_id
        out.push({ item, status: callId ? (resultStatus[callId] ?? null) : null })
      }
    }
    return out
  })

  // Interval: mark newly completed items in agedOut; expire items older than AGE_OUT_MS
  const intervalId = setInterval(() => {
    const now = Date.now()
    let changed = false
    const newMap = new Map(agedOut)

    for (const entry of merged) {
      if (entry.status !== null && !newMap.has(entry.item.item_id)) {
        newMap.set(entry.item.item_id, now)
        changed = true
      }
    }

    for (const [id, completedAt] of newMap) {
      if (now - completedAt >= AGE_OUT_MS) {
        newMap.delete(id)
        changed = true
      }
    }

    if (changed) agedOut = newMap
  }, 500)

  onDestroy(() => clearInterval(intervalId))

  // Visible list: exclude aged-out items, cap at MAX_VISIBLE newest
  let visibleItems = $derived(
    merged
      .filter((entry) => !agedOut.has(entry.item.item_id))
      .slice(-MAX_VISIBLE)
  )

  let isVisible = $derived(visibleItems.length > 0 || thinking)

  function describeItem(item) {
    for (const part of item.content) {
      if (part.ToolCall) {
        return part.ToolCall.semantic_header || part.ToolCall.name?.toLowerCase() || '?'
      }
    }
    return '?'
  }
</script>

{#if isVisible}
  <div
    class="bg-[#1a1a1a] border-t border-[#333] px-3 py-1.5 overflow-hidden"
    data-testid="thread-activity-drawer"
    transition:slide={{ duration: 180 }}
  >
    {#if visibleItems.length === 0 && thinking}
      <!-- Optimistic waiting state: blinking dots -->
      <div class="flex items-center gap-[0.4em] py-[1px]">
        <span class="text-[#4a6a4a] select-none flex-shrink-0 text-[0.78rem]">›</span>
        <span class="font-mono text-[0.78rem] leading-[1.35] text-[#4a6a4a] thinking-blink">...</span>
      </div>
    {:else}
      {#each visibleItems as entry (entry.item.item_id)}
        <div class="flex items-center gap-[0.4em] py-[1px]">
          <span class="flex-shrink-0 select-none text-[0.78rem] leading-[1.35]">
            {#if entry.status === 'error'}
              <span class="text-red-400">✗</span>
            {:else if entry.status === 'ok'}
              <span class="text-[#5faf5f]">✓</span>
            {:else}
              <span class="text-[#4a8a4a]">›</span>
            {/if}
          </span>
          <span
            class="font-mono text-[0.78rem] leading-[1.35] whitespace-nowrap overflow-hidden text-ellipsis min-w-0 {entry.status === 'error' ? 'text-red-400' : entry.status === 'ok' ? 'text-[#8fbf8f]' : 'text-[#5faf5f]'}"
          >{describeItem(entry.item)}</span>
        </div>
      {/each}
    {/if}
  </div>
{/if}

<style>
  @keyframes thinking-blink {
    0%, 100% { opacity: 0.35; }
    50% { opacity: 0.9; }
  }

  .thinking-blink {
    animation: thinking-blink 1.2s infinite ease-in-out;
  }
</style>
