<script>
  /**
   * ThreadActivityDrawer — slide-up drawer above the thread reply input showing tool call activity.
   *
   * Styled to match the channel activity strip (Channel.svelte) — uses theme colors,
   * muted-foreground text, and bouncing dots for the thinking state.
   *
   * Props:
   *   channelName    — the channel whose tool items to display (e.g. "web")
   *   threadParentId — when set, reads from threadToolItems[id] instead of agentToolItems[channel]
   *   thinking       — true when the user just sent a message and we're waiting for tool calls
   *
   * Click the drawer to expand it into a scrollable panel showing the full tool call history.
   * Click again or press Escape to collapse back to the compact view.
   */
  import { agentToolItems, threadToolItems, threadForkOwners } from './store.js'
  import { getForkOwnerColor } from './avenue-colors.js'
  import { onDestroy } from 'svelte'
  import { slide } from 'svelte/transition'

  let { channelName, threadParentId = null, thinking = false } = $props()

  // Use the fork owner's avenue color for thinking dots instead of hardcoded lead gold.
  // getForkOwnerColor extracts the avenue prefix from compound fork session names
  // (e.g., "park-discuss-ab12" → "park" → park's cyan) and falls back to lead gold
  // for non-avenue prefixes (channel leads, anonymous forks, unknown owners).
  let dotColor = $derived(getForkOwnerColor($threadForkOwners[threadParentId]))

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

  // Drawer stays visible whenever there's history to show (even if all items have aged out
  // in collapsed mode), so users can always click to expand and see past tool calls.
  let isVisible = $derived(merged.length > 0 || thinking)

  // Auto-scroll to bottom when new items arrive in expanded mode,
  // but only if the user hasn't manually scrolled up to read history.
  const SCROLL_THRESHOLD = 50 // px from bottom to consider "at bottom"
  $effect(() => {
    if (expanded && scrollContainer && displayItems.length) {
      // Check if user is near the bottom before the DOM update
      const { scrollTop, scrollHeight, clientHeight } = scrollContainer
      const isNearBottom = scrollHeight - scrollTop - clientHeight < SCROLL_THRESHOLD
      if (isNearBottom) {
        requestAnimationFrame(() => {
          if (scrollContainer) {
            scrollContainer.scrollTop = scrollContainer.scrollHeight
          }
        })
      }
    }
  })

  function toggleExpanded() {
    if (expanded) {
      // Collapsing: reset completion timestamps to now so items that completed
      // during expansion get a fresh AGE_OUT_MS grace period instead of expiring
      // immediately (since the original timestamps may be older than AGE_OUT_MS).
      const now = Date.now()
      const refreshed = new Map(completedAt)
      let changed = false
      for (const [id, ts] of refreshed) {
        if (now - ts >= AGE_OUT_MS && !expired.has(id)) {
          refreshed.set(id, now)
          changed = true
        }
      }
      if (changed) completedAt = refreshed
    }
    expanded = !expanded
  }

  function handleKeydown(e) {
    if (e.key === 'Escape' && expanded) {
      // Collapse the drawer; do NOT stopPropagation so a second Escape
      // naturally bubbles to parent handlers (e.g. close thread panel).
      toggleExpanded()
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
    class="activity-drawer border-t border-border {expanded ? 'expanded' : ''}"
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
        <span class="text-[0.72rem] text-muted-foreground/50 font-mono">
          {#if expanded}
            {merged.length} tool call{merged.length !== 1 ? 's' : ''}
          {:else if hiddenCount > 0}
            +{hiddenCount} more
          {/if}
        </span>
        <span class="text-[0.72rem] text-muted-foreground/40 transition-transform duration-150" class:rotate-180={expanded}>
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
      {#each displayItems as entry (entry.item.item_id)}
        {@const dimmed = expanded && completedAt.has(entry.item.item_id)}
        <div class="flex items-center gap-[6px] py-[1px]" class:opacity-45={dimmed}>
          <span class="flex-shrink-0 select-none text-[0.82rem] leading-[1.35] text-muted-foreground/60">
            {#if entry.status === 'error'}
              <span class="text-destructive">✗</span>
            {:else if entry.status === 'ok'}
              ✓
            {:else}
              ›
            {/if}
          </span>
          <span
            class="font-mono text-[0.82rem] leading-[1.35] whitespace-nowrap overflow-hidden text-ellipsis min-w-0 {entry.status === 'error' ? 'text-destructive' : 'text-muted-foreground'}"
          >{describeItem(entry.item)}</span>
        </div>
      {/each}
      {#if thinking}
        <!-- Thinking indicator: bouncing dots matching the channel activity strip -->
        <div class="flex items-center gap-[6px] py-[1px]">
          <span class="typing-dots flex gap-[3px] items-center">
            <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {dotColor}"></span>
            <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {dotColor}"></span>
            <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {dotColor}"></span>
          </span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .activity-drawer {
    background: hsl(var(--card));
    transition: max-height 0.2s ease;
  }

  .activity-drawer:hover {
    background: hsl(var(--accent));
  }

  .activity-drawer.expanded {
    background: hsl(var(--card));
  }

  .activity-drawer.expanded:hover {
    background: hsl(var(--card));
  }
</style>
