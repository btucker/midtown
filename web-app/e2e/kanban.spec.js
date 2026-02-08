// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes, MOCK_STATUS } from './helpers.js'

test.describe('Kanban board', () => {
  // The compiled app may render two .kanban elements (collapsed + expanded).
  // Scope all kanban queries to the first instance.
  const kanbanSelector = '.kanban >> nth=0'

  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    await expect(page.locator(kanbanSelector)).toBeVisible()
  })

  test('renders four columns: Backlog, In Progress, Review, Done', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    await expect(kanban.locator('.column-title.backlog')).toContainText('Backlog')
    await expect(kanban.locator('.column-title.in-progress')).toContainText('In Progress')
    await expect(kanban.locator('.column-title.review')).toContainText('Review')
    await expect(kanban.locator('.column-title.done')).toContainText('Done')
  })

  test('shows task counts in column headers', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    // Backlog: 2 pending tasks, In Progress: 0 (task #2 has PR so it's in review)
    await expect(kanban.locator('.column-title.backlog .count')).toHaveText('(2)')
    await expect(kanban.locator('.column-title.in-progress .count')).toHaveText('(0)')
    await expect(kanban.locator('.column-title.review .count')).toHaveText('(2)')
    await expect(kanban.locator('.column-title.done .count')).toHaveText('(2)')
  })

  test('renders backlog task cards with id and subject', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    const backlogColumn = kanban.locator('.kanban-column').first()
    const cards = backlogColumn.locator('.kanban-card')

    // 2 pending tasks: Fix login bug (#1), Refactor database (#3)
    await expect(cards).toHaveCount(2)
    await expect(cards.first().locator('.task-id')).toHaveText('#1')
    await expect(cards.first().locator('.task-subject')).toHaveText('Fix login bug')
  })

  test('renders in-progress task cards with owner', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    const inProgressColumn = kanban.locator('.kanban-column').nth(1)
    const cards = inProgressColumn.locator('.kanban-card')

    // Task #2 has a PR, so In Progress is empty with default mock
    await expect(cards).toHaveCount(0)
    await expect(inProgressColumn.locator('.empty')).toContainText('No tasks')
  })

  test('renders review column with PR cards and CI status dots', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    const reviewColumn = kanban.locator('.kanban-column').nth(2)
    const cards = reviewColumn.locator('.kanban-card')

    await expect(cards).toHaveCount(2)

    // First PR: has task_id so shows task info on first line, PR link in card-detail
    const firstCard = cards.first()
    await expect(firstCard.locator('.task-id')).toHaveText('#2')
    await expect(firstCard.locator('.task-subject')).toHaveText('Add auth endpoint')
    await expect(firstCard.locator('.card-detail .pr-link')).toHaveText('PR#42')

    // CI dot should be present
    const ciDot = firstCard.locator('.ci-dot')
    await expect(ciDot).toBeVisible()
  })

  test('PR links point to GitHub', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    const reviewColumn = kanban.locator('.kanban-column').nth(2)
    // First PR has task_id, so PR link is in card-detail
    const prLink = reviewColumn.locator('.card-detail .pr-link').first()

    await expect(prLink).toHaveAttribute('href', /\/pull\/42$/)
    await expect(prLink).toHaveAttribute('target', '_blank')
  })

  test('renders done column with merged PRs', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    const doneColumn = kanban.locator('.kanban-column').nth(3)
    const cards = doneColumn.locator('.kanban-card')

    await expect(cards).toHaveCount(2)
    await expect(cards.first().locator('.pr-link')).toHaveText('PR#40')
    await expect(cards.first().locator('.pr-title')).toHaveText('chore: Update deps')
  })

  test('shows empty state text when columns have no items', async ({ page }) => {
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

    const kanban = page.locator(kanbanSelector)
    await expect(kanban).toBeVisible()

    // Each column should show its empty text
    await expect(kanban.locator('.empty', { hasText: 'No tasks' }).first()).toBeVisible()
    await expect(kanban.locator('.empty', { hasText: 'No PRs' })).toBeVisible()
    await expect(kanban.locator('.empty', { hasText: 'No merged PRs' })).toBeVisible()
  })

  test('kanban updates automatically via status polling', async ({ page }) => {
    // Set up a dynamic mock that adds a task after the first status fetch
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

    const kanban = page.locator(kanbanSelector)
    const backlogColumn = kanban.locator('.kanban-column').first()

    // Initially 2 backlog tasks
    await expect(backlogColumn.locator('.kanban-card')).toHaveCount(2)

    // Wait for the polling interval to fire — the new task should appear
    await expect(backlogColumn.locator('.kanban-card')).toHaveCount(3, { timeout: 15000 })
    await expect(backlogColumn.locator('.kanban-card').last().locator('.task-subject')).toHaveText('New polled task')
  })

  test('kanban board is visible on all tabs', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)

    // Channel tab (default) - kanban visible
    await expect(kanban).toBeVisible()

    // Status tab - kanban still visible
    await page.locator('nav').getByRole('button', { name: 'Status' }).click()
    await expect(kanban).toBeVisible()

    // Tmux tab - kanban still visible
    await page.locator('nav').getByRole('button', { name: 'Tmux' }).click()
    await expect(kanban).toBeVisible()
  })

  test('tasks with open PRs do not show in "In Progress" column', async ({ page }) => {
    // Task #2 has an associated open PR #42 (via [Midtown #2] in PR title)
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

    const kanban = page.locator(kanbanSelector)
    const inProgressColumn = kanban.locator('.kanban-column').nth(1)
    const reviewColumn = kanban.locator('.kanban-column').nth(2)

    // In Progress should only show task #3 (no PR)
    await expect(inProgressColumn.locator('.kanban-card')).toHaveCount(1)
    await expect(inProgressColumn.locator('.task-id')).toHaveText('#3')
    await expect(inProgressColumn.locator('.task-subject')).toHaveText('Refactor database')

    // Review should show PR #42 with task info
    await expect(reviewColumn.locator('.kanban-card')).toHaveCount(1)
    await expect(reviewColumn.locator('.task-id')).toHaveText('#2')
    await expect(reviewColumn.locator('.task-subject')).toHaveText('Add auth endpoint')
    await expect(reviewColumn.locator('.pr-link')).toHaveText('PR#42')
  })
})
