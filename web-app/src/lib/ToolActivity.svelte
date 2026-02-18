<script>
  /**
   * ToolActivity — shows the most recent tool call/result items for one agent.
   *
   * Props:
   *   agentName  — coworker name (e.g. "amsterdam")
   *   items      — UniversalItem[] for this agent (newest last)
   *   maxVisible — how many items to show when collapsed (default: 3)
   */
  let { agentName, items = [], maxVisible = 3 } = $props()

  let expanded = $state(false)

  /**
   * Build a merged view: ToolResult items are folded into their matching ToolCall.
   * Each entry is { item, status } where status is null (in-progress), 'ok', or 'error'.
   */
  let merged = $derived.by(() => {
    // Collect result statuses keyed by call_id
    const resultStatus = {}
    for (const item of items) {
      for (const part of item.content) {
        if (part.ToolResult) {
          resultStatus[part.ToolResult.call_id] = part.ToolResult.is_error ? 'error' : 'ok'
        }
      }
    }

    // Build display list: only ToolCall items, annotated with completion status
    const out = []
    for (const item of items) {
      if (item.content.some((p) => p.ToolCall)) {
        const callId = item.content.find((p) => p.ToolCall)?.ToolCall?.call_id
        out.push({ item, status: callId ? (resultStatus[callId] ?? null) : null })
      }
    }
    return out
  })

  // Most recent items first for display
  let sorted = $derived([...merged].reverse())
  let visible = $derived(expanded ? sorted : sorted.slice(0, maxVisible))
  let hasMore = $derived(sorted.length > maxVisible)

  function describeItem(item) {
    for (const part of item.content) {
      if (part.ToolCall) {
        return part.ToolCall.semantic_header || part.ToolCall.name?.toLowerCase() || '?'
      }
    }
    return '?'
  }

  function isError(entry) {
    return entry.status === 'error'
  }

  function isCompleted(entry) {
    return entry.status === 'ok'
  }

  function isInProgress(entry) {
    return entry.status === null
  }
</script>

{#if merged.length > 0}
  <div class="tool-activity mt-[2px] ml-[4.2em] text-[0.80rem] leading-[1.4]">
    {#each visible as entry (entry.item.item_id)}
      <div
        class="tool-item flex items-center gap-[0.4em] py-[1px]"
        class:text-red-400={isError(entry)}
        class:text-[#4a4a4a]={isInProgress(entry)}
      >
        <span class="tool-icon flex-shrink-0 select-none">
          {#if isError(entry)}
            <span class="text-[#af3a3a]">✗</span>
          {:else if isCompleted(entry)}
            <span class="text-[#3a5a3a]">✓</span>
          {:else}
            <span class="text-[#3a6a3a]">›</span>
          {/if}
        </span>
        <span class="font-mono truncate">{describeItem(entry.item)}</span>
      </div>
    {/each}
    {#if hasMore}
      <button
        class="text-[#3a5a5a] hover:text-[#5fafaf] text-[0.78rem] mt-[1px] bg-transparent border-none cursor-pointer p-0"
        onclick={() => { expanded = !expanded }}
      >
        {expanded ? '▲ show less' : `▼ ${sorted.length - maxVisible} more`}
      </button>
    {/if}
  </div>
{/if}
