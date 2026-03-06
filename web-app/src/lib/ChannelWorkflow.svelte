<script>
  import { activeChannel, activeProject } from './store.js'
  import MermaidDiagram from './MermaidDiagram.svelte'

  /** @type {{ script_source: string, script_path: string|null, script_content: string, mermaid: string, plugins: Array<{source: string, path: string, files: string[]}> } | null} */
  let data = $state(null)
  let loading = $state(false)
  let error = $state('')

  $effect(() => {
    const project = $activeProject
    const channel = $activeChannel
    if (!project || !channel) return

    const controller = new AbortController()

    loading = true
    data = null
    error = ''

    fetch(
      `/api/projects/${encodeURIComponent(project)}/channels/${encodeURIComponent(channel)}/workflow`,
      { signal: controller.signal },
    )
      .then((res) => {
        if (!res.ok) throw new Error(`HTTP ${res.status}`)
        return res.json()
      })
      .then((d) => {
        data = d
      })
      .catch((e) => {
        if (e.name !== 'AbortError') {
          console.error('Failed to fetch workflow:', e)
          error = e.message
        }
      })
      .finally(() => {
        loading = false
      })

    return () => controller.abort()
  })

  const SOURCE_LABELS = {
    'default': 'Default (built-in)',
    'channel-local': 'Channel-specific (local)',
    'channel-repo': 'Channel-specific (repo)',
    'project-local': 'Project default (local)',
    'project-repo': 'Project default (repo)',
  }
</script>

<div class="workflow-layout">
  {#if loading}
    <div class="placeholder">Loading...</div>
  {:else if error}
    <div class="placeholder">
      <p>Failed to load workflow</p>
      <p class="hint">{error}</p>
    </div>
  {:else if data}
    <div class="workflow-content">
      <!-- Workflow diagram -->
      <section class="workflow-section">
        <h2 class="section-title">State Machine</h2>
        <div class="diagram-container">
          <MermaidDiagram code={data.mermaid} />
        </div>
      </section>

      <!-- Workflow source info -->
      <section class="workflow-section">
        <h2 class="section-title">Workflow Script</h2>
        <div class="info-card">
          <div class="info-row">
            <span class="info-label">Source</span>
            <span class="info-value">{SOURCE_LABELS[data.script_source] || data.script_source}</span>
          </div>
          {#if data.script_path}
            <div class="info-row">
              <span class="info-label">Path</span>
              <span class="info-value mono">{data.script_path}</span>
            </div>
          {/if}
        </div>
        <details class="script-details">
          <summary>View source</summary>
          <pre class="script-source"><code>{data.script_content}</code></pre>
        </details>
      </section>

      <!-- Plugins -->
      <section class="workflow-section">
        <h2 class="section-title">Plugins</h2>
        {#if data.plugins.length === 0}
          <p class="empty-hint">No plugins configured for this channel.</p>
        {:else}
          {#each data.plugins as plugin}
            <div class="info-card">
              <div class="info-row">
                <span class="info-label">Source</span>
                <span class="info-value">{SOURCE_LABELS[plugin.source] || plugin.source}</span>
              </div>
              <div class="info-row">
                <span class="info-label">Path</span>
                <span class="info-value mono">{plugin.path}</span>
              </div>
              <div class="info-row">
                <span class="info-label">Files</span>
                <span class="info-value">{plugin.files.join(', ')}</span>
              </div>
            </div>
          {/each}
        {/if}
      </section>
    </div>
  {/if}
</div>

<style>
  .workflow-layout {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    height: 100%;
  }

  .workflow-content {
    padding: 20px 24px;
    max-width: 800px;
  }

  .placeholder {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    gap: 8px;
    color: hsl(var(--muted-foreground));
    text-align: center;
    padding: 40px 24px;
  }

  .placeholder p {
    margin: 0;
    font-size: 0.9rem;
  }

  .hint {
    font-size: 0.78rem !important;
    opacity: 0.7;
  }

  .workflow-section {
    margin-bottom: 28px;
  }

  .section-title {
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: hsl(var(--muted-foreground));
    margin: 0 0 10px;
    padding-bottom: 6px;
    border-bottom: 1px solid hsl(var(--border));
  }

  .diagram-container {
    margin-bottom: 8px;
  }

  .info-card {
    background: hsl(var(--card));
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    padding: 10px 14px;
    margin-bottom: 8px;
  }

  .info-row {
    display: flex;
    gap: 12px;
    padding: 4px 0;
    font-size: 0.82rem;
    align-items: baseline;
  }

  .info-label {
    color: hsl(var(--muted-foreground));
    flex-shrink: 0;
    min-width: 60px;
    font-weight: 500;
  }

  .info-value {
    color: hsl(var(--foreground) / 0.85);
    word-break: break-all;
  }

  .info-value.mono {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.78rem;
  }

  .empty-hint {
    font-size: 0.82rem;
    color: hsl(var(--muted-foreground));
    font-style: italic;
    margin: 0;
  }

  .script-details {
    margin-top: 8px;
  }

  .script-details summary {
    font-size: 0.82rem;
    color: hsl(var(--primary));
    cursor: pointer;
    padding: 4px 0;
    user-select: none;
  }

  .script-details summary:hover {
    text-decoration: underline;
  }

  .script-source {
    background: hsl(var(--accent));
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    padding: 14px 16px;
    overflow-x: auto;
    margin: 8px 0 0;
  }

  .script-source code {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.78rem;
    line-height: 1.55;
    color: hsl(var(--foreground) / 0.85);
  }

  @media (max-width: 767px) {
    .workflow-content {
      padding: 16px;
    }
  }
</style>
