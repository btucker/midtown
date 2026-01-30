// @ts-check
import { test, expect } from '@playwright/test'
import { mockAllRoutes } from './helpers.js'

const MOCK_PROJECT = {
  name: 'myproject',
  status: 'running',
  webhook_port: 9999,
}

// Valid 65-byte P-256 public key in base64url (matches real VAPID key format)
const MOCK_VAPID_KEY =
  'BEl3x0k2v0qIPIQdNH4GrHcT40Qr1dmhgcFNOKSFBE6Xp1hLwp8bMCq1bGOFnSemIaIwJad4e-LTDsaJEh2Cgs'

/**
 * Mock the push browser APIs (serviceWorker, PushManager, Notification)
 * and the project list so the app switches to a per-project daemon.
 */
async function setupPushTest(page, { permission = 'default', subscribed = false } = {}) {
  // Mock project list so app switches to per-project daemon
  await page.route('**/api/projects', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify([MOCK_PROJECT]),
    })
  )

  // Mock push endpoints from any origin — the subscribe/unsubscribe flow
  // must complete regardless of whether the JS routes to gateway or daemon.
  // Routing correctness is verified by inspecting captured request URLs.
  await page.route('**/api/push/**', (route) => {
    const url = route.request().url()
    if (url.includes('/vapid-key')) {
      return route.fulfill({
        status: 200,
        contentType: 'application/json',
        body: JSON.stringify({ publicKey: MOCK_VAPID_KEY }),
      })
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: '{}' })
  })

  await mockAllRoutes(page)

  // Inject push API stubs before the page loads
  await page.addInitScript(
    ({ permission, subscribed }) => {
      // Stub Notification
      // @ts-ignore
      window.Notification = {
        permission,
        requestPermission: async () => {
          // @ts-ignore
          window.Notification.permission = 'granted'
          return 'granted'
        },
      }

      // Create realistic ArrayBuffer keys (p256dh = 65 bytes, auth = 16 bytes)
      function fakeKey(len) {
        const buf = new ArrayBuffer(len)
        const view = new Uint8Array(buf)
        for (let i = 0; i < len; i++) view[i] = (i + 1) % 256
        return buf
      }

      const mockSubscription = subscribed
        ? {
            endpoint: 'https://fcm.example.com/test-endpoint',
            getKey: (name) => fakeKey(name === 'p256dh' ? 65 : 16),
            unsubscribe: async () => true,
          }
        : null

      // Stub PushManager
      const pushManager = {
        getSubscription: async () => mockSubscription,
        subscribe: async () => ({
          endpoint: 'https://fcm.example.com/test-endpoint',
          getKey: (name) => fakeKey(name === 'p256dh' ? 65 : 16),
          unsubscribe: async () => true,
        }),
      }

      // Stub serviceWorker
      Object.defineProperty(navigator, 'serviceWorker', {
        value: {
          ready: Promise.resolve({ pushManager }),
          register: async () => ({ pushManager }),
          controller: null,
          addEventListener: () => {},
        },
        configurable: true,
      })

      // Stub PushManager on window for feature detection
      // @ts-ignore
      window.PushManager = class PushManager {}
    },
    { permission, subscribed }
  )
}

test.describe('Push notifications', () => {
  test('bell icon is visible when push is supported', async ({ page }) => {
    await setupPushTest(page)
    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toBeVisible()
    // Should show muted bell (not subscribed)
    await expect(bell).toHaveText('🔕')
  })

  test('bell icon shows active bell when subscribed', async ({ page }) => {
    await setupPushTest(page, { permission: 'granted', subscribed: true })
    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toBeVisible()
    await expect(bell).toHaveText('🔔')
    await expect(bell).toHaveClass(/subscribed/)
  })

  test('bell icon is disabled when permission is denied', async ({ page }) => {
    await setupPushTest(page, { permission: 'denied' })
    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toBeVisible()
    await expect(bell).toBeDisabled()
    await expect(bell).toHaveClass(/denied/)
  })

  test('clicking bell subscribes via push API', async ({ page }) => {
    const pushRequests = []
    await setupPushTest(page)

    page.on('request', (req) => {
      if (req.url().includes('/push/')) {
        pushRequests.push(req.url())
      }
    })

    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toBeVisible()
    await bell.click()

    // Wait for the subscription API call
    await page.waitForResponse(
      (res) => res.url().includes('/push/subscribe') && res.status() === 200
    )

    // Verify the push API was called (VAPID key + subscribe)
    expect(pushRequests.some((url) => url.includes('/push/vapid-key'))).toBe(true)
    expect(pushRequests.some((url) => url.includes('/push/subscribe'))).toBe(true)

    // Bell should now show subscribed state
    await expect(bell).toHaveText('🔔')
  })

  test('clicking bell while subscribed unsubscribes via push API', async ({ page }) => {
    const pushRequests = []
    await setupPushTest(page, { permission: 'granted', subscribed: true })

    page.on('request', (req) => {
      if (req.url().includes('/push/')) {
        pushRequests.push(req.url())
      }
    })

    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toHaveText('🔔')
    await bell.click()

    // Wait for the unsubscribe API call
    await page.waitForResponse(
      (res) => res.url().includes('/push/unsubscribe') && res.status() === 200
    )

    expect(pushRequests.some((url) => url.includes('/push/unsubscribe'))).toBe(true)

    // Bell should now show unsubscribed state
    await expect(bell).toHaveText('🔕')
  })

  // This test validates the fix from PR #342: push.js must use getApiBase()
  // to route API calls to the per-project daemon. It fails when the gateway
  // serves a stale static build (pre-fix JS), but passes in CI with a fresh build.
  test('push API calls route to per-project daemon, not gateway', async ({ page }) => {
    const pushRequests = []
    await setupPushTest(page)

    page.on('request', (req) => {
      if (req.url().includes('/push/')) {
        pushRequests.push(req.url())
      }
    })

    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toBeVisible()
    await bell.click()

    await page.waitForResponse(
      (res) => res.url().includes('/push/subscribe') && res.status() === 200
    )

    // The fix: push API calls should go to http://localhost:9999/api/push/*
    // (per-project daemon), not /api/push/* on the gateway (port 47022)
    const subscribeReq = pushRequests.find((url) => url.includes('/push/subscribe'))
    expect(subscribeReq).toContain(`localhost:${MOCK_PROJECT.webhook_port}`)

    const vapidReq = pushRequests.find((url) => url.includes('/push/vapid-key'))
    expect(vapidReq).toContain(`localhost:${MOCK_PROJECT.webhook_port}`)
  })

  test('bell title reflects permission state', async ({ page }) => {
    await setupPushTest(page)
    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toHaveAttribute('title', 'Enable push notifications')
  })

  test('denied bell title explains blocked state', async ({ page }) => {
    await setupPushTest(page, { permission: 'denied' })
    await page.goto('/')

    const bell = page.locator('.push-toggle')
    await expect(bell).toHaveAttribute('title', 'Notifications blocked in browser settings')
  })
})
