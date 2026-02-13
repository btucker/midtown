// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes, MOCK_STATUS } from './helpers.js'

test.describe('Kanban board', () => {
  // Note: The new layout does not include the Kanban component in the main view.
  // The Kanban component exists but is not currently rendered in the app.
  // These tests are skipped until the Kanban is re-integrated into the layout.

  test.skip('renders four columns: Backlog, In Progress, Review, Done', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    await expect(kanban.locator('.column-title.backlog')).toContainText('Backlog')
    await expect(kanban.locator('.column-title.in-progress')).toContainText('In Progress')
    await expect(kanban.locator('.column-title.review')).toContainText('Review')
    await expect(kanban.locator('.column-title.done')).toContainText('Done')
  })

  test.skip('shows task counts in column headers', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    await expect(kanban.locator('.column-title.backlog .count')).toHaveText('(2)')
    await expect(kanban.locator('.column-title.in-progress .count')).toHaveText('(0)')
    await expect(kanban.locator('.column-title.review .count')).toHaveText('(2)')
    await expect(kanban.locator('.column-title.done .count')).toHaveText('(2)')
  })

  test.skip('renders backlog task cards with id and subject', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    const backlogColumn = kanban.locator('.kanban-column').first()
    const cards = backlogColumn.locator('.kanban-card')

    await expect(cards).toHaveCount(2)
    await expect(cards.first().locator('.task-id')).toHaveText('!1')
    await expect(cards.first().locator('.task-subject')).toHaveText('Fix login bug')
  })

  test.skip('renders in-progress task cards with owner', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    const inProgressColumn = kanban.locator('.kanban-column').nth(1)
    const cards = inProgressColumn.locator('.kanban-card')

    await expect(cards).toHaveCount(0)
    await expect(inProgressColumn.locator('.empty')).toContainText('No tasks')
  })

  test.skip('renders review column with PR cards and CI status dots', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    const reviewColumn = kanban.locator('.kanban-column').nth(2)
    const cards = reviewColumn.locator('.kanban-card')

    await expect(cards).toHaveCount(2)

    const firstCard = cards.first()
    await expect(firstCard.locator('.task-id')).toHaveText('!2')
    await expect(firstCard.locator('.task-subject')).toHaveText('Add auth endpoint')
    await expect(firstCard.locator('.card-detail .pr-link')).toHaveText('PR #42')

    const ciDot = firstCard.locator('.ci-dot')
    await expect(ciDot).toBeVisible()
  })

  test.skip('PR links point to GitHub', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    const reviewColumn = kanban.locator('.kanban-column').nth(2)
    const prLink = reviewColumn.locator('.card-detail .pr-link').first()

    await expect(prLink).toHaveAttribute('href', /\/pull\/42$/)
    await expect(prLink).toHaveAttribute('target', '_blank')
  })

  test.skip('renders done column with merged PRs', async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    const doneColumn = kanban.locator('.kanban-column').nth(3)
    const cards = doneColumn.locator('.kanban-card')

    await expect(cards).toHaveCount(2)
    await expect(cards.first().locator('.pr-link, .pr-link-text')).toContainText('PR#40')
    await expect(cards.first().locator('.pr-title')).toHaveText('chore: Update deps')
  })

  test.skip('shows empty state text when columns have no items', async ({ page }) => {
    await mockAllRoutes(page, {
      status: {
        daemon: 'running',
        coworkers: [],
        tasks: [],
        pull_requests: [],
        merged_prs: [],
      },
    })
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    await expect(kanban).toBeVisible()

    await expect(kanban.locator('.empty', { hasText: 'No tasks' }).first()).toBeVisible()
    await expect(kanban.locator('.empty', { hasText: 'No PRs' })).toBeVisible()
    await expect(kanban.locator('.empty', { hasText: 'No merged PRs' })).toBeVisible()
  })

  test.skip('kanban updates automatically via status polling', async ({ page }) => {
    let statusCallCount = 0
    const updatedStatus = {
      ...MOCK_STATUS,
      tasks: [
        ...MOCK_STATUS.tasks,
        { id: 4, subject: 'New polled task', status: 'pending', owner: null },
      ],
    }

    await page.unroute('**/api/status')
    await page.route('**/api/status', (route) => {
      statusCallCount++
      const data = statusCallCount <= 1 ? MOCK_STATUS : updatedStatus
      route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(data) })
    })
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    const backlogColumn = kanban.locator('.kanban-column').first()

    await expect(backlogColumn.locator('.kanban-card')).toHaveCount(2)
    await expect(backlogColumn.locator('.kanban-card')).toHaveCount(3, { timeout: 15000 })
    await expect(backlogColumn.locator('.kanban-card').last().locator('.task-subject')).toHaveText('New polled task')
  })

  test.skip('tasks with open PRs do not show in "In Progress" column', async ({ page }) => {
    await mockAllRoutes(page, {
      status: {
        daemon: 'running',
        coworkers: [],
        tasks: [
          { id: 1, subject: 'Fix login bug', status: 'pending', owner: null },
          { id: 2, subject: 'Add auth endpoint', status: 'in_progress', owner: 'park' },
          { id: 3, subject: 'Refactor database', status: 'in_progress', owner: 'amsterdam' },
        ],
        pull_requests: [
          {
            number: 42,
            title: 'feat: Add auth endpoint [Midtown #2]',
            author: 'park',
            status: 'awaiting review',
            task_id: 2,
            task_name: 'Add auth endpoint',
          },
        ],
        merged_prs: [],
      },
    })
    await page.goto('/')
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()

    const kanban = page.locator('.kanban >> nth=0')
    const inProgressColumn = kanban.locator('.kanban-column').nth(1)
    const reviewColumn = kanban.locator('.kanban-column').nth(2)

    await expect(inProgressColumn.locator('.kanban-card')).toHaveCount(1)
    await expect(inProgressColumn.locator('.task-id')).toHaveText('!3')
    await expect(inProgressColumn.locator('.task-subject')).toHaveText('Refactor database')

    await expect(reviewColumn.locator('.kanban-card')).toHaveCount(1)
    await expect(reviewColumn.locator('.task-id')).toHaveText('!2')
    await expect(reviewColumn.locator('.task-subject')).toHaveText('Add auth endpoint')
    await expect(reviewColumn.locator('.pr-link')).toContainText('PR')
  })
})
