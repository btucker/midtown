// @ts-check

/**
 * Shared mock data and route-setup helpers for e2e tests.
 */

export const DETAIL_TASK_ID = 202
export const THREAD_TASK_ID = 303
export const THREAD_PARENT_ID = 'msg-thread-parent'

export const MOCK_MESSAGES = [
  {
    id: 'msg-1',
    channel: 'midtown',
    from: 'lead',
    content: 'Starting the sprint. Track !202 for release readiness.',
    timestamp: '2025-01-15T10:00:00Z',
    msg_type: 'message',
    reply_count: 2,
    last_reply: {
      id: 'msg-1-reply-2',
      from: 'park',
      content: 'Reviewed the plan, looks good.',
      timestamp: '2025-01-15T10:05:30Z',
    },
  },
  {
    id: 'msg-2',
    channel: 'midtown',
    from: 'park',
    content: '/me investigating flaky thread panel screenshot capture',
    timestamp: '2025-01-15T10:01:00Z',
    msg_type: 'action',
  },
  {
    id: 'msg-3',
    channel: 'midtown',
    from: 'midtown',
    content: 'Daemon restarted successfully',
    timestamp: '2025-01-15T10:02:00Z',
    msg_type: 'message',
  },
  {
    id: 'msg-1-reply-1',
    channel: 'midtown',
    from: 'madison',
    content: 'Docs updated to include browser support.',
    timestamp: '2025-01-15T10:04:00Z',
    msg_type: 'message',
    thread_parent_id: 'msg-1',
  },
  {
    id: 'msg-1-reply-2',
    channel: 'midtown',
    from: 'park',
    content: 'Reviewed the plan, looks good.',
    timestamp: '2025-01-15T10:05:30Z',
    msg_type: 'message',
    thread_parent_id: 'msg-1',
  },
  {
    id: THREAD_PARENT_ID,
    channel: 'midtown',
    from: 'amsterdam',
    content: 'PR is up: **auth endpoint** [link](https://github.com/example/pull/1) — thread for blockers on !303.',
    timestamp: '2025-01-15T10:03:00Z',
    msg_type: 'message',
    reply_count: 1,
    last_reply: {
      id: 'thread-msg-2',
      from: 'park',
      content: 'Will drop notes in the thread.',
      timestamp: '2025-01-15T10:06:00Z',
    },
  },
  {
    id: 'msg-5',
    channel: 'midtown',
    from: 'amsterdam',
    content: 'PR is up: **auth endpoint** [link](https://github.com/example/pull/1)',
    timestamp: '2025-01-15T10:04:00Z',
    msg_type: 'message',
  },
  {
    id: 'thread-msg-2',
    channel: 'midtown',
    from: 'park',
    content: 'Dropped the first batch of replies.',
    timestamp: '2025-01-15T10:06:00Z',
    msg_type: 'message',
    thread_parent_id: THREAD_PARENT_ID,
  },
]

export const MOCK_THREAD_REPLIES = {
  'msg-1': [
    {
      id: 'msg-1-reply-1',
      thread_parent_id: 'msg-1',
      from: 'madison',
      content: 'Docs updated to include browser support.',
      timestamp: '2025-01-15T10:04:00Z',
    },
    {
      id: 'msg-1-reply-2',
      thread_parent_id: 'msg-1',
      from: 'park',
      content: 'Reviewed the plan, looks good.',
      timestamp: '2025-01-15T10:05:30Z',
    },
  ],
  [THREAD_PARENT_ID]: [
    {
      id: 'thread-msg-2',
      thread_parent_id: THREAD_PARENT_ID,
      from: 'park',
      content: 'Dropped the first batch of replies.',
      timestamp: '2025-01-15T10:06:00Z',
    },
  ],
}

export const MOCK_STATUS = {
  daemon: 'running',
  repo_name: 'midtown',
  repo_full_name: 'btucker/midtown',
  repo_status: {
    commit_hash: 'abc1234',
    commit_time: '2025-01-15T09:30:00Z',
    ci_status: 'success',
    release_tag: 'v0.5.0',
    release_time: '2025-01-14T18:00:00Z',
  },
  repo_statuses: [
    { label: 'midtown', fullName: 'btucker/midtown' },
    { label: 'sdk', fullName: 'btucker/midtown-sdk' },
  ],
  max_coworkers: 8,
  coworkers: [
    {
      name: 'park',
      status: 'active',
      phase: 'developing',
      task_id: DETAIL_TASK_ID,
      pr_number: 42,
      progress: 55,
      time_estimate: '~8m',
      health: 'green',
      current_task: 'Add Playwright e2e tests',
      started_at: '2025-01-15T09:00:00Z',
    },
    {
      name: 'amsterdam',
      status: 'active',
      phase: 'reviewing',
      task_id: null,
      pr_number: 77,
      progress: 80,
      time_estimate: null,
      health: 'yellow',
      current_task: 'Review PR #77',
      started_at: '2025-01-15T08:30:00Z',
    },
    {
      name: 'madison',
      status: 'idle',
      phase: null,
      task_id: null,
      pr_number: null,
      health: 'green',
      progress: null,
      current_task: null,
      started_at: '2025-01-15T08:00:00Z',
    },
  ],
  tasks: [
    {
      id: DETAIL_TASK_ID,
      subject: 'Harden Playwright mocks',
      description: 'Add WebSocket stubs and expand coverage.',
      status: 'in_progress',
      owner: 'park',
      blocked_by: [],
      channel: 'midtown',
      message_id: null,
    },
    {
      id: THREAD_TASK_ID,
      subject: 'Discuss thread UX polish',
      description: 'Use the thread to coordinate follow-ups.',
      status: 'pending',
      owner: null,
      blocked_by: [],
      channel: 'midtown',
      message_id: THREAD_PARENT_ID,
    },
  ],
  pull_requests: [
    {
      number: 42,
      title: 'feat: Add auth endpoint [Midtown #202]',
      author: 'park',
      reviewer: 'amsterdam',
      status: 'awaiting review',
      review_posted: false,
      reviewer_assigned_at: '2025-01-15T09:10:00Z',
      created_at: '2025-01-15T08:10:00Z',
      task_id: DETAIL_TASK_ID,
      task_name: 'Harden Playwright mocks',
      repo: 'midtown',
    },
    {
      number: 77,
      title: 'fix: Login redirect',
      author: 'amsterdam',
      reviewer: 'park',
      status: 'approved',
      review_posted: true,
      reviewer_assigned_at: '2025-01-15T07:50:00Z',
      created_at: '2025-01-15T07:30:00Z',
      task_id: null,
      task_name: null,
      repo: 'sdk',
    },
  ],
  merged_prs: [
    { number: 40, title: 'chore: Update deps', mergedAt: '2025-01-14T12:00:00Z', repo: 'midtown' },
    { number: 39, title: 'feat: Add status page', mergedAt: '2025-01-13T15:00:00Z', repo: 'sdk' },
  ],
  agent_tool_items: {
    midtown: [
      {
        item_id: 'call-1',
        status: 'InProgress',
        timestamp: '2025-01-15T10:07:00Z',
        content: [
          { ToolCall: { call_id: 'call-1', name: 'npm run test:e2e', semantic_header: 'test:e2e' } },
        ],
      },
    ],
  },
  kanban_columns: {
    backlog: [
      { id: 501, subject: 'Write release notes', status: 'pending', channel: 'ops' },
    ],
    in_progress: [
      { id: DETAIL_TASK_ID, subject: 'Harden Playwright mocks', status: 'in_progress', channel: 'midtown' },
    ],
    review: [
      { number: 42, title: 'feat: Add auth endpoint [Midtown #202]', status: 'awaiting review' },
    ],
    done: [
      { number: 40, title: 'chore: Update deps', mergedAt: '2025-01-14T12:00:00Z' },
    ],
  },
}

export const MOCK_PROJECTS = [
  { name: 'test-project', status: 'running', webhook_port: 47099 },
]

export const MOCK_CHANNELS = {
  channels: [
    { name: 'midtown', is_archived: false },
  ]
}

export const MOCK_LEAD_PANE = {
  content: 'claude> Running tests...\n$ npm test\nAll 42 tests passed.',
}

export const MOCK_USAGE = [
  {
    provider: 'claude',
    profile: 'default',
    account_email: 'lead@example.com',
    session_util: 32,
    session_resets: new Date(Date.now() + 1000 * 60 * 45).toISOString(),
    week_util: 58,
    week_resets: new Date(Date.now() + 1000 * 60 * 60 * 24 * 3).toISOString(),
  },
  {
    provider: 'codex',
    profile: 'fast',
    account_email: 'codex@example.com',
    session_util: 12,
    session_resets: new Date(Date.now() + 1000 * 60 * 30).toISOString(),
    week_util: 80,
    week_resets: new Date(Date.now() + 1000 * 60 * 60 * 24 * 2).toISOString(),
  },
]

function deepCopy(obj) {
  return JSON.parse(JSON.stringify(obj))
}

function mergeDeep(target, source) {
  for (const key of Object.keys(source)) {
    const value = source[key]
    if (value && typeof value === 'object' && !Array.isArray(value)) {
      target[key] = mergeDeep(target[key] ?? {}, value)
    } else {
      target[key] = value
    }
  }
  return target
}

/**
 * Intercept all API routes with mock data.
 * Also stubs WebSocket so the page doesn't fail trying to connect to a real daemon.
 * @param {import('@playwright/test').Page} page
 * @param {object} [overrides]
 * @param {object[]} [overrides.messages]
 * @param {object} [overrides.status]
 * @param {object} [overrides.leadPane]
 */
export async function mockAllRoutes(page, overrides = {}) {
  const msgs = overrides.messages ? overrides.messages : deepCopy(MOCK_MESSAGES)
  const status = overrides.status
    ? mergeDeep(deepCopy(MOCK_STATUS), overrides.status)
    : deepCopy(MOCK_STATUS)
  const leadPane = overrides.leadPane ? mergeDeep(deepCopy(MOCK_LEAD_PANE), overrides.leadPane) : deepCopy(MOCK_LEAD_PANE)
  const usage = overrides.usage ?? MOCK_USAGE
  const threadReplies = overrides.threadReplies
    ? { ...deepCopy(MOCK_THREAD_REPLIES), ...overrides.threadReplies }
    : deepCopy(MOCK_THREAD_REPLIES)
  const projects = overrides.projects ?? MOCK_PROJECTS

  await page.addInitScript(() => {
    window.__mockWebSockets = []
    window.__mockWsMessages = []

    class MockWebSocket extends EventTarget {
      constructor(url) {
        super()
        this.url = url
        this.readyState = MockWebSocket.OPEN
        window.__mockWebSockets.push(this)
        queueMicrotask(() => {
          const event = new Event('open')
          if (typeof this.onopen === 'function') {
            this.onopen(event)
          }
          this.dispatchEvent(event)
        })
      }

      send(data) {
        window.__mockWsMessages.push(data)
      }

      close() {
        this.readyState = MockWebSocket.CLOSED
        const event = new Event('close')
        if (typeof this.onclose === 'function') {
          this.onclose(event)
        }
        this.dispatchEvent(event)
      }
    }

    MockWebSocket.CONNECTING = 0
    MockWebSocket.OPEN = 1
    MockWebSocket.CLOSING = 2
    MockWebSocket.CLOSED = 3

    window.WebSocket = MockWebSocket

    window.__dispatchWsMessage = (payload) => {
      const data = typeof payload === 'string' ? payload : JSON.stringify(payload)
      const event = new MessageEvent('message', { data })
      for (const socket of window.__mockWebSockets) {
        if (typeof socket.onmessage === 'function') {
          socket.onmessage(event)
        }
        socket.dispatchEvent(event)
      }
    }
  })

  await page.route('**/api/projects', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(projects) })
  )

  await page.route('**/api/channels', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_CHANNELS) })
  )

  await page.route('**/api/channels/history*', (route) => {
    const url = new URL(route.request().url())
    const channelParam = url.searchParams.get('channel')
    const parentId = url.searchParams.get('thread_parent_id')
    const body = parentId
      ? JSON.stringify(threadReplies[parentId] ?? [])
      : channelParam
        ? JSON.stringify(msgs.filter((m) => (m.channel || 'midtown') === channelParam))
        : JSON.stringify(msgs)
    route.fulfill({ status: 200, contentType: 'application/json', body })
  })

  await page.route('**/api/status', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(status) })
  )

  await page.route('**/api/questions', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ questions: [] }) })
  )

  await page.route('**/api/channels/message', (route) =>
    route.fulfill({ status: 204, body: '' })
  )

  await page.route('**/api/health', (route) =>
    route.fulfill({ status: 200, contentType: 'text/plain', body: 'ok' })
  )

  await page.route('**/api/projects/*/tmux-windows', (route) =>
    route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: 'no session' }) })
  )

  await page.route('**/api/projects/*/tmux-pane*', (route) =>
    route.fulfill({ status: 404, contentType: 'application/json', body: JSON.stringify({ error: 'no session' }) })
  )

  await page.route('**/api/lead-pane', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(leadPane) })
  )

  await page.route('**/api/usage', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ usage }) })
  )
}

export async function loadApp(page, overrides = {}) {
  await mockAllRoutes(page, overrides)
  await page.goto('/')
  await page.waitForSelector('[data-testid="channel-input"], textarea[placeholder*="Message to"]', {
    timeout: 10000,
  })
}

export async function getSentWebSocketMessages(page) {
  return page.evaluate(() => {
    const rawMessages = window.__mockWsMessages || []
    return rawMessages.map((raw) => {
      try {
        return JSON.parse(raw)
      } catch {
        return raw
      }
    })
  })
}

export async function dispatchWsMessage(page, payload) {
  await page.evaluate((data) => {
    window.__dispatchWsMessage?.(data)
  }, payload)
}
