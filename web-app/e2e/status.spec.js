// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes, MOCK_STATUS } from './helpers.js'

test.describe('Status view', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    // Wait for the app to load
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })

  test('daemon status is available via API', async ({ page }) => {
    // The status is fetched and used internally - verify via page context
    const statusData = await page.evaluate(async () => {
      const response = await fetch('/api/status')
      return response.json()
    })
    expect(statusData.daemon).toBe('running')
  })

  test('coworker data is available via API', async ({ page }) => {
    const statusData = await page.evaluate(async () => {
      const response = await fetch('/api/status')
      return response.json()
    })
    expect(statusData.coworkers).toHaveLength(2)
    expect(statusData.coworkers[0].name).toBe('park')
    expect(statusData.coworkers[0].status).toBe('active')
  })

  test('task data is available via API', async ({ page }) => {
    const statusData = await page.evaluate(async () => {
      const response = await fetch('/api/status')
      return response.json()
    })
    expect(statusData.tasks).toHaveLength(3)
    expect(statusData.tasks[0].subject).toBe('Fix login bug')
  })

  test('coworker status is shown in sidebar footer', async ({ page }) => {
    // The sidebar footer shows coworker status - look for coworker names
    await expect(page.locator('text=/park|amsterdam/i').first()).toBeVisible()
  })

  test('shows empty coworker data when no coworkers', async ({ page }) => {
    await mockAllRoutes(page, {
      status: {
        daemon: 'idle',
        coworkers: [],
        tasks: [],
        pull_requests: [],
        merged_prs: [],
      },
    })
    await page.goto('/')

    // App should still load
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })
})
