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

  // Most recent items first for display
  let sorted = $derived([...items].reverse())
  let visible = $derived(expanded ? sorted : sorted.slice(0, maxVisible))
  let hasMore = $derived(sorted.length > maxVisible)

  /**
   * Extract the semantic_header from a ToolCall content part, or a
   * short description from a ToolResult part.
   */
  function describeItem(item) {
    for (const part of item.content) {
      if (part.ToolCall) {
        return part.ToolCall.semantic_header || part.ToolCall.name?.toLowerCase() || '?'
      }
      if (part.ToolResult) {
        return part.ToolResult.is_error ? '✗ error' : '✓ result'
      }
    }
    return '?'
  }

  function isToolCall(item) {
    return item.content.some((p) => p.ToolCall)
  }

  function isError(item) {
    return item.content.some((p) => p.ToolResult?.is_error)
  }
</script>

{#if items.length > 0}
  <div class="tool-activity mt-[2px] ml-[4.2em] text-[0.80rem] leading-[1.4]">
    {#each visible as item (item.item_id)}
      <div
        class="tool-item flex items-center gap-[0.4em] py-[1px]"
        class:text-red-400={isError(item)}
        class:text-[#4a4a4a]={isToolCall(item)}
      >
        <span class="tool-icon flex-shrink-0 select-none">
          {#if isToolCall(item)}
            <span class="text-[#3a6a3a]">›</span>
          {:else if isError(item)}
            <span class="text-[#af3a3a]">✗</span>
          {:else}
            <span class="text-[#3a5a3a]">✓</span>
          {/if}
        </span>
        <span class="font-mono truncate">{describeItem(item)}</span>
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
