<script lang="ts">
/**
 * BashBlock — renders a Bash tool call with command + collapsible output.
 *
 * Starts in 'preview' state (3-line output preview). Click to expand/collapse.
 * No auto-collapse — ToolRunSummary handles the time-based collapse.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 */
import { highlightBlock } from "./highlighting.ts";

let { block } = $props();

let expanded = $state(false);

let command = $derived(block.input?.command || "");
let outputText = $derived.by(() => {
	if (!block.output) return "";
	if (typeof block.output === "string") return block.output;
	if (block.output.stdout) return block.output.stdout;
	if (block.output.output) return block.output.output;
	return JSON.stringify(block.output, null, 2);
});
let hasOutput = $derived(outputText !== "");
let outputLines = $derived(outputText.split("\n"));

// Detect output language based on content
function detectOutputLanguage(output) {
	// Unified diff format
	if (output.startsWith("diff ")) {
		return "diff";
	}
	// JSON output (e.g., from jq, npm, cargo metadata)
	if (output.trim().startsWith("{") || output.trim().startsWith("[")) {
		return "json";
	}
	// Default to bash for shell output
	return "bash";
}

// Highlighted versions for display
let highlightedCommand = $derived(highlightBlock(command, "bash"));
let outputLang = $derived(detectOutputLanguage(outputText));
let highlightedOutput = $derived(highlightBlock(outputText, outputLang));

// 3-line preview for the preview state
let previewText = $derived(outputLines.slice(0, 3).join("\n"));
let highlightedPreview = $derived(highlightBlock(previewText, outputLang));
let isLong = $derived(outputLines.length > 3);
</script>

<div class="bash-block" class:bash-error={block.error}>
  <button class="bash-header" onclick={() => expanded = !expanded} aria-expanded={expanded}>
    <span class="bash-chevron">{expanded ? '▾' : '▸'}</span>
    <span class="bash-prompt">$</span>
    <span class="bash-command">{@html highlightedCommand}</span>
  </button>

  {#if hasOutput && !expanded && isLong}
    <div class="bash-output bash-preview" onclick={() => expanded = true} role="button" tabindex="0" onkeydown={(e) => e.key === 'Enter' && (expanded = true)}>
      <pre>{@html highlightedPreview}</pre>
    </div>
  {:else if hasOutput && (expanded || !isLong)}
    <div class="bash-output">
      <pre>{@html highlightedOutput}</pre>
    </div>
  {/if}
</div>

<style>
  .bash-block {
    margin: 6px 0;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    line-height: 1.45;
  }

  .bash-error {
    border-color: hsl(var(--destructive) / 0.5);
  }

  .bash-header {
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

  .bash-header:hover {
    background: hsl(var(--sidebar-background) / 0.8);
  }

  .bash-chevron {
    flex-shrink: 0;
    width: 1em;
    color: hsl(var(--muted-foreground));
    font-size: 0.7rem;
  }

  .bash-prompt {
    flex-shrink: 0;
    color: hsl(var(--muted-foreground));
    font-weight: 600;
  }

  .bash-command {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .bash-output {
    padding: 6px 10px;
    overflow-x: auto;
    background: transparent;
  }

  .bash-output pre {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.4;
    white-space: pre-wrap;
    word-break: break-all;
    color: hsl(var(--foreground));
  }

  .bash-preview {
    max-height: calc(3 * 1.4em);
    overflow: hidden;
    position: relative;
    cursor: pointer;
  }

  .bash-preview::after {
    content: '';
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 1.4em;
    background: linear-gradient(to bottom, transparent, hsl(var(--background)));
    pointer-events: none;
  }

  .bash-error .bash-output pre {
    color: hsl(var(--destructive));
  }
</style>
