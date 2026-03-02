<script>
  import { onMount } from 'svelte'
  import Download from '@lucide/svelte/icons/download'
  import Share from '@lucide/svelte/icons/share'
  import X from '@lucide/svelte/icons/x'

  const STORAGE_KEY = 'midtown-pwa-install-dismissed'

  let showBanner = $state(false)
  let deferredPrompt = $state(null)
  let isIos = $state(false)

  function isStandalone() {
    return window.matchMedia('(display-mode: standalone)').matches ||
      navigator.standalone === true
  }

  function isDismissed() {
    try {
      return localStorage.getItem(STORAGE_KEY) === '1'
    } catch {
      return false
    }
  }

  function dismiss() {
    showBanner = false
    try {
      localStorage.setItem(STORAGE_KEY, '1')
    } catch {
      // localStorage unavailable — banner just hides for this session
    }
  }

  async function handleInstall() {
    if (!deferredPrompt) return
    deferredPrompt.prompt()
    const { outcome } = await deferredPrompt.userChoice
    deferredPrompt = null
    if (outcome === 'accepted') {
      dismiss()
    }
  }

  onMount(() => {
    if (isStandalone() || isDismissed()) return

    // Detect iOS Safari (not standalone, not Chrome/Firefox on iOS)
    const ua = navigator.userAgent
    const isiOS = /iPhone|iPad|iPod/.test(ua) && !navigator.standalone
    // Exclude non-Safari browsers on iOS (they show as CriOS, FxiOS, etc.)
    const isSafari = isiOS && /Safari/.test(ua) && !/CriOS|FxiOS|OPiOS|EdgiOS/.test(ua)

    if (isSafari) {
      isIos = true
      showBanner = true
      return
    }

    // Android/Chrome: listen for beforeinstallprompt
    function handleBeforeInstall(e) {
      e.preventDefault()
      deferredPrompt = e
      showBanner = true
    }

    window.addEventListener('beforeinstallprompt', handleBeforeInstall)

    return () => {
      window.removeEventListener('beforeinstallprompt', handleBeforeInstall)
    }
  })
</script>

{#if showBanner}
  <div class="install-banner flex items-center gap-2 border-b border-border bg-card px-3 py-2 text-[0.82rem] text-foreground">
    {#if isIos}
      <Share size={15} class="flex-shrink-0 text-muted-foreground" />
      <span class="flex-1 min-w-0">
        Install Midtown: tap
        <svg class="inline align-[-2px]" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M4 12v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8"/><polyline points="16 6 12 2 8 6"/><line x1="12" y1="2" x2="12" y2="15"/></svg>
        then <strong>"Add to Home Screen"</strong>
      </span>
    {:else}
      <Download size={15} class="flex-shrink-0 text-muted-foreground" />
      <span class="flex-1 min-w-0">Install Midtown for quick access</span>
      <button
        class="install-btn flex-shrink-0 rounded bg-primary px-2.5 py-0.5 text-[0.78rem] font-medium text-primary-foreground transition-opacity hover:opacity-80"
        onclick={handleInstall}
      >
        Install
      </button>
    {/if}
    <button
      class="dismiss-btn flex-shrink-0 rounded p-0.5 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
      onclick={dismiss}
      title="Dismiss"
    >
      <X size={14} />
    </button>
  </div>
{/if}

