// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes, MOCK_STATUS } from './helpers.js'

test.describe('Status tab', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await page.locator('nav').getByRole('button', { name: 'Status' }).click()
    await expect(page.locator('.status-container')).toBeVisible()
  })

  test('shows daemon status section with running indicator', async ({ page }) => {
    await expect(page.locator('h2', { hasText: 'Daemon' })).toBeVisible()

    const daemonStatus = page.locator('.daemon-status')
    await expect(daemonStatus).toBeVisible()
    await expect(daemonStatus.locator('.status-text')).toHaveText('running')

    // Green dot for running state
    const dot = daemonStatus.locator('.status-dot')
    await expect(dot).toBeVisible()
    await expect(dot).toHaveCSS('background-color', 'rgb(95, 175, 95)') // #5faf5f
  })

  test('shows coworker cards with name, status badge, and task', async ({ page }) => {
    const heading = page.locator('h2', { hasText: /Coworkers/ })
    await expect(heading).toContainText('(2)')

    const cards = page.locator('.coworker-card')
    await expect(cards).toHaveCount(2)

    // First coworker: park
    const parkCard = cards.first()
    await expect(parkCard.locator('.coworker-name')).toHaveText('park')
    await expect(parkCard.locator('.status-badge')).toHaveText('active')
    await expect(parkCard.locator('.task-text')).toHaveText('Add Playwright e2e tests')
    await expect(parkCard.locator('.started-at')).toContainText('Started:')

    // Second coworker: amsterdam (idle, no task)
    const amsterdamCard = cards.nth(1)
    await expect(amsterdamCard.locator('.coworker-name')).toHaveText('amsterdam')
    await expect(amsterdamCard.locator('.status-badge')).toHaveText('idle')
    // No current task shown
    await expect(amsterdamCard.locator('.task-text')).toHaveCount(0)
  })

  test('shows task list with id, subject, and status', async ({ page }) => {
    await expect(page.locator('h2', { hasText: 'Tasks' })).toBeVisible()

    const tasks = page.locator('.task-item')
    await expect(tasks).toHaveCount(3)

    const firstTask = tasks.first()
    await expect(firstTask.locator('.task-id')).toHaveText('#1')
    await expect(firstTask.locator('.task-subject')).toHaveText('Fix login bug')
    await expect(firstTask.locator('.task-status')).toHaveText('pending')
  })

  test('refresh button fetches fresh status data', async ({ page }) => {
    let fetchCount = 0
    await page.route('**/api/status', (route) => {
      fetchCount++
      route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify(MOCK_STATUS),
      })
    })

    const beforeCount = fetchCount
    await page.locator('.refresh-btn').click()
    // Wait for network request
    await page.waitForTimeout(500)
    expect(fetchCount).toBeGreaterThan(beforeCount)
  })

  test('shows empty states when no coworkers or tasks', async ({ page }) => {
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
    await page.locator('nav').getByRole('button', { name: 'Status' }).click()

    await expect(page.locator('.status-container p.empty', { hasText: 'No active coworkers' })).toBeVisible()
    await expect(page.locator('.status-container p.empty', { hasText: 'No tasks' })).toBeVisible()
  })

  test('daemon status shows correct color for idle state', async ({ page }) => {
    await mockAllRoutes(page, {
      status: { ...MOCK_STATUS, daemon: 'idle' },
    })
    await page.goto('/')
    await page.locator('nav').getByRole('button', { name: 'Status' }).click()

    const dot = page.locator('.daemon-status .status-dot')
    await expect(dot).toHaveCSS('background-color', 'rgb(215, 175, 95)') // #d7af5f yellow
  })
})
