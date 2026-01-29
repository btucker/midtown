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

// Repository status (commit, CI, release)
export const repoStatus = writable({
  repoName: '',
  commitHash: '',
  commitTime: null,
  ciStatus: null,
  releaseTag: null,
  releaseTime: null,
})

// Multi-project support
// List of discovered projects: [{name, status, daemon_socket, webhook_port}]
export const projects = writable([])

// Currently selected project name (null = single-project mode)
export const activeProject = writable(null)

// Whether the app is running in multi-project mode (served from shared webserver)
export const multiProjectMode = writable(false)
