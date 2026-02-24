// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes, DETAIL_TASK_ID, THREAD_TASK_ID } from './helpers.js'

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
    expect(statusData.coworkers.length).toBeGreaterThanOrEqual(3)
    const park = statusData.coworkers.find((c) => c.name === 'park')
    expect(park).toMatchObject({ status: 'active', phase: 'dev', task_id: DETAIL_TASK_ID })
  })

  test('task data is available via API', async ({ page }) => {
    const statusData = await page.evaluate(async () => {
      const response = await fetch('/api/status')
      return response.json()
    })
    expect(statusData.tasks).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          id: DETAIL_TASK_ID,
          subject: 'Harden Playwright mocks',
          status: 'in_progress',
        }),
        expect.objectContaining({
          id: THREAD_TASK_ID,
          subject: 'Discuss thread UX polish',
          status: 'pending',
        }),
      ])
    )
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
