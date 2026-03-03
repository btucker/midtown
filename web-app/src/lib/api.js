import { get } from 'svelte/store'
import {
  messages,
  messagesByChannel,
  channels,
  activeChannel,
  connected,
  coworkers,
  maxCoworkers,
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
  agentToolItems,
  threadToolItems,
  pendingQuestions,
  threadData,
  deepLinkMsgId,
  threadOwnership,
  showArchivedChannels,
} from './store.js'

// Maximum number of tool call items retained per agent in the activity store.
const MAX_TOOL_ITEMS_PER_AGENT = 20

// How long (ms) to keep tool call items visible after a channel lead posts a message.
// This prevents the activity strip from vanishing the instant the lead's response arrives.
const TOOL_ITEMS_CLEAR_DELAY_MS = 4000

// Pending clear timeouts keyed by the agentToolItems channel key.
// Allows the universal_items handler to cancel a pending clear if new tool
// activity arrives before the delay expires (agent is still working).
const agentClearTimeouts = new Map()

// Tracks which fork session owns each thread's tool items (thread_parent_id → agent_name).
// Used by the thread-clear guard to ensure only the owning fork's messages trigger a clear,
// preventing coworkers or the lead from prematurely clearing a fork's tool display.
const threadOwners = new Map()

let ws = null
let reconnectTimeout = null
let statusPollInterval = null
let usagePollInterval = null

// ── Browser history navigation ──────────────────────────────────────────────
// Tracks whether we're currently handling a popstate event to prevent
// circular history pushes (popstate → store change → pushState).
let _handlingPopstate = false

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
        is_dm:
          typeof ch === 'object'
            ? ch.is_dm || ch.name.startsWith('dm-')
            : ch.startsWith('dm-'),
      }))
      // Backend already returns channels sorted with main project channel first
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
  messagesByChannel.set({ [projectName]: [] })
  channels.set([{ name: projectName, unread: 0, has_pr: false, ci_status: null }])
  activeChannel.set(projectName)
  coworkers.set([])
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
  repoStatuses.set([])
  usageData.set([])
  agentClearTimeouts.forEach((t) => clearTimeout(t))
  agentClearTimeouts.clear()
  threadOwners.clear()
  agentToolItems.set({})
  threadToolItems.set({})
  threadData.set(null)
  connected.set(false)

  // Set the new active project
  activeProject.set(projectName)

  if (webhookPort) {
    if (window.location.protocol === 'https:') {
      // HTTPS: proxy through the webserver to avoid mixed content errors.
      // The webserver forwards requests to the daemon's webhook port.
      projectApiBase = `${window.location.origin}/api/projects/${projectName}/proxy`
    } else {
      // HTTP: connect to the project's daemon directly via its webhook port
      projectApiBase = `http://${window.location.hostname}:${webhookPort}`
    }
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

// Backward-compat fallback for older history payloads:
// if thread replies are included inline, compute reply_count/last_reply on
// parents and filter replies from the main timeline.
function annotateThreadReplyCounts(msgs) {
  const replyCountMap = {}
  const lastReplyMap = {}
  for (const m of msgs) {
    if (m.thread_parent_id) {
      replyCountMap[m.thread_parent_id] = (replyCountMap[m.thread_parent_id] || 0) + 1
      lastReplyMap[m.thread_parent_id] = m
    }
  }
  return msgs
    .filter((m) => !m.thread_parent_id)
    .map((m) =>
      replyCountMap[m.id]
        ? { ...m, reply_count: replyCountMap[m.id], last_reply: lastReplyMap[m.id] }
        : m
    )
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
        const channelMsgs = annotateThreadReplyCounts(data)
        messagesByChannel.update((byChannel) => ({
          ...byChannel,
          [channelName]: channelMsgs,
        }))
      } else {
        // Fetching all messages (initial load) - group by channel
        const byChannel = {}
        for (const msg of data) {
          const name = msg.channel || get(activeProject)
          if (!byChannel[name]) {
            byChannel[name] = []
          }
          byChannel[name].push(msg)
        }

        // Compute reply counts and filter thread replies from main timeline.
        // annotateThreadReplyCounts returns only top-level messages (thread
        // replies filtered out), keeping both stores consistent with the
        // real-time WS handler which never adds thread replies to messages[].
        for (const [ch, channelMsgs] of Object.entries(byChannel)) {
          byChannel[ch] = annotateThreadReplyCounts(channelMsgs)
        }

        // Set legacy store with filtered (top-level only) messages so it
        // stays consistent with messagesByChannel and the WS handler.
        messages.set(Object.values(byChannel).flat())

        // Merge rather than replace: preserve messages for channels not in this
        // response (e.g. channels with no recent history). Using .set() would
        // wipe them on WS reconnect, causing blank channels until re-tapped.
        //
        // Pre-merge: strip any pending (optimistic) messages from existing channels.
        // If the WS echo was lost during a disconnect, a pending message in a
        // low-traffic channel would survive the merge as a "ghost" forever. Clearing
        // pending entries first is safe — if the message actually sent, it comes back
        // clean in byChannel (for its channel) or is simply gone (network loss).
        messagesByChannel.update((existing) => {
          const withoutPending = Object.fromEntries(
            Object.entries(existing).map(([ch, msgs]) => [ch, msgs.filter((m) => !m.pending)])
          )
          return { ...withoutPending, ...byChannel }
        })

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
  // Hydrate pending questions from daemon (survives page refresh / WebSocket reconnect)
  try {
    const res = await fetch(`${getApiBase()}/questions`)
    if (res.ok) {
      const data = await res.json()
      pendingQuestions.set(data.questions || [])
    }
  } catch {
    // Non-critical — questions will arrive via WebSocket events
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

  // Always update multi-repo statuses (empty array clears previous entries on project switch)
  repoStatuses.set(data.repo_statuses || [])
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

// Handle incoming WebSocket updates.
// Exported for testing only — production code uses this via the WS onmessage handler.
export function handleUpdate(update) {
  switch (update.type) {
    case 'channel_message': {
      const msg = update.data
      const channelName = msg.channel || get(activeProject)

      if (msg.thread_parent_id) {
        // Thread reply — update thread panel if open for this parent, and
        // bump reply_count on the parent message, but do NOT add to the
        // main channel timeline.
        threadData.update((td) => {
          if (td && td.parentMessage?.id === msg.thread_parent_id) {
            // Remove the first pending optimistic reply with matching content/sender before
            // appending the real server-confirmed message. Only match when the confirmed
            // message is from 'user' (guards against a different participant posting the
            // same text and incorrectly consuming our placeholder).
            let threadDeduplicated = false
            const withoutPending = td.messages.filter((m) => {
              if (!threadDeduplicated && m.pending && m.content === msg.content && msg.from === 'user') {
                threadDeduplicated = true
                return false
              }
              return true
            })
            return { ...td, messages: [...withoutPending, msg] }
          }
          return td
        })

        // Schedule a delayed clear of thread tool activity when the owning fork posts a reply.
        // Only the fork that produced the thread's tool items should trigger this clear —
        // a coworker or lead posting to the same thread must not prematurely clear the display.
        const threadOwner = threadOwners.get(msg.thread_parent_id)
        if (msg.from && threadOwner && msg.from === threadOwner) {
          const threadClearKey = `thread:${msg.thread_parent_id}`
          if (agentClearTimeouts.has(threadClearKey)) {
            clearTimeout(agentClearTimeouts.get(threadClearKey))
          }
          const timeout = setTimeout(() => {
            agentClearTimeouts.delete(threadClearKey)
            threadOwners.delete(msg.thread_parent_id)
            threadToolItems.update((byThread) => {
              const updated = { ...byThread }
              delete updated[msg.thread_parent_id]
              return updated
            })
          }, TOOL_ITEMS_CLEAR_DELAY_MS)
          agentClearTimeouts.set(threadClearKey, timeout)
        }

        // Increment reply_count on the parent message in messagesByChannel
        messagesByChannel.update((byChannel) => {
          const channelMsgs = byChannel[channelName]
          if (!channelMsgs) return byChannel
          return {
            ...byChannel,
            [channelName]: channelMsgs.map((m) => {
              if (m.id === msg.thread_parent_id) {
                return {
                  ...m,
                  reply_count: (m.reply_count || 0) + 1,
                  last_reply: msg,
                }
              }
              return m
            }),
          }
        })
      } else {
        // Top-level message — add to stores, removing any matching pending optimistic message first.
        // Add to legacy messages array
        messages.update((msgs) => [...msgs, msg])

        // Add to channel-specific messages, deduplicating pending optimistic entries.
        // If the user sent this message optimistically, a pending placeholder with the
        // same content will be in the list. Remove the first such match before appending
        // the server-confirmed message.
        messagesByChannel.update((byChannel) => {
          const channelMsgs = byChannel[channelName] || []
          // Only dedup when the confirmed message is from 'user': prevents a different
          // channel participant posting identical text from consuming our placeholder.
          let deduplicated = false
          const withoutPending = channelMsgs.filter((m) => {
            if (!deduplicated && m.pending && m.content === msg.content && msg.from === 'user') {
              deduplicated = true
              return false
            }
            return true
          })
          return { ...byChannel, [channelName]: [...withoutPending, msg] }
        })

        // Update channel list - increment unread if not viewing this channel.
        // Only for top-level messages — thread replies don't appear in the
        // main timeline, so they should not increment the unread badge.
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

        // Schedule a delayed clear of tool activity when a sender posts a message.
        // Using a delay (rather than clearing immediately) keeps recently completed
        // tool calls visible long enough for the user to read them before the
        // activity strip resets. Only applies to top-level messages; a thread
        // reply mid-task should not clear the coworker's tool activity.
        //
        // Key by channelName (not msg.from): agentToolItems is channel-keyed, so
        // the correct key to delete is the channel the message was posted to.
        // Exception: skip when the main lead posts to the main channel — deleting
        // 'midtown' tool items on every lead response would clear in-progress activity.
        const fromLower = msg.from ? msg.from.toLowerCase() : ''
        const isMainLeadOnMainChannel =
          channelName === 'midtown' && (fromLower === 'lead' || fromLower === 'midtown')
        if (msg.from && !isMainLeadOnMainChannel) {
          if (agentClearTimeouts.has(channelName)) {
            clearTimeout(agentClearTimeouts.get(channelName))
          }
          const timeout = setTimeout(() => {
            agentClearTimeouts.delete(channelName)
            agentToolItems.update((byAgent) => {
              const updated = { ...byAgent }
              delete updated[channelName]
              return updated
            })
          }, TOOL_ITEMS_CLEAR_DELAY_MS)
          agentClearTimeouts.set(channelName, timeout)
          // Note: pending questions are NOT cleared on channel messages. A coworker
          // posting a /me status update does not mean their question was answered.
          // Questions are cleared by: (1) the daemon via nudge delivery, (2) optimistic
          // removal in sendAnswer(), or (3) a new coworker_question event replacing it.
        }
      }
      break
    }
    case 'coworker_status': {
      // Skip channel lead sessions (ch-<channel>) and the lead itself.
      // Channel leads are scoped to a specific topic channel and must not
      // appear in the general coworker status panel.
      const name = update.data.name
      if (name && (name.startsWith('ch-') || name.toLowerCase() === 'lead')) {
        break
      }
      coworkers.update((list) => {
        const idx = list.findIndex((c) => c.name === name)
        if (idx >= 0) {
          list[idx] = { ...list[idx], ...update.data }
          return [...list]
        }
        return [...list, update.data]
      })
      break
    }
    case 'universal_items': {
      // Tool call activity keyed by channel or thread.
      // data: { agent_name, channel, thread_parent_id?, items }
      // When thread_parent_id is present, the items belong to a forked lead working
      // in a thread — route them to threadToolItems so they appear in the thread panel
      // instead of the main channel activity strip.
      const threadId = update.data.thread_parent_id
      if (threadId) {
        // Track which fork session owns this thread's tool items
        if (update.data.agent_name) {
          threadOwners.set(threadId, update.data.agent_name)
        }
        if (agentClearTimeouts.has(`thread:${threadId}`)) {
          clearTimeout(agentClearTimeouts.get(`thread:${threadId}`))
          agentClearTimeouts.delete(`thread:${threadId}`)
        }
        threadToolItems.update((byThread) => {
          const existing = byThread[threadId] || []
          const merged = [...existing, ...update.data.items].slice(-MAX_TOOL_ITEMS_PER_AGENT)
          return { ...byThread, [threadId]: merged }
        })
      } else {
        // Channel-scoped: main lead or channel lead tool calls.
        const channelKey = update.data.channel ?? get(activeProject)
        if (agentClearTimeouts.has(channelKey)) {
          clearTimeout(agentClearTimeouts.get(channelKey))
          agentClearTimeouts.delete(channelKey)
        }
        agentToolItems.update((byChannel) => {
          const existing = byChannel[channelKey] || []
          const merged = [...existing, ...update.data.items].slice(-MAX_TOOL_ITEMS_PER_AGENT)
          return { ...byChannel, [channelKey]: merged }
        })
      }
      break
    }
    case 'coworker_question':
      pendingQuestions.update((qs) => {
        // Replace existing question from same coworker (only one question per coworker at a time)
        const filtered = qs.filter((q) => q.coworker_name !== update.data.coworker_name)
        return [...filtered, update.data]
      })
      break
    case 'channel_list_changed':
      // Re-fetch full channel list from server to get accurate state
      fetchChannels(get(showArchivedChannels))
      break
    case 'thread_ownership': {
      const { thread_parent_id, has_dedicated_session } = update.data
      threadOwnership.update((map) => ({
        ...map,
        [thread_parent_id]: has_dedicated_session,
      }))
      break
    }
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
export function sendMessage(content, channel = null, threadParentId = null) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    const message = {
      type: 'send_message',
      content,
    }
    // Include channel if specified (null/undefined means use default)
    if (channel) {
      message.channel = channel
    }
    if (threadParentId) {
      message.thread_parent_id = threadParentId
    }
    ws.send(JSON.stringify(message))

    // Optimistically add the message to the store immediately so the user sees
    // their message without waiting for the server round-trip.
    const channelName = channel || 'midtown'
    const tempId = 'pending-' + crypto.randomUUID()
    const optimisticMsg = {
      id: tempId,
      from: 'user',
      content,
      channel: channelName,
      timestamp: new Date().toISOString(),
      pending: true,
    }

    if (threadParentId) {
      // Thread reply: add to threadData if the panel is open for this parent
      threadData.update((td) => {
        if (!td) return td
        return { ...td, messages: [...td.messages, optimisticMsg] }
      })
    } else {
      // Top-level message: add to channel message list
      messagesByChannel.update((byChannel) => {
        const channelMsgs = byChannel[channelName] || []
        return { ...byChannel, [channelName]: [...channelMsgs, optimisticMsg] }
      })
    }
  } else {
    console.error('WebSocket not connected')
  }
}

// Answer a coworker's pending question
export function sendAnswer(coworkerName, answer) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({
      type: 'answer_question',
      coworker_name: coworkerName,
      answer,
    }))
    // Optimistically remove from pending questions
    pendingQuestions.update((qs) => qs.filter((q) => q.coworker_name !== coworkerName))
  } else {
    console.error('WebSocket not connected')
  }
}

// Create a dedicated session for a thread (fork)
export function forkThread(threadParentId, channelName) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({
      type: 'fork_thread',
      thread_parent_id: threadParentId,
      channel: channelName,
    }))
  }
}

// Return a thread to the channel lead (kill dedicated session)
export function unforkThread(threadParentId, channelName) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({
      type: 'unfork_thread',
      thread_parent_id: threadParentId,
      channel: channelName,
    }))
  }
}

// Query whether a thread has a dedicated session
export function queryThreadOwnership(threadParentId, channelName) {
  if (ws && ws.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify({
      type: 'get_thread_ownership',
      thread_parent_id: threadParentId,
      channel: channelName,
    }))
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

// Start an OAuth login flow for a profile.
// The backend spawns the CLI which opens the default browser for OAuth.
// Returns { ok: true } on success, or { ok: false, error: string } on failure.
export async function startAuthLogin(email, provider = 'claude') {
  try {
    const res = await fetch(`${getApiBase()}/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email, provider }),
    })
    if (res.ok) return { ok: true }
    let errorMsg = `Login failed (${res.status})`
    try {
      const body = await res.json()
      if (body.error) errorMsg = body.error
    } catch (_) { /* response not JSON */ }
    return { ok: false, error: errorMsg }
  } catch (err) {
    console.error('Failed to start auth login:', err)
    return { ok: false, error: 'Network error' }
  }
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

// Fetch thread history (parent message + replies) for a given parent message
export async function fetchThread(channelName, parentMessageId) {
  try {
    const params = new URLSearchParams({
      channel: channelName,
      thread_parent_id: parentMessageId,
    })
    const res = await fetch(`${getApiBase()}/channels/history?${params}`)
    if (res.ok) {
      const data = await res.json()
      return data
    }
  } catch (err) {
    console.warn('Failed to fetch thread:', err)
  }
  return []
}

// Open a thread panel for the given parent message
// Open a thread panel for the given parent message.
// Pass { pushState: false } during deep-link initialization to avoid
// pushing a history entry that replaceNavState would immediately replace.
export function openThread(parentMessage, channelName, { pushState = true } = {}) {
  // Find any tasks associated with this thread's parent message.
  // Check both thread_id and message_id: for tasks created with --thread-id,
  // thread_id (the conversation root) differs from message_id (the announcement).
  const { inProgress, backlog } = get(kanbanData)
  const allTasks = [...inProgress, ...backlog]
  const tasks = allTasks.filter(
    (t) => t.thread_id === parentMessage.id || t.message_id === parentMessage.id,
  )
  // Show panel immediately with loading state, then populate with replies
  threadData.set({ parentMessage, channelName, messages: [], tasks })
  // Query thread ownership so the UI knows whether a dedicated session exists
  queryThreadOwnership(parentMessage.id, channelName)
  if (pushState) {
    pushNavState({ channel: channelName, thread: parentMessage.id })
  }
  fetchThread(channelName, parentMessage.id).then((fetched) => {
    // Guard against stale fetch: the user may have opened a different thread
    // (or closed the panel) while this fetch was in flight. Only apply results
    // if the panel still refers to the same parent message. Also merge rather
    // than overwrite so any WS-delivered replies that arrived during the fetch
    // are preserved (append them after the fetched history).
    threadData.update((td) => {
      if (!td || td.parentMessage?.id !== parentMessage.id) return td
      // The backend includes the parent message in the response — extract it
      // so we can upgrade parentMessage with real data from the API.
      const fetchedParent = fetched.find((m) => m.id === parentMessage.id)
      const replies = fetched.filter((m) => m.id !== parentMessage.id)
      // Deduplicate: WS may have already appended some replies
      const fetchedIds = new Set(replies.map((r) => r.id))
      const wsOnly = td.messages.filter((r) => !fetchedIds.has(r.id))
      return {
        ...td,
        parentMessage: fetchedParent ?? td.parentMessage,
        messages: [...replies, ...wsOnly],
      }
    })
  })
}

// Open a thread panel for a task, showing task card(s) above the thread.
// If task.thread_id or task.message_id is present, fetches thread replies.
// If neither is present, shows the task card with no backing thread.
export function openTaskThread(task, channelName) {
  if (!task.thread_id && !task.message_id) {
    // No creation message — show task card only, replies sent as top-level messages
    threadData.set({ parentMessage: null, channelName, messages: [], tasks: [task] })
    pushNavState({ channel: channelName })
    return
  }

  // Resolve thread parent: prefer thread_id (the conversation thread root) over
  // message_id (the announcement message). Falls back to resolving via the
  // creation message's thread_parent_id for legacy tasks without thread_id.
  const channelMsgs = get(messagesByChannel)[channelName] || []
  const parentMessageId = task.thread_id
    ?? channelMsgs.find((m) => m.id === task.message_id)?.thread_parent_id
    ?? task.message_id

  // Find all tasks whose thread roots under the same parent
  const { inProgress, backlog } = get(kanbanData)
  const allTasks = [...inProgress, ...backlog]
  const tasks = allTasks.filter((t) => {
    if (!t.thread_id && !t.message_id) return false
    const tParent = t.thread_id
      ?? channelMsgs.find((m) => m.id === t.message_id)?.thread_parent_id
      ?? t.message_id
    return tParent === parentMessageId
  })
  // Always include the clicked task even if not found above
  if (!tasks.find((t) => t.id === task.id)) tasks.unshift(task)

  // Use the real channel message if available so the MessageRow gets correct
  // timestamp, sender, and content.  Fall back to a synthetic stub only when
  // the message hasn't loaded yet (rare edge case).
  const parentMessage = channelMsgs.find((m) => m.id === parentMessageId)
    ?? { id: parentMessageId, from: 'lead', content: task.subject }
  threadData.set({ parentMessage, channelName, messages: [], tasks })
  pushNavState({ channel: channelName, thread: parentMessageId })
  fetchThread(channelName, parentMessageId).then((fetched) => {
    threadData.update((td) => {
      if (!td || td.parentMessage?.id !== parentMessageId) return td
      // Extract parent from response — replaces synthetic stub with real data
      const fetchedParent = fetched.find((m) => m.id === parentMessageId)
      const replies = fetched.filter((m) => m.id !== parentMessageId)
      const fetchedIds = new Set(replies.map((r) => r.id))
      const wsOnly = td.messages.filter((r) => !fetchedIds.has(r.id))
      return {
        ...td,
        parentMessage: fetchedParent ?? td.parentMessage,
        messages: [...replies, ...wsOnly],
      }
    })
  })
}

// ── Browser history helpers ──────────────────────────────────────────────────

// Build a URL path for the given navigation state.
// Always includes `channel` when a thread is present so deep-links work
// even when the channel name matches the project name.
function buildNavUrl(state) {
  const project = get(activeProject)
  if (!project) return '/'
  let url = '/' + encodeURIComponent(project)
  const needsChannel = state.channel && (state.channel !== project || state.thread)
  if (needsChannel) {
    url += '?channel=' + encodeURIComponent(state.channel)
  }
  if (state.thread) {
    url += (url.includes('?') ? '&' : '?') + 'thread=' + encodeURIComponent(state.thread)
  }
  if (state.msg) {
    url += (url.includes('?') ? '&' : '?') + 'msg=' + encodeURIComponent(state.msg)
  }
  return url
}

// Push a new history entry for a user-initiated navigation event.
// No-op when handling a popstate event (prevents circular pushes).
export function pushNavState(state) {
  if (_handlingPopstate) return
  history.pushState(state, '', buildNavUrl(state))
}

// Replace the current history entry (initial state or URL sync).
export function replaceNavState(state) {
  history.replaceState(state, '', buildNavUrl(state))
}

// Set up the popstate listener for browser back/forward navigation.
// Returns a cleanup function to remove the listener.
export function setupHistoryNavigation() {
  function handlePopstate(e) {
    const state = e.state
    if (!state) return

    _handlingPopstate = true
    try {
      // Channel navigation
      if (state.channel && state.channel !== get(activeChannel)) {
        activeChannel.set(state.channel)
        channels.update((list) =>
          list.map((ch) => (ch.name === state.channel ? { ...ch, unread: 0 } : ch))
        )
        const currentMessages = get(messagesByChannel)[state.channel]
        if (!currentMessages || currentMessages.length === 0) {
          fetchHistory(state.channel)
        }
      }

      // Thread navigation
      if (state.thread) {
        if (state.msg) {
          deepLinkMsgId.set(state.msg)
        }
        const channel = state.channel || get(activeChannel)
        const channelMsgs = get(messagesByChannel)[channel] || []
        const parentMsg = channelMsgs.find((m) => m.id === state.thread)
        if (parentMsg) {
          openThread(parentMsg, channel)
        } else {
          // Message not in loaded messages — use a stub; openThread will fetch the data
          openThread({ id: state.thread, from: '', content: '' }, channel)
        }
      } else {
        threadData.set(null)
      }
    } finally {
      _handlingPopstate = false
    }
  }

  window.addEventListener('popstate', handlePopstate)
  return () => window.removeEventListener('popstate', handlePopstate)
}

// Close the thread panel.
// Pass { pushState: false } when the caller will push its own history entry
// (e.g. selectChannel, selectDm) to avoid duplicate entries.
export function closeThread({ pushState = true } = {}) {
  threadData.set(null)
  if (pushState) {
    pushNavState({ channel: get(activeChannel) })
  }
}

// Search messages across all channels
export async function searchMessages(query, limit = 50) {
  try {
    const params = new URLSearchParams({ q: query, limit: String(limit) })
    const res = await fetch(`${getApiBase()}/search?${params}`)
    if (res.ok) {
      return await res.json()
    }
    console.error(`Search API returned ${res.status}: ${res.statusText}`)
    return { results: [], query, total: 0, error: true }
  } catch (err) {
    console.error('Failed to search messages:', err)
    return { results: [], query, total: 0, error: true }
  }
}

// Select (or create-then-select) a DM channel for the given coworker name.
// DM channels are named `dm-<coworkerName>` on the backend.
// If the channel doesn't exist yet, it's created first, then selected.
export async function selectDm(coworkerName) {
  const channelName = `dm-${coworkerName}`

  closeThread({ pushState: false })

  const currentChannels = get(channels)
  const exists = currentChannels.some((ch) => ch.name === channelName)

  if (!exists) {
    // Optimistically add the DM channel so the sidebar shows it immediately,
    // regardless of whether the backend create or subsequent fetchChannels succeeds.
    channels.update((list) => [
      ...list,
      { name: channelName, unread: 0, has_pr: false, ci_status: null, is_dm: true },
    ])

    try {
      const res = await fetch(`${getApiBase()}/channels/create`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: channelName }),
      })
      if (!res.ok) {
        const errorData = await res.json()
        console.error('Failed to create DM channel:', errorData.error)
      } else {
        // Refresh channel list so the sidebar reflects canonical backend state
        await fetchChannels(get(showArchivedChannels))
      }
    } catch (err) {
      console.error('Failed to create DM channel:', err)
    }
  }

  activeChannel.set(channelName)
  pushNavState({ channel: channelName })

  channels.update((channelList) =>
    channelList.map((ch) => (ch.name === channelName ? { ...ch, unread: 0 } : ch))
  )

  const currentMessages = get(messagesByChannel)[channelName]
  if (!currentMessages || currentMessages.length === 0) {
    fetchHistory(channelName)
  }
}
