<script>
  import { onMount } from 'svelte'
  import Channel from './lib/Channel.svelte'
  import Status from './lib/Status.svelte'
  import { messages, connected, coworkers } from './lib/store.js'
  import { connectWebSocket, fetchHistory, fetchStatus } from './lib/api.js'

  let activeTab = $state('channel')

  onMount(async () => {
    // Load initial data
    await Promise.all([fetchHistory(), fetchStatus()])
    // Connect WebSocket for live updates
    connectWebSocket()
  })
</script>

<main>
  <header>
    <h1>Midtown</h1>
    <span class="connection-status" class:connected={$connected}>
      {$connected ? 'Connected' : 'Disconnected'}
    </span>
  </header>

  <nav>
    <button
      class:active={activeTab === 'channel'}
      onclick={() => (activeTab = 'channel')}
    >
      Channel
    </button>
    <button
      class:active={activeTab === 'status'}
      onclick={() => (activeTab = 'status')}
    >
      Status
    </button>
  </nav>

  <div class="content">
    {#if activeTab === 'channel'}
      <Channel />
    {:else if activeTab === 'status'}
      <Status />
    {/if}
  </div>
</main>

<style>
  :global(*) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(body) {
    font-family: system-ui, -apple-system, sans-serif;
    background: #1a1a2e;
    color: #eee;
    min-height: 100vh;
    overscroll-behavior: none;
  }

  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    height: 100dvh;
    max-width: 600px;
    margin: 0 auto;
  }

  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 12px 16px;
    background: #16213e;
    border-bottom: 1px solid #0f3460;
  }

  h1 {
    font-size: 1.25rem;
    color: #00d9ff;
  }

  .connection-status {
    font-size: 0.75rem;
    padding: 4px 8px;
    border-radius: 12px;
    background: #e94560;
  }

  .connection-status.connected {
    background: #4ade80;
    color: #1a1a2e;
  }

  nav {
    display: flex;
    background: #16213e;
    border-bottom: 1px solid #0f3460;
  }

  nav button {
    flex: 1;
    padding: 12px;
    border: none;
    background: transparent;
    color: #888;
    font-size: 0.875rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  nav button.active {
    color: #00d9ff;
    border-bottom: 2px solid #00d9ff;
  }

  nav button:hover:not(.active) {
    color: #ccc;
  }

  .content {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>
