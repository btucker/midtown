// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Lead/Terminal view', () => {
  // Note: The new layout doesn't have visible UI to switch to Lead view.
  // The activeView state controls which view is shown but there's no tab UI.
  // These tests focus on API endpoint verification and component behavior when view is active.

  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
  })

  test('zellij-web-url API endpoint is available', async ({ page }) => {
    await page.goto('/')
    const result = await page.evaluate(async () => {
      const res = await fetch('/api/projects/test-project/zellij-web-url')
      return { status: res.status, data: res.ok ? await res.json() : null }
    })
    // The endpoint should respond with 200 and return url + session
    expect(result.status).toBe(200)
    expect(result.data).toHaveProperty('url')
    expect(result.data).toHaveProperty('session')
  })

  test('app loads with channel view by default', async ({ page }) => {
    await page.goto('/')
    // The default view is 'board' which shows the channel
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })
})
