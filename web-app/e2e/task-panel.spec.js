// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp, DETAIL_TASK_ID } from './helpers.js'

test.describe('Task detail panel', () => {
  test('opens desktop detail panel with task metadata', async ({ page }) => {
    await loadApp(page)
    await page.getByRole('link', { name: `!${DETAIL_TASK_ID}` }).first().click()

    const detailPanel = page.getByTestId('detail-panel')
    await expect(detailPanel).toBeVisible()
    await expect(detailPanel).toContainText('Harden Playwright mocks')
    await expect(detailPanel).toContainText('Add WebSocket stubs and expand coverage.')
    await expect(detailPanel).toContainText('in_progress')
    await expect(detailPanel).toContainText('park')
  })

  test('opens mobile modal when tapping task link', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await loadApp(page)
    await page.getByRole('link', { name: `!${DETAIL_TASK_ID}` }).first().click()

    const modal = page.getByTestId('task-modal')
    await expect(modal).toBeVisible()
    await expect(modal).toContainText('Harden Playwright mocks')
    await expect(modal).toContainText('Add WebSocket stubs and expand coverage.')
  })
})
