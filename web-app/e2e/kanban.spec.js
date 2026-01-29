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
    // Backlog: 2 pending tasks, In Progress: 1
    await expect(kanban.locator('.column-title.backlog .count')).toHaveText('(2)')
    await expect(kanban.locator('.column-title.in-progress .count')).toHaveText('(1)')
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

    await expect(cards).toHaveCount(1)
    await expect(cards.first().locator('.task-id')).toHaveText('#2')
    await expect(cards.first().locator('.task-subject')).toHaveText('Add auth endpoint')
    await expect(cards.first().locator('.card-detail')).toContainText('park')
  })

  test('renders review column with PR cards and CI status dots', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    const reviewColumn = kanban.locator('.kanban-column').nth(2)
    const cards = reviewColumn.locator('.kanban-card')

    await expect(cards).toHaveCount(2)

    // First PR: awaiting review (yellow dot)
    const firstCard = cards.first()
    await expect(firstCard.locator('.pr-link')).toHaveText('PR#42')
    await expect(firstCard.locator('.pr-title')).toHaveText('feat: Add auth endpoint')
    await expect(firstCard.locator('.card-detail')).toContainText('park')

    // CI dot should be present
    const ciDot = firstCard.locator('.ci-dot')
    await expect(ciDot).toBeVisible()
  })

  test('PR links point to GitHub', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)
    const reviewColumn = kanban.locator('.kanban-column').nth(2)
    const prLink = reviewColumn.locator('.pr-link').first()

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

  test('kanban board is visible on all tabs', async ({ page }) => {
    const kanban = page.locator(kanbanSelector)

    // Channel tab (default) - kanban visible
    await expect(kanban).toBeVisible()

    // Status tab - kanban still visible
    await page.locator('nav').getByRole('button', { name: 'Status' }).click()
    await expect(kanban).toBeVisible()

    // Lead tab - kanban still visible
    await page.locator('nav').getByRole('button', { name: 'Lead' }).click()
    await expect(kanban).toBeVisible()
  })
})
