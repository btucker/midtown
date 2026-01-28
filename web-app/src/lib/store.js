import { writable } from 'svelte/store'

// Channel messages
export const messages = writable([])

// WebSocket connection status
export const connected = writable(false)

// Coworker status
export const coworkers = writable([])

// Daemon status
export const daemonStatus = writable(null)

// Kanban board data (derived from status API)
export const kanbanData = writable({
  backlog: [],
  inProgress: [],
  review: [],
  done: [],
})
