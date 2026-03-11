<script>
/**
 * EditBlock — renders an Edit tool call with a collapsible header and DiffView.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 *           input.file_path — path being edited
 *           input.old_string — text that was replaced
 *           input.new_string — replacement text
 *   timestamp — ISO 8601 timestamp of the parent message (for auto-collapse)
 */
import DiffView from "./DiffView.svelte";
import { createAutoCollapse } from "./useAutoCollapse.js";

let { block, timestamp = null } = $props();

let filePath = $derived(block.input?.file_path || "unknown");
let oldString = $derived(block.input?.old_string || "");
let newString = $derived(block.input?.new_string || "");

let displayState = $state("collapsed");
let userOverride = $state(false);

const ac = $derived.by(() => createAutoCollapse(timestamp));

$effect.pre(() => {
	if (!userOverride) displayState = ac.initial;
});

$effect(() => {
	if (userOverride) return;
	const currentAc = ac;
	currentAc.startTimer(() => {
		displayState = "collapsed";
	});
	return () => currentAc.clearTimer();
});

function toggle() {
	userOverride = true;
	ac.clearTimer();
	displayState = displayState === "expanded" ? "collapsed" : "expanded";
}
</script>

<div class="edit-block">
  <button class="edit-header" onclick={toggle} aria-expanded={displayState === 'expanded'}>
    <span class="edit-chevron">{displayState === 'expanded' ? '▾' : '▸'}</span>
    <span class="edit-path">Edit {filePath}</span>
  </button>

  {#if displayState === 'preview'}
    <div class="edit-preview">
      <DiffView {filePath} {oldString} {newString} bare />
    </div>
  {:else if displayState === 'expanded'}
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
