<script lang="ts">
/**
 * ToolDataBlocks — dispatches an array of ToolBlock objects to the right component.
 *
 * Consecutive Read blocks are collapsed into a single ReadGroupBlock.
 *
 * Props:
 *   blocks — ToolBlock[] from msg.tool_data
 */
import BashBlock from "./BashBlock.svelte";
import EditBlock from "./EditBlock.svelte";
import ReadBlock from "./ReadBlock.svelte";
import ReadGroupBlock from "./ReadGroupBlock.svelte";
import TodoBlock from "./TodoBlock.svelte";
import ToolBlockGeneric from "./ToolBlockGeneric.svelte";

let { blocks } = $props();

type Segment =
	| { kind: "single"; block: (typeof blocks)[number] }
	| { kind: "read-group"; blocks: (typeof blocks)[number][] };

/**
 * Group consecutive Read blocks into segments. Non-Read blocks stay as
 * individual segments; runs of 2+ Read blocks become a read-group.
 */
let segments: Segment[] = $derived.by(() => {
	const result: Segment[] = [];
	let readRun: (typeof blocks)[number][] = [];

	function flushReads() {
		if (readRun.length === 0) return;
		if (readRun.length === 1) {
			result.push({ kind: "single", block: readRun[0] });
		} else {
			result.push({ kind: "read-group", blocks: [...readRun] });
		}
		readRun = [];
	}

	for (const block of blocks) {
		if (block.tool_name === "Read") {
			readRun.push(block);
		} else {
			flushReads();
			result.push({ kind: "single", block });
		}
	}
	flushReads();
	return result;
});
</script>

{#each segments as segment (segment.kind === 'read-group' ? segment.blocks[0] : segment.block)}
  {#if segment.kind === 'read-group'}
    <ReadGroupBlock blocks={segment.blocks} />
  {:else if segment.block.tool_name === 'Bash'}
    <BashBlock block={segment.block} />
  {:else if segment.block.tool_name === 'Edit'}
    <EditBlock block={segment.block} />
  {:else if segment.block.tool_name === 'Read'}
    <ReadBlock block={segment.block} />
  {:else if segment.block.tool_name === 'TodoWrite'}
    <TodoBlock block={segment.block} />
  {:else}
    <ToolBlockGeneric block={segment.block} />
  {/if}
{/each}
