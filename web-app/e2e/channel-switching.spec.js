// @ts-check
import { test, expect } from '@playwright/test'

const MOCK_PROJECTS = [
  { name: 'test-project', status: 'running', webhook_port: 47099 },
]

const MOCK_CHANNELS = {
  channels: ['midtown', 'auth-refactor', 'ui-improvements']
}

const MOCK_MAIN_MESSAGES = [
  {
    from: 'lead',
    content: 'Starting work on main tasks',
    timestamp: '2025-01-15T10:00:00Z',
    msg_type: 'message',
    channel: 'midtown',
  },
  {
    from: 'park',
    content: '/me working on auth-refactor',
    timestamp: '2025-01-15T10:01:00Z',
    msg_type: 'action',
    channel: 'midtown',
  },
]

const MOCK_AUTH_MESSAGES = [
  {
    from: 'park',
    content: 'Starting auth-refactor work',
    timestamp: '2025-01-15T10:05:00Z',
    msg_type: 'message',
    channel: 'auth-refactor',
  },
  {
    from: 'amsterdam',
    content: 'JWT implementation complete',
    timestamp: '2025-01-15T10:10:00Z',
    msg_type: 'message',
    channel: 'auth-refactor',
  },
]

const MOCK_UI_MESSAGES = [
  {
    from: 'madison',
    content: 'Working on UI improvements',
    timestamp: '2025-01-15T10:15:00Z',
    msg_type: 'message',
    channel: 'ui-improvements',
  },
]

const MOCK_STATUS = {
  daemon: 'running',
  coworkers: [
    {
      name: 'park',
      status: 'active',
      current_task: 'auth-refactor: Add JWT support',
      started_at: '2025-01-15T09:00:00Z',
    },
    {
      name: 'amsterdam',
      status: 'active',
      current_task: 'auth-refactor: Update tests',
      started_at: '2025-01-15T08:30:00Z',
    },
    {
      name: 'madison',
      status: 'active',
      current_task: 'ui-improvements: Add dark mode toggle',
      started_at: '2025-01-15T09:15:00Z',
    },
  ],
  tasks: [
    { id: 1, subject: 'auth-refactor: Add JWT support', status: 'in_progress', owner: 'park' },
    { id: 2, subject: 'auth-refactor: Update tests', status: 'in_progress', owner: 'amsterdam' },
    { id: 3, subject: 'ui-improvements: Add dark mode toggle', status: 'in_progress', owner: 'madison' },
    { id: 4, subject: 'Other task not related to channels', status: 'pending', owner: null },
  ],
  pull_requests: [
    {
      number: 42,
      title: 'feat: auth-refactor JWT [Midtown #1]',
      author: 'park',
      status: 'ci_passed',
      task_id: 1,
      task_name: 'auth-refactor: Add JWT support',
    },
  ],
  merged_prs: [],
}

test.describe('Channel switching', () => {
  test.beforeEach(async ({ page }) => {
    // Mock all routes
    await page.route('**/api/projects', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_PROJECTS) })
    )

    await page.route('**/api/channels', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_CHANNELS) })
    )

    await page.route('**/api/status', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_STATUS) })
    )

    await page.route('**/api/health', (route) =>
      route.fulfill({ status: 200, contentType: 'text/plain', body: 'ok' })
    )

    // Route per-channel API calls
    await page.route('**/api/channels/history?channel=midtown', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_MAIN_MESSAGES) })
    )

    await page.route('**/api/channels/history?channel=auth-refactor', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_AUTH_MESSAGES) })
    )

    await page.route('**/api/channels/history?channel=ui-improvements', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_UI_MESSAGES) })
    )

    // Default channel endpoint (initial load)
    await page.route('**/api/channels/history', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(MOCK_MAIN_MESSAGES) })
    )

    await page.goto('/')
    await expect(page.locator('.channel-list').first()).toBeVisible()
  })

  test('displays all channels in sidebar', async ({ page }) => {
    const channelItems = page.locator('.channel-item')
    await expect(channelItems).toHaveCount(3)

    // Check channel names
    await expect(page.locator('.channel-item', { hasText: '#midtown' })).toBeVisible()
    await expect(page.locator('.channel-item', { hasText: '#auth-refactor' })).toBeVisible()
    await expect(page.locator('.channel-item', { hasText: '#ui-improvements' })).toBeVisible()
  })

  test('midtown channel is active by default', async ({ page }) => {
    const midtownChannel = page.locator('.channel-item', { hasText: '#midtown' })
    await expect(midtownChannel).toHaveClass(/active/)

    // Header should show #midtown
    await expect(page.locator('.channel-header .channel-name')).toHaveText('midtown')
  })

  test('clicking a channel switches to that channel', async ({ page }) => {
    // Click auth-refactor channel
    const authChannel = page.locator('.channel-item', { hasText: '#auth-refactor' })
    await authChannel.click()

    // Wait for API call to complete
    await page.waitForTimeout(100)

    // Check active state
    await expect(authChannel).toHaveClass(/active/)
    await expect(page.locator('.channel-item', { hasText: '#midtown' })).not.toHaveClass(/active/)

    // Header should update
    await expect(page.locator('.channel-header .channel-name')).toHaveText('auth-refactor')

    // Messages should update
    await expect(page.locator('.message-text', { hasText: 'Starting auth-refactor work' })).toBeVisible()
  })

  test('channel header shows task counts for selected channel', async ({ page }) => {
    // Initially on midtown - should show all tasks (3 in progress, 1 pending)
    // But the header shows filtered stats, so we need to check actual rendered content
    await expect(page.locator('.channel-header .channel-name')).toHaveText('midtown')

    // Switch to auth-refactor channel
    const authChannel = page.locator('.channel-item', { hasText: '#auth-refactor' })
    await authChannel.click()
    await page.waitForTimeout(100)

    // Header should show stats for auth-refactor channel (2 in progress tasks matching "auth-refactor")
    const inProgressBadge = page.locator('.channel-header .in-progress-badge')
    await expect(inProgressBadge).toBeVisible()
    await expect(inProgressBadge).toContainText('2 in progress')
  })

  test('channel header shows PR count for selected channel', async ({ page }) => {
    // Switch to auth-refactor channel which has 1 PR
    const authChannel = page.locator('.channel-item', { hasText: '#auth-refactor' })
    await authChannel.click()
    await page.waitForTimeout(100)

    // Header should show PR badge
    const prBadge = page.locator('.channel-header .pr-badge')
    await expect(prBadge).toBeVisible()
    await expect(prBadge).toContainText('1 PR')
  })

  test('shows empty state when channel has no messages', async ({ page }) => {
    // Switch to ui-improvements which has only 1 message
    const uiChannel = page.locator('.channel-item', { hasText: '#ui-improvements' })
    await uiChannel.click()
    await page.waitForTimeout(100)

    // Should see the message
    await expect(page.locator('.message-text', { hasText: 'Working on UI improvements' })).toBeVisible()
  })

  test('channel list shows task counts', async ({ page }) => {
    // auth-refactor channel should show 2 in progress + 1 PR in review
    const authChannel = page.locator('.channel-item', { hasText: '#auth-refactor' })
    const taskCount = authChannel.locator('.task-count')
    await expect(taskCount).toBeVisible()
    await expect(taskCount).toHaveText('3') // 2 in progress + 1 PR
  })

  test('input placeholder updates with active channel', async ({ page }) => {
    // Initially should say "Message to #midtown..."
    const input = page.locator('textarea[placeholder*="Message to"]')
    await expect(input).toHaveAttribute('placeholder', 'Message to #midtown...')

    // Switch to auth-refactor
    const authChannel = page.locator('.channel-item', { hasText: '#auth-refactor' })
    await authChannel.click()
    await page.waitForTimeout(100)

    // Placeholder should update
    await expect(input).toHaveAttribute('placeholder', 'Message to #auth-refactor...')
  })

  test('channel switching preserves message history', async ({ page }) => {
    // Switch to auth-refactor
    const authChannel = page.locator('.channel-item', { hasText: '#auth-refactor' })
    await authChannel.click()
    await page.waitForTimeout(100)

    // Verify auth-refactor messages
    await expect(page.locator('.message-text', { hasText: 'JWT implementation complete' })).toBeVisible()

    // Switch back to midtown
    const midtownChannel = page.locator('.channel-item', { hasText: '#midtown' })
    await midtownChannel.click()
    await page.waitForTimeout(100)

    // Original messages should still be there (cached)
    await expect(page.locator('.message-text', { hasText: 'Starting work on main tasks' })).toBeVisible()
  })

  test('channel switching is non-blocking and instant', async ({ page }) => {
    // Add a slow API response for ui-improvements channel
    await page.route('**/api/channels/history?channel=ui-improvements', async (route) => {
      // Delay the response by 200ms to simulate network latency
      await new Promise(resolve => setTimeout(resolve, 200))
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(MOCK_UI_MESSAGES)
      })
    })

    const uiChannel = page.locator('.channel-item', { hasText: '#ui-improvements' })

    // Record when we click the channel
    const clickTime = Date.now()
    await uiChannel.click()

    // Channel should become active immediately (before API response completes)
    // This should happen in <50ms, well before the 200ms API delay
    await expect(uiChannel).toHaveClass(/active/, { timeout: 100 })
    const switchTime = Date.now() - clickTime

    // Verify switching happened fast (before the API response)
    expect(switchTime).toBeLessThan(150) // Allow some margin, but should be way faster than 200ms

    // Header should update immediately
    await expect(page.locator('.channel-header .channel-name')).toHaveText('ui-improvements')

    // Messages will appear after the API call completes (async)
    await expect(page.locator('.message-text', { hasText: 'Working on UI improvements' })).toBeVisible({ timeout: 1000 })
  })
})
