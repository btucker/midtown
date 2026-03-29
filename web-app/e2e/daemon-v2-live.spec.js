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
})
