import {
  messages,
  connected,
  coworkers,
  leadTyping,
  daemonStatus,
  kanbanData,
  repoStatus,
  projects,
  activeProject,
  repoStatuses,
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
  coworkers.set([])
  leadTyping.set(false)
  daemonStatus.set(null)
  kanbanData.set({ backlog: [], inProgress: [], review: [], done: [] })
  repoStatus.set({
    repoName: '',
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
function getApiBase() {
  return projectApiBase ? `${projectApiBase}/api` : '/api'
}

// Fetch channel message history
export async function fetchHistory() {
  try {
    const res = await fetch(`${getApiBase()}/channel`)
    if (res.ok) {
      const data = await res.json()
      messages.set(data)
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

  kanbanData.set({
    backlog: tasks.filter((t) => t.status === 'pending'),
    inProgress: tasks.filter((t) => t.status === 'in_progress'),
    review: prs.map((pr) => ({
      number: pr.number,
      title: pr.title,
      author: pr.author,
      status: pr.status,
      reviewer: pr.reviewer,
      reviewer_assigned_at: pr.reviewer_assigned_at,
      created_at: pr.created_at,
      repo: pr.repo || null,
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

// Handle incoming WebSocket updates
function handleUpdate(update) {
  switch (update.type) {
    case 'channel_message':
      messages.update((msgs) => [...msgs, update.data])
      // Dismiss typing indicator when lead posts a message
      if (update.data.from?.toLowerCase() === 'lead') {
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
      // Auto-dismiss after 10s if no further updates (safety net)
      if (leadTypingTimeout) clearTimeout(leadTypingTimeout)
      if (update.data.working) {
        leadTypingTimeout = setTimeout(() => leadTyping.set(false), 10000)
      }
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
