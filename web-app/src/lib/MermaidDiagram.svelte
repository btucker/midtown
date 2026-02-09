<script>
  import { getSelkie } from './selkie.js'
  import { getBiggerPicture, calculateFitToWidthScale } from './biggerPicture.js'

  let { code } = $props()
  let svgHtml = $state('')
  let error = $state('')
  let loading = $state(true)

  let counter = 0

  // Strip dangerous elements and attributes from SVG output
  function sanitizeSvg(svg) {
    const parser = new DOMParser()
    const doc = parser.parseFromString(svg, 'image/svg+xml')
    // Remove script tags and foreignObject
    for (const tag of ['script', 'foreignObject']) {
      for (const el of doc.querySelectorAll(tag)) {
        el.remove()
      }
    }
    // Remove event handler attributes from all elements
    for (const el of doc.querySelectorAll('*')) {
      for (const attr of [...el.attributes]) {
        if (attr.name.startsWith('on')) {
          el.removeAttribute(attr.name)
        }
      }
    }
    return new XMLSerializer().serializeToString(doc.documentElement)
  }

  // Convert SVG string to data URL for Bigger Picture
  function svgToDataUrl(svgString) {
    return `data:image/svg+xml;base64,${btoa(unescape(encodeURIComponent(svgString)))}`
  }

  $effect(() => {
    const currentCode = code
    loading = true
    error = ''
    svgHtml = ''

    getSelkie().then((selkie) => {
      try {
        const id = `mermaid-${counter++}`
        // Apply dark theme directive to match TUI rendering
        const themedCode = `%%{init: {"theme": "dark"}}%%\n${currentCode}`
        const result = selkie.render(id, themedCode)
        svgHtml = sanitizeSvg(result.svg)
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

  function handleExpand() {
    if (!svgHtml) return

    const bp = getBiggerPicture()
    if (!bp) return

    // Calculate initial scale to fit SVG to 95% of viewport width
    // This matches the fit-to-width behavior from the old MermaidModal
    const scale = calculateFitToWidthScale(svgHtml)

    // Convert SVG to data URL and open in lightbox
    const dataUrl = svgToDataUrl(svgHtml)
    bp.open({
      items: [{ img: dataUrl }],
      // Start at first (and only) item
      position: 0,
      // Apply fit-to-width scale
      scale: scale,
    })
  }
</script>

{#if loading}
  <div class="mermaid-loading">Loading diagram...</div>
{:else if error}
  <div class="mermaid-error">
    <div class="mermaid-error-label">Diagram error</div>
    <pre class="mermaid-error-text">{error}</pre>
  </div>
{:else}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div class="mermaid-diagram" onclick={handleExpand} title="Click to expand">
    {@html svgHtml}
    <div class="expand-hint">Click to expand</div>
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
    cursor: pointer;
    position: relative;
  }

  .mermaid-diagram:hover {
    outline: 1px solid #3a3a5e;
  }

  .expand-hint {
    position: absolute;
    top: 6px;
    right: 8px;
    font-size: 0.7rem;
    color: #585858;
    opacity: 0;
    transition: opacity 0.15s;
    pointer-events: none;
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
  }

  .mermaid-diagram:hover .expand-hint {
    opacity: 1;
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
