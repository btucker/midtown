import { writable } from 'svelte/store'

// Channel messages - now keyed by channel name
// Format: { 'midtown': [...messages], 'auth-refactor': [...messages], ... }
export const messagesByChannel = writable({ midtown: [] })

// List of available channels with metadata
// Format: [{ name: 'midtown', unread: 0, has_pr: false, ci_status: null }, ...]
export const channels = writable([{ name: 'midtown', unread: 0, has_pr: false, ci_status: null }])

// Currently active/selected channel name
export const activeChannel = writable('midtown')

// Legacy: single message array for backward compatibility during transition
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

// API usage data (session + weekly utilization)
// Format: { session_util, session_resets, week_util, week_resets, account_email }
export const usageData = writable(null)

// Detail panel state (desktop three-column layout)
// Format: { type: 'task'|'pr'|'coworker', data: {...} } or null when closed
export const detailPanelData = writable(null)

// Viewport width tracking for responsive breakpoints
// true when viewport > 1024px (wide desktop layout)
export const isWideScreen = writable(false)
