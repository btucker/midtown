<script>
  import { onMount, onDestroy } from 'svelte'
  import { connected, activeProject } from './store.js'
  import { sendWsMessage, onNextError, clearErrorCallback } from './api.js'

  let zellijUrl = $state(null)
  let loading = $state(true)
  let error = $state(null)
  let nudgeText = $state('')
  let nudgeStatus = $state(null)
  let nudgeError = $state(null)
  let nudgeStatusTimeout = null
  let nudgeErrorTimeout = null
  let pendingErrorCallbackId = null

  async function fetchZellijUrl() {
    try {
      const project = $activeProject
      if (!project) return
      loading = true
      error = null
      const res = await fetch(`/api/projects/${encodeURIComponent(project)}/zellij-web-url`)
      if (res.ok) {
        const data = await res.json()
        zellijUrl = data.url
        loading = false
      } else {
        error = `Failed to get Zellij web client URL (${res.status})`
        loading = false
      }
    } catch (err) {
      error = 'Failed to connect to server'
      loading = false
    }
  }

  function retry() {
    zellijUrl = null
    fetchZellijUrl()
  }

  function sendNudge() {
    const text = nudgeText.trim()
    if (!text) return

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
      pendingErrorCallbackId = null
    })

    if (sendWsMessage({ type: 'nudge', target: 'lead', message: text })) {
      nudgeText = ''
      nudgeStatus = 'sent'
      nudgeError = null
      if (nudgeStatusTimeout) clearTimeout(nudgeStatusTimeout)
      nudgeStatusTimeout = setTimeout(() => { nudgeStatus = null }, 2000)
      clearErrorCallback(pendingErrorCallbackId)
      pendingErrorCallbackId = null
    } else {
      nudgeError = 'Not connected to server'
      if (nudgeErrorTimeout) clearTimeout(nudgeErrorTimeout)
      nudgeErrorTimeout = setTimeout(() => { nudgeError = null }, 4000)
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

  onMount(() => {
    fetchZellijUrl()
  })

  onDestroy(() => {
    if (nudgeStatusTimeout) clearTimeout(nudgeStatusTimeout)
    if (nudgeErrorTimeout) clearTimeout(nudgeErrorTimeout)
    if (pendingErrorCallbackId !== null) {
      clearErrorCallback(pendingErrorCallbackId)
      pendingErrorCallbackId = null
    }
  })
</script>

<div class="flex-1 flex flex-col overflow-hidden bg-background">
  {#if loading}
    <div class="flex-1 flex items-center justify-center text-muted-foreground text-[0.85rem]">
      Loading Zellij web client...
    </div>
  {:else if error}
    <div class="flex-1 flex flex-col items-center justify-center gap-4 p-6">
      <div class="text-destructive text-[0.85rem] text-center">{error}</div>
      <button
        class="px-4 py-2 bg-accent border border-border rounded text-[hsl(var(--link-default))] text-[0.8rem] cursor-pointer hover:bg-accent/80"
        onclick={retry}
      >
        Retry
      </button>
      <div class="text-muted-foreground text-[0.75rem] text-center max-w-sm leading-relaxed">
        Make sure the Zellij web client is enabled:
        <code class="block mt-2 px-3 py-1.5 bg-card rounded text-foreground text-[0.7rem]">zellij options --enable-web-client</code>
      </div>
    </div>
  {:else if zellijUrl}
    <iframe
      src={zellijUrl}
      title="Zellij Terminal"
      class="flex-1 w-full border-none bg-background"
      sandbox="allow-same-origin allow-scripts allow-popups allow-forms"
      allow="clipboard-read; clipboard-write"
    ></iframe>
  {/if}

  <div class="flex items-center gap-1.5 px-2 py-2 bg-card border-t border-border" style="padding-bottom: calc(0.5rem + env(safe-area-inset-bottom, 0px));">
    <input
      class="flex-1 px-2.5 py-2 bg-background border border-border rounded text-foreground font-['SF_Mono',Monaco,Menlo,Consolas,monospace] text-[0.8rem] outline-none focus:border-[hsl(var(--link-default))] placeholder:text-muted-foreground"
      type="text"
      placeholder="Message lead"
      bind:value={nudgeText}
      onkeydown={handleNudgeKeydown}
    />
    <button
      class="px-3 py-2 bg-accent border border-border rounded text-[hsl(var(--link-default))] text-[0.75rem] cursor-pointer whitespace-nowrap hover:bg-accent/80 disabled:opacity-40 disabled:cursor-default"
      onclick={sendNudge}
      disabled={!nudgeText.trim()}
    >
      Send
    </button>
    {#if nudgeStatus === 'sent'}
      <span class="text-[hsl(var(--link-default))] text-[0.7rem] whitespace-nowrap animate-[fade-out_2s_forwards]">Sent</span>
    {/if}
    {#if nudgeError}
      <span class="text-destructive text-[0.7rem] whitespace-nowrap animate-[fade-out_4s_forwards]">{nudgeError}</span>
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
