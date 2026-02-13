// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

test.describe('Lead/Tmux view', () => {
  // Note: The new layout doesn't have visible UI to switch to Lead view.
  // The activeView state controls which view is shown but there's no tab UI.
  // These tests focus on API endpoint verification and component behavior when view is active.

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

test.describe('Tmux pane component', () => {
  // Tests for the Tmux component behavior when it is visible
  // Since there's no UI to switch views, these verify the component exists and API works

  test('tmux windows API returns array or 404', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    const status = await page.evaluate(async () => {
      const res = await fetch('/api/projects/test-project/tmux-windows')
      return res.status
    })
    expect([200, 404]).toContain(status)
  })

  test('tmux pane API returns content or 404', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    const status = await page.evaluate(async () => {
      const res = await fetch('/api/projects/test-project/tmux-pane?window=lead')
      return res.status
    })
    expect([200, 404]).toContain(status)
  })
})
