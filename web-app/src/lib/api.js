import {
  messages,
  messagesByChannel,
  channels,
  activeChannel,
  connected,
  coworkers,
  leadTyping,
  daemonStatus,
  kanbanData,
  repoStatus,
  projects,
  activeProject,
  repoStatuses,
  authProfiles,
  authSwitching,
} from './store.js'

let ws = null
let reconnectTimeout = null
let statusPollInterval = null
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
    fetchHistory()
    fetchStatus()
    connectWebSocket()
    // Poll status every 10s to keep kanban board current
    statusPollInterval = setInterval(fetchStatus, 10000)
  }
}

// Get the API base for the current project
export function getApiBase() {
  return projectApiBase ? `${projectApiBase}/api` : '/api'
}

// Fetch channel message history
export async function fetchHistory() {
  try {
    const res = await fetch(`${getApiBase()}/channel`)
    if (res.ok) {
      const data = await res.json()
      messages.set(data)

      // Group messages by channel
      const byChannel = {}
      const channelSet = new Set()
      for (const msg of data) {
        const channelName = msg.channel || 'midtown'
        if (!byChannel[channelName]) {
          byChannel[channelName] = []
        }
        byChannel[channelName].push(msg)
        channelSet.add(channelName)
      }

      messagesByChannel.set(byChannel)

      // Build channel list
      const channelList = Array.from(channelSet).map((name) => ({
        name,
        unread: 0,
        has_pr: false,
        ci_status: null,
      }))
      // Ensure midtown is first
      channelList.sort((a, b) => {
        if (a.name === 'midtown') return -1
        if (b.name === 'midtown') return 1
        return a.name.localeCompare(b.name)
      })
      channels.set(channelList)
    }
  } catch (err) {
    console.error('Failed to fetch history:', err)
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
      updateKanbanData(data)
      updateRepoStatus(data)
    }
  } catch (err) {
    console.error('Failed to fetch status:', err)
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
    if (reconnectTimeout) {
      clearTimeout(reconnectTimeout)
      reconnectTimeout = null
      // Fetch recent history to get messages sent during disconnection
      fetchHistory()
    }
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

      // Update channel list if this is a new channel
      channels.update((channelList) => {
        if (!channelList.find((ch) => ch.name === channelName)) {
          return [
            ...channelList,
            { name: channelName, unread: 0, has_pr: false, ci_status: null },
          ]
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
          list[idx] = update.data
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
export async function fetchAuthProfiles() {
  try {
    const res = await fetch(`${getApiBase()}/auth/profiles`)
    if (res.ok) {
      const data = await res.json()
      authProfiles.set(data)
      return data
    }
  } catch (err) {
    console.error('Failed to fetch auth profiles:', err)
  }
  return []
}

// Switch to a different auth profile via the daemon RPC.
// Returns { ok: true } on success, or { ok: false, error: string } on failure.
export async function switchAuthProfile(profile) {
  authSwitching.set(true)
  try {
    const res = await fetch(`${getApiBase()}/auth/switch`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ profile }),
    })
    if (res.ok) {
      await fetchAuthProfiles()
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
