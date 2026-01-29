// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes, MOCK_MESSAGES } from './helpers.js'

test.describe('Channel messaging', () => {
  test.beforeEach(async ({ page }) => {
    await mockAllRoutes(page)
    await page.goto('/')
    // Wait for messages to render
    await expect(page.locator('.messages')).toBeVisible()
    await expect(page.locator('.message-line, .system-message').first()).toBeVisible()
  })

  test('renders message history from API', async ({ page }) => {
    // Regular messages render as .message-line, system messages as .system-message
    const allMessages = page.locator('.message-line, .system-message')
    // We have 5 mock messages: 1 regular (lead), 1 action (park), 1 system (midtown), 2 regular (amsterdam)
    await expect(allMessages).toHaveCount(5)
  })

  test('displays sender names with color', async ({ page }) => {
    // Lead's name should appear
    const leadSender = page.locator('.sender-name', { hasText: 'lead' })
    await expect(leadSender).toBeVisible()
    // Should have the lead color
    await expect(leadSender).toHaveCSS('color', 'rgb(215, 215, 135)') // #d7d787
  })

  test('groups consecutive messages from same sender', async ({ page }) => {
    // amsterdam sends 2 messages in a row - sender name should only appear once
    const amsterdamHeaders = page.locator('.sender-name', { hasText: 'amsterdam' })
    await expect(amsterdamHeaders).toHaveCount(1)

    // But there should be 2 message lines from amsterdam
    // amsterdam's messages have .message-text containing "PR is up" and "Tests are green"
    const prMsg = page.locator('.message-text', { hasText: 'PR is up' })
    const greenMsg = page.locator('.message-text', { hasText: 'Tests are green' })
    await expect(prMsg).toBeVisible()
    await expect(greenMsg).toBeVisible()
  })

  test('renders action messages with star indicator', async ({ page }) => {
    // park's /me message should show as action with star
    const actionStar = page.locator('.action-star')
    await expect(actionStar).toBeVisible()
    await expect(actionStar).toHaveText('*')

    // Action text should strip the /me prefix
    const actionText = page.locator('.action-text')
    await expect(actionText).toContainText('investigating auth bug')
    // Should NOT contain /me prefix
    await expect(actionText).not.toContainText('/me')
  })

  test('renders system/daemon messages in gray without sender header', async ({ page }) => {
    // midtown message should render as .system-message
    const systemMsg = page.locator('.system-message')
    await expect(systemMsg).toBeVisible()
    await expect(systemMsg).toContainText('Daemon restarted successfully')
  })

  test('renders markdown bold and links', async ({ page }) => {
    // amsterdam's message has **auth endpoint** and a markdown link
    const boldText = page.locator('.message-text strong', { hasText: 'auth endpoint' })
    await expect(boldText).toBeVisible()

    const link = page.locator('.message-text a[href="https://github.com/example/pull/1"]')
    await expect(link).toBeVisible()
    await expect(link).toHaveText('link')
    await expect(link).toHaveAttribute('target', '_blank')
  })

  test('shows timestamps in HH:MM format', async ({ page }) => {
    // Messages should have time gutters
    const timeGutter = page.locator('.time-gutter').first()
    await expect(timeGutter).toBeVisible()
    // Format should be HH:MM (24h)
    await expect(timeGutter).toHaveText(/^\d{2}:\d{2}$/)
  })

  test('message input is present and send button is disabled when empty', async ({ page }) => {
    const input = page.getByPlaceholder('Message to lead...')
    await expect(input).toBeVisible()

    const sendBtn = page.getByRole('button', { name: 'Send' })
    await expect(sendBtn).toBeDisabled()
  })

  test('send button enables when text is entered', async ({ page }) => {
    const input = page.getByPlaceholder('Message to lead...')
    await input.fill('Hello team')

    const sendBtn = page.getByRole('button', { name: 'Send' })
    await expect(sendBtn).toBeEnabled()
  })

  test('shows empty state when no messages', async ({ page }) => {
    // Re-navigate with empty messages
    await page.route('**/api/channel', (route) =>
      route.fulfill({ status: 200, contentType: 'application/json', body: '[]' })
    )
    await page.goto('/')

    await expect(page.locator('.empty-state')).toBeVisible()
    await expect(page.locator('.empty-state')).toContainText('No messages yet')
  })

  test('displays coworker current task in sender header', async ({ page }) => {
    // park has current_task "Add Playwright e2e tests" in mock status
    const senderTask = page.locator('.sender-task')
    await expect(senderTask.first()).toBeVisible()
    await expect(senderTask.first()).toContainText('Add Playwright e2e tests')
  })
})
