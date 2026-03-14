<script lang="ts">
/**
 * EditBlock — renders an Edit tool call with a collapsible header and DiffView.
 *
 * Starts in 'preview' state. Click to expand/collapse.
 * No auto-collapse — ToolRunSummary handles the time-based collapse.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 *           input.file_path — path being edited
 *           input.old_string — text that was replaced
 *           input.new_string — replacement text
 */
import DiffView from "./DiffView.svelte";

let { block } = $props();

let expanded = $state(false);

let filePath = $derived(block.input?.file_path || "unknown");
let oldString = $derived(block.input?.old_string || "");
let newString = $derived(block.input?.new_string || "");
</script>

<div class="edit-block">
  <button class="edit-header" onclick={() => expanded = !expanded} aria-expanded={expanded}>
    <span class="edit-chevron">{expanded ? '▾' : '▸'}</span>
    <span class="edit-path">Edit {filePath}</span>
  </button>

  {#if !expanded}
    <div class="edit-preview">
      <DiffView {filePath} {oldString} {newString} bare />
    </div>
  {:else}
    <DiffView {filePath} {oldString} {newString} bare />
  {/if}
</div>

<style>
  .edit-block {
    margin: 6px 0;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    overflow: hidden;
  }

  .edit-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 10px;
    background: hsl(var(--accent));
    border: none;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: hsl(var(--foreground));
    text-align: left;
  }

  .edit-header:hover {
    background: hsl(var(--accent) / 0.8);
  }

  .edit-chevron {
    flex-shrink: 0;
    width: 1em;
    color: hsl(var(--muted-foreground));
    font-size: 0.7rem;
  }

  .edit-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .edit-preview {
    max-height: calc(1.4em * 6 + 12px);
    overflow: hidden;
    position: relative;
  }

  .edit-preview::after {
    content: '';
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 3em;
    background: linear-gradient(transparent, hsl(var(--card)));
    pointer-events: none;
  }
</style>
