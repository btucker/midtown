<script>
/**
 * ToolDataBlocks — dispatches an array of ToolBlock objects to the right component.
 *
 * Props:
 *   blocks — ToolBlock[] from msg.tool_data
 */
import BashBlock from "./BashBlock.svelte";
import EditBlock from "./EditBlock.svelte";
import TodoBlock from "./TodoBlock.svelte";
import ToolBlockGeneric from "./ToolBlockGeneric.svelte";

let { blocks } = $props();
</script>

{#each blocks as block (block)}
  {#if block.tool_name === 'Bash'}
    <BashBlock {block} />
  {:else if block.tool_name === 'Edit'}
    <EditBlock {block} />
  {:else if block.tool_name === 'TodoWrite'}
    <TodoBlock {block} />
  {:else}
    <ToolBlockGeneric {block} />
  {/if}
{/each}
