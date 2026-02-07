<script>
  import { onMount, onDestroy } from 'svelte'
  import { connected, activeProject } from './store.js'
  import { sendWsMessage, onNextError } from './api.js'

  let paneContent = $state('')
  let error = $state(null)
  let windows = $state([])
  let selectedWindow = $state('lead')
  let nudgeText = $state('')
  let nudgeStatus = $state(null)
  let nudgeError = $state(null)
  let interval = null
  let windowInterval = null
  let paneEl = null
  let resizeTimeout = null
  let nudgeStatusTimeout = null
  let nudgeErrorTimeout = null
  let lastSentCols = 0

  // Approximate character width for a monospace font at 0.8rem.
  // This converts pixel width → terminal columns for the resize message.
  const CHAR_WIDTH_PX = 7.7

  function getViewportCols() {
    if (!paneEl) return 80
    // Use the pane content element's width minus padding (12px each side = 24px)
    // plus extra 20px to account for UI chrome/scrollbars and prevent horizontal scrolling
    const usable = paneEl.clientWidth - 44
    return Math.max(80, Math.floor(usable / CHAR_WIDTH_PX))
  }

  function sendViewWindow() {
    if (!selectedWindow) return
    const cols = getViewportCols()
    if (sendWsMessage({ type: 'view_window', window: selectedWindow, cols })) {
      lastSentCols = cols
    }
  }

  function sendLeaveWindow() {
    sendWsMessage({ type: 'leave_window' })
  }

  async function fetchWindows() {
    try {
      const project = $activeProject
      if (!project) return
      const res = await fetch(`/api/projects/${encodeURIComponent(project)}/tmux-windows`)
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
      const project = $activeProject
      if (!project) return
      const res = await fetch(`/api/projects/${encodeURIComponent(project)}/tmux-pane?window=${encodeURIComponent(selectedWindow)}`)
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

  function sendNudge() {
    const text = nudgeText.trim()
    if (!text || !selectedWindow) return

    // Register error handler before sending
    onNextError((errorMsg) => {
      nudgeError = errorMsg
      nudgeStatus = null
      if (nudgeErrorTimeout) clearTimeout(nudgeErrorTimeout)
      nudgeErrorTimeout = setTimeout(() => { nudgeError = null }, 4000)
    })

    if (sendWsMessage({ type: 'nudge', target: selectedWindow, message: text })) {
      nudgeText = ''
      nudgeStatus = 'sent'
      nudgeError = null
      if (nudgeStatusTimeout) clearTimeout(nudgeStatusTimeout)
      nudgeStatusTimeout = setTimeout(() => { nudgeStatus = null }, 2000)
    } else {
      nudgeError = 'Not connected to server'
      if (nudgeErrorTimeout) clearTimeout(nudgeErrorTimeout)
      nudgeErrorTimeout = setTimeout(() => { nudgeError = null }, 4000)
    }
  }

  function handleNudgeKeydown(e) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      sendNudge()
    }
  }

  function sendEscape() {
    if (!selectedWindow) return
    sendWsMessage({ type: 'send_key', target: selectedWindow, key: 'Escape' })
  }

  function selectWindow(name) {
    selectedWindow = name
    paneContent = ''
    error = null
    nudgeText = ''
    nudgeStatus = null
    fetchPane()
    sendViewWindow()
  }

  function startPolling() {
    if (interval) return
    fetchWindows()
    fetchPane()
    interval = setInterval(() => {
      fetchPane()
      // Re-send view_window periodically to recover from dropped messages.
      // Only sends if the viewport cols have changed or no successful send yet.
      const cols = getViewportCols()
      if (cols !== lastSentCols) {
        sendViewWindow()
      }
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

  function handleResize() {
    // Debounce resize events — only send after 300ms of no resizing
    if (resizeTimeout) clearTimeout(resizeTimeout)
    resizeTimeout = setTimeout(() => {
      sendViewWindow()
    }, 300)
  }

  // Re-send the view_window message whenever the WebSocket connects or reconnects.
  // On reconnect, the backend assigns a new conn_id and the old viewer tracking is
  // cleaned up, so we must re-register as a viewer to keep the resize in effect.
  $effect(() => {
    if ($connected && selectedWindow) {
      sendViewWindow()
    }
  })

  onMount(() => {
    startPolling()
    window.addEventListener('resize', handleResize)
  })

  onDestroy(() => {
    stopPolling()
    sendLeaveWindow()
    window.removeEventListener('resize', handleResize)
    if (resizeTimeout) clearTimeout(resizeTimeout)
    if (nudgeStatusTimeout) clearTimeout(nudgeStatusTimeout)
    if (nudgeErrorTimeout) clearTimeout(nudgeErrorTimeout)
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
  <pre class="pane-content" bind:this={paneEl}>{paneContent}</pre>
  <div class="nudge-bar">
    <button class="esc-btn" onclick={sendEscape} title="Send Escape key">
      Esc
    </button>
    <input
      class="nudge-input"
      type="text"
      placeholder="Message {selectedWindow}"
      bind:value={nudgeText}
      onkeydown={handleNudgeKeydown}
    />
    <button class="nudge-send" onclick={sendNudge} disabled={!nudgeText.trim()}>
      Send
    </button>
    {#if nudgeStatus === 'sent'}
      <span class="nudge-status">Sent</span>
    {/if}
    {#if nudgeError}
      <span class="nudge-error">{nudgeError}</span>
    {/if}
  </div>
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

  .nudge-bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 8px;
    margin-bottom: 8px;
    background: #1a1a1a;
    border-top: 1px solid #3a3a3a;
  }

  .esc-btn {
    padding: 8px 10px;
    background: #2a2a2a;
    border: 1px solid #3a3a3a;
    border-radius: 4px;
    color: #888;
    font-size: 0.7rem;
    font-family: inherit;
    cursor: pointer;
    white-space: nowrap;
  }

  .esc-btn:hover {
    background: #3a3a3a;
    color: #aaa;
  }

  .nudge-input {
    flex: 1;
    padding: 8px 10px;
    background: #0d0d0d;
    border: 1px solid #3a3a3a;
    border-radius: 4px;
    color: #d4d4d4;
    font-family: 'SF Mono', 'Monaco', 'Menlo', 'Consolas', monospace;
    font-size: 0.8rem;
    outline: none;
  }

  .nudge-input:focus {
    border-color: #5fafaf;
  }

  .nudge-input::placeholder {
    color: #585858;
  }

  .nudge-send {
    padding: 8px 12px;
    background: #2a3a3a;
    border: 1px solid #3a3a3a;
    border-radius: 4px;
    color: #5fafaf;
    font-size: 0.75rem;
    cursor: pointer;
    white-space: nowrap;
  }

  .nudge-send:hover:not(:disabled) {
    background: #3a4a4a;
  }

  .nudge-send:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .nudge-status {
    color: #5faf5f;
    font-size: 0.7rem;
    white-space: nowrap;
    animation: fade-out 2s forwards;
  }

  .nudge-error {
    color: #e94560;
    font-size: 0.7rem;
    white-space: nowrap;
    animation: fade-out 4s forwards;
  }

  @keyframes fade-out {
    0% { opacity: 1; }
    70% { opacity: 1; }
    100% { opacity: 0; }
  }
</style>
