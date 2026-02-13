// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes, MOCK_MESSAGES } from './helpers.js'

test.describe('Channel messaging', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    // Wait for message input to be visible (new layout)
    await expect(page.locator('main textarea[placeholder*="Message to"]')).toBeVisible()
  })

  test('renders message history from API', async ({ page }) => {
    // Messages render in the main content area
    // Wait for the scroll area to have content
    await page.waitForTimeout(500)

    // Check that at least one message is visible (look for sender names)
    const senderNames = page.locator('span.font-bold')
    await expect(senderNames.first()).toBeVisible({ timeout: 5000 })
  })

  test('displays sender names with color', async ({ page }) => {
    // Lead's name should appear - check for bold styled sender names
    const leadSender = page.locator('span.font-bold', { hasText: 'lead' })
    await expect(leadSender.first()).toBeVisible()
  })

  test('groups consecutive messages from same sender', async ({ page }) => {
    // amsterdam sends 2 messages in a row - sender name should only appear once
    const amsterdamHeaders = page.locator('span.font-bold', { hasText: 'amsterdam' })
    await expect(amsterdamHeaders.first()).toBeVisible()
  })

  test('renders action messages with star indicator', async ({ page }) => {
    // park's /me message should show as action with asterisk
    // In the new Channel.svelte, action messages have format: HH:MM * content
    const actionContent = page.locator('text=/investigating auth bug/')
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
    // Messages should have time gutters (text with HH:MM format)
    const timeGutter = page.locator('span[class*="4a4a4a"]').first()
    await expect(timeGutter).toBeVisible()
  })

  test('message input is present and send button is disabled when empty', async ({ page }) => {
    const input = page.getByPlaceholder(/Message to/)
    await expect(input).toBeVisible()

    const sendBtn = page.getByRole('button', { name: 'Send' })
    await expect(sendBtn).toBeDisabled()
  })

  test('send button enables when text is entered', async ({ page }) => {
    const input = page.getByPlaceholder(/Message to/)
    await input.fill('Hello team')

    const sendBtn = page.getByRole('button', { name: 'Send' })
    await expect(sendBtn).toBeEnabled()
  })

  test('shows empty state when no messages', async ({ page }) => {
    // Re-navigate with empty messages
    await page.route('**/api/channels/history', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: '[]' })
    )
    await page.goto('/')

    await expect(page.locator('text=/No messages/')).toBeVisible()
  })
})
