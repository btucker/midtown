import { messages, connected, coworkers, daemonStatus, kanbanData } from './store.js'

let ws = null
let reconnectTimeout = null

const API_BASE = '/api'

// Fetch channel message history
export async function fetchHistory() {
  try {
    const res = await fetch(`${API_BASE}/channel`)
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
    const res = await fetch(`${API_BASE}/status`)
    if (res.ok) {
      const data = await res.json()
      daemonStatus.set(data)
      coworkers.set(data.coworkers || [])
      updateKanbanData(data)
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
    })),
    done: mergedPrs.slice(0, 10).map((pr) => ({
      number: pr.number,
      title: pr.title,
      mergedAt: pr.mergedAt,
    })),
  })
}

// Connect to WebSocket for live updates
export function connectWebSocket() {
  if (ws) {
    ws.close()
  }

  const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:'
  const host = window.location.host
  const wsUrl = `${protocol}//${host}/api/ws`

  ws = new WebSocket(wsUrl)

  ws.onopen = () => {
    console.log('WebSocket connected')
    connected.set(true)
    if (reconnectTimeout) {
      clearTimeout(reconnectTimeout)
      reconnectTimeout = null
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
