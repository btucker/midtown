<script lang="ts">
/**
 * ReadGroupBlock — renders a group of consecutive Read tool calls as a single
 * collapsible summary line.
 *
 * Collapsed: "Read 3 files (file1.rs, file2.rs, file3.rs)"
 * Expanded: individual ReadBlock components for each file.
 *
 * Props:
 *   blocks — ToolBlock[] (all Read blocks in the group)
 */
import ReadBlock from "./ReadBlock.svelte";

let { blocks } = $props();

let expanded = $state(false);

let filePaths = $derived(
	blocks.map((b: { input?: { file_path?: string } }) => {
		const fp = b.input?.file_path || "unknown";
		const parts = fp.split("/");
		return parts[parts.length - 1];
	}),
);

let summary = $derived.by(() => {
	const names = filePaths.slice(0, 5);
	const rest = filePaths.length - names.length;
	const label = names.join(", ") + (rest > 0 ? `, +${rest} more` : "");
	return `${blocks.length} files (${label})`;
});
</script>

<div class="read-group">
  <button class="read-group-header" onclick={() => expanded = !expanded} aria-expanded={expanded}>
    <span class="read-group-chevron">{expanded ? '▾' : '▸'}</span>
    <span class="read-group-label">Read</span>
    <span class="read-group-summary">{summary}</span>
  </button>

  {#if expanded}
    <div class="read-group-body">
      {#each blocks as block (block)}
        <ReadBlock {block} />
      {/each}
    </div>
  {/if}
</div>

<style>
  .read-group {
    margin: 6px 0;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    line-height: 1.45;
  }

  .read-group-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 10px;
    background: hsl(var(--sidebar-background));
    border: none;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: hsl(var(--foreground));
    text-align: left;
  }

  .read-group-header:hover {
    background: hsl(var(--sidebar-background) / 0.8);
  }

  .read-group-chevron {
    flex-shrink: 0;
    width: 1em;
    color: hsl(var(--muted-foreground));
    font-size: 0.7rem;
  }

  .read-group-label {
    flex-shrink: 0;
    color: hsl(var(--muted-foreground));
    font-weight: 600;
  }

  .read-group-summary {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: hsl(var(--foreground));
  }

  .read-group-body {
    padding: 4px 8px 8px;
  }

  .read-group-body :global(.read-block) {
    margin: 4px 0;
  }
</style>
