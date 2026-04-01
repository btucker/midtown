<script lang="ts">
import { tick } from "svelte";

// Props
let {
	items = [], // Array of items to filter and display
	show = $bindable(false), // Whether dropdown is visible
	selectedIndex = $bindable(0), // Currently highlighted item index
	position = {}, // { top, left } positioning for dropdown
	getLabel = (item: unknown) => String(item), // Function to get display label
	getValue = (item: unknown) => String(item), // Function to get inserted value
	getDescription = (_item: unknown): string | null => null, // Function to get optional description
	getSeparator = (_item: unknown) => false, // Function to check if a divider should render above this item
	onSelect = (_item: unknown) => {}, // Callback when item is selected
} = $props();

let dropdownElement: HTMLDivElement | null = $state(null);

// Auto-scroll the highlighted item into view when selection changes
$effect(() => {
	if (show && dropdownElement && selectedIndex >= 0) {
		tick().then(() => {
			const highlighted = dropdownElement?.querySelector(".highlighted");
			if (highlighted) {
				highlighted.scrollIntoView({ block: "nearest", behavior: "smooth" });
			}
		});
	}
});

function handleItemClick(item: unknown) {
	onSelect(item);
	show = false;
}
</script>

{#if show && items.length > 0}
  <div
    class="absolute z-[1000] bg-card border-2 border-border rounded-lg max-h-[280px] overflow-y-auto shadow-[0_4px_12px_rgba(0,0,0,0.5)] min-w-[200px] max-w-[400px] -translate-y-[calc(100%+8px)]"
    bind:this={dropdownElement}
    style:top="{position.top}px"
    style:left="{position.left}px"
    style:width={position.width ? `${position.width}px` : 'auto'}
    data-testid="autocomplete-dropdown"
  >
    {#each items as item, i}
      {#if getSeparator(item)}
        <div class="h-px bg-border mx-2.5 my-1"></div>
      {/if}
      <button
        data-testid="autocomplete-item"
        type="button"
        class="flex flex-col items-start gap-0.5 px-3.5 py-2.5 w-full border-none bg-transparent text-foreground text-left cursor-pointer transition-colors duration-150 border-b border-border last:border-b-0 hover:bg-accent {i === selectedIndex ? 'highlighted bg-accent' : ''}"
        onclick={() => handleItemClick(item)}
      >
        <span class="font-semibold text-[0.95rem] font-['SF_Mono',Menlo,Consolas,Monaco,'Courier_New',monospace]">{getLabel(item)}</span>
        {#if getDescription(item)}
          <span class="text-muted-foreground text-[0.85rem] truncate max-w-full">{getDescription(item)}</span>
        {/if}
      </button>
    {/each}
  </div>
{/if}
