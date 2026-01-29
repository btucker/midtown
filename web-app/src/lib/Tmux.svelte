<script>
  import { onMount, onDestroy } from 'svelte'
  import { connected } from './store.js'

  let paneContent = $state('')
  let error = $state(null)
  let windows = $state([])
  let selectedWindow = $state('lead')
  let interval = null
  let windowInterval = null

  async function fetchWindows() {
    try {
      const res = await fetch('/api/tmux-windows')
      if (res.ok) {
        const data = await res.json()
        windows = data.windows || []
        // If selected window no longer exists, reset to first available
        if (windows.length > 0 && !windows.includes(selectedWindow)) {
          selectedWindow = windows[0]
        }
      }
    } catch {
      // Silently ignore — windows list is non-critical
    }
  }

  async function fetchPane() {
    try {
      const res = await fetch(`/api/tmux-pane?window=${encodeURIComponent(selectedWindow)}`)
      if (res.ok) {
        const data = await res.json()
        paneContent = stripAnsi(data.content)
        error = null
      } else if (res.status === 404) {
        paneContent = ''
        error = `Window "${selectedWindow}" not found`
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

  function selectWindow(name) {
    selectedWindow = name
    paneContent = ''
    error = null
    fetchPane()
  }

  function startPolling() {
    if (interval) return
    fetchWindows()
    fetchPane()
    interval = setInterval(() => {
      fetchPane()
      // Refresh window list less frequently
    }, 1000)
    // Refresh windows every 10 seconds
    windowInterval = setInterval(fetchWindows, 10000)
  }

  function stopPolling() {
    if (interval) {
      clearInterval(interval)
      interval = null
    }
    if (windowInterval) {
      clearInterval(windowInterval)
      windowInterval = null
    }
  }

  onMount(() => {
    startPolling()
  })

  onDestroy(() => {
    stopPolling()
  })
</script>

<div class="tmux-container">
  <div class="window-selector">
    {#each windows as win}
      <button
        class="window-btn"
        class:active={selectedWindow === win}
        onclick={() => selectWindow(win)}
      >
        {win}
      </button>
    {/each}
    {#if windows.length === 0}
      <span class="no-windows">No windows</span>
    {/if}
  </div>
  {#if error}
    <div class="error-banner">{error}</div>
  {/if}
  <pre class="pane-content">{paneContent}</pre>
</div>

<style>
  .tmux-container {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: #0d0d0d;
  }

  .window-selector {
    display: flex;
    gap: 0;
    padding: 0;
    background: #1a1a1a;
    border-bottom: 1px solid #3a3a3a;
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
    scrollbar-width: none;
  }

  .window-selector::-webkit-scrollbar {
    display: none;
  }

  .window-btn {
    padding: 8px 14px;
    border: none;
    background: transparent;
    color: #585858;
    font-family: inherit;
    font-size: 0.75rem;
    cursor: pointer;
    white-space: nowrap;
    transition: color 0.15s;
  }

  .window-btn:hover {
    color: #a8a8a8;
  }

  .window-btn.active {
    color: #5fafaf;
    border-bottom: 2px solid #5fafaf;
  }

  .no-windows {
    padding: 8px 14px;
    color: #585858;
    font-size: 0.75rem;
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
