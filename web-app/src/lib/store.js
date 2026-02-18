import { writable } from 'svelte/store'

// Channel messages - now keyed by channel name
// Format: { 'midtown': [...messages], 'auth-refactor': [...messages], ... }
export const messagesByChannel = writable({ midtown: [] })

// Load unread counts from localStorage if available
function loadUnreadCounts() {
  if (typeof localStorage !== 'undefined') {
    const stored = localStorage.getItem('midtown_unread_counts')
    if (stored) {
      try {
        return JSON.parse(stored)
      } catch (e) {
        console.warn('Failed to parse stored unread counts:', e)
      }
    }
  }
  return {}
}

// Save unread counts to localStorage
function saveUnreadCounts(channelList) {
  if (typeof localStorage !== 'undefined') {
    const counts = {}
    channelList.forEach((ch) => {
      if (ch.unread > 0) {
        counts[ch.name] = ch.unread
      }
    })
    localStorage.setItem('midtown_unread_counts', JSON.stringify(counts))
  }
}

// List of available channels with metadata
// Format: [{ name: 'midtown', unread: 0, has_pr: false, ci_status: null }, ...]
const storedUnread = loadUnreadCounts()
export const channels = writable([
  { name: 'midtown', unread: storedUnread['midtown'] || 0, has_pr: false, ci_status: null },
])

// Subscribe to channels to persist unread counts
channels.subscribe((channelList) => {
  saveUnreadCounts(channelList)
})

// Currently active/selected channel name
export const activeChannel = writable('midtown')

// Legacy: single message array for backward compatibility during transition
export const messages = writable([])

// WebSocket connection status
export const connected = writable(false)

// Coworker status
export const coworkers = writable([])

// Maximum number of coworkers that can be spawned
export const maxCoworkers = writable(8)

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

// Auth profiles: Map of provider -> [{name, is_current, has_credentials}]
// Example: { 'claude': [...], 'codex': [...], 'zai': [...] }
export const authProfilesByProvider = writable({})

// Legacy: single flat array for backward compatibility
export const authProfiles = writable([])

// Currently selected auth provider ('claude', 'codex', 'zai')
export const selectedAuthProvider = writable('claude')

// Whether an auth switch is in progress
export const authSwitching = writable(false)

// API usage data (session + weekly utilization)
// Format: Array of { provider, profile, session_util, session_resets, week_util, week_resets, account_email }
export const usageData = writable([])

// Detail panel state (desktop three-column layout)
// Format: { type: 'task'|'pr'|'coworker', data: {...} } or null when closed
export const detailPanelData = writable(null)

// Viewport width tracking for responsive breakpoints
// true when viewport > 1024px (wide desktop layout)
export const isWideScreen = writable(false)

// Whether to show archived channels in the channel list (default: false)
export const showArchivedChannels = writable(false)

// Recent tool call activity keyed by channel name.
// 'midtown' holds the main lead's tool calls; topic channel names hold their channel lead's tool calls.
// Format: { 'midtown': [{ item_id, kind, content, status, timestamp }, ...], 'web': [...], ... }
// Each array holds the most recent items (capped at MAX_TOOL_ITEMS_PER_AGENT) for display.
export const agentToolItems = writable({})
