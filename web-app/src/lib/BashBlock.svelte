<script>
/**
 * BashBlock — renders a Bash tool call with command + collapsible output.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 */
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
let isLong = $derived(outputLines.length > 10);

function toggle() {
	expanded = !expanded;
}
</script>

<div class="bash-block" class:bash-error={block.error}>
  <button class="bash-header" onclick={toggle} aria-expanded={expanded}>
    <span class="bash-chevron">{expanded || !isLong ? '▾' : '▸'}</span>
    <span class="bash-prompt">$</span>
    <span class="bash-command">{command}</span>
  </button>

  {#if hasOutput}
    <div class="bash-output" class:bash-collapsed={isLong && !expanded}>
      <pre>{outputText}</pre>
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
    background: hsl(var(--accent));
    border: none;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: hsl(var(--foreground));
    text-align: left;
  }

  .bash-header:hover {
    background: hsl(var(--accent) / 0.8);
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
    background: hsl(var(--card));
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

  .bash-collapsed {
    max-height: 14em;
    overflow-y: hidden;
    position: relative;
  }

  .bash-collapsed::after {
    content: '';
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 3em;
    background: linear-gradient(transparent, hsl(var(--card)));
    pointer-events: none;
  }

  .bash-error .bash-output pre {
    color: hsl(var(--destructive));
  }
</style>
