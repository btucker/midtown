// @ts-check
import { test, expect } from '@playwright/test'

test.describe('Web UI', () => {
  test.describe('Channel page', () => {
    test('loads and shows header with connection status', async ({ page }) => {
      await page.goto('/')

      // Header should show app title
      await expect(page.locator('h1')).toHaveText('Midtown')

      // Connection status indicator should be present
      const status = page.locator('.connection-status')
      await expect(status).toBeVisible()
    })

    test('shows channel messages', async ({ page }) => {
      await page.goto('/')

      // Channel tab should be active by default
      const channelBtn = page.getByRole('button', { name: 'Channel' })
      await expect(channelBtn).toHaveClass(/active/)

      // Wait for messages to load from the API
      // The channel should have at least one message if the daemon is running
      const messageContainer = page.locator('.messages')
      await expect(messageContainer).toBeVisible()

      // Check that at least one message is rendered (from channel history)
      const messages = page.locator('.message')
      await expect(messages.first()).toBeVisible({ timeout: 5000 })
    })

    test('has a message input form', async ({ page }) => {
      await page.goto('/')

      const input = page.getByPlaceholder(/message/i)
      await expect(input).toBeVisible()

      const sendBtn = page.getByRole('button', { name: 'Send' })
      await expect(sendBtn).toBeVisible()
    })
  })

  test.describe('Status page', () => {
    test('loads and shows daemon status', async ({ page }) => {
      await page.goto('/')

      // Navigate to Status tab
      await page.getByRole('button', { name: 'Status' }).click()

      // Daemon section should be visible with status text
      await expect(page.locator('h2', { hasText: 'Daemon' })).toBeVisible()
      const daemonStatus = page.locator('.daemon-status')
      await expect(daemonStatus).toBeVisible()

      // Status dot should be present (indicates daemon state)
      await expect(page.locator('.status-dot')).toBeVisible()
    })

    test('shows coworker section', async ({ page }) => {
      await page.goto('/')
      await page.getByRole('button', { name: 'Status' }).click()

      // Coworkers heading should show count
      const coworkersHeading = page.locator('h2', { hasText: /Coworkers/ })
      await expect(coworkersHeading).toBeVisible()

      // Should show either coworker cards or "No active coworkers" message
      const coworkerList = page.locator('.coworker-list')
      const emptyMsg = page.locator('.empty', { hasText: 'No active coworkers' })
      const hasCoworkers = await coworkerList.isVisible().catch(() => false)
      const hasEmpty = await emptyMsg.isVisible().catch(() => false)
      expect(hasCoworkers || hasEmpty).toBe(true)
    })

    test('shows tasks section', async ({ page }) => {
      await page.goto('/')
      await page.getByRole('button', { name: 'Status' }).click()

      // Tasks heading should be present
      await expect(page.locator('h2', { hasText: 'Tasks' })).toBeVisible()

      // Should show either task items or "No tasks" message
      const taskList = page.locator('.task-list')
      const emptyMsg = page.locator('.empty', { hasText: 'No tasks' })
      const hasTasks = await taskList.isVisible().catch(() => false)
      const hasEmpty = await emptyMsg.isVisible().catch(() => false)
      expect(hasTasks || hasEmpty).toBe(true)
    })

    test('refresh button triggers data reload', async ({ page }) => {
      await page.goto('/')
      await page.getByRole('button', { name: 'Status' }).click()

      // Intercept the status API call
      const statusPromise = page.waitForResponse(
        (res) => res.url().includes('/api/status') && res.status() === 200
      )

      // Click refresh
      await page.getByRole('button', { name: 'Refresh' }).click()

      // Should trigger a new /api/status fetch
      const response = await statusPromise
      expect(response.ok()).toBe(true)
    })
  })

  test.describe('Tab navigation', () => {
    test('switches between Channel and Status tabs', async ({ page }) => {
      await page.goto('/')

      // Channel should be active initially
      const channelBtn = page.getByRole('button', { name: 'Channel' })
      const statusBtn = page.getByRole('button', { name: 'Status' })
      await expect(channelBtn).toHaveClass(/active/)

      // Click Status tab
      await statusBtn.click()
      await expect(statusBtn).toHaveClass(/active/)

      // Status content should be visible
      await expect(page.locator('.status-container')).toBeVisible()

      // Click back to Channel
      await channelBtn.click()
      await expect(channelBtn).toHaveClass(/active/)

      // Channel content should be visible
      await expect(page.locator('.messages')).toBeVisible()
    })
  })

  test.describe('WebSocket connection', () => {
    test('establishes WebSocket connection', async ({ page }) => {
      // Listen for WebSocket connections
      const wsPromise = page.waitForEvent('websocket', {
        timeout: 10000,
      })

      await page.goto('/')

      const ws = await wsPromise
      expect(ws.url()).toContain('/api/ws')

      // Connection status should show "Connected"
      await expect(page.locator('.connection-status')).toHaveText('Connected', {
        timeout: 5000,
      })
    })
  })

  test.describe('API endpoints', () => {
    test('health endpoint returns ok', async ({ request }) => {
      const response = await request.get('/api/health')
      expect(response.ok()).toBe(true)
      expect(await response.text()).toBe('ok')
    })

    test('status endpoint returns valid JSON', async ({ request }) => {
      const response = await request.get('/api/status')
      expect(response.ok()).toBe(true)

      const data = await response.json()
      expect(data).toHaveProperty('daemon')
      expect(data).toHaveProperty('coworkers')
      expect(data).toHaveProperty('tasks')
      expect(Array.isArray(data.coworkers)).toBe(true)
      expect(Array.isArray(data.tasks)).toBe(true)
    })

    test('channel endpoint returns message array', async ({ request }) => {
      const response = await request.get('/api/channel')
      expect(response.ok()).toBe(true)

      const data = await response.json()
      expect(Array.isArray(data)).toBe(true)

      // Each message should have expected fields
      if (data.length > 0) {
        expect(data[0]).toHaveProperty('from')
        expect(data[0]).toHaveProperty('content')
        expect(data[0]).toHaveProperty('timestamp')
      }
    })
  })
})
