<script>
  import { authProfilesByProvider, selectedAuthProvider, authSwitching } from './store.js'
  import { fetchAllAuthProfiles, switchAuthProfile } from './api.js'
  import { onMount } from 'svelte'

  let open = $state(false)
  let error = $state(null)

  onMount(() => {
    fetchAllAuthProfiles()
  })

  function toggle() {
    if (!$authSwitching) {
      open = !open
      error = null
    }
  }

  async function selectProfile(profile, provider) {
    if (profile.is_current || $authSwitching) return
    open = false
    error = null
    const result = await switchAuthProfile(profile.name, provider)
    if (!result.ok) {
      error = result.error
      setTimeout(() => { error = null }, 5000)
    } else {
      // Refresh all profiles after switching
      await fetchAllAuthProfiles()
    }
  }

  function selectProvider(provider) {
    $selectedAuthProvider = provider
  }

  function handleClickOutside(event) {
    if (open && !event.target.closest('.auth-switcher')) {
      open = false
    }
  }

  $effect(() => {
    if (open) {
      document.addEventListener('click', handleClickOutside, true)
      return () => document.removeEventListener('click', handleClickOutside, true)
    }
  })

  // Get profiles for the currently selected provider
  const currentProviderProfiles = $derived($authProfilesByProvider[$selectedAuthProvider] || [])

  // Find the current profile across all providers
  const currentProfile = $derived((() => {
    for (const [provider, profiles] of Object.entries($authProfilesByProvider)) {
      const current = profiles.find((p) => p.is_current)
      if (current) {
        return { ...current, provider }
      }
    }
    return null
  })())

  // Get list of available providers (only those with profiles)
  const availableProviders = $derived(Object.keys($authProfilesByProvider))

  // Provider display names
  const providerNames = {
    'claude': 'Claude',
    'codex': 'Codex',
    'zai': 'z.ai'
  }
</script>

{#if availableProviders.length > 0}
  <div class="relative">
    <button
      class="flex items-center gap-1 bg-sidebar-accent border border-sidebar-border rounded-md px-2 py-1 text-muted-foreground text-[0.75rem] cursor-pointer transition-all duration-150 hover:bg-sidebar-accent/80 hover:text-sidebar-foreground hover:border-[hsl(var(--link-default))] disabled:opacity-60 disabled:cursor-wait"
      class:opacity-60={$authSwitching}
      class:cursor-wait={$authSwitching}
      onclick={toggle}
      disabled={$authSwitching}
      title={$authSwitching ? 'Switching profile...' : 'Switch auth profile'}
    >
      {#if $authSwitching}
        <span class="inline-block w-2.5 h-2.5 border-[1.5px] border-muted-foreground border-t-[hsl(var(--link-default))] rounded-full animate-spin"></span>
      {:else}
        <span class="before:content-['🔑'] before:text-[0.7rem]"></span>
      {/if}
      <span class="text-muted-foreground shrink-0">{providerNames[currentProfile?.provider] || '...'}</span>
      <span class="text-muted-foreground mx-0.5 shrink-0">/</span>
      <span class="max-w-[60px] truncate">{currentProfile?.name || '...'}</span>
    </button>

    {#if error}
      <div class="absolute top-full right-0 mt-1 px-2 py-1 bg-destructive/15 border border-destructive rounded text-destructive text-[0.7rem] whitespace-nowrap z-[100]">
        {error}
      </div>
    {/if}

    {#if open}
      <div class="absolute top-full right-0 mt-1 bg-card border border-sidebar-border rounded-md min-w-[180px] z-[100] shadow-[0_4px_12px_rgba(0,0,0,0.4)]">
        {#if availableProviders.length > 1}
          <div class="flex border-b border-sidebar-border pt-1 px-1">
            {#each availableProviders as provider}
              <button
                class="flex-1 px-2 py-1 border-none bg-transparent text-muted-foreground text-[0.7rem] cursor-pointer border-b-2 border-transparent transition-all duration-150 hover:text-sidebar-foreground {provider === $selectedAuthProvider ? 'text-[hsl(var(--link-default))] border-b-[hsl(var(--link-default))]' : ''}"
                onclick={() => selectProvider(provider)}
              >
                {providerNames[provider]}
              </button>
            {/each}
          </div>
        {/if}

        <div class="overflow-hidden">
          {#each currentProviderProfiles as profile}
            <button
              class="flex items-center gap-1.5 w-full px-2.5 py-2 border-none bg-transparent text-muted-foreground text-[0.75rem] cursor-pointer text-left transition-colors duration-100 hover:bg-sidebar-accent hover:text-sidebar-foreground disabled:cursor-default {profile.is_current ? 'text-[hsl(var(--link-default))]' : ''} {!profile.has_credentials ? 'opacity-50' : ''}"
              disabled={profile.is_current}
              title={!profile.has_credentials ? 'No credentials — run midtown auth login' : ''}
              onclick={() => selectProfile(profile, $selectedAuthProvider)}
            >
              <span class="text-[0.5rem] shrink-0">{profile.is_current ? '\u25CF' : '\u25CB'}</span>
              <span class="flex-1 truncate">{profile.name}</span>
              {#if !profile.has_credentials}
                <span class="text-[0.6rem] px-1 py-[1px] bg-destructive text-destructive-foreground rounded shrink-0">no auth</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>
    {/if}
  </div>
{/if}
