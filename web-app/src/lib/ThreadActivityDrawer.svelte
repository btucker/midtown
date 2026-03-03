<script>
  /**
   * ThreadActivityDrawer — terminal-styled slide-up drawer above the thread reply input.
   *
   * Props:
   *   channelName    — the channel whose tool items to display (e.g. "web")
   *   threadParentId — when set, reads from threadToolItems[id] instead of agentToolItems[channel]
   *   thinking       — true when the user just sent a message and we're waiting for tool calls
   *
   * Click the drawer to expand it into a scrollable panel showing the full tool call history.
   * Click again or press Escape to collapse back to the compact view.
   */
  import { agentToolItems, threadToolItems } from './store.js'
  import { onDestroy } from 'svelte'
  import { slide } from 'svelte/transition'

  let { channelName, threadParentId = null, thinking = false } = $props()

  const AGE_OUT_MS = 3000
  const MAX_VISIBLE = 10

  let expanded = $state(false)
  let scrollContainer = $state(null)

  // Two-phase age-out:
  //   completedAt  Map<item_id, timestampMs> — when each item finished (never deleted)
  //   expired      Set<item_id>              — items hidden after AGE_OUT_MS has elapsed
  // Keeping them separate ensures completed items remain visible for 3s before hiding,
  // and prevents the re-appear loop that occurs when a single map is both populated and
  // deleted (deletion causes the entry to be re-added on the next interval tick).
  let completedAt = $state(new Map())
  let expired = $state(new Set())

  // Reset state when the thread changes (switching threads clears stale state)
  $effect(() => {
    channelName // track dependency
    threadParentId // track dependency
    completedAt = new Map()
    expired = new Set()
    expanded = false
  })

  // Build merged view: fold ToolResults into their ToolCalls.
  // When threadParentId is set, read from the thread-scoped store; otherwise fall back
  // to the channel-scoped store (for non-fork threads like DM channels).
  let merged = $derived.by(() => {
    const items = threadParentId
      ? ($threadToolItems[threadParentId] || [])
      : ($agentToolItems[channelName ?? 'midtown'] || [])
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

  // Interval: record completion timestamps; move items to `expired` after AGE_OUT_MS.
  // When expanded, skip phase 2 (expiring) so items remain visible.
  const intervalId = setInterval(() => {
    const now = Date.now()
    let changedCompleted = false
    let changedExpired = false
    const newCompleted = new Map(completedAt)
    const newExpired = new Set(expired)

    // Phase 1: stamp newly completed items (always runs)
    for (const entry of merged) {
      if (entry.status !== null && !newCompleted.has(entry.item.item_id)) {
        newCompleted.set(entry.item.item_id, now)
        changedCompleted = true
      }
    }

    // Phase 2: move stale completed items to the expired set (skip when expanded)
    if (!expanded) {
      for (const [id, ts] of newCompleted) {
        if (!newExpired.has(id) && now - ts >= AGE_OUT_MS) {
          newExpired.add(id)
          changedExpired = true
        }
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

  // Items to display: when expanded, show everything; when collapsed, show filtered list
  let displayItems = $derived(expanded ? merged : visibleItems)

  let isVisible = $derived(visibleItems.length > 0 || thinking || (expanded && merged.length > 0))

  // Auto-scroll to bottom when new items arrive in expanded mode
  $effect(() => {
    if (expanded && scrollContainer && displayItems.length) {
      // Tick: wait for DOM update before scrolling
      requestAnimationFrame(() => {
        if (scrollContainer) {
          scrollContainer.scrollTop = scrollContainer.scrollHeight
        }
      })
    }
  })

  function toggleExpanded() {
    expanded = !expanded
  }

  function handleKeydown(e) {
    if (e.key === 'Escape' && expanded) {
      expanded = false
      e.stopPropagation()
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault()
      toggleExpanded()
    }
  }

  function describeItem(item) {
    for (const part of item.content) {
      if (part.ToolCall) {
        return part.ToolCall.semantic_header || part.ToolCall.name?.toLowerCase() || '?'
      }
    }
    return '?'
  }

  // Count of hidden items (only meaningful when collapsed)
  let hiddenCount = $derived(merged.length - visibleItems.length)
</script>

{#if isVisible}
  <!-- svelte-ignore a11y_no_static_element_interactions a11y_no_noninteractive_tabindex -->
  <div
    class="activity-drawer border-t border-[#333] {expanded ? 'expanded' : ''}"
    class:cursor-pointer={merged.length > 0}
    data-testid="thread-activity-drawer"
    onclick={merged.length > 0 ? toggleExpanded : undefined}
    onkeydown={handleKeydown}
    role={merged.length > 0 ? 'button' : undefined}
    tabindex={merged.length > 0 ? 0 : undefined}
    aria-expanded={merged.length > 0 ? expanded : undefined}
    aria-label={merged.length > 0 ? (expanded ? 'Collapse tool call history' : 'Expand tool call history') : undefined}
    transition:slide={{ duration: 180 }}
  >
    {#if merged.length > 0}
      <!-- Header bar with chevron and count -->
      <div class="flex items-center justify-between px-3 py-1 select-none">
        <span class="text-[0.68rem] text-[#666] font-mono">
          {#if expanded}
            {merged.length} tool call{merged.length !== 1 ? 's' : ''}
          {:else if hiddenCount > 0}
            +{hiddenCount} more
          {/if}
        </span>
        <span class="text-[0.68rem] text-[#555] transition-transform duration-150" class:rotate-180={expanded}>
          ▾
        </span>
      </div>
    {/if}

    <div
      bind:this={scrollContainer}
      class="px-3 {expanded ? 'overflow-y-auto' : 'overflow-hidden'}"
      class:pb-1.5={!expanded || displayItems.length === 0}
      style={expanded ? 'max-height: 50vh;' : ''}
    >
      {#if displayItems.length === 0 && thinking}
        <!-- Optimistic waiting state: blinking dots -->
        <div class="flex items-center gap-[0.4em] py-[1px]">
          <span class="text-[#4a6a4a] select-none flex-shrink-0 text-[0.78rem]">›</span>
          <span class="font-mono text-[0.78rem] leading-[1.35] text-[#4a6a4a] thinking-blink">...</span>
        </div>
      {:else}
        {#each displayItems as entry (entry.item.item_id)}
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

  .activity-drawer {
    background: #1a1a1a;
    transition: max-height 0.2s ease;
  }

  .activity-drawer:hover {
    background: #1e1e1e;
  }

  .activity-drawer.expanded {
    background: #181818;
  }

  .activity-drawer.expanded:hover {
    background: #181818;
  }
</style>
