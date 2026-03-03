<script>
  import { onMount } from 'svelte'
  import { SidebarProvider, Sidebar, SidebarContent, SidebarHeader, SidebarFooter, SidebarTrigger } from '$lib/components/ui/sidebar'
  import Channel from '$lib/Channel.svelte'
  import ChannelPrList from '$lib/ChannelPrList.svelte'
  import ChannelNotes from '$lib/ChannelNotes.svelte'
  import ChannelList from '$lib/ChannelList.svelte'
  import ChannelHeader from '$lib/ChannelHeader.svelte'
  import ThreadPanel from '$lib/ThreadPanel.svelte'
  import PendingQuestions from '$lib/PendingQuestions.svelte'
  import Status from '$lib/Status.svelte'
  import Tmux from '$lib/Tmux.svelte'
  import CoworkerStatus from '$lib/CoworkerStatus.svelte'
  import AccountPanel from '$lib/AccountPanel.svelte'
  import CelebrationEffects from '$lib/CelebrationEffects.svelte'
  import SwipeGestures from '$lib/SwipeGestures.svelte'
  import MiniRepoStatus from '$lib/MiniRepoStatus.svelte'
  import SearchPalette from '$lib/SearchPalette.svelte'
  import InstallBanner from '$lib/InstallBanner.svelte'
  import { messages, connected, coworkers, projects, activeProject, activeChannel, channels, activeChannelTab, threadData, isWideScreen, deepLinkMsgId } from '$lib/store.js'
  import { connectWebSocket, fetchHistory, fetchStatus, fetchProjects, switchProject, setupHistoryNavigation, replaceNavState, openThread } from '$lib/api.js'
  import { theme, toggleTheme } from '$lib/theme.js'
  import { Sun, Moon } from 'lucide-svelte'
  import SearchIcon from '@lucide/svelte/icons/search'
  import Bell from '@lucide/svelte/icons/bell'
  import BellOff from '@lucide/svelte/icons/bell-off'
  import { pushSupported, pushPermission, pushSubscribed, subscribePush, unsubscribePush, checkPushSubscription } from '$lib/push.js'

  $effect(() => {
    if ($theme === 'dark') {
      document.documentElement.classList.add('dark')
    } else {
      document.documentElement.classList.remove('dark')
    }
    const favicon = document.getElementById('favicon')
    if (favicon) favicon.href = $theme === 'dark' ? '/favicon-dark.png' : '/favicon-light.png'
  })

  let activeView = $state('board') // 'board' (channel list + chat) or 'status' or 'tmux'
  let projectDropdownOpen = $state(false)
  let searchOpen = $state(false)

  function handleKeydown(e) {
    if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
      e.preventDefault()
      searchOpen = !searchOpen
    }
  }

  // DM channel detection for tab bar filtering
  let activeChannelMeta = $derived($channels.find((ch) => ch.name === $activeChannel) ?? null)
  let isActiveDm = $derived(activeChannelMeta?.is_dm ?? $activeChannel.startsWith('dm-'))

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

  async function togglePush() {
    if ($pushSubscribed) {
      await unsubscribePush()
    } else {
      await subscribePush()
    }
  }

  onMount(async () => {
    // Initialize push notification support check
    checkPushSubscription()

    // Always in multi-project mode — served from shared gateway on port 47022
    const projectList = await fetchProjects()

    // Prefer the project named in the URL path (e.g. /my-project → 'my-project')
    const rawSegment = window.location.pathname.split('/').filter(Boolean)[0] ?? null
    const urlProjectName = rawSegment ? decodeURIComponent(rawSegment) : null
    let targetProject = null
    if (urlProjectName) {
      targetProject = projectList.find(p => p.name === urlProjectName && p.status === 'running' && p.webhook_port)
    }
    if (!targetProject) {
      targetProject = projectList.find(p => p.status === 'running' && p.webhook_port)
    }
    if (targetProject) {
      switchProject(targetProject.name, targetProject.webhook_port)

      // Deep-link: read channel/thread/msg from URL query params
      const params = new URLSearchParams(window.location.search)
      const urlChannel = params.get('channel')
      const urlThread = params.get('thread')
      const urlMsg = params.get('msg')

      // Deep-link: restore channel and/or thread from URL.
      // Use the URL channel if present, else default to the project's main channel.
      const deepLinkChannel = urlChannel || targetProject.name
      if (urlChannel) {
        activeChannel.set(urlChannel)
      }
      if (urlThread) {
        // If a specific message is targeted, store it so ThreadPanel can scroll to it
        if (urlMsg) {
          deepLinkMsgId.set(urlMsg)
        }
        openThread({ id: urlThread, from: '', content: '' }, deepLinkChannel, { pushState: false })
      }
      replaceNavState({ channel: deepLinkChannel, thread: urlThread || undefined, msg: urlMsg || undefined })
    }
    // Set up browser back/forward navigation
    const cleanupHistory = setupHistoryNavigation()

    // Refresh project list every 30s
    const projectInterval = setInterval(fetchProjects, 30000)
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
      cleanupHistory()
      clearInterval(projectInterval)
      window.removeEventListener('resize', updateViewportWidth)
      document.removeEventListener('visibilitychange', handleVisibilityChange)
    }
  })

  function selectProject(project) {
    if (project.status === 'running' && project.webhook_port) {
      switchProject(project.name, project.webhook_port)
      replaceNavState({ channel: project.name })
      projectDropdownOpen = false
    }
  }


</script>

<svelte:window onkeydown={handleKeydown} />

<svelte:head>
  <title>Midtown</title>
  <meta name="theme-color" content={$theme === 'dark' ? '#1A232D' : '#F0E8D6'} />
</svelte:head>

<div class="app-container flex h-dvh w-full overflow-hidden bg-background text-foreground">
  {#if $activeProject}
    <CelebrationEffects />
    <SearchPalette bind:open={searchOpen} />
    <SidebarProvider>
      <SwipeGestures />
      <Sidebar>
        <SidebarHeader class="p-3 pt-safe-offset-3">
          <div class="header-left">
            <img src={$theme === 'dark' ? '/midtown-dark-logo.svg' : '/midtown-light-logo.svg'} alt="Midtown" class="header-logo hidden md:block" />
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
                          <span class="active-check">✓</span>
                        {/if}
                      </button>
                    {/each}
                  </div>
                {/if}
              </div>
            {:else}
              <h1>Midtown</h1>
            {/if}
            <button
              class="theme-toggle"
              onclick={() => searchOpen = true}
              title="Search messages (⌘K)"
            >
              <SearchIcon size={16} />
            </button>
            <button
              class="theme-toggle"
              class:push-subscribed={$pushSubscribed}
              onclick={togglePush}
              disabled={!$pushSupported || $pushPermission === 'denied'}
              title={!$pushSupported
                ? 'Push notifications not supported in this browser'
                : $pushPermission === 'denied'
                  ? 'Notifications blocked in browser settings'
                  : $pushSubscribed
                    ? 'Disable push notifications'
                    : 'Enable push notifications'}
            >
              {#if $pushSubscribed}
                <Bell size={16} />
              {:else}
                <BellOff size={16} />
              {/if}
            </button>
            <button
              data-testid="theme-toggle"
              class="theme-toggle"
              onclick={toggleTheme}
              title={$theme === 'dark' ? 'Switch to light theme' : 'Switch to dark theme'}
            >
              {#if $theme === 'dark'}
                <Sun size={16} />
              {:else}
                <Moon size={16} />
              {/if}
            </button>
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

        <SidebarFooter class="p-2 pb-1">
          {#if activeView === 'board'}
            <CoworkerStatus />
            <AccountPanel />
          {/if}
        </SidebarFooter>
      </Sidebar>

      <main class="flex-1 flex flex-col h-full overflow-hidden">
        <!-- Mobile header with sidebar trigger -->
        <header class="mobile-header flex items-center px-2 pb-2 pt-safe-offset-2 border-b border-border bg-sidebar md:hidden">
          <SidebarTrigger />
          <span class="ml-2 text-sm text-muted-foreground">{$activeProject}</span>
          <div class="mobile-channel active-channel-display ml-4">
            {#if isActiveDm}
              <span class="channel-hash">@</span>{$activeChannel.slice(3)}
            {:else}
              <span class="channel-hash">#</span>{$activeChannel}
            {/if}
          </div>
          <MiniRepoStatus />
          <button
            class="mobile-search-btn ml-auto p-1.5 text-muted-foreground hover:text-foreground"
            onclick={togglePush}
            disabled={!$pushSupported || $pushPermission === 'denied'}
            title={!$pushSupported
              ? 'Push notifications not supported'
              : $pushPermission === 'denied'
                ? 'Notifications blocked'
                : $pushSubscribed
                  ? 'Disable push notifications'
                  : 'Enable push notifications'}
          >
            {#if $pushSubscribed}
              <Bell size={16} />
            {:else}
              <BellOff size={16} />
            {/if}
          </button>
          <button
            class="mobile-search-btn p-1.5 text-muted-foreground hover:text-foreground"
            onclick={() => searchOpen = true}
            title="Search messages"
          >
            <SearchIcon size={16} />
          </button>
        </header>

        <InstallBanner />

        {#if activeView === 'board'}
          <div class="board-content flex flex-1 min-h-0 overflow-hidden" class:thread-open-mobile={!!$threadData}>
            <div class="channel-main">
              <ChannelHeader />
              {#if !isActiveDm}
                <div class="channel-tab-bar">
                  {#each [['messages', 'Messages'], ['prs', 'PRs'], ['notes', 'Notes']] as [tab, label]}
                    {@const isActive = ($activeChannelTab[$activeChannel] || 'messages') === tab}
                    <button
                      class="channel-tab"
                      class:active={isActive}
                      onclick={() => activeChannelTab.update((t) => ({ ...t, [$activeChannel]: tab }))}
                    >{label}</button>
                  {/each}
                </div>
              {/if}
              {#if isActiveDm || ($activeChannelTab[$activeChannel] || 'messages') === 'messages'}
                <PendingQuestions />
                <Channel />
              {:else if ($activeChannelTab[$activeChannel] || 'messages') === 'prs'}
                <ChannelPrList />
              {:else if ($activeChannelTab[$activeChannel] || 'messages') === 'notes'}
                <ChannelNotes />
              {/if}
            </div>

            <!-- Right panel: thread panel -->
            {#if $threadData}
              <ThreadPanel />
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

  h1 {
    font-size: 1.3rem;
    font-weight: 700;
    color: hsl(var(--primary));
    letter-spacing: -0.02em;
  }

  /* Connection indicator */

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

  .theme-toggle {
    background: none;
    border: none;
    color: hsl(var(--muted-foreground));
    cursor: pointer;
    padding: 5px;
    display: flex;
    align-items: center;
    border-radius: 4px;
    transition: color 0.2s, background 0.2s;
  }

  .theme-toggle:hover:not(:disabled) {
    color: hsl(var(--foreground));
    background: hsl(var(--accent));
  }

  .theme-toggle:disabled {
    opacity: 0.3;
    cursor: not-allowed;
  }

  .theme-toggle.push-subscribed {
    color: hsl(var(--primary));
  }

  /* Sidebar content */
  .sidebar-scroll {
    flex: 1;
    overflow-y: auto;
  }

  /* Unified channel tab bar */
  .channel-tab-bar {
    display: flex;
    border-bottom: 1px solid hsl(var(--border));
    background: hsl(var(--card));
    flex-shrink: 0;
  }

  .channel-tab {
    padding: 6px 16px;
    font-size: 0.8rem;
    font-weight: 500;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: hsl(var(--muted-foreground));
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
    margin-bottom: -1px;
  }

  /* Mobile: equal-width tabs for full-width touch targets */
  @media (max-width: 767px) {
    .channel-tab {
      flex: 1;
    }
  }

  .channel-tab:hover {
    color: hsl(var(--foreground));
  }

  .channel-tab.active {
    color: hsl(var(--foreground));
    border-bottom-color: hsl(var(--primary));
  }

  /* Channel main area */
  .board-content {
    position: relative;
  }

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

  @media (max-width: 1023px) {
    .board-content .channel-main {
      width: 100%;
      flex: 0 0 100%;
      transition: transform 0.24s ease;
      will-change: transform;
      transform: translateX(0);
    }

    .board-content.thread-open-mobile .channel-main {
      transform: translateX(-100%);
    }
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
