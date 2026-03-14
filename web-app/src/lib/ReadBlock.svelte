<script lang="ts">
/**
 * ReadBlock — renders a Read tool call with syntax-highlighted file content.
 *
 * Parses the cat-n format output (line numbers + content), displays a
 * non-selectable line number gutter, and applies language-aware syntax
 * highlighting based on the file extension.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 */
import { escapeHtml, getLanguage, highlightBlock } from "./highlighting.ts";

let { block } = $props();

let expanded = $state(false);

let filePath = $derived(block.input?.file_path || "unknown");
let lang = $derived(getLanguage(filePath));

// Parse cat-n format: "     1→content" (spaces + number + tab + content)
let parsedLines = $derived.by(() => {
	if (!block.output) return [];
	const raw = typeof block.output === "string" ? block.output : JSON.stringify(block.output, null, 2);
	const lines = raw
		.split("\n")
		.filter((l) => l.length > 0)
		.map((line) => {
			const match = line.match(/^\s*(\d+)\t(.*)$/);
			if (match) {
				return { num: match[1], content: match[2] };
			}
			return { num: "", content: line };
		});

	// For error state, escape HTML without syntax highlighting
	if (block.error) {
		return lines.map((l) => ({ ...l, html: escapeHtml(l.content) }));
	}

	// Highlight the full block to preserve multi-line token context,
	// then split back into individual lines
	const fullText = lines.map((l) => l.content).join("\n");
	const highlightedHtml = highlightBlock(fullText, lang);
	const highlightedLines = highlightedHtml.split("\n");

	return lines.map((l, i) => ({ ...l, html: highlightedLines[i] || escapeHtml(l.content) }));
});

let totalLines = $derived(parsedLines.length);
let isLong = $derived(totalLines > 8);
let previewLines = $derived(parsedLines.slice(0, 3));

// Short path for display — show just the filename or last 2 segments
let shortPath = $derived.by(() => {
	const parts = filePath.split("/");
	if (parts.length <= 2) return filePath;
	return `…/${parts.slice(-2).join("/")}`;
});
</script>

<div class="read-block" class:read-error={block.error}>
  <button class="read-header" onclick={() => expanded = !expanded} aria-expanded={expanded}>
    <span class="read-chevron">{expanded ? '▾' : '▸'}</span>
    <span class="read-label">Read</span>
    <span class="read-path" title={filePath}>{shortPath}</span>
    <span class="read-stats">{totalLines} lines</span>
    {#if block.error}
      <span class="read-error-badge">error</span>
    {/if}
  </button>

  {#if !expanded && isLong}
    <div class="read-body read-preview">
      {#each previewLines as line}
        <div class="read-line">
          <span class="read-line-num">{line.num}</span>
          <span class="read-line-content">{@html line.html}</span>
        </div>
      {/each}
    </div>
  {:else}
    <div class="read-body" class:read-scrollable={isLong}>
      {#each parsedLines as line}
        <div class="read-line">
          <span class="read-line-num">{line.num}</span>
          <span class="read-line-content">{@html line.html}</span>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .read-block {
    margin: 6px 0;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    line-height: 1.45;
  }

  .read-error {
    border-color: hsl(var(--destructive) / 0.5);
  }

  .read-header {
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

  .read-header:hover {
    background: hsl(var(--accent) / 0.8);
  }

  .read-chevron {
    flex-shrink: 0;
    width: 1em;
    color: hsl(var(--muted-foreground));
    font-size: 0.7rem;
  }

  .read-label {
    flex-shrink: 0;
    color: hsl(var(--muted-foreground));
    font-weight: 600;
  }

  .read-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: hsl(var(--foreground));
  }

  .read-stats {
    flex-shrink: 0;
    font-size: 0.72rem;
    color: hsl(var(--muted-foreground));
  }

  .read-error-badge {
    flex-shrink: 0;
    font-size: 0.68rem;
    padding: 1px 5px;
    border-radius: 3px;
    background: hsl(var(--destructive) / 0.15);
    color: hsl(var(--destructive));
  }

  .read-body {
    overflow-x: auto;
    background: hsl(var(--card));
  }

  .read-scrollable {
    max-height: 400px;
    overflow-y: auto;
  }

  .read-preview {
    max-height: calc(3 * 1.45em);
    overflow: hidden;
    position: relative;
  }

  .read-preview::after {
    content: '';
    position: absolute;
    bottom: 0;
    left: 0;
    right: 0;
    height: 1.45em;
    background: linear-gradient(to bottom, transparent, hsl(var(--card)));
    pointer-events: none;
  }

  .read-line {
    display: flex;
    padding: 0 10px 0 0;
    min-height: 1.45em;
    white-space: pre;
  }

  .read-line-num {
    flex-shrink: 0;
    width: 4.5em;
    padding: 0 8px 0 10px;
    text-align: right;
    user-select: none;
    -webkit-user-select: none;
    color: hsl(var(--muted-foreground));
    opacity: 0.5;
  }

  .read-line-content {
    flex: 1;
    min-width: 0;
  }

  .read-error .read-body {
    color: hsl(var(--destructive));
  }
</style>
