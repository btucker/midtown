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

  // Two-phase age-out:
  //   completedAt  Map<item_id, timestampMs> — when each item finished (never deleted)
  //   expired      Set<item_id>              — items hidden after AGE_OUT_MS has elapsed
  // Keeping them separate ensures completed items remain visible for 3s before hiding,
  // and prevents the re-appear loop that occurs when a single map is both populated and
  // deleted (deletion causes the entry to be re-added on the next interval tick).
  let completedAt = $state(new Map())
  let expired = $state(new Set())

  // Reset both maps when channelName changes (switching threads clears stale state)
  $effect(() => {
    channelName // track dependency
    completedAt = new Map()
    expired = new Set()
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

  // Interval: record completion timestamps; move items to `expired` after AGE_OUT_MS
  const intervalId = setInterval(() => {
    const now = Date.now()
    let changedCompleted = false
    let changedExpired = false
    const newCompleted = new Map(completedAt)
    const newExpired = new Set(expired)

    // Phase 1: stamp newly completed items
    for (const entry of merged) {
      if (entry.status !== null && !newCompleted.has(entry.item.item_id)) {
        newCompleted.set(entry.item.item_id, now)
        changedCompleted = true
      }
    }

    // Phase 2: move stale completed items to the expired set
    for (const [id, ts] of newCompleted) {
      if (!newExpired.has(id) && now - ts >= AGE_OUT_MS) {
        newExpired.add(id)
        changedExpired = true
      }
    }

    if (changedCompleted) completedAt = newCompleted
    if (changedExpired) expired = newExpired
  }, 500)

  onDestroy(() => clearInterval(intervalId))

  // Visible list: exclude expired items, cap at MAX_VISIBLE newest
  let visibleItems = $derived(
    merged
      .filter((entry) => !expired.has(entry.item.item_id))
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
