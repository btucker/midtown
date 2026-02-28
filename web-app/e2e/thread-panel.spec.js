// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp, getSentWebSocketMessages } from './helpers.js'

test.describe('Thread panel', () => {
  test.beforeEach(async ({ page }) => {
    await loadApp(page)
  })

  test('opens from reply button and shows thread replies', async ({ page }) => {
    const message = page.getByTestId('message-row').first()
    const summary = message.getByTestId('thread-summary')
    await expect(summary).toBeVisible()
    await expect(summary).toContainText('2 replies')

    await message.hover()
    const replyButton = message.getByTestId('thread-reply-button')
    await replyButton.click()

    const panel = page.getByTestId('thread-panel')
    await expect(panel).toBeVisible()
    await expect(panel).toContainText('Docs updated to include browser support.')

    await page.getByTestId('thread-close-button').click()
    await expect(panel).toBeHidden()
  })

  test('sends a reply via Enter key', async ({ page }) => {
    const summary = page.getByTestId('thread-summary').first()
    await expect(summary).toBeVisible()
    await summary.click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()
    const input = page.getByTestId('thread-input').first()
    await input.fill('Thread reply from test')
    await page.keyboard.press('Enter')

    const messages = await getSentWebSocketMessages(page)
    expect(messages).toContainEqual(
      expect.objectContaining({
        content: 'Thread reply from test',
        channel: 'midtown',
        thread_parent_id: 'msg-1',
      })
    )
  })

  test('thread reply input clears after sending', async ({ page }) => {
    const summary = page.getByTestId('thread-summary').first()
    await expect(summary).toBeVisible()
    await summary.click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()
    const input = page.getByTestId('thread-input').first()
    await input.fill('Reply clear test')
    await page.keyboard.press('Enter')

    await expect(input).toHaveValue('')
  })

  test('submit does not programmatically blur the thread textarea', async ({ page }) => {
    const summary = page.getByTestId('thread-summary').first()
    await expect(summary).toBeVisible()
    await summary.click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()
    const input = page.getByTestId('thread-input').first()
    await input.fill('Reply blur test')

    await page.evaluate(() => {
      const ta = document.querySelector('[data-testid="thread-input"]')
      window.__programmaticBlurCount = 0
      const origBlur = ta.blur.bind(ta)
      ta.blur = () => { window.__programmaticBlurCount++; origBlur() }
    })

    await page.keyboard.press('Enter')

    const blurCount = await page.evaluate(() => window.__programmaticBlurCount)
    expect(blurCount).toBe(0)
    await expect(input).toHaveValue('')
  })

  test('Escape key closes the thread panel', async ({ page }) => {
    await page.getByTestId('thread-summary').first().click()
    const panel = page.getByTestId('thread-panel')
    await expect(panel).toBeVisible()
    await page.keyboard.press('Escape')
    await expect(panel).toBeHidden()
  })
})
