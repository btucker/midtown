<script>
  import { onMount, onDestroy } from 'svelte'
  import { connected } from './store.js'

  let paneContent = $state('')
  let error = $state(null)
  let polling = $state(false)
  let interval = null

  async function fetchPane() {
    try {
      const res = await fetch('/api/lead-pane')
      if (res.ok) {
        const data = await res.json()
        paneContent = stripAnsi(data.content)
        error = null
      } else if (res.status === 404) {
        paneContent = ''
        error = 'Lead session not found'
      } else {
        error = `Error: ${res.status}`
      }
    } catch (err) {
      error = 'Failed to connect'
    }
  }

  // Strip ANSI escape codes for plain text rendering
  function stripAnsi(text) {
    // Matches ANSI escape sequences: CSI sequences, OSC sequences, and simple escapes
    return text.replace(/\x1b\[[0-9;]*[a-zA-Z]|\x1b\][^\x07]*\x07|\x1b[()][0-9A-B]|\x1b\[[\?]?[0-9;]*[hlm]/g, '')
  }

  function startPolling() {
    if (interval) return
    polling = true
    fetchPane()
    interval = setInterval(fetchPane, 1000)
  }

  function stopPolling() {
    if (interval) {
      clearInterval(interval)
      interval = null
    }
    polling = false
  }

  onMount(() => {
    startPolling()
  })

  onDestroy(() => {
    stopPolling()
  })
</script>

<div class="lead-container">
  {#if error}
    <div class="error-banner">{error}</div>
  {/if}
  <pre class="pane-content">{paneContent}</pre>
</div>

<style>
  .lead-container {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: #0d0d0d;
  }

  .error-banner {
    padding: 8px 16px;
    background: #2a1a1a;
    color: #e94560;
    font-size: 0.8rem;
    text-align: center;
    border-bottom: 1px solid #3a1a1a;
  }

  .pane-content {
    flex: 1;
    overflow: auto;
    padding: 8px 12px;
    margin: 0;
    font-family: 'SF Mono', 'Monaco', 'Menlo', 'Consolas', monospace;
    font-size: 0.8rem;
    line-height: 1.3;
    white-space: pre;
    color: #d4d4d4;
    -webkit-overflow-scrolling: touch;
  }
</style>
