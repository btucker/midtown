<script>
  import { authProfilesByProvider, selectedAuthProvider, authSwitching } from './store.js'
  import { fetchAllAuthProfiles, switchAuthProfile } from './api.js'
  import { onMount } from 'svelte'

  let open = $state(false)
  let error = $state(null)
  let fetchError = $state(null)

  onMount(async () => {
    const result = await fetchAllAuthProfiles()
    if (result && !result.ok) {
      fetchError = result.error
    }
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
    }
    // Note: switchAuthProfile already calls fetchAllAuthProfiles() on success
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

{#if fetchError}
  <div class="auth-switcher">
    <div class="auth-error fetch-error">
      <span class="error-icon">⚠️</span>
      <span class="error-message">{fetchError}</span>
    </div>
  </div>
{:else if availableProviders.length > 0}
  <div class="auth-switcher">
    <button
      class="auth-trigger"
      class:switching={$authSwitching}
      onclick={toggle}
      disabled={$authSwitching}
      title={$authSwitching ? 'Switching profile...' : 'Switch auth profile'}
    >
      {#if $authSwitching}
        <span class="spinner"></span>
      {:else}
        <span class="profile-icon"></span>
      {/if}
      <span class="profile-provider">{providerNames[currentProfile?.provider] || '...'}</span>
      <span class="profile-separator">/</span>
      <span class="profile-name">{currentProfile?.name || '...'}</span>
    </button>

    {#if error}
      <div class="auth-error">{error}</div>
    {/if}

    {#if open}
      <div class="auth-dropdown">
        {#if availableProviders.length > 1}
          <div class="provider-tabs">
            {#each availableProviders as provider}
              <button
                class="provider-tab"
                class:active={provider === $selectedAuthProvider}
                onclick={() => selectProvider(provider)}
              >
                {providerNames[provider]}
              </button>
            {/each}
          </div>
        {/if}

        <div class="profile-list">
          {#each currentProviderProfiles as profile}
            <button
              class="auth-option"
              class:current={profile.is_current}
              class:no-creds={!profile.has_credentials}
              onclick={() => selectProfile(profile, $selectedAuthProvider)}
              disabled={profile.is_current}
              title={!profile.has_credentials ? 'No credentials — run midtown auth login' : ''}
            >
              <span class="option-indicator">{profile.is_current ? '\u25CF' : '\u25CB'}</span>
              <span class="option-name">{profile.name}</span>
              {#if !profile.has_credentials}
                <span class="option-badge">no auth</span>
              {/if}
            </button>
          {/each}
        </div>
      </div>
    {/if}
  </div>
{/if}

<style>
  .auth-switcher {
    position: relative;
  }

  .auth-trigger {
    display: flex;
    align-items: center;
    gap: 4px;
    background: #303030;
    border: 1px solid #3a3a3a;
    border-radius: 6px;
    padding: 4px 8px;
    color: #a8a8a8;
    font-size: 0.75rem;
    cursor: pointer;
    transition: all 0.15s;
  }

  .auth-trigger:hover:not(:disabled) {
    background: #3a3a3a;
    color: #d0d0d0;
    border-color: #5fafaf;
  }

  .auth-trigger.switching {
    opacity: 0.6;
    cursor: wait;
  }

  .profile-icon::before {
    content: '\1F511';
    font-size: 0.7rem;
  }

  .profile-provider {
    color: #808080;
    flex-shrink: 0;
  }

  .profile-separator {
    color: #585858;
    margin: 0 2px;
    flex-shrink: 0;
  }

  .profile-name {
    max-width: 60px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .spinner {
    display: inline-block;
    width: 10px;
    height: 10px;
    border: 1.5px solid #585858;
    border-top-color: #5fafaf;
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .auth-dropdown {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 4px;
    background: #262626;
    border: 1px solid #3a3a3a;
    border-radius: 6px;
    min-width: 180px;
    z-index: 100;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  }

  .provider-tabs {
    display: flex;
    border-bottom: 1px solid #3a3a3a;
    padding: 4px 4px 0;
  }

  .provider-tab {
    flex: 1;
    padding: 4px 8px;
    border: none;
    background: transparent;
    color: #808080;
    font-size: 0.7rem;
    cursor: pointer;
    border-bottom: 2px solid transparent;
    transition: all 0.15s;
  }

  .provider-tab:hover {
    color: #d0d0d0;
  }

  .provider-tab.active {
    color: #5fafaf;
    border-bottom-color: #5fafaf;
  }

  .profile-list {
    overflow: hidden;
  }

  .auth-option {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 8px 10px;
    border: none;
    background: transparent;
    color: #a8a8a8;
    font-size: 0.75rem;
    cursor: pointer;
    text-align: left;
    transition: background 0.1s;
  }

  .auth-option:hover:not(:disabled) {
    background: #303030;
    color: #d0d0d0;
  }

  .auth-option.current {
    color: #5fafaf;
    cursor: default;
  }

  .auth-option.no-creds {
    opacity: 0.5;
  }

  .option-indicator {
    font-size: 0.5rem;
    flex-shrink: 0;
  }

  .option-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .option-badge {
    font-size: 0.6rem;
    padding: 1px 4px;
    background: #af5f5f;
    color: #1c1c1c;
    border-radius: 3px;
    flex-shrink: 0;
  }

  .auth-error {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 4px;
    padding: 4px 8px;
    background: #3a2020;
    border: 1px solid #af5f5f;
    border-radius: 4px;
    color: #e08080;
    font-size: 0.7rem;
    white-space: nowrap;
    z-index: 100;
  }

  .fetch-error {
    position: static;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    white-space: normal;
    max-width: 100%;
  }

  .error-icon {
    flex-shrink: 0;
  }

  .error-message {
    flex: 1;
    font-size: 0.75rem;
    line-height: 1.3;
  }
</style>
