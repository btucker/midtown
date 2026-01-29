<script>
  import { onMount } from 'svelte'
  import Channel from './lib/Channel.svelte'
  import Status from './lib/Status.svelte'
  import Tmux from './lib/Tmux.svelte'
  import Kanban from './lib/Kanban.svelte'
  import { messages, connected, coworkers } from './lib/store.js'
  import { connectWebSocket, fetchHistory, fetchStatus } from './lib/api.js'

  let activeTab = $state('channel')

  onMount(async () => {
    // Load initial data
    await Promise.all([fetchHistory(), fetchStatus()])
    // Connect WebSocket for live updates
    connectWebSocket()
    // Refresh status every 30s for kanban updates
    const interval = setInterval(fetchStatus, 30000)
    return () => clearInterval(interval)
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
    <button
      class:active={activeTab === 'tmux'}
      onclick={() => (activeTab = 'tmux')}
    >
      Tmux
    </button>
  </nav>

  <Kanban />

  <div class="content">
    {#if activeTab === 'channel'}
      <Channel />
    {:else if activeTab === 'status'}
      <Status />
    {:else if activeTab === 'tmux'}
      <Tmux />
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
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
    background: #1c1c1c;
    color: #d0d0d0;
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
    padding-top: calc(env(safe-area-inset-top) + 12px);
    background: #262626;
    border-bottom: 1px solid #3a3a3a;
  }

  h1 {
    font-size: 1.25rem;
    color: #5fafaf;
  }

  .connection-status {
    font-size: 0.75rem;
    padding: 4px 8px;
    border-radius: 12px;
    background: #af5f5f;
    color: #1c1c1c;
  }

  .connection-status.connected {
    background: #5faf5f;
    color: #1c1c1c;
  }

  nav {
    display: flex;
    background: #262626;
    border-bottom: 1px solid #3a3a3a;
  }

  nav button {
    flex: 1;
    padding: 12px;
    border: none;
    background: transparent;
    color: #585858;
    font-size: 0.875rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  nav button.active {
    color: #5fafaf;
    border-bottom: 2px solid #5fafaf;
  }

  nav button:hover:not(.active) {
    color: #a8a8a8;
  }

  .content {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }
</style>
