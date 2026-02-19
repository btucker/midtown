<script>
  import { onMount } from 'svelte'
  import { SidebarProvider, Sidebar, SidebarContent, SidebarHeader, SidebarFooter, SidebarTrigger } from '$lib/components/ui/sidebar'
  import Channel from '$lib/Channel.svelte'
  import ChannelList from '$lib/ChannelList.svelte'
  import ChannelHeader from '$lib/ChannelHeader.svelte'
  import DetailPanel from '$lib/DetailPanel.svelte'
  import ThreadPanel from '$lib/ThreadPanel.svelte'
  import PendingQuestions from '$lib/PendingQuestions.svelte'
  import Status from '$lib/Status.svelte'
  import Tmux from '$lib/Tmux.svelte'
  import CoworkerStatus from '$lib/CoworkerStatus.svelte'
  import UsageBars from '$lib/UsageBars.svelte'
  import AuthSwitcher from '$lib/AuthSwitcher.svelte'
  import { messages, connected, coworkers, projects, activeProject, activeChannel, detailPanelData, threadData, isWideScreen } from '$lib/store.js'
  import { connectWebSocket, fetchHistory, fetchStatus, fetchProjects, switchProject } from '$lib/api.js'
  import {
    pushSupported,
    pushPermission,
    pushSubscribed,
    pushError,
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

  // Auto-clear push errors after 5 seconds
  $effect(() => {
    if ($pushError) {
      const timeout = setTimeout(() => pushError.set(null), 5000)
      return () => clearTimeout(timeout)
    }
  })

  function closeDetailPanel() {
    detailPanelData.set(null)
  }
</script>

<svelte:head>
  <title>Midtown</title>
</svelte:head>

<div class="app-container flex h-dvh w-full overflow-hidden bg-background text-foreground">
  {#if $activeProject}
    <SidebarProvider>
      <Sidebar>
        <SidebarHeader class="p-3 pt-safe-offset-3 border-b border-sidebar-border">
          <div class="header-left">
            <img src="/logo.png" alt="Midtown" class="header-logo hidden md:block" />
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
              <div class="push-wrapper">
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
                {#if $pushError}
                  <div class="push-error">{$pushError}</div>
                {/if}
              </div>
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

        <SidebarFooter class="border-t border-sidebar-border p-2 pb-1">
          {#if activeView === 'board'}
            <CoworkerStatus />
            <UsageBars />
          {/if}
        </SidebarFooter>
      </Sidebar>

      <main class="flex-1 flex flex-col h-full overflow-hidden">
        <!-- Mobile header with sidebar trigger -->
        <header class="mobile-header flex items-center px-2 pb-2 pt-safe-offset-2 border-b border-border bg-sidebar md:hidden">
          <SidebarTrigger />
          <span class="ml-2 text-sm text-muted-foreground">{$activeProject}</span>
          <div class="mobile-channel active-channel-display ml-4">
            <span class="channel-hash">#</span>{$activeChannel}
          </div>
        </header>

        {#if activeView === 'board'}
          <div class="flex flex-1 min-h-0 overflow-hidden">
            <div class="channel-main">
              <ChannelHeader />
              <PendingQuestions />
              <Channel />
            </div>

            <!-- Right panel: thread OR detail panel (mutually exclusive) -->
            {#if $threadData}
              <ThreadPanel />
            {:else if $detailPanelData}
              <DetailPanel panelData={$detailPanelData} onClose={closeDetailPanel} />
            {/if}
          </div>
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
    color: hsl(var(--primary));
    letter-spacing: -0.02em;
  }

  /* Connection indicator */
  .connection-dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: hsl(var(--destructive));
    flex-shrink: 0;
    box-shadow: 0 0 8px hsl(var(--destructive) / 0.4);
  }

  .connection-dot.connected {
    background: hsl(var(--primary));
    box-shadow: 0 0 8px hsl(var(--primary) / 0.5);
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
    color: hsl(var(--primary));
    font-size: 1.15rem;
    font-weight: 700;
    cursor: pointer;
    padding: 4px 6px;
    border-radius: 5px;
    transition: background 0.15s;
  }

  .project-trigger:hover {
    background: hsl(var(--accent));
  }

  .project-name {
    max-width: 170px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .dropdown-arrow {
    font-size: 0.75rem;
    color: hsl(var(--muted-foreground));
  }

  .project-dropdown {
    position: absolute;
    top: 100%;
    left: 0;
    margin-top: 6px;
    background: hsl(var(--card));
    border: 2px solid hsl(var(--border));
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
    color: hsl(var(--foreground) / 0.7);
    font-size: 0.85rem;
    cursor: pointer;
    text-align: left;
    transition: background 0.1s;
  }

  .project-option:hover:not(:disabled) {
    background: hsl(var(--accent));
    color: hsl(var(--foreground));
  }

  .project-option.active {
    color: hsl(var(--primary));
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
    color: hsl(var(--primary));
    font-size: 0.8rem;
  }

  .project-status-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: hsl(var(--muted-foreground));
    flex-shrink: 0;
  }

  .project-status-dot.running {
    background: hsl(var(--primary));
  }

  .push-wrapper {
    position: relative;
  }

  .push-error {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 4px;
    padding: 4px 8px;
    background: hsl(var(--destructive));
    color: hsl(var(--destructive-foreground));
    font-size: 0.7rem;
    border-radius: 4px;
    white-space: nowrap;
    z-index: 100;
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
    min-height: 0;
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  .active-channel-display {
    font-size: 1.1rem;
    font-weight: 700;
    color: hsl(var(--foreground));
    display: flex;
    align-items: baseline;
    gap: 2px;
  }

  .channel-hash {
    color: hsl(var(--muted-foreground));
  }

  /* No project message */
  .no-project {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 10px;
    color: hsl(var(--muted-foreground));
  }

  .no-project .hint {
    font-size: 0.85rem;
  }

  .no-project code {
    background: hsl(var(--accent));
    padding: 3px 8px;
    border-radius: 4px;
    color: hsl(var(--foreground) / 0.7);
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
