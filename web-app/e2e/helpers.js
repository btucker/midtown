// @ts-check

/**
 * Shared mock data and route-setup helpers for e2e tests.
 */

export const MOCK_MESSAGES = [
  {
    from: 'lead',
    content: 'Starting the sprint',
    timestamp: '2025-01-15T10:00:00Z',
    msg_type: 'message',
  },
  {
    from: 'park',
    content: '/me investigating auth bug',
    timestamp: '2025-01-15T10:01:00Z',
    msg_type: 'action',
  },
  {
    from: 'midtown',
    content: 'Daemon restarted successfully',
    timestamp: '2025-01-15T10:02:00Z',
    msg_type: 'message',
  },
  {
    from: 'amsterdam',
    content: 'PR is up: **auth endpoint** [link](https://github.com/example/pull/1)',
    timestamp: '2025-01-15T10:03:00Z',
    msg_type: 'message',
  },
  {
    from: 'amsterdam',
    content: 'Tests are green',
    timestamp: '2025-01-15T10:04:00Z',
    msg_type: 'message',
  },
]

export const MOCK_STATUS = {
  daemon: 'running',
  coworkers: [
    {
      name: 'park',
      status: 'active',
      current_task: 'Add Playwright e2e tests',
      started_at: '2025-01-15T09:00:00Z',
    },
    {
      name: 'madison',
      status: 'active',
      current_task: 'Fix autocomplete bug',
      started_at: '2025-01-15T09:15:00Z',
    },
    {
      name: 'amsterdam',
      status: 'idle',
      current_task: null,
      started_at: '2025-01-15T08:30:00Z',
    },
  ],
  tasks: [
    { id: 1, subject: 'Fix login bug', status: 'pending', owner: null },
    { id: 2, subject: 'Add auth endpoint', status: 'in_progress', owner: 'park' },
    { id: 3, subject: 'Refactor database', status: 'pending', owner: null },
  ],
  pull_requests: [
    {
      number: 42,
      title: 'feat: Add auth endpoint [Midtown #2]',
      author: 'park',
      status: 'awaiting review',
      task_id: 2,
      task_name: 'Add auth endpoint',
    },
    {
      number: 43,
      title: 'fix: Login redirect',
      author: 'amsterdam',
      status: 'approved',
      task_id: null,
      task_name: null,
    },
  ],
  merged_prs: [
    { number: 40, title: 'chore: Update deps', mergedAt: '2025-01-14T12:00:00Z' },
    { number: 39, title: 'feat: Add status page', mergedAt: '2025-01-13T15:00:00Z' },
  ],
}

export const MOCK_PROJECTS = [
  { name: 'test-project', status: 'running', webhook_port: 47099 },
]

export const MOCK_LEAD_PANE = {
  content: 'claude> Running tests...\n$ npm test\nAll 42 tests passed.',
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
  const msgs = overrides.messages ?? MOCK_MESSAGES
  const status = overrides.status ?? MOCK_STATUS
  const leadPane = overrides.leadPane ?? MOCK_LEAD_PANE

  await page.route('**/api/projects', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_PROJECTS) })
  )

  await page.route('**/api/channels/history', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(msgs) })
  )

  await page.route('**/api/status', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(status) })
  )

  await page.route('**/api/health', (route) =>
    route.fulfill({ status: 200, contentType: 'text/plain', body: 'ok' })
  )

  await page.route('**/api/lead-pane', (route) =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(leadPane) })
  )
}
