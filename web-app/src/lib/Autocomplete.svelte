<script>
  import { tick } from 'svelte'

  // Props
  let {
    items = [],          // Array of items to filter and display
    show = $bindable(false),     // Whether dropdown is visible
    selectedIndex = $bindable(0), // Currently highlighted item index
    position = {},       // { top, left } positioning for dropdown
    getLabel = (item) => String(item),      // Function to get display label
    getValue = (item) => String(item),      // Function to get inserted value
    getDescription = (item) => null,        // Function to get optional description
    onSelect = () => {},                    // Callback when item is selected
  } = $props()

  let dropdownElement = $state(null)

  // Auto-scroll the highlighted item into view when selection changes
  $effect(() => {
    if (show && dropdownElement && selectedIndex >= 0) {
      tick().then(() => {
        const highlighted = dropdownElement?.querySelector('.autocomplete-item.highlighted')
        if (highlighted) {
          highlighted.scrollIntoView({ block: 'nearest', behavior: 'smooth' })
        }
      })
    }
  })

  function handleItemClick(item) {
    onSelect(item)
    show = false
  }
</script>

{#if show && items.length > 0}
  <div
    class="autocomplete-dropdown"
    bind:this={dropdownElement}
    style:top="{position.top}px"
    style:left="{position.left}px"
    style:width={position.width ? `${position.width}px` : 'auto'}
  >
    {#each items as item, i}
      <button
        type="button"
        class="autocomplete-item"
        class:highlighted={i === selectedIndex}
        onclick={() => handleItemClick(item)}
      >
        <span class="item-label">{getLabel(item)}</span>
        {#if getDescription(item)}
          <span class="item-description">{getDescription(item)}</span>
        {/if}
      </button>
    {/each}
  </div>
{/if}

<style>
  .autocomplete-dropdown {
    position: fixed;
    z-index: 1000;
    background: #1a1a1a;
    border: 2px solid #2a2a2a;
    border-radius: 8px;
    max-height: 280px;
    overflow-y: auto;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.5);
    min-width: 200px;
    max-width: 400px;
    /* Position above the trigger point by shifting up by own height + gap */
    transform: translateY(calc(-100% - 8px));
  }

  .autocomplete-item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 10px 14px;
    width: 100%;
    border: none;
    background: transparent;
    color: #d0d0d0;
    text-align: left;
    cursor: pointer;
    transition: background-color 0.15s;
    border-bottom: 1px solid #2a2a2a;
  }

  .autocomplete-item:last-child {
    border-bottom: none;
  }

  .autocomplete-item.highlighted {
    background: #2a2a2a;
  }

  .autocomplete-item:hover {
    background: #2a2a2a;
  }

  .item-label {
    font-weight: 600;
    font-size: 0.95rem;
    font-family: 'SF Mono', 'Menlo', 'Consolas', 'Monaco', 'Courier New', monospace;
  }

  .item-description {
    color: #888;
    font-size: 0.85rem;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }
</style>
