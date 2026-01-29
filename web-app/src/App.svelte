<script>
  import { onMount } from 'svelte'
  import Channel from './lib/Channel.svelte'
  import Status from './lib/Status.svelte'
  import Tmux from './lib/Tmux.svelte'
  import Kanban from './lib/Kanban.svelte'
  import { messages, connected, coworkers, projects, activeProject, multiProjectMode } from './lib/store.js'
  import { connectWebSocket, fetchHistory, fetchStatus, detectMode, fetchProjects, switchProject } from './lib/api.js'
  import {
    pushSupported,
    pushPermission,
    pushSubscribed,
    subscribePush,
    unsubscribePush,
    checkPushSubscription,
  } from './lib/push.js'

  let activeTab = $state('channel')

  onMount(async () => {
    const isMultiProject = detectMode()

    if (isMultiProject) {
      // Multi-project mode: fetch project list, auto-select first running project
      const projectList = await fetchProjects()
      const running = projectList.find(p => p.status === 'running' && p.webhook_port)
      if (running) {
        switchProject(running.name, running.webhook_port)
      }
      // Refresh project list every 30s
      const projectInterval = setInterval(fetchProjects, 30000)
      // Check push notification status
      checkPushSubscription()
      return () => clearInterval(projectInterval)
    } else {
      // Single-project mode: connect directly as before
      await Promise.all([fetchHistory(), fetchStatus()])
      connectWebSocket()
      // Check push notification status
      checkPushSubscription()
      const interval = setInterval(fetchStatus, 30000)
      return () => clearInterval(interval)
    }
  })

  function selectProject(project) {
    if (project.status === 'running' && project.webhook_port) {
      switchProject(project.name, project.webhook_port)
    }
  }

  async function togglePush() {
    if ($pushSubscribed) {
      await unsubscribePush()
    } else {
      await subscribePush()
    }
  }
</script>

<main>
  <header>
    <div class="header-title">
      <img src="/logo.png" alt="Midtown" class="header-logo" />
      <h1>Midtown</h1>
    </div>
    <div class="header-controls">
      {#if $pushSupported}
        <button
          class="push-toggle"
          class:subscribed={$pushSubscribed}
          class:denied={$pushPermission === 'denied'}
          onclick={togglePush}
          disabled={$pushPermission === 'denied'}
          title={$pushPermission === 'denied'
            ? 'Notifications blocked in browser settings'
            : $pushSubscribed
              ? 'Disable push notifications'
              : 'Enable push notifications'}
        >
          {$pushSubscribed ? '\u{1F514}' : '\u{1F515}'}
        </button>
      {/if}
      <span class="connection-status" class:connected={$connected}>
        {$connected ? 'Connected' : 'Disconnected'}
      </span>
    </div>
  </header>

  {#if $multiProjectMode && $projects.length > 0}
    <div class="project-tabs">
      {#each $projects as project}
        <button
          class="project-tab"
          class:active={$activeProject === project.name}
          class:stopped={project.status !== 'running'}
          onclick={() => selectProject(project)}
          title={project.status === 'running' ? `Port ${project.webhook_port || 'N/A'}` : 'Stopped'}
        >
          <span class="project-status-dot" class:running={project.status === 'running'}></span>
          {project.name}
        </button>
      {/each}
    </div>
  {/if}

  {#if !$multiProjectMode || $activeProject}
    <Kanban />

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

    <div class="content">
      {#if activeTab === 'channel'}
        <Channel />
      {:else if activeTab === 'status'}
        <Status />
      {:else if activeTab === 'tmux'}
        <Tmux />
      {/if}
    </div>
  {:else if $multiProjectMode}
    <div class="no-project">
      <p>No running projects found.</p>
      <p class="hint">Start a project with <code>midtown start</code></p>
    </div>
  {/if}
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

  .header-title {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .header-logo {
    width: 28px;
    height: 28px;
    border-radius: 6px;
  }

  .header-controls {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .push-toggle {
    background: none;
    border: none;
    font-size: 1.1rem;
    cursor: pointer;
    padding: 4px;
    opacity: 0.6;
    transition: opacity 0.2s;
  }

  .push-toggle.subscribed {
    opacity: 1;
  }

  .push-toggle.denied {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .push-toggle:hover:not(.denied) {
    opacity: 1;
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

  .project-tabs {
    display: flex;
    gap: 2px;
    padding: 6px 8px;
    background: #1c1c1c;
    border-bottom: 1px solid #3a3a3a;
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
  }

  .project-tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 12px;
    border: 1px solid #3a3a3a;
    border-radius: 6px;
    background: #262626;
    color: #a8a8a8;
    font-size: 0.8rem;
    cursor: pointer;
    white-space: nowrap;
    transition: all 0.15s;
  }

  .project-tab:hover:not(.active) {
    background: #303030;
    color: #d0d0d0;
  }

  .project-tab.active {
    background: #303030;
    color: #5fafaf;
    border-color: #5fafaf;
  }

  .project-tab.stopped {
    opacity: 0.5;
    cursor: default;
  }

  .project-status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #585858;
  }

  .project-status-dot.running {
    background: #5faf5f;
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

  .no-project {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 8px;
    color: #585858;
  }

  .no-project .hint {
    font-size: 0.8rem;
  }

  .no-project code {
    background: #303030;
    padding: 2px 6px;
    border-radius: 3px;
    color: #a8a8a8;
  }
</style>
