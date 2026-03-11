<script>
/**
 * ToolBlockGeneric — fallback renderer for tool calls without a specific component.
 *
 * Shows the tool name as a header with a collapsible JSON view of input/output.
 * Auto-collapses after 30s: preview (1 line) → collapsed (header only).
 *
 * Props:
 *   block     — ToolBlock { tool_name, input, output, error }
 *   timestamp — ISO 8601 timestamp of the parent message (for auto-collapse)
 */
import { highlightBlock } from "./highlighting.js";
import { createAutoCollapse } from "./useAutoCollapse.js";

let { block, timestamp = null } = $props();

let displayState = $state("collapsed");
let userOverride = $state(false);

const ac = $derived.by(() => createAutoCollapse(timestamp));

$effect.pre(() => {
	if (!userOverride) displayState = ac.initial;
});

$effect(() => {
	if (userOverride) return;
	ac.startTimer(() => {
		displayState = "collapsed";
	});
	return () => ac.clearTimer();
});

function toggle() {
	userOverride = true;
	displayState = displayState === "expanded" ? "collapsed" : "expanded";
}

let summary = $derived.by(() => {
	const inp = block.input;
	if (!inp) return block.tool_name;
	if (inp.file_path) return `${block.tool_name} ${inp.file_path}`;
	if (inp.pattern) return `${block.tool_name} ${inp.pattern}`;
	if (inp.query) return `${block.tool_name} "${inp.query}"`;
	return block.tool_name;
});

// Highlighted JSON for display
let highlightedInput = $derived(highlightBlock(JSON.stringify(block.input, null, 2), "json"));
let highlightedOutput = $derived.by(() => {
	if (!block.output) return "";
	const outputStr = typeof block.output === "string" ? block.output : JSON.stringify(block.output, null, 2);
	return highlightBlock(outputStr, "json");
});

// Single-line output preview for the "preview" state
let outputFirstLine = $derived.by(() => {
	if (!block.output) return "";
	const str = typeof block.output === "string" ? block.output : JSON.stringify(block.output, null, 2);
	return str.split("\n")[0] || "";
});
let highlightedOutputFirstLine = $derived(highlightBlock(outputFirstLine, "json"));
</script>

<div class="tool-generic" class:tool-error={block.error}>
  <button class="tool-header" onclick={toggle} aria-expanded={displayState === 'expanded'}>
    <span class="tool-chevron">{displayState === 'expanded' ? '▾' : '▸'}</span>
    <span class="tool-name">{summary}</span>
    {#if block.error}
      <span class="tool-error-badge">error</span>
    {/if}
  </button>

  {#if displayState === 'preview' && block.output}
    <div class="tool-body tool-preview">
      <pre>{@html highlightedOutputFirstLine}</pre>
    </div>
  {:else if displayState === 'expanded'}
    <div class="tool-body">
      <pre>{@html highlightedInput}</pre>
      {#if block.output}
        <div class="tool-output-divider">output</div>
        <pre>{@html highlightedOutput}</pre>
      {/if}
    </div>
  {/if}
</div>

<style>
  .tool-generic {
    margin: 6px 0;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    line-height: 1.45;
  }

  .tool-error {
    border-color: hsl(var(--destructive) / 0.5);
  }

  .tool-header {
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

  .tool-header:hover {
    background: hsl(var(--accent) / 0.8);
  }

  .tool-chevron {
    flex-shrink: 0;
    width: 1em;
    color: hsl(var(--muted-foreground));
    font-size: 0.7rem;
  }

  .tool-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tool-error-badge {
    flex-shrink: 0;
    font-size: 0.68rem;
    padding: 1px 5px;
    border-radius: 3px;
    background: hsl(var(--destructive) / 0.15);
    color: hsl(var(--destructive));
  }

  .tool-body {
    padding: 6px 10px;
    overflow-x: auto;
    max-height: 300px;
    overflow-y: auto;
    background: hsl(var(--card));
  }

  .tool-body pre {
    margin: 0;
    font-size: 0.72rem;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-all;
    color: hsl(var(--foreground));
  }

  .tool-output-divider {
    margin: 6px 0 4px;
    font-size: 0.68rem;
    color: hsl(var(--muted-foreground));
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }

  .tool-preview {
    max-height: 1.45em;
    overflow: hidden;
    position: relative;
  }

  .tool-preview::after {
    content: '';
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 100%;
    background: linear-gradient(to right, transparent 60%, hsl(var(--card)));
    pointer-events: none;
  }
</style>
