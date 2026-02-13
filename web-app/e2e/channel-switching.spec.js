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
    // Wait for the message input to be visible in new layout
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })

  test('displays all channels in sidebar', async ({ page }) => {
    // Check for channel names in the channel list - use more specific selector
    await expect(page.locator('button:has-text("#midtown")')).toBeVisible()
    await expect(page.locator('button:has-text("#auth-refactor")')).toBeVisible()
    await expect(page.locator('button:has-text("#ui-improvements")')).toBeVisible()
  })

  test('midtown channel is active by default', async ({ page }) => {
    // The input placeholder should show #midtown
    const input = page.locator('textarea[placeholder*="Message to"]')
    await expect(input).toHaveAttribute('placeholder', 'Message to #midtown...')
  })

  test('clicking a channel switches to that channel', async ({ page }) => {
    // Click auth-refactor channel button
    const authChannel = page.locator('button:has-text("#auth-refactor")')
    await authChannel.click()

    // Wait for API call to complete
    await page.waitForTimeout(100)

    // Input placeholder should update
    const input = page.locator('textarea[placeholder*="Message to"]')
    await expect(input).toHaveAttribute('placeholder', 'Message to #auth-refactor...')

    // Messages should update
    await expect(page.locator('text=/Starting auth-refactor work/')).toBeVisible()
  })

  test('shows messages when channel has messages', async ({ page }) => {
    // Switch to ui-improvements which has 1 message
    const uiChannel = page.locator('button:has-text("#ui-improvements")')
    await uiChannel.click()
    await page.waitForTimeout(100)

    // Should see the message
    await expect(page.locator('text=/Working on UI improvements/')).toBeVisible()
  })

  test('input placeholder updates with active channel', async ({ page }) => {
    // Initially should say "Message to #midtown..."
    const input = page.locator('textarea[placeholder*="Message to"]')
    await expect(input).toHaveAttribute('placeholder', 'Message to #midtown...')

    // Switch to auth-refactor
    const authChannel = page.locator('button:has-text("#auth-refactor")')
    await authChannel.click()
    await page.waitForTimeout(100)

    // Placeholder should update
    await expect(input).toHaveAttribute('placeholder', 'Message to #auth-refactor...')
  })

  test('channel switching preserves message history', async ({ page }) => {
    // Switch to auth-refactor
    const authChannel = page.locator('button:has-text("#auth-refactor")')
    await authChannel.click()
    await page.waitForTimeout(100)

    // Verify auth-refactor messages
    await expect(page.locator('text=/JWT implementation complete/')).toBeVisible()

    // Switch back to midtown
    const midtownChannel = page.locator('button:has-text("#midtown")')
    await midtownChannel.click()
    await page.waitForTimeout(100)

    // Original messages should still be there (cached)
    await expect(page.locator('text=/Starting work on main tasks/')).toBeVisible()
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

    const uiChannel = page.locator('button:has-text("#ui-improvements")')

    // Record when we click the channel
    const clickTime = Date.now()
    await uiChannel.click()

    // Input placeholder should update immediately (non-blocking)
    const input = page.locator('textarea[placeholder*="Message to"]')
    await expect(input).toHaveAttribute('placeholder', 'Message to #ui-improvements...', { timeout: 100 })

    const switchTime = Date.now() - clickTime

    // Verify switching happened fast (before the API response)
    expect(switchTime).toBeLessThan(150) // Allow some margin, but should be way faster than 200ms

    // Messages will appear after the API call completes (async)
    await expect(page.locator('text=/Working on UI improvements/')).toBeVisible({ timeout: 1000 })
  })
})
