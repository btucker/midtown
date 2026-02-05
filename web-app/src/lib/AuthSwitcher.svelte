<script>
  import { authProfiles, authSwitching } from './store.js'
  import { fetchAuthProfiles, switchAuthProfile } from './api.js'
  import { onMount } from 'svelte'

  let open = $state(false)

  onMount(() => {
    fetchAuthProfiles()
  })

  function toggle() {
    if (!$authSwitching) {
      open = !open
    }
  }

  async function select(profile) {
    if (profile.is_current || $authSwitching) return
    open = false
    await switchAuthProfile(profile.name)
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

  const currentProfile = $derived($authProfiles.find((p) => p.is_current))
</script>

{#if $authProfiles.length > 0}
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
      <span class="profile-name">{currentProfile?.name || '...'}</span>
    </button>

    {#if open}
      <div class="auth-dropdown">
        {#each $authProfiles as profile}
          <button
            class="auth-option"
            class:current={profile.is_current}
            class:no-creds={!profile.has_credentials}
            onclick={() => select(profile)}
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

  .profile-name {
    max-width: 80px;
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
    min-width: 160px;
    z-index: 100;
    overflow: hidden;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
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
</style>
