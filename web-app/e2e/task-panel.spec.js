// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp, DETAIL_TASK_ID } from './helpers.js'

test.describe('Task card in thread panel', () => {
  test('opens thread panel with task card when clicking task link', async ({ page }) => {
    await loadApp(page)
    await page.getByRole('link', { name: `!${DETAIL_TASK_ID}` }).first().click()

    const threadPanel = page.getByTestId('thread-panel')
    await expect(threadPanel).toBeVisible()
    const taskCard = threadPanel.getByTestId('task-card')
    await expect(taskCard).toBeVisible()
    await expect(taskCard).toContainText(`!${DETAIL_TASK_ID}`)
    await expect(taskCard).toContainText('Harden Playwright mocks')
    await expect(taskCard).toContainText('in progress')
    await expect(taskCard).toContainText('park')
  })

  test('opens mobile thread panel with task card when tapping task link', async ({ page }) => {
    await page.setViewportSize({ width: 390, height: 844 })
    await loadApp(page)
    await page.getByRole('link', { name: `!${DETAIL_TASK_ID}` }).first().click()

    const threadPanel = page.getByTestId('thread-panel-mobile')
    await expect(threadPanel).toBeVisible()
    const taskCard = threadPanel.getByTestId('task-card')
    await expect(taskCard).toBeVisible()
    await expect(taskCard).toContainText(`!${DETAIL_TASK_ID}`)
    await expect(taskCard).toContainText('Harden Playwright mocks')
  })
})
