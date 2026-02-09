<script>
  import { onMount } from 'svelte'
  import Channel from './lib/Channel.svelte'
  import ChannelList from './lib/ChannelList.svelte'
  import Status from './lib/Status.svelte'
  import Tmux from './lib/Tmux.svelte'
  import Kanban from './lib/Kanban.svelte'
  import UsageBars from './lib/UsageBars.svelte'
  import AuthSwitcher from './lib/AuthSwitcher.svelte'
  import { messages, connected, coworkers, projects, activeProject } from './lib/store.js'
  import { connectWebSocket, fetchHistory, fetchStatus, fetchProjects, switchProject } from './lib/api.js'
  import {
    pushSupported,
    pushPermission,
    pushSubscribed,
    subscribePush,
    unsubscribePush,
    checkPushSubscription,
  } from './lib/push.js'

  let activeView = $state('board') // 'board' (channel list + chat) or 'status' or 'tmux'
  let sidebarOpen = $state(false)
  let projectDropdownOpen = $state(false)

  function toggleSidebar() {
    sidebarOpen = !sidebarOpen
  }

  function closeSidebar() {
    sidebarOpen = false
  }

  function toggleProjectDropdown() {
    projectDropdownOpen = !projectDropdownOpen
  }

  function handleProjectClickOutside(event) {
    if (projectDropdownOpen && !event.target.closest('.project-selector')) {
      projectDropdownOpen = false
    }
  }

  $effect(() => {
    if (projectDropdownOpen) {
      document.addEventListener('click', handleProjectClickOutside, true)
      return () => document.removeEventListener('click', handleProjectClickOutside, true)
    }
  })

  onMount(async () => {
    // Always in multi-project mode — served from shared gateway on port 47022
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
  })

  function selectProject(project) {
    if (project.status === 'running' && project.webhook_port) {
      switchProject(project.name, project.webhook_port)
      projectDropdownOpen = false
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
    <div class="header-left">
      <img src="/logo.png" alt="Midtown" class="header-logo" />
      {#if $projects.length > 0}
        <div class="project-selector">
          <button class="project-trigger" onclick={toggleProjectDropdown}>
            <span class="project-status-dot" class:running={$projects.find(p => p.name === $activeProject)?.status === 'running'}></span>
            <span class="project-name">{$activeProject || 'Select project'}</span>
            <span class="dropdown-arrow">{projectDropdownOpen ? '\u25B4' : '\u25BE'}</span>
          </button>
          {#if projectDropdownOpen}
            <div class="project-dropdown">
              {#each $projects as project}
                <button
                  class="project-option"
                  class:active={$activeProject === project.name}
                  class:stopped={project.status !== 'running'}
                  onclick={() => selectProject(project)}
                  disabled={project.status !== 'running'}
                  title={project.status === 'running' ? `Port ${project.webhook_port || 'N/A'}` : 'Stopped'}
                >
                  <span class="project-status-dot" class:running={project.status === 'running'}></span>
                  <span class="option-name">{project.name}</span>
                  {#if $activeProject === project.name}
                    <span class="active-check">\u2713</span>
                  {/if}
                </button>
              {/each}
            </div>
          {/if}
        </div>
      {:else}
        <h1>Midtown</h1>
      {/if}
    </div>
    <div class="header-controls">
      <AuthSwitcher />
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
      <span
        class="connection-dot"
        class:connected={$connected}
        title={$connected ? 'Connected' : 'Disconnected'}
      ></span>
    </div>
  </header>

  {#if $activeProject}
    <nav>
      <button
        class:active={activeView === 'board'}
        onclick={() => (activeView = 'board')}
      >
        Board
      </button>
      <button
        class:active={activeView === 'status'}
        onclick={() => (activeView = 'status')}
      >
        Status
      </button>
      <button
        class:active={activeView === 'tmux'}
        onclick={() => (activeView = 'tmux')}
      >
        tmux
      </button>
    </nav>

    <div class="content">
      {#if activeView === 'board'}
        <div class="split-panel">
          <button class="mobile-menu-toggle" onclick={toggleSidebar} aria-label="Toggle menu">
            ☰
          </button>
          <aside class="board-sidebar" class:open={sidebarOpen}>
            <button class="mobile-close" onclick={closeSidebar} aria-label="Close menu">
              ✕
            </button>
            <div class="sidebar-scroll">
              <Kanban />
              <ChannelList />
            </div>
            <UsageBars />
          </aside>
          {#if sidebarOpen}
            <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
            <div class="sidebar-overlay" onclick={closeSidebar} role="button" tabindex="0"></div>
          {/if}
          <main class="channel-main">
            <Channel />
          </main>
        </div>
      {:else if activeView === 'status'}
        <Status />
      {:else if activeView === 'tmux'}
        <Tmux />
      {/if}
    </div>
  {:else}
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

  .header-left {
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

  /* Connection indicator: compact colored dot */
  .connection-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: #af5f5f;
    flex-shrink: 0;
  }

  .connection-dot.connected {
    background: #5faf5f;
  }

  /* Project selector dropdown */
  .project-selector {
    position: relative;
  }

  .project-trigger {
    display: flex;
    align-items: center;
    gap: 6px;
    background: transparent;
    border: none;
    color: #5fafaf;
    font-size: 1.1rem;
    font-weight: 600;
    cursor: pointer;
    padding: 2px 4px;
    border-radius: 4px;
    transition: background 0.15s;
  }

  .project-trigger:hover {
    background: #303030;
  }

  .project-name {
    max-width: 160px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dropdown-arrow {
    font-size: 0.7rem;
    color: #585858;
  }

  .project-dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    margin-top: 4px;
    background: #262626;
    border: 1px solid #3a3a3a;
    border-radius: 6px;
    min-width: 180px;
    z-index: 100;
    overflow: hidden;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.4);
  }

  .project-option {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 8px 10px;
    border: none;
    background: transparent;
    color: #a8a8a8;
    font-size: 0.8rem;
    cursor: pointer;
    text-align: left;
    transition: background 0.1s;
  }

  .project-option:hover:not(:disabled) {
    background: #303030;
    color: #d0d0d0;
  }

  .project-option.active {
    color: #5fafaf;
  }

  .project-option.stopped {
    opacity: 0.5;
    cursor: default;
  }

  .project-option .option-name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .active-check {
    color: #5fafaf;
    font-size: 0.75rem;
  }

  .project-status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: #585858;
    flex-shrink: 0;
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

  .split-panel {
    display: flex;
    flex: 1;
    overflow: hidden;
    position: relative;
  }

  .mobile-menu-toggle {
    display: none;
    position: absolute;
    top: 12px;
    left: 12px;
    z-index: 50;
    width: 40px;
    height: 40px;
    border: 1px solid #3a3a3a;
    border-radius: 8px;
    background: #262626;
    color: #d0d0d0;
    font-size: 1.25rem;
    cursor: pointer;
    transition: all 0.2s;
  }

  .mobile-menu-toggle:hover {
    background: #303030;
    border-color: #5fafaf;
  }

  .mobile-close {
    display: none;
  }

  .sidebar-overlay {
    display: none;
  }

  .board-sidebar {
    width: 280px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: #1c1c1c;
    border-right: 1px solid #3a3a3a;
  }

  .sidebar-scroll {
    flex: 1;
    overflow-y: auto;
  }

  .channel-main {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  /* Responsive: collapse sidebar on narrow screens */
  @media (max-width: 768px) {
    .mobile-menu-toggle {
      display: block;
    }

    .mobile-close {
      display: block;
      position: absolute;
      top: calc(env(safe-area-inset-top) + 12px);
      right: 12px;
      width: 32px;
      height: 32px;
      border: none;
      background: transparent;
      color: #d0d0d0;
      font-size: 1.5rem;
      cursor: pointer;
      z-index: 101;
    }

    .board-sidebar {
      position: fixed;
      left: 0;
      top: 0;
      bottom: 0;
      padding-top: env(safe-area-inset-top);
      padding-bottom: env(safe-area-inset-bottom);
      z-index: 100;
      transform: translateX(-100%);
      transition: transform 0.3s ease;
      box-shadow: 2px 0 8px rgba(0, 0, 0, 0.5);
    }

    .board-sidebar.open {
      transform: translateX(0);
    }

    .sidebar-overlay {
      display: block;
      position: fixed;
      top: 0;
      left: 0;
      right: 0;
      bottom: 0;
      background: rgba(0, 0, 0, 0.5);
      z-index: 99;
    }

    .channel-main {
      width: 100%;
    }
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

  /* Bigger Picture lightbox styles - dark theme */
  :global(.bp-wrap) {
    background: rgba(0, 0, 0, 0.9) !important;
  }

  :global(.bp-x) {
    background: #2a2a2a !important;
    border: 1px solid #4a4a4a !important;
    color: #d0d0d0 !important;
    width: 40px !important;
    height: 40px !important;
    border-radius: 50% !important;
    opacity: 1 !important;
    top: calc(env(safe-area-inset-top, 0px) + 12px) !important;
    right: calc(env(safe-area-inset-right, 0px) + 12px) !important;
  }

  :global(.bp-x:hover) {
    background: #3a2020 !important;
    border-color: #ff5f5f !important;
    color: #ff5f5f !important;
  }

  :global(.bp-x::before),
  :global(.bp-x::after) {
    background: currentColor !important;
  }

  :global(.bp-lr) {
    background: #2a2a2a !important;
    border: 1px solid #4a4a4a !important;
    color: #d0d0d0 !important;
    opacity: 1 !important;
  }

  :global(.bp-lr:hover) {
    background: #303030 !important;
    border-color: #5fafaf !important;
    color: #5fafaf !important;
  }

  :global(.bp-img) {
    background: transparent !important;
  }

  /* Ensure lightbox is above other z-indexed elements */
  :global(.bp-wrap) {
    z-index: 2000 !important;
  }
</style>
