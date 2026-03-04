// @ts-check
import { test, expect } from '@playwright/test'
import { loadApp, dispatchWsMessage, THREAD_PARENT_ID } from './helpers.js'

// msg-1 is a top-level message with reply_count: 2, NOT backed by a task.
// THREAD_PARENT_ID (msg-thread-parent) IS backed by task #303 (message_id match).
const NON_TASK_MSG_ID = 'msg-1'

test.describe('Sidebar thread tracking', () => {
  test.beforeEach(async ({ page }) => {
    // Clear localStorage before each test so tracked threads don't leak between tests
    await page.addInitScript(() => {
      localStorage.removeItem('midtown_tracked_threads')
      localStorage.removeItem('midtown_thread_unread')
      localStorage.removeItem('midtown_dismissed_threads')
    })
    await loadApp(page)
    // Navigate to #midtown where mock messages live
    await page.getByLabel('Select channel midtown').click()
    // Wait for messages to load
    await page.getByTestId('message-row').first().waitFor({ state: 'visible' })
  })

  test('opening a thread tracks it in the sidebar', async ({ page }) => {
    // Initially no sidebar thread rows
    await expect(page.getByTestId('sidebar-thread-row')).toHaveCount(0)

    // Open a thread by clicking the thread summary on msg-1
    const message = page.getByTestId('message-row').first()
    const summary = message.getByTestId('thread-summary')
    await expect(summary).toBeVisible()
    await summary.click()

    // Thread panel should open
    await expect(page.getByTestId('thread-panel')).toBeVisible()

    // Now a sidebar thread row should appear under #midtown
    const threadRow = page.getByTestId('sidebar-thread-row')
    await expect(threadRow).toHaveCount(1)

    // The subject should contain text from the parent message
    const subject = threadRow.getByTestId('sidebar-thread-subject')
    await expect(subject).toBeVisible()
    await expect(subject).not.toHaveText('')
  })

  test('no count badge shown when thread has no unreads', async ({ page }) => {
    // Open thread for msg-1 — no unreads yet
    const summary = page.getByTestId('thread-summary').first()
    await summary.click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()

    // Only the unread badge exists; no separate reply count
    await expect(page.getByTestId('sidebar-thread-unread-badge')).toHaveCount(0)
  })

  test('WS thread reply increments unread count', async ({ page }) => {
    // First, open the thread to track it
    const summary = page.getByTestId('thread-summary').first()
    await summary.click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()

    // Close the thread panel so new replies count as unread
    await page.getByTestId('thread-close-button').click()
    await expect(page.getByTestId('thread-panel')).toBeHidden()

    // Initially no unread badge
    await expect(page.getByTestId('sidebar-thread-unread-badge')).toHaveCount(0)

    // Simulate a WS reply in the tracked thread from a non-user sender
    await dispatchWsMessage(page, {
      type: 'channel_message',
      data: {
        id: 'ws-reply-1',
        channel: 'midtown',
        from: 'park',
        content: 'New reply from coworker',
        timestamp: new Date().toISOString(),
        msg_type: 'message',
        thread_parent_id: NON_TASK_MSG_ID,
      },
    })

    // Unread badge should appear with count 1
    const badge = page.getByTestId('sidebar-thread-unread-badge')
    await expect(badge).toBeVisible()
    await expect(badge).toHaveText('1')

    // Send another reply — count should increment to 2
    await dispatchWsMessage(page, {
      type: 'channel_message',
      data: {
        id: 'ws-reply-2',
        channel: 'midtown',
        from: 'amsterdam',
        content: 'Another reply',
        timestamp: new Date().toISOString(),
        msg_type: 'message',
        thread_parent_id: NON_TASK_MSG_ID,
      },
    })

    await expect(badge).toHaveText('2')
  })

  test('clicking sidebar thread opens panel and clears unreads', async ({ page }) => {
    // Open + close thread to track it
    const summary = page.getByTestId('thread-summary').first()
    await summary.click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()
    await page.getByTestId('thread-close-button').click()
    await expect(page.getByTestId('thread-panel')).toBeHidden()

    // Add an unread
    await dispatchWsMessage(page, {
      type: 'channel_message',
      data: {
        id: 'ws-reply-3',
        channel: 'midtown',
        from: 'park',
        content: 'Yet another reply',
        timestamp: new Date().toISOString(),
        msg_type: 'message',
        thread_parent_id: NON_TASK_MSG_ID,
      },
    })
    await expect(page.getByTestId('sidebar-thread-unread-badge')).toBeVisible()

    // Click the sidebar thread row
    await page.getByTestId('sidebar-thread-row').click()

    // Thread panel should reopen
    await expect(page.getByTestId('thread-panel')).toBeVisible()

    // Unread badge should be gone
    await expect(page.getByTestId('sidebar-thread-unread-badge')).toHaveCount(0)
  })

  test('dismiss button removes thread from sidebar', async ({ page }) => {
    // Track a thread
    const summary = page.getByTestId('thread-summary').first()
    await summary.click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()
    await expect(page.getByTestId('sidebar-thread-row')).toHaveCount(1)

    // Close the thread panel first (so we can interact with the sidebar dismiss)
    await page.getByTestId('thread-close-button').click()
    await expect(page.getByTestId('thread-panel')).toBeHidden()

    // Hover the thread row to reveal the dismiss button, then click it
    const threadRow = page.getByTestId('sidebar-thread-row')
    await threadRow.hover()
    const dismissBtn = threadRow.getByTestId('sidebar-thread-dismiss')
    // Force click since button may be opacity:0 until hover transition completes
    await dismissBtn.click({ force: true })

    // Thread should be gone
    await expect(page.getByTestId('sidebar-thread-row')).toHaveCount(0)

    // Re-opening the same thread should NOT re-track it (it's dismissed)
    await page.getByTestId('thread-summary').first().click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()
    await expect(page.getByTestId('sidebar-thread-row')).toHaveCount(0)
  })

  test('task-backed thread does not appear in sidebar thread list', async ({ page }) => {
    // THREAD_PARENT_ID is backed by task #303 (message_id match).
    // Open it — it should be tracked but immediately suppressed by dedup.
    const messages = page.getByTestId('message-row')
    // Find the message with THREAD_PARENT_ID and click its thread summary
    const threadParentRow = page.locator(`[data-testid="message-row"]`).filter({
      has: page.locator(`[data-testid="thread-summary"]`),
    })
    // There are two messages with thread summaries; click the second one (THREAD_PARENT_ID)
    const secondThread = threadParentRow.nth(1)
    await secondThread.getByTestId('thread-summary').click()
    await expect(page.getByTestId('thread-panel')).toBeVisible()

    // The thread should NOT appear as a sidebar thread row because it's task-backed
    // (task #303 has message_id = THREAD_PARENT_ID)
    // Only msg-1 thread rows could appear, but we didn't open that one
    await expect(page.getByTestId('sidebar-thread-row')).toHaveCount(0)
  })

  // Note: localStorage persistence works at the store level (stores init from localStorage
  // on module load), but switchProject() unconditionally clears thread stores during app
  // initialization, which defeats reload persistence. This is by design — project switch
  // should clear project-scoped state. Within a session, localStorage keeps data across
  // the various store operations.
})
