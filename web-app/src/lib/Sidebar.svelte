<script>
  import ChannelList from './ChannelList.svelte'
  import CoworkerStatus from './CoworkerStatus.svelte'
  import UsageBars from './UsageBars.svelte'
  import AuthSwitcher from './AuthSwitcher.svelte'
  import { connected } from './store.js'
  import {
    pushSupported,
    pushPermission,
    pushSubscribed,
    subscribePush,
    unsubscribePush,
  } from './push.js'

  let channelsExpanded = $state(true)

  function toggleChannels() {
    channelsExpanded = !channelsExpanded
  }

  async function togglePush() {
    if ($pushSubscribed) {
      await unsubscribePush()
    } else {
      await subscribePush()
    }
  }
</script>

<div class="sidebar">
  <!-- Channels section -->
  <div class="sidebar-section channels-section">
    <button class="section-header" onclick={toggleChannels}>
      <span class="section-title">CHANNELS</span>
      <span class="expand-icon">{channelsExpanded ? '\u25BC' : '\u25B6'}</span>
    </button>
    {#if channelsExpanded}
      <div class="section-content">
        <ChannelList />
      </div>
    {/if}
  </div>

  <!-- Footer section with coworker status, usage bars, and controls -->
  <div class="sidebar-footer">
    <CoworkerStatus />
    <UsageBars />
    <div class="footer-controls">
      <AuthSwitcher />
      {#if $pushSupported}
        <button
          class="push-toggle"
          class:subscribed={$pushSubscribed}
          class:denied={$pushPermission === 'denied'}
          onclick={togglePush}
          disabled={$pushPermission === 'denied'}
          title={$pushPermission === 'denied'
            ? 'Notifications blocked'
            : $pushSubscribed
              ? 'Disable notifications'
              : 'Enable notifications'}
        >
          {$pushSubscribed ? '\u{1F514}' : '\u{1F515}'}
        </button>
      {/if}
      <span
        class="connection-dot"
        class:connected={$connected}
        title={$connected ? 'Connected' : 'Disconnected'}
      ></span>
    </div>
  </div>
</div>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    height: 100%;
    background: #0f0f0f;
    border-right: 2px solid #2a2a2a;
  }

  .sidebar-section {
    border-bottom: 1px solid #1a1a1a;
  }

  /* Section headers */
  .section-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    width: 100%;
    padding: 10px 14px;
    background: transparent;
    border: none;
    color: #606060;
    font-size: 0.7rem;
    font-weight: 700;
    letter-spacing: 0.05em;
    cursor: pointer;
    transition: all 0.15s;
  }

  .section-header:hover {
    background: #1a1a1a;
    color: #a0a0a0;
  }

  .section-title {
    flex: 1;
    text-align: left;
  }

  .coworker-count {
    font-size: 0.65rem;
    opacity: 0.7;
    margin-left: 4px;
  }

  .expand-icon {
    font-size: 0.6rem;
    opacity: 0.6;
  }

  .section-content {
    padding: 0;
  }

  /* Footer */
  .sidebar-footer {
    margin-top: auto;
    border-top: 2px solid #2a2a2a;
    background: #0a0a0a;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px;
    padding-bottom: calc(8px + env(safe-area-inset-bottom, 0px));
  }

  .footer-controls {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 10px;
  }

  .push-toggle {
    background: none;
    border: none;
    font-size: 1rem;
    cursor: pointer;
    padding: 4px;
    opacity: 0.5;
    transition: opacity 0.2s;
  }

  .push-toggle.subscribed {
    opacity: 1;
  }

  .push-toggle.denied {
    opacity: 0.25;
    cursor: not-allowed;
  }

  .push-toggle:hover:not(.denied) {
    opacity: 1;
  }

  .connection-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #af5f5f;
    flex-shrink: 0;
    box-shadow: 0 0 6px rgba(175, 95, 95, 0.4);
  }

  .connection-dot.connected {
    background: #5faf5f;
    box-shadow: 0 0 6px rgba(95, 175, 95, 0.5);
  }
</style>
