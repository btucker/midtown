<script>
  import { onMount, onDestroy } from 'svelte'
  import { connected, activeProject } from './store.js'
  import { sendWsMessage, onNextError, clearErrorCallback } from './api.js'

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
  let pendingErrorCallbackId = null  // Track callback ID for cleanup on unmount

  // Approximate character width for a monospace font at 0.8rem.
  // This converts pixel width to terminal columns for the resize message.
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

    // Clear any previous pending callback
    if (pendingErrorCallbackId !== null) {
      clearErrorCallback(pendingErrorCallbackId)
      pendingErrorCallbackId = null
    }

    // Register error handler before sending
    pendingErrorCallbackId = onNextError((errorMsg) => {
      nudgeError = errorMsg
      nudgeStatus = null
      if (nudgeErrorTimeout) clearTimeout(nudgeErrorTimeout)
      nudgeErrorTimeout = setTimeout(() => { nudgeError = null }, 4000)
      pendingErrorCallbackId = null  // Callback consumed
    })

    if (sendWsMessage({ type: 'nudge', target: selectedWindow, message: text })) {
      nudgeText = ''
      nudgeStatus = 'sent'
      nudgeError = null
      if (nudgeStatusTimeout) clearTimeout(nudgeStatusTimeout)
      nudgeStatusTimeout = setTimeout(() => { nudgeStatus = null }, 2000)
      // Clear the error callback on success to prevent memory leak
      clearErrorCallback(pendingErrorCallbackId)
      pendingErrorCallbackId = null
    } else {
      nudgeError = 'Not connected to server'
      if (nudgeErrorTimeout) clearTimeout(nudgeErrorTimeout)
      nudgeErrorTimeout = setTimeout(() => { nudgeError = null }, 4000)
      // Clear the error callback since we handled the error immediately
      clearErrorCallback(pendingErrorCallbackId)
      pendingErrorCallbackId = null
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
    // Clean up pending error callback to prevent memory leak and state updates on unmounted component
    if (pendingErrorCallbackId !== null) {
      clearErrorCallback(pendingErrorCallbackId)
      pendingErrorCallbackId = null
    }
  })
</script>

<div class="flex-1 flex flex-col overflow-hidden bg-[#0d0d0d]">
  <div class="flex gap-0 p-0 bg-[#1a1a1a] border-b border-[#3a3a3a] overflow-x-auto [-webkit-overflow-scrolling:touch] [&::-webkit-scrollbar]:hidden [scrollbar-width:none]">
    {#each windows as win}
      <button
        class="px-3.5 py-2 border-none bg-transparent text-[#585858] text-[0.75rem] cursor-pointer whitespace-nowrap transition-colors duration-150 hover:text-[#a8a8a8] {selectedWindow === win ? 'text-[#5fafaf] border-b-2 border-[#5fafaf]' : ''}"
        onclick={() => selectWindow(win)}
      >
        {win}
      </button>
    {/each}
    {#if windows.length === 0}
      <span class="px-3.5 py-2 text-[#585858] text-[0.75rem]">No windows</span>
    {/if}
  </div>
  {#if error}
    <div class="px-4 py-2 bg-[#2a1a1a] text-[#e94560] text-[0.8rem] text-center border-b border-[#3a1a1a]">{error}</div>
  {/if}
  <pre class="flex-1 overflow-auto px-3 py-2 m-0 font-['SF_Mono',Monaco,Menlo,Consolas,monospace] text-[0.8rem] leading-snug whitespace-pre text-[#d4d4d4] [-webkit-overflow-scrolling:touch]" bind:this={paneEl}>{paneContent}</pre>
  <div class="flex items-center gap-1.5 px-2 py-2 mb-2 bg-[#1a1a1a] border-t border-[#3a3a3a]">
    <button
      class="px-2.5 py-2 bg-[#2a2a2a] border border-[#3a3a3a] rounded text-[#888] text-[0.7rem] cursor-pointer whitespace-nowrap hover:bg-[#3a3a3a] hover:text-[#aaa]"
      onclick={sendEscape}
      title="Send Escape key"
    >
      Esc
    </button>
    <input
      class="flex-1 px-2.5 py-2 bg-[#0d0d0d] border border-[#3a3a3a] rounded text-[#d4d4d4] font-['SF_Mono',Monaco,Menlo,Consolas,monospace] text-[0.8rem] outline-none focus:border-[#5fafaf] placeholder:text-[#585858]"
      type="text"
      placeholder="Message {selectedWindow}"
      bind:value={nudgeText}
      onkeydown={handleNudgeKeydown}
    />
    <button
      class="px-3 py-2 bg-[#2a3a3a] border border-[#3a3a3a] rounded text-[#5fafaf] text-[0.75rem] cursor-pointer whitespace-nowrap hover:bg-[#3a4a4a] disabled:opacity-40 disabled:cursor-default"
      onclick={sendNudge}
      disabled={!nudgeText.trim()}
    >
      Send
    </button>
    {#if nudgeStatus === 'sent'}
      <span class="text-[#5faf5f] text-[0.7rem] whitespace-nowrap animate-[fade-out_2s_forwards]">Sent</span>
    {/if}
    {#if nudgeError}
      <span class="text-[#e94560] text-[0.7rem] whitespace-nowrap animate-[fade-out_4s_forwards]">{nudgeError}</span>
    {/if}
  </div>
</div>

<style>
  @keyframes fade-out {
    0% { opacity: 1; }
    70% { opacity: 1; }
    100% { opacity: 0; }
  }
</style>
