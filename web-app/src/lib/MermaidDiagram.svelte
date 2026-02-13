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
  <div class="text-[#585858] text-[0.8rem] py-2">Loading diagram...</div>
{:else if error}
  <div class="bg-[#2a1a1a] border border-[#5a2a2a] rounded-md px-3 py-2 my-1.5 text-[0.8rem]">
    <div class="text-[#ff5f5f] font-semibold mb-1">Diagram error</div>
    <pre class="text-[#a0a0a0] m-0 whitespace-pre-wrap break-words">{error}</pre>
  </div>
{:else}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
  <div
    class="group bg-[#1a1a2e] rounded-md p-3 my-1.5 overflow-x-auto leading-none cursor-pointer relative hover:outline hover:outline-1 hover:outline-[#3a3a5e] [&>svg]:max-w-full [&>svg]:h-auto [&>svg]:block"
    onclick={handleExpand}
    title="Click to expand"
  >
    {@html svgHtml}
    <div class="absolute top-1.5 right-2 text-[0.7rem] text-[#585858] opacity-0 transition-opacity duration-150 pointer-events-none font-['SF_Mono',Menlo,Consolas,monospace] group-hover:opacity-100">Click to expand</div>
  </div>
{/if}
