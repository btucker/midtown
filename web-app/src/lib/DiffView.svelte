<script>
  /**
   * DiffView — renders an edit_file diff with syntax highlighting.
   *
   * Props:
   *   filePath  — the file being edited (e.g. "src/main.rs")
   *   oldString — the text that was replaced
   *   newString — the replacement text
   */
  import hljs from 'highlight.js/lib/core'

  let { filePath, oldString, newString } = $props()
  let expanded = $state(false)

  const EXT_TO_LANG = {
    rs: 'rust',
    js: 'javascript',
    jsx: 'javascript',
    ts: 'typescript',
    tsx: 'typescript',
    py: 'python',
    sh: 'bash',
    bash: 'bash',
    zsh: 'bash',
    json: 'json',
    toml: 'toml',
    yaml: 'yaml',
    yml: 'yaml',
    css: 'css',
    svelte: 'xml',
    html: 'xml',
    xml: 'xml',
    md: 'xml',
  }

  function getLanguage(path) {
    if (!path) return null
    const ext = path.split('.').pop()?.toLowerCase()
    return ext ? (EXT_TO_LANG[ext] || null) : null
  }

  function highlightLine(text, lang) {
    if (!lang || !hljs.getLanguage(lang)) return escapeHtml(text)
    try {
      return hljs.highlight(text, { language: lang }).value
    } catch {
      return escapeHtml(text)
    }
  }

  function escapeHtml(str) {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
  }

  let lang = $derived(getLanguage(filePath))
  let oldLines = $derived((oldString || '').split('\n'))
  let newLines = $derived((newString || '').split('\n'))

  // Short filename for display
  let shortPath = $derived(filePath || 'unknown')

  function toggle() {
    expanded = !expanded
  }
</script>

<div class="diff-view">
  <button
    class="diff-header"
    onclick={toggle}
    aria-expanded={expanded}
  >
    <span class="diff-chevron">{expanded ? '▾' : '▸'}</span>
    <span class="diff-path">{shortPath}</span>
    <span class="diff-stats">
      {#if oldLines.length > 0 && !(oldLines.length === 1 && oldLines[0] === '')}
        <span class="diff-stat-del">−{oldLines.length}</span>
      {/if}
      {#if newLines.length > 0 && !(newLines.length === 1 && newLines[0] === '')}
        <span class="diff-stat-add">+{newLines.length}</span>
      {/if}
    </span>
  </button>

  {#if expanded}
    <div class="diff-body">
      {#each oldLines as line}
        {#if !(oldLines.length === 1 && line === '')}
          <div class="diff-line diff-line-del">
            <span class="diff-line-prefix">−</span>
            <span class="diff-line-content">{@html highlightLine(line, lang)}</span>
          </div>
        {/if}
      {/each}
      {#each newLines as line}
        {#if !(newLines.length === 1 && line === '')}
          <div class="diff-line diff-line-add">
            <span class="diff-line-prefix">+</span>
            <span class="diff-line-content">{@html highlightLine(line, lang)}</span>
          </div>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .diff-view {
    margin: 6px 0;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    line-height: 1.45;
  }

  .diff-header {
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

  .diff-header:hover {
    background: hsl(var(--accent) / 0.8);
  }

  .diff-chevron {
    flex-shrink: 0;
    width: 1em;
    color: hsl(var(--muted-foreground));
    font-size: 0.7rem;
  }

  .diff-path {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: hsl(var(--foreground));
  }

  .diff-stats {
    flex-shrink: 0;
    display: flex;
    gap: 6px;
    font-size: 0.72rem;
  }

  .diff-stat-del {
    color: #b31d28;
  }

  .diff-stat-add {
    color: #22863a;
  }

  :global(.dark) .diff-stat-del {
    color: #f97583;
  }

  :global(.dark) .diff-stat-add {
    color: #85e89d;
  }

  .diff-body {
    overflow-x: auto;
    max-height: 400px;
    overflow-y: auto;
  }

  .diff-line {
    display: flex;
    padding: 0 10px;
    min-height: 1.45em;
    white-space: pre;
  }

  .diff-line-del {
    background-color: #ffeef0;
    color: #24292e;
  }

  .diff-line-add {
    background-color: #f0fff4;
    color: #24292e;
  }

  :global(.dark) .diff-line-del {
    background-color: rgba(248, 81, 73, 0.15);
    color: #e1e4e8;
  }

  :global(.dark) .diff-line-add {
    background-color: rgba(63, 185, 80, 0.15);
    color: #e1e4e8;
  }

  .diff-line-prefix {
    flex-shrink: 0;
    width: 1.5em;
    user-select: none;
    color: inherit;
    opacity: 0.6;
  }

  .diff-line-content {
    flex: 1;
    min-width: 0;
  }
</style>
