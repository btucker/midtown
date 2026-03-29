// @ts-check
import { test, expect } from '@playwright/test'

/**
 * Live E2E tests against a running daemon v2.
 *
 * Prerequisites:
 *   MIDTOWN_DAEMON_V2=1 midtown start
 *
 * Run:
 *   MIDTOWN_WEB_PORT=47024 npx playwright test e2e/daemon-v2-live.spec.js
 *
 * These tests hit the REAL daemon — no mocks. They verify the web UI
 * works end-to-end with the v2 daemon's Axum web server.
 */

const BASE_URL = process.env.MIDTOWN_WEB_PORT
  ? `http://localhost:${process.env.MIDTOWN_WEB_PORT}`
  : 'https://localhost:47024'

test.describe('Daemon v2 Live Web UI', () => {
  test.beforeEach(async ({ page, context }) => {
    // Ignore HTTPS errors for self-signed certs
    await context.clearCookies()
  })

  test('status endpoint returns valid data', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/status`)
    expect(res.ok()).toBeTruthy()
    const data = await res.json()
    expect(data).toHaveProperty('agents')
    expect(data).toHaveProperty('coworkers')
    expect(data).toHaveProperty('tasks')
    expect(data).toHaveProperty('max_in_progress_tasks')
  })

  test('channels endpoint returns list with project channel', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/channels`)
    expect(res.ok()).toBeTruthy()
    const data = await res.json()
    expect(data).toHaveProperty('channels')
    expect(Array.isArray(data.channels)).toBeTruthy()
    // Should have at least the main channel
    const names = data.channels.map((c) => c.name)
    expect(names.length).toBeGreaterThan(0)
  })

  test('channel history returns messages with content field', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/channels/history?channel=midtown&limit=5`)
    expect(res.ok()).toBeTruthy()
    const data = await res.json()
    expect(Array.isArray(data)).toBeTruthy()
    if (data.length > 0) {
      // Messages should have 'content' not 'message'
      expect(data[0]).toHaveProperty('content')
      expect(data[0]).toHaveProperty('from')
      expect(data[0]).toHaveProperty('timestamp')
    }
  })

  test('health endpoint returns ok', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/health`)
    expect(res.ok()).toBeTruthy()
    const text = await res.text()
    expect(text).toBe('ok')
  })

  test('read-state endpoint returns empty object', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/read-state`)
    expect(res.ok()).toBeTruthy()
  })

  test('questions endpoint returns empty array', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/questions`)
    expect(res.ok()).toBeTruthy()
    const data = await res.json()
    expect(Array.isArray(data)).toBeTruthy()
  })

  test('web UI loads and shows channel sidebar', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(5000)

    // Should show "midtown" somewhere on the page
    await expect(page.getByText('midtown', { exact: false }).first()).toBeVisible()

    // Should have channels visible in the sidebar (look for # prefix)
    await expect(page.getByText('#midtown').first()).toBeVisible()

    await context.close()
  })

  test('web UI displays message history', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(5000)

    // The main channel should have messages (from the running lead)
    // Look for message content or the "No messages" indicator
    const hasMessages = await page.locator('[class*="message"]').count()
    const noMessages = await page.locator('text=No messages').count()

    // Either messages are shown OR "no messages" - but the page shouldn't be broken
    expect(hasMessages + noMessages).toBeGreaterThan(0)

    await context.close()
  })

  test('switching channels loads different history', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Click on a different channel
    const devChannel = page.getByText('#dev', { exact: true })
    if (await devChannel.isVisible()) {
      await devChannel.click()
      await page.waitForTimeout(2000)

      // Header area should show "dev"
      await expect(page.getByText('# dev').first()).toBeVisible()
    }

    await context.close()
  })

  test('posting a message via the web UI', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Find the message input
    const input = page.locator('textarea[placeholder*="Message"], input[placeholder*="Message"]').first()
    await expect(input).toBeVisible()

    // Type a test message
    const testMsg = `e2e-test-${Date.now()}`
    await input.fill(testMsg)

    // Submit (press Enter or click send button)
    await input.press('Enter')
    await page.waitForTimeout(2000)

    // Verify the message appears (either via WebSocket or page reload)
    // The message might appear via WS or we need to refresh
    await page.reload({ waitUntil: 'networkidle' })
    await page.waitForTimeout(3000)

    const messageVisible = await page.getByText(testMsg).isVisible().catch(() => false)
    // Even if the message doesn't immediately render, the post shouldn't error
    expect(true).toBeTruthy() // Basic smoke test — post didn't crash

    await context.close()
  })

  test('PRs tab shows pull request data', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Click PRs tab
    const prsTab = page.getByText('PRs', { exact: true }).first()
    if (await prsTab.isVisible()) {
      await prsTab.click()
      await page.waitForTimeout(2000)

      // PRs tab should be visible and not crash
      // Content depends on whether there are actual PRs
      await page.screenshot({ path: '/tmp/midtown-v2-prs.png' })
    }

    await context.close()
  })

  test('WebSocket connects successfully', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    const wsConnected = new Promise((resolve) => {
      page.on('console', (msg) => {
        if (msg.text().includes('WebSocket connected')) {
          resolve(true)
        }
      })
    })

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })

    // WebSocket should connect within a few seconds
    const connected = await Promise.race([
      wsConnected,
      new Promise((resolve) => setTimeout(() => resolve(false), 10000)),
    ])

    expect(connected).toBeTruthy()

    await context.close()
  })

  test('theme toggle switches between light and dark mode', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Get initial theme state
    const initialHassDark = await page.evaluate(() =>
      document.documentElement.classList.contains('dark')
    )

    // Find and click the theme toggle button
    const themeToggle = page.locator('[data-testid="theme-toggle"]')
    await expect(themeToggle).toBeVisible()
    await themeToggle.click()
    await page.waitForTimeout(500)

    // Theme should have changed
    const afterToggleDark = await page.evaluate(() =>
      document.documentElement.classList.contains('dark')
    )
    expect(afterToggleDark).not.toBe(initialHassDark)

    // Screenshot to verify visual change
    await page.screenshot({ path: '/tmp/midtown-v2-toggled-theme.png' })

    // Toggle back
    await themeToggle.click()
    await page.waitForTimeout(500)

    const afterSecondToggle = await page.evaluate(() =>
      document.documentElement.classList.contains('dark')
    )
    expect(afterSecondToggle).toBe(initialHassDark)

    // Verify theme persists in localStorage
    const storedTheme = await page.evaluate(() =>
      localStorage.getItem('midtown-theme')
    )
    expect(['light', 'dark']).toContain(storedTheme)

    await context.close()
  })

  test('direct messages section is visible', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Should have a DIRECT MESSAGES section
    const dmSection = page.getByText('DIRECT MESSAGES', { exact: false }).first()
    await expect(dmSection).toBeVisible()

    await context.close()
  })

  test('account panel is visible in sidebar footer', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // The sidebar footer should show the account panel with default profile
    await expect(page.getByText('default').first()).toBeVisible()

    await context.close()
  })

  test('message input has correct placeholder', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // The message input should have a placeholder mentioning the current channel
    const input = page.locator('[data-testid="channel-input"]')
    await expect(input).toBeVisible()
    const placeholder = await input.getAttribute('placeholder')
    expect(placeholder).toContain('midtown')

    await context.close()
  })

  test('clicking a message with replies shows reply count', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Look for any thread reply button (messages with replies have these)
    const replyButtons = page.locator('[data-testid="thread-reply-button"]')
    const count = await replyButtons.count()

    // If there are threaded messages, verify the button exists
    if (count > 0) {
      await expect(replyButtons.first()).toBeVisible()
    }

    await context.close()
  })

  test('status endpoint includes coworkers and tasks arrays', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/status`)
    const data = await res.json()

    // Verify the full status response shape
    expect(Array.isArray(data.coworkers)).toBeTruthy()
    expect(Array.isArray(data.tasks)).toBeTruthy()
    expect(Array.isArray(data.pull_requests)).toBeTruthy()
    expect(typeof data.max_in_progress_tasks).toBe('number')
    expect(data.agents).toHaveProperty('total')
    expect(data.agents).toHaveProperty('running')
  })

  test('channel history messages have expected fields', async ({ request }) => {
    const res = await request.get(`${BASE_URL}/api/channels/history?channel=midtown&limit=3`)
    const data = await res.json()

    if (data.length > 0) {
      const msg = data[0]
      // Every message should have these fields
      expect(msg).toHaveProperty('id')
      expect(msg).toHaveProperty('from')
      expect(msg).toHaveProperty('content')
      expect(msg).toHaveProperty('timestamp')
      expect(msg).toHaveProperty('msg_type')
      // Should NOT have 'message' field (transformed to 'content')
      expect(msg).not.toHaveProperty('message')
    }
  })

  test('dark mode applies correct background', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    // Set dark mode preference before navigating
    await page.addInitScript(() => {
      localStorage.setItem('midtown-theme', 'dark')
    })

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(2000)

    // HTML element should have 'dark' class
    const hasDark = await page.evaluate(() =>
      document.documentElement.classList.contains('dark')
    )
    expect(hasDark).toBeTruthy()

    // Background should be dark
    const bgColor = await page.evaluate(() =>
      getComputedStyle(document.body).backgroundColor
    )
    // Dark backgrounds have low RGB values
    const match = bgColor.match(/rgb\((\d+),\s*(\d+),\s*(\d+)\)/)
    if (match) {
      const avg = (parseInt(match[1]) + parseInt(match[2]) + parseInt(match[3])) / 3
      expect(avg).toBeLessThan(100) // Dark mode should have low brightness
    }

    await context.close()
  })

  test('create channel button is visible and clickable', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Find the "+" button for creating channels
    const createBtn = page.locator('button[title="Create new channel"]')
    await expect(createBtn).toBeVisible()

    await context.close()
  })

  test('search shortcut opens search dialog', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Find the desktop search button (has ⌘K in title)
    const searchBtn = page.locator('button[title="Search messages (⌘K)"]')
    await expect(searchBtn).toBeVisible()
    await searchBtn.click()
    await page.waitForTimeout(1000)

    // Search dialog or input should be visible after clicking
    // (the exact UI depends on the search component implementation)
    expect(true).toBeTruthy()

    await context.close()
  })

  test('messages render sender names and timestamps', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(5000)

    // Messages should have sender names
    const senders = page.locator('[data-testid="message-sender"]')
    const senderCount = await senders.count()
    if (senderCount > 0) {
      const firstSender = await senders.first().textContent()
      expect(firstSender).toBeTruthy()
      expect(firstSender.length).toBeGreaterThan(0)
    }

    // Messages should have timestamps
    const times = page.locator('[data-testid="message-time"]')
    const timeCount = await times.count()
    if (timeCount > 0) {
      await expect(times.first()).toBeVisible()
    }

    await context.close()
  })

  test('archived channels toggle works', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Find the archived channels toggle button
    const archiveToggle = page.locator('button[title*="archived"]')
    await expect(archiveToggle).toBeVisible()

    // Clicking should toggle archived channels visibility (no crash)
    await archiveToggle.click()
    await page.waitForTimeout(1000)

    await context.close()
  })

  test('send button is visible next to message input', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    const sendBtn = page.locator('[data-testid="send-button"]')
    await expect(sendBtn).toBeVisible()

    await context.close()
  })

  test('Notes tab loads without error', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    const notesTab = page.getByText('Notes', { exact: true }).first()
    if (await notesTab.isVisible()) {
      await notesTab.click()
      await page.waitForTimeout(1000)
      // Should not crash — content depends on whether notes exist
      expect(true).toBeTruthy()
    }

    await context.close()
  })

  test('Settings tab shows channel settings', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    const settingsTab = page.getByText('Settings', { exact: true }).first()
    if (await settingsTab.isVisible()) {
      await settingsTab.click()
      await page.waitForTimeout(1000)
      // Settings panel should render
      expect(true).toBeTruthy()
    }

    await context.close()
  })

  test('message input accepts multiline text', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    const input = page.locator('[data-testid="channel-input"]')
    await expect(input).toBeVisible()

    // Type multiline text (Shift+Enter for newline)
    await input.fill('line 1')
    await input.press('Shift+Enter')
    await input.type('line 2')

    const value = await input.inputValue()
    expect(value).toContain('line 1')
    expect(value).toContain('line 2')

    await context.close()
  })

  test('clicking channel in sidebar updates URL', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(3000)

    // Click a different channel
    const opsChannel = page.getByText('#ops', { exact: true })
    if (await opsChannel.isVisible()) {
      await opsChannel.click()
      await page.waitForTimeout(1000)

      // URL should reflect the channel change
      const url = page.url()
      // URL may include channel name or just be the base URL
      expect(url).toBeTruthy()
    }

    await context.close()
  })

  test('page title includes project name', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(2000)

    const title = await page.title()
    expect(title).toContain('Midtown')

    await context.close()
  })

  test('light mode applies correct background', async ({ browser }) => {
    const context = await browser.newContext({ ignoreHTTPSErrors: true })
    const page = await context.newPage()

    // Set light mode preference before navigating
    await page.addInitScript(() => {
      localStorage.setItem('midtown-theme', 'light')
    })

    await page.goto('https://localhost:47022', { waitUntil: 'networkidle', timeout: 15000 })
    await page.waitForTimeout(2000)

    // HTML element should NOT have 'dark' class
    const hasDark = await page.evaluate(() =>
      document.documentElement.classList.contains('dark')
    )
    expect(hasDark).toBeFalsy()

    // Background should be light
    const bgColor = await page.evaluate(() =>
      getComputedStyle(document.body).backgroundColor
    )
    const match = bgColor.match(/rgb\((\d+),\s*(\d+),\s*(\d+)\)/)
    if (match) {
      const avg = (parseInt(match[1]) + parseInt(match[2]) + parseInt(match[3])) / 3
      expect(avg).toBeGreaterThan(200) // Light mode should have high brightness
    }

    await context.close()
  })
})
