<script>
  import { getSelkie } from './selkie.js'

  let { code } = $props()
  let svgHtml = $state('')
  let error = $state('')
  let loading = $state(true)

  let counter = 0

  $effect(() => {
    const currentCode = code
    loading = true
    error = ''
    svgHtml = ''

    getSelkie().then((selkie) => {
      try {
        const id = `mermaid-${counter++}`
        const result = selkie.render(id, currentCode)
        svgHtml = result.svg
        error = ''
      } catch (e) {
        error = e.message || String(e)
        svgHtml = ''
      }
      loading = false
    }).catch((e) => {
      error = `Failed to load renderer: ${e.message || e}`
      loading = false
    })
  })
</script>

{#if loading}
  <div class="mermaid-loading">Loading diagram...</div>
{:else if error}
  <div class="mermaid-error">
    <div class="mermaid-error-label">Diagram error</div>
    <pre class="mermaid-error-text">{error}</pre>
  </div>
{:else}
  <div class="mermaid-diagram">
    {@html svgHtml}
  </div>
{/if}

<style>
  .mermaid-diagram {
    background: #1a1a2e;
    border-radius: 6px;
    padding: 12px;
    margin: 6px 0;
    overflow-x: auto;
    line-height: 1;
  }

  .mermaid-diagram :global(svg) {
    max-width: 100%;
    height: auto;
    display: block;
  }

  .mermaid-loading {
    color: #585858;
    font-size: 0.8rem;
    padding: 8px 0;
  }

  .mermaid-error {
    background: #2a1a1a;
    border: 1px solid #5a2a2a;
    border-radius: 6px;
    padding: 8px 12px;
    margin: 6px 0;
    font-size: 0.8rem;
  }

  .mermaid-error-label {
    color: #ff5f5f;
    font-weight: 600;
    margin-bottom: 4px;
  }

  .mermaid-error-text {
    color: #a0a0a0;
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
  }
</style>
