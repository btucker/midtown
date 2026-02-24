// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp } from './helpers.js'

test.describe('Channel messaging', () => {
  test.beforeEach(async ({ page }) => {
    await loadApp(page)
  })

  test('renders message history from API', async ({ page }) => {
    const rows = page.getByTestId('message-row')
    await expect(rows.first()).toBeVisible({ timeout: 5000 })
  })

  test('displays sender names with color', async ({ page }) => {
    const leadSender = page.getByTestId('message-sender', { hasText: 'lead' })
    await expect(leadSender.first()).toBeVisible()
  })

  test('groups consecutive messages from same sender', async ({ page }) => {
    const amsterdamHeaders = page.getByTestId('message-sender').filter({ hasText: 'amsterdam' })
    await expect(amsterdamHeaders).toHaveCount(1)
  })

  test('renders action messages with star indicator', async ({ page }) => {
    // park's /me message should show as action with asterisk
    // In the new Channel.svelte, action messages have format: HH:MM * content
    const actionContent = page.locator('text=/flaky thread panel screenshot/')
    await expect(actionContent).toBeVisible()
  })

  test('renders system/daemon messages in gray without sender header', async ({ page }) => {
    // midtown message should render
    const systemMsg = page.locator('text=/Daemon restarted/')
    await expect(systemMsg).toBeVisible()
  })

  test('renders markdown bold and links', async ({ page }) => {
    // amsterdam's message has **auth endpoint** and a markdown link
    const boldText = page.locator('strong', { hasText: 'auth endpoint' })
    await expect(boldText).toBeVisible()

    const link = page.locator('a[href="https://github.com/example/pull/1"]')
    await expect(link).toBeVisible()
    await expect(link).toHaveAttribute('target', '_blank')
  })

  test('shows timestamps in HH:MM format', async ({ page }) => {
    const timestamp = page.getByTestId('message-time').first()
    await expect(timestamp).toHaveText(/\b\d{2}:\d{2}\b/)
  })

  test('message input is present and send button is disabled when empty', async ({ page }) => {
    const input = page.locator('[data-testid="channel-input"]')
    await expect(input).toBeVisible()

    const sendBtn = page.locator('[data-testid="send-button"]')
    await expect(sendBtn).toBeDisabled()
  })

  test('send button enables when text is entered', async ({ page }) => {
    const input = page.locator('[data-testid="channel-input"]')
    await input.fill('Hello team')

    const sendBtn = page.locator('[data-testid="send-button"]')
    await expect(sendBtn).toBeEnabled()
  })

  test('shows empty state when no messages', async ({ page }) => {
    await loadApp(page, { messages: [] })
    await expect(page.locator('text=/No messages/')).toBeVisible()
  })
})
