// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp, getSentWebSocketMessages } from './helpers.js'

test.describe('Message input behaviors', () => {
  test.beforeEach(async ({ page }) => {
    await loadApp(page)
  })

  test('Enter sends message over WebSocket', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.fill('Keyboard shortcut message')
    await page.keyboard.press('Enter')

    const messages = await getSentWebSocketMessages(page)
    expect(messages).toContainEqual(expect.objectContaining({ content: 'Keyboard shortcut message' }))
  })

  test('Shift+Enter inserts newline without sending', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.fill('Hello')
    await page.keyboard.press('Shift+Enter')
    await expect(input).toHaveValue('Hello\n')

    const messages = await getSentWebSocketMessages(page)
    expect(messages).toEqual([])
  })

  test('Escape closes autocomplete dropdown', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.type('@p')
    const dropdown = page.getByTestId('autocomplete-dropdown')
    await expect(dropdown).toBeVisible()

    await page.keyboard.press('Escape')
    await expect(dropdown).toBeHidden()
  })

  test('input clears after sending via Enter', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.fill('Test message')
    await page.keyboard.press('Enter')

    await expect(input).toHaveValue('')
  })

  test('input clears after sending via Send button', async ({ page }) => {
    const input = page.getByTestId('channel-input')
    await input.fill('Button send test')
    await page.getByTestId('send-button').click()

    await expect(input).toHaveValue('')
  })

  test('pasting an image shows a removable preview', async ({ page }) => {
    await page.evaluate(() => {
      const input = document.querySelector('[data-testid="channel-input"]')
      const dataTransfer = new DataTransfer()
      const blob = new Blob(['fake'], { type: 'image/png' })
      const file = new File([blob], 'preview.png', { type: 'image/png' })
      dataTransfer.items.add(file)
      const event = new Event('paste', { bubbles: true })
      Object.defineProperty(event, 'clipboardData', { value: dataTransfer, configurable: true })
      input.dispatchEvent(event)
    })

    const preview = page.getByTestId('file-preview')
    await expect(preview).toBeVisible()
    await page.getByLabel('Remove file').click()
    await expect(preview).toBeHidden()
  })
})
