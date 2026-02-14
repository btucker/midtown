import { get } from 'svelte/store'
import {
  messages,
  messagesByChannel,
  channels,
  activeChannel,
  connected,
  coworkers,
  maxCoworkers,
  leadTyping,
  daemonStatus,
  kanbanData,
  repoStatus,
  projects,
  activeProject,
  repoStatuses,
  authProfiles,
  authProfilesByProvider,
  selectedAuthProvider,
  authSwitching,
  usageData,
} from './store.js'

let ws = null
let reconnectTimeout = null
let statusPollInterval = null
let usagePollInterval = null
let leadTypingTimeout = null

// Base URL for the current project's daemon API.
// Always connects via the project's webhook port.
let projectApiBase = ''

const WEBSERVER_API = '/api'

// Fetch the list of projects from the shared webserver
export async function fetchProjects() {
  try {
    const res = await fetch(`${WEBSERVER_API}/projects`)
    if (res.ok) {
      const data = await res.json()
      projects.set(data)
      return data
    }
  } catch (err) {
    console.error('Failed to fetch projects:', err)
  }
  return []
}

// Fetch the list of available channels
export async function fetchChannels(includeArchived = false) {
  try {
    const url = includeArchived
      ? `${getApiBase()}/channels?include_archived=true`
      : `${getApiBase()}/channels`
    const res = await fetch(url)
    if (res.ok) {
      const data = await res.json()
      const channelList = data.channels.map((ch) => ({
        name: typeof ch === 'string' ? ch : ch.name,
        unread: 0,
        has_pr: false,
        ci_status: null,
        is_archived: typeof ch === 'object' && ch.is_archived,
      }))
      // Ensure midtown is first
      channelList.sort((a, b) => {
        if (a.name === 'midtown') return -1
        if (b.name === 'midtown') return 1
        return a.name.localeCompare(b.name)
      })
      channels.set(channelList)
      return channelList
    }
  } catch (err) {
    // Retain last-known-good channel list on transient network errors.
    // The channel list will refresh on WebSocket reconnect or next poll.
    console.warn('Failed to fetch channels (retaining cached data):', err)
  }
  return []
}

// Switch to a different project by name
export function switchProject(projectName, webhookPort) {
  // Disconnect existing WebSocket
  if (ws) {
    ws.close()
    ws = null
  }
  if (reconnectTimeout) {
    clearTimeout(reconnectTimeout)
    reconnectTimeout = null
  }
  if (statusPollInterval) {
    clearInterval(statusPollInterval)
    statusPollInterval = null
  }
  if (usagePollInterval) {
    clearInterval(usagePollInterval)
    usagePollInterval = null
  }

  // Clear current state
  messages.set([])
  messagesByChannel.set({ midtown: [] })
  channels.set([{ name: 'midtown', unread: 0, has_pr: false, ci_status: null }])
  activeChannel.set('midtown')
  coworkers.set([])
  leadTyping.set(false)
  daemonStatus.set(null)
  kanbanData.set({ backlog: [], inProgress: [], review: [], done: [] })
  repoStatus.set({
    repoName: '',
    fullName: '',
    commitHash: '',
    commitTime: null,
    ciStatus: null,
    releaseTag: null,
    releaseTime: null,
  })
  usageData.set([])
  connected.set(false)

  // Set the new active project
  activeProject.set(projectName)

  if (webhookPort) {
    // Connect to the project's daemon directly via its webhook port
    projectApiBase = `http://${window.location.hostname}:${webhookPort}`
  } else {
    // No webhook port - project daemon may not be running
    projectApiBase = ''
  }

  // Load data from the new project
  if (projectApiBase) {
    // Fetch channels first to populate the sidebar immediately.
    // Note: fetchHistory() also builds a channel list from messages,
    // but this ensures all channels (including empty ones) appear immediately.
    fetchChannels()
    fetchHistory()
    fetchStatus()
    fetchUsage()
    connectWebSocket()
    // Poll status every 30s (matching daemon's kanban cache TTL)
    statusPollInterval = setInterval(fetchStatus, 30000)
    // Poll usage every 2 minutes (matching TUI refresh interval)
    usagePollInterval = setInterval(fetchUsage, 120000)
  }
}

// Get the API base for the current project
export function getApiBase() {
  return projectApiBase ? `${projectApiBase}/api` : '/api'
}

// Fetch channel message history
// If channelName is provided, fetches only that channel's messages.
// Otherwise, fetches all messages from the main channel.
export async function fetchHistory(channelName = null) {
  try {
    const url = channelName
      ? `${getApiBase()}/channels/history?channel=${encodeURIComponent(channelName)}`
      : `${getApiBase()}/channels/history`
    const res = await fetch(url)
    if (res.ok) {
      const data = await res.json()

      if (channelName) {
        // Fetching a specific channel - update only that channel's messages
        messagesByChannel.update((byChannel) => ({
          ...byChannel,
          [channelName]: data,
        }))
      } else {
        // Fetching all messages (initial load) - group by channel
        messages.set(data)

        const byChannel = {}
        for (const msg of data) {
          const name = msg.channel || 'midtown'
          if (!byChannel[name]) {
            byChannel[name] = []
          }
          byChannel[name].push(msg)
        }

        messagesByChannel.set(byChannel)

        // Channels are already populated by fetchChannels() which calls the
        // backend's Channel::list(). We no longer derive channels from message
        // content to avoid showing ghost channels for invalid/deleted .jsonl files.
      }
    }
  } catch (err) {
    // Retain last-known-good data on transient network errors so the
    // channel view doesn't flash empty. Messages will refresh on the
    // next successful WebSocket reconnect or manual channel switch.
    console.warn('Failed to fetch history (retaining cached data):', err)
  }
}

// Fetch daemon/coworker status and update kanban data
export async function fetchStatus() {
  try {
    const res = await fetch(`${getApiBase()}/status`)
    if (res.ok) {
      const data = await res.json()
      daemonStatus.set(data)
      coworkers.set(data.coworkers || [])
      if (data.max_coworkers !== undefined) {
        maxCoworkers.set(data.max_coworkers)
      }
      updateKanbanData(data)
      updateRepoStatus(data)
    }
  } catch (err) {
    console.error('Failed to fetch status:', err)
  }
}

// Fetch API usage data (session + weekly utilization)
export async function fetchUsage() {
  try {
    const res = await fetch(`${getApiBase()}/usage`)
    if (res.status === 204) {
      // 204 No Content means no credentials available — clear the store
      // so the UI shows the loading/empty state instead of stale data.
      usageData.set([])
      return
    }
    if (res.ok) {
      const data = await res.json()
      // Extract usage array from response (backend provides both array and flat fields for backwards compat)
      usageData.set(data.usage || [])
    }
  } catch (err) {
    // Retain last-known-good data on transient network errors so the
    // UsageBars component doesn't disappear and reappear. Data will
    // refresh on the next successful 2-minute poll cycle.
    console.warn('Failed to fetch usage (retaining cached data):', err)
  }
}

function updateKanbanData(data) {
  const tasks = data.tasks || []
  const prs = data.pull_requests || []
  const mergedPrs = data.merged_prs || []

  // Build set of task IDs that have open PRs (normalized to strings for comparison)
  const tasksWithOpenPrs = new Set(
    prs.map((pr) => String(pr.task_id)).filter((id) => id !== 'null' && id !== 'undefined')
  )

  kanbanData.set({
    backlog: tasks.filter((t) => t.status === 'pending'),
    // Exclude tasks with open PRs - they belong in the Review column
    inProgress: tasks.filter((t) => t.status === 'in_progress' && !tasksWithOpenPrs.has(String(t.id))),
    review: prs.map((pr) => ({
      number: pr.number,
      title: pr.title,
      author: pr.author,
      status: pr.status,
      reviewer: pr.reviewer,
      reviewer_assigned_at: pr.reviewer_assigned_at,
      review_posted: pr.review_posted || false,
      created_at: pr.created_at,
      repo: pr.repo || null,
      task_id: pr.task_id,
      task_name: pr.task_name,
    })),
    done: mergedPrs.slice(0, 10).map((pr) => ({
      number: pr.number,
      title: pr.title,
      mergedAt: pr.mergedAt,
      repo: pr.repo || null,
    })),
  })
}

function updateRepoStatus(data) {
  const rs = data.repo_status || {}
  repoStatus.set({
    repoName: data.repo_name || '',
    fullName: data.repo_full_name || '',
    commitHash: rs.commit_hash || '',
    commitTime: rs.commit_time || null,
    ciStatus: rs.ci_status || null,
    releaseTag: rs.release_tag || null,
    releaseTime: rs.release_time || null,
  })

  // Update multi-repo statuses if available
  const repos = data.repo_statuses || []
  if (repos.length > 0) {
    repoStatuses.set(repos)
  }
}

// Connect to WebSocket for live updates
export function connectWebSocket() {
  if (ws) {
    ws.close()
  }

  const base = projectApiBase || `${window.location.protocol}//${window.location.host}`
  const protocol = base.startsWith('https') ? 'wss:' : 'ws:'
  const host = base.replace(/^https?:\/\//, '')
  const wsUrl = `${protocol}//${host}/api/ws`

  ws = new WebSocket(wsUrl)

  ws.onopen = () => {
    console.log('WebSocket connected')
    connected.set(true)

    // Always fetch history on connect/reconnect to ensure we have all messages.
    // This covers: initial page load, reconnection after network loss,
    // and page becoming active again after being backgrounded.
    const wasReconnect = reconnectTimeout !== null
    if (reconnectTimeout) {
      clearTimeout(reconnectTimeout)
      reconnectTimeout = null
    }

    // Fetch history for all active channels to catch up on any missed messages
    fetchHistory()
    console.log(wasReconnect ? 'Reconnected - fetching message history' : 'Connected - loading initial history')
  }

  ws.onclose = () => {
    console.log('WebSocket disconnected')
    connected.set(false)
    // Auto-reconnect after 3 seconds
    reconnectTimeout = setTimeout(connectWebSocket, 3000)
  }

  ws.onerror = (err) => {
    console.error('WebSocket error:', err)
  }

  ws.onmessage = (event) => {
    try {
      const update = JSON.parse(event.data)
      handleUpdate(update)
    } catch (err) {
      console.error('Failed to parse message:', err)
    }
  }
}

// Callbacks for handling error responses from the server
const errorCallbacks = new Map()
let nextErrorCallbackId = 1

// Register a callback to handle the next error from the server
// Returns a callback ID that can be used to unregister if needed
export function onNextError(callback) {
  const id = nextErrorCallbackId++
  errorCallbacks.set(id, callback)
  return id
}

// Unregister an error callback
export function clearErrorCallback(id) {
  errorCallbacks.delete(id)
}

// Handle incoming WebSocket updates
function handleUpdate(update) {
  switch (update.type) {
    case 'channel_message':
      const msg = update.data
      const channelName = msg.channel || 'midtown'

      // Add to legacy messages array
      messages.update((msgs) => [...msgs, msg])

      // Add to channel-specific messages
      messagesByChannel.update((byChannel) => {
        const channelMsgs = byChannel[channelName] || []
        return {
          ...byChannel,
          [channelName]: [...channelMsgs, msg],
        }
      })

      // Update channel list - increment unread if not viewing this channel
      const currentActiveChannel = get(activeChannel)

      // Only update unread counts for channels that already exist in the list.
      // We no longer auto-add channels from message content to prevent ghost
      // channels. New channels will appear after the next fetchChannels() call
      // (triggered by status polling or manual refresh).
      channels.update((channelList) => {
        const existingChannel = channelList.find((ch) => ch.name === channelName)
        if (existingChannel && channelName !== currentActiveChannel) {
          // Channel exists - increment unread if it's not the active channel
          return channelList.map((ch) =>
            ch.name === channelName ? { ...ch, unread: ch.unread + 1 } : ch
          )
        }
        return channelList
      })

      // Dismiss typing indicator when lead posts a message
      if (msg.from?.toLowerCase() === 'lead') {
        leadTyping.set(false)
        if (leadTypingTimeout) clearTimeout(leadTypingTimeout)
      }
      break
    case 'coworker_status':
      coworkers.update((list) => {
        const idx = list.findIndex((c) => c.name === update.data.name)
        if (idx >= 0) {
          list[idx] = { ...list[idx], ...update.data }
          return [...list]
        }
        return [...list, update.data]
      })
      break
    case 'lead_typing':
      leadTyping.set(update.data.working)
      // Auto-dismiss after 45s if no further updates (safety net).
      // The daemon uses a 30s grace period before sending working=false,
      // so this client timeout should be longer to avoid premature dismissal.
      if (leadTypingTimeout) clearTimeout(leadTypingTimeout)
      if (update.data.working) {
        leadTypingTimeout = setTimeout(() => leadTyping.set(false), 45000)
      }
      break
    case 'error':
      // Invoke all registered error callbacks and then clear them
      errorCallbacks.forEach((callback) => callback(update.data.message))
      errorCallbacks.clear()
      break
    default:
      console.log('Unknown update type:', update.type)
  }
}

// Send a message to the lead via WebSocket
export function sendMessage(content) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(
      JSON.stringify({
        type: 'send_message',
        content,
      })
    )
  } else {
    console.error('WebSocket not connected')
  }
}

// Send a raw JSON message over the WebSocket (for view_window / leave_window).
// Returns true if the message was sent, false if the WebSocket was not open.
export function sendWsMessage(msg) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(msg))
    return true
  }
  return false
}

// Fetch auth profiles from the current project's daemon
// If provider is specified, fetches profiles for that provider only.
// If provider is null/undefined, fetches profiles for the default provider (claude).
export async function fetchAuthProfiles(provider = null) {
  try {
    const url = provider
      ? `${getApiBase()}/auth/profiles?provider=${encodeURIComponent(provider)}`
      : `${getApiBase()}/auth/profiles`
    const res = await fetch(url)
    if (res.ok) {
      const data = await res.json()
      // Update the legacy store for backward compat
      if (!provider || provider === 'claude') {
        authProfiles.set(data)
      }
      return data
    }
  } catch (err) {
    console.error('Failed to fetch auth profiles:', err)
  }
  return []
}

// Fetch profiles for all providers and populate authProfilesByProvider.
// Only includes providers that have at least one profile configured.
export async function fetchAllAuthProfiles() {
  const providers = ['claude', 'codex', 'zai']
  const byProvider = {}

  for (const provider of providers) {
    const profiles = await fetchAuthProfiles(provider)
    if (profiles.length > 0) {
      byProvider[provider] = profiles
    }
  }

  authProfilesByProvider.set(byProvider)

  // Update legacy store with claude profiles if available
  if (byProvider.claude) {
    authProfiles.set(byProvider.claude)
  }

  return byProvider
}

// Switch to a different auth profile via the daemon RPC.
// Parameters:
//   - profile: Profile name to switch to (e.g., "work", "personal")
//   - provider: Provider name ('claude', 'codex', or 'zai')
// Returns { ok: true } on success, or { ok: false, error: string } on failure.
export async function switchAuthProfile(profile, provider) {
  authSwitching.set(true)
  try {
    const res = await fetch(`${getApiBase()}/auth/switch`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ profile, provider }),
    })
    if (res.ok) {
      // Refresh all profiles after switching
      await fetchAllAuthProfiles()
      return { ok: true }
    }
    let errorMsg = `Switch failed (${res.status})`
    try {
      const body = await res.json()
      if (body.error) errorMsg = body.error
    } catch (_) { /* response not JSON */ }
    console.error('Auth switch failed:', errorMsg)
    return { ok: false, error: errorMsg }
  } catch (err) {
    console.error('Failed to switch auth profile:', err)
    return { ok: false, error: 'Network error' }
  } finally {
    authSwitching.set(false)
  }
}

// Upload a file (image or document) to the daemon.
// Returns { ok: true, path, filename } on success, or { ok: false, error } on failure.
export async function uploadFile(file) {
  try {
    const formData = new FormData()
    formData.append('file', file)

    const res = await fetch(`${getApiBase()}/upload`, {
      method: 'POST',
      body: formData,
    })

    if (res.ok) {
      const data = await res.json()
      return { ok: true, path: data.path, filename: data.filename }
    }

    let errorMsg = `Upload failed (${res.status})`
    try {
      const body = await res.json()
      if (body.error) errorMsg = body.error
    } catch (_) { /* response not JSON */ }
    console.error('Upload failed:', errorMsg)
    return { ok: false, error: errorMsg }
  } catch (err) {
    console.error('Failed to upload file:', err)
    return { ok: false, error: 'Network error' }
  }
}
