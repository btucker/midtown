// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp, MOCK_STATUS } from './helpers.js'

const AUTOCOMPLETE_STATUS = {
  ...MOCK_STATUS,
  coworkers: [
    ...MOCK_STATUS.coworkers,
    {
      name: 'mercer',
      status: 'active',
      phase: 'dev',
      task_id: null,
      pr_number: null,
      health: 'green',
      progress: null,
      current_task: 'Review PR #42',
      started_at: '2025-01-15T10:00:00Z',
    },
  ],
}

test.describe('Autocomplete', () => {
  test.beforeEach(async ({ page }) => {
    await loadApp(page, { status: AUTOCOMPLETE_STATUS })
  })

  test('shows coworker suggestions for @ mentions', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.click()
    await input.type('@m')

    const dropdown = page.getByTestId('autocomplete-dropdown')
    await expect(dropdown).toBeVisible()
    await expect(dropdown).toContainText('@madison')
    await expect(dropdown).toContainText('@mercer')
  })

  test('navigates task suggestions with arrow keys and inserts selection', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.fill('Working on ')
    await input.type('!')

    const dropdown = page.getByTestId('autocomplete-dropdown')
    await expect(dropdown).toBeVisible()
    await expect(dropdown).toContainText('Harden Playwright mocks')

    await page.keyboard.press('ArrowDown')
    await page.keyboard.press('Enter')

    await expect(input).toHaveValue(/!303/)
  })

  test('Escape hides autocomplete dropdown', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.type('@p')
    const dropdown = page.getByTestId('autocomplete-dropdown')
    await expect(dropdown).toBeVisible()

    await page.keyboard.press('Escape')
    await expect(dropdown).toBeHidden()
  })
})
