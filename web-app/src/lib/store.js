import { writable } from 'svelte/store'

// Channel messages
export const messages = writable([])

// WebSocket connection status
export const connected = writable(false)

// Coworker status
export const coworkers = writable([])

// Lead typing/working indicator
export const leadTyping = writable(false)

// Daemon status
export const daemonStatus = writable(null)

// Kanban board data (derived from status API)
export const kanbanData = writable({
  backlog: [],
  inProgress: [],
  review: [],
  done: [],
})

// Repository status (commit, CI, release) - primary repo
export const repoStatus = writable({
  repoName: '',
  fullName: '',
  commitHash: '',
  commitTime: null,
  ciStatus: null,
  releaseTag: null,
  releaseTime: null,
})

// Multi-repo statuses (array of {label, fullName, commitHash, commitTime, ciStatus, releaseTag, releaseTime})
export const repoStatuses = writable([])

// Multi-project support
// List of discovered projects: [{name, status, daemon_socket, webhook_port}]
export const projects = writable([])

// Currently selected project name (null = single-project mode)
export const activeProject = writable(null)

// Whether the app is running in multi-project mode (always true — served from shared webserver)
export const multiProjectMode = writable(true)

// Auth profiles: [{name, is_current, has_credentials}]
export const authProfiles = writable([])

// Whether an auth switch is in progress
export const authSwitching = writable(false)
