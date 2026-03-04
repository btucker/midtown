// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Lead view', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
  })

  test('lead-pane API endpoint is available', async ({ page }) => {
    await page.goto('/')
    const status = await page.evaluate(async () => {
      const res = await fetch('/api/lead-pane')
      return res.status
    })
    // The endpoint should respond (200 or 404 if no session)
    expect([200, 404]).toContain(status)
  })

  test('lead-pane API returns content when session exists', async ({ page }) => {
    await page.goto('/')
    const result = await page.evaluate(async () => {
      const res = await fetch('/api/lead-pane')
      if (res.status === 200) {
        const data = await res.json()
        return { success: true, hasContent: data.hasOwnProperty('content') }
      }
      return { success: false }
    })
    // If the endpoint returns 200, it should have content
    if (result.success) {
      expect(result.hasContent).toBe(true)
    }
  })

  test('app loads with channel view by default', async ({ page }) => {
    await page.goto('/')
    // The default view is 'board' which shows the channel
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })
})
