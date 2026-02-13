<script>
  import { onMount } from 'svelte'
  import { SidebarProvider, Sidebar, SidebarContent, SidebarHeader, SidebarFooter, SidebarTrigger } from '$lib/components/ui/sidebar'
  import Channel from '$lib/Channel.svelte'
  import ChannelList from '$lib/ChannelList.svelte'
  import ChannelHeader from '$lib/ChannelHeader.svelte'
  import DetailPanel from '$lib/DetailPanel.svelte'
  import Status from '$lib/Status.svelte'
  import Tmux from '$lib/Tmux.svelte'
  import CoworkerStatus from '$lib/CoworkerStatus.svelte'
  import UsageBars from '$lib/UsageBars.svelte'
  import AuthSwitcher from '$lib/AuthSwitcher.svelte'
  import { messages, connected, coworkers, projects, activeProject, activeChannel, detailPanelData, isWideScreen } from '$lib/store.js'
  import { connectWebSocket, fetchHistory, fetchStatus, fetchProjects, switchProject } from '$lib/api.js'
  import {
    pushSupported,
    pushPermission,
    pushSubscribed,
    subscribePush,
    unsubscribePush,
    checkPushSubscription,
  } from '$lib/push.js'

  let activeView = $state('board') // 'board' (channel list + chat) or 'status' or 'tmux'
  let projectDropdownOpen = $state(false)

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

    // Initialize and listen for viewport width changes
    function updateViewportWidth() {
      isWideScreen.set(window.innerWidth > 1024)
    }

    // Set initial value
    updateViewportWidth()

    // Add resize listener
    window.addEventListener('resize', updateViewportWidth)

    // Reload history when page becomes visible again (handles PWA resume from background)
    function handleVisibilityChange() {
      if (!document.hidden && $activeProject) {
        // Page became visible and we have an active project - refresh history
        fetchHistory()
      }
    }

    document.addEventListener('visibilitychange', handleVisibilityChange)

    return () => {
      clearInterval(projectInterval)
      window.removeEventListener('resize', updateViewportWidth)
      document.removeEventListener('visibilitychange', handleVisibilityChange)
    }
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

  function closeDetailPanel() {
    detailPanelData.set(null)
  }
</script>

<svelte:head>
  <title>Midtown</title>
</svelte:head>

<div class="app-container flex h-screen w-screen overflow-hidden bg-slate-900 text-slate-200">
  {#if $activeProject}
    <SidebarProvider>
      <Sidebar class="bg-slate-950 border-r-2 border-slate-800">
        <SidebarHeader class="p-3 border-b border-slate-800">
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
        </SidebarHeader>

        <SidebarContent>
          {#if activeView === 'board'}
            <div class="sidebar-scroll">
              <ChannelList />
            </div>
          {:else if activeView === 'status'}
            <Status />
          {:else if activeView === 'tmux'}
            <Tmux />
          {/if}
        </SidebarContent>

        <SidebarFooter class="border-t-2 border-slate-800 bg-slate-950 p-2 pb-[calc(0.5rem+env(safe-area-inset-bottom))]">
          {#if activeView === 'board'}
            <CoworkerStatus />
            <UsageBars />
          {/if}
        </SidebarFooter>
      </Sidebar>

      <main class="flex-1 flex flex-col h-full overflow-hidden">
        <!-- Mobile header with sidebar trigger -->
        <header class="flex items-center p-2 border-b border-slate-800 bg-slate-950 md:hidden">
          <SidebarTrigger />
          <span class="ml-2 text-sm text-slate-400">{$activeProject}</span>
          <div class="active-channel-display ml-4">
            <span class="channel-hash">#</span>{$activeChannel}
          </div>
        </header>

        {#if activeView === 'board'}
          <div class="channel-main">
            <ChannelHeader />
            <Channel />
          </div>

          <!-- Detail panel (desktop only, shown on wide screens) -->
          {#if $detailPanelData}
            <DetailPanel panelData={$detailPanelData} onClose={closeDetailPanel} />
          {/if}
        {:else if activeView === 'status'}
          <!-- Status view shown in sidebar -->
        {:else if activeView === 'tmux'}
          <!-- Tmux view shown in sidebar -->
        {/if}
      </main>
    </SidebarProvider>
  {:else}
    <div class="no-project">
      <p>No running projects found.</p>
      <p class="hint">Start a project with <code>midtown start</code></p>
    </div>
  {/if}
</div>

<style>
  :global(*) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(body) {
    font-family: 'SF Mono', 'Menlo', 'Consolas', 'Monaco', 'Courier New', monospace;
    background: #0a0a0a;
    color: #d0d0d0;
    min-height: 100vh;
    overscroll-behavior: none;
    -webkit-font-smoothing: antialiased;
    -moz-osx-font-smoothing: grayscale;
  }

  .app-container {
    /* Mobile: centered layout with max-width */
  }

  @media (max-width: 768px) {
    .app-container {
      max-width: 600px;
      margin: 0 auto;
    }
  }

  /* Header styles */
  .header-left {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .header-logo {
    width: 32px;
    height: 32px;
    border-radius: 6px;
  }

  .header-controls {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  h1 {
    font-size: 1.3rem;
    font-weight: 700;
    color: #5faf5f;
    letter-spacing: -0.02em;
  }

  /* Connection indicator */
  .connection-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: #af5f5f;
    flex-shrink: 0;
    box-shadow: 0 0 8px rgba(175, 95, 95, 0.4);
  }

  .connection-dot.connected {
    background: #5faf5f;
    box-shadow: 0 0 8px rgba(95, 175, 95, 0.5);
  }

  /* Project selector */
  .project-selector {
    position: relative;
  }

  .project-trigger {
    display: flex;
    align-items: center;
    gap: 7px;
    background: transparent;
    border: none;
    color: #5faf5f;
    font-size: 1.15rem;
    font-weight: 700;
    cursor: pointer;
    padding: 4px 6px;
    border-radius: 5px;
    transition: background 0.15s;
  }

  .project-trigger:hover {
    background: #252525;
  }

  .project-name {
    max-width: 170px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dropdown-arrow {
    font-size: 0.75rem;
    color: #606060;
  }

  .project-dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    margin-top: 6px;
    background: #1a1a1a;
    border: 2px solid #2a2a2a;
    border-radius: 6px;
    min-width: 190px;
    z-index: 100;
    overflow: hidden;
    box-shadow: 0 6px 16px rgba(0, 0, 0, 0.6);
  }

  .project-option {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    padding: 10px 12px;
    border: none;
    background: transparent;
    color: #a0a0a0;
    font-size: 0.85rem;
    cursor: pointer;
    text-align: left;
    transition: background 0.1s;
  }

  .project-option:hover:not(:disabled) {
    background: #252525;
    color: #d0d0d0;
  }

  .project-option.active {
    color: #5faf5f;
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
    color: #5faf5f;
    font-size: 0.8rem;
  }

  .project-status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: #606060;
    flex-shrink: 0;
  }

  .project-status-dot.running {
    background: #5faf5f;
  }

  .push-toggle {
    background: none;
    border: none;
    font-size: 1.15rem;
    cursor: pointer;
    padding: 5px;
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

  /* Sidebar content */
  .sidebar-scroll {
    flex: 1;
    overflow-y: auto;
  }

  /* Channel main area */
  .channel-main {
    flex: 1;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .active-channel-display {
    font-size: 1.1rem;
    font-weight: 700;
    color: #d0d0d0;
    display: flex;
    align-items: baseline;
    gap: 2px;
  }

  .channel-hash {
    color: #606060;
  }

  /* No project message */
  .no-project {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: #606060;
  }

  .no-project .hint {
    font-size: 0.85rem;
  }

  .no-project code {
    background: #252525;
    padding: 3px 8px;
    border-radius: 4px;
    color: #a0a0a0;
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

  :global(.bp-prev),
  :global(.bp-next) {
    background: #2a2a2a !important;
    border: 1px solid #4a4a4a !important;
    color: #d0d0d0 !important;
    opacity: 1 !important;
  }

  :global(.bp-prev) {
    left: env(safe-area-inset-left, 0px) !important;
  }

  :global(.bp-next) {
    right: env(safe-area-inset-right, 0px) !important;
  }

  :global(.bp-prev:hover),
  :global(.bp-next:hover) {
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
