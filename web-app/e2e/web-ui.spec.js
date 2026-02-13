// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Web UI', () => {
  test.describe('Channel page', () => {
    test('loads and shows header with connection status', async ({ page }) => {
      await mockAllRoutes(page)
      await page.goto('/')

      // Connection status indicator should be present in sidebar header
      const status = page.locator('.connection-dot')
      await expect(status).toBeVisible()
    })

    test('shows channel messages', async ({ page }) => {
      await mockAllRoutes(page)
      await page.goto('/')

      // Wait for messages to load from the API
      const messageContainer = page.locator('main')
      await expect(messageContainer).toBeVisible()

      // Check that message input is rendered
      const input = page.locator('main textarea[placeholder*="Message to"]')
      await expect(input).toBeVisible({ timeout: 5000 })
    })

    test('has a message input form', async ({ page }) => {
      await mockAllRoutes(page)
      await page.goto('/')

      const input = page.getByPlaceholder(/Message to/)
      await expect(input).toBeVisible()

      const sendBtn = page.getByRole('button', { name: 'Send' })
      await expect(sendBtn).toBeVisible()
    })
  })

  test.describe('Sidebar', () => {
    test('shows project selector', async ({ page }) => {
      await mockAllRoutes(page)
      await page.goto('/')

      // Project selector should be visible in sidebar header
      await expect(page.locator('.project-selector')).toBeVisible()
    })

    test('shows channel list', async ({ page }) => {
      await mockAllRoutes(page)
      await page.goto('/')

      // Channel list should be visible in sidebar content - use more specific selector
      await expect(page.locator('button:has-text("#midtown")')).toBeVisible()
    })

    test('shows push toggle when supported', async ({ page }) => {
      await mockAllRoutes(page)
      await page.goto('/')

      // Push toggle is in sidebar header
      const pushToggle = page.locator('.push-toggle')
      await expect(pushToggle).toBeVisible()
    })
  })

  test.describe('WebSocket connection', () => {
    test.skip('establishes WebSocket connection', async ({ page }) => {
      // This test requires a real WebSocket server, skip in mock environment
      await mockAllRoutes(page)

      // Listen for WebSocket connections
      const wsPromise = page.waitForEvent('websocket', {
        timeout: 10000,
      })

      await page.goto('/')

      const ws = await wsPromise
      expect(ws.url()).toContain('/api/ws')

      // Connection status should show "Connected"
      await expect(page.locator('.connection-dot.connected')).toBeVisible({
        timeout: 5000,
      })
    })
  })

  test.describe('API endpoints', () => {
    test.skip('health endpoint returns ok', async ({ page }) => {
      // This test requires the dev server to be running with API routes
      await mockAllRoutes(page)
      const response = await page.evaluate(async () => {
        const res = await fetch('/api/health')
        return { ok: res.ok, text: await res.text() }
      })
      expect(response.ok).toBe(true)
      expect(response.text).toBe('ok')
    })

    test.skip('status endpoint returns valid JSON', async ({ page }) => {
      // This test requires the dev server to be running with API routes
      await mockAllRoutes(page)
      const data = await page.evaluate(async () => {
        const res = await fetch('/api/status')
        return res.json()
      })

      expect(data).toHaveProperty('daemon')
      expect(data).toHaveProperty('coworkers')
      expect(data).toHaveProperty('tasks')
      expect(Array.isArray(data.coworkers)).toBe(true)
      expect(Array.isArray(data.tasks)).toBe(true)
    })

    test.skip('channel endpoint returns message array', async ({ page }) => {
      // This test requires the dev server to be running with API routes
      await mockAllRoutes(page)
      const data = await page.evaluate(async () => {
        const res = await fetch('/api/channels/history')
        return res.json()
      })

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

test.describe('Mobile layout', () => {
  test('mobile header is visible on small screens', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 375, height: 667 })
    await page.goto('/')

    // Mobile header with sidebar trigger should be visible (uses .mobile-header class)
    const header = page.locator('.mobile-header')
    await expect(header).toBeVisible()
  })

  test('active channel is shown in mobile header', async ({ page }) => {
    await mockAllRoutes(page)
    await page.setViewportSize({ width: 375, height: 667 })
    await page.goto('/')

    // Active channel display should show current channel (uses .mobile-channel class)
    await expect(page.locator('.mobile-channel')).toBeVisible()
  })
})
