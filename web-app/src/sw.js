import { precacheAndRoute, cleanupOutdatedCaches } from 'workbox-precaching'
import { clientsClaim } from 'workbox-core'

// Take control of all pages immediately when activated
clientsClaim()

// Clean up old caches from previous versions
cleanupOutdatedCaches()

// Workbox precaching - the manifest is injected by vite-plugin-pwa at build time
precacheAndRoute(self.__WB_MANIFEST)

// Listen for SKIP_WAITING message from the client (sent by vite-plugin-pwa's registerSW)
// This allows immediate activation of new service workers
self.addEventListener('message', (event) => {
  if (event.data && event.data.type === 'SKIP_WAITING') {
    self.skipWaiting()
  }
})

// Handle incoming push notifications
self.addEventListener('push', (event) => {
  if (!event.data) return

  let data
  try {
    data = event.data.json()
  } catch {
    data = { title: 'Midtown', body: event.data.text() }
  }

  const title = data.title || 'Midtown'
  const options = {
    body: data.body || '',
    icon: '/pwa-192x192.png',
    badge: '/pwa-192x192.png',
    tag: data.tag || 'default',
    renotify: true,
    data: {
      url: data.url || '/',
    },
  }

  // Skip notification if a client window is focused (user is already in the app)
  event.waitUntil(
    clients
      .matchAll({ type: 'window', includeUncontrolled: true })
      .then((windowClients) => {
        const hasFocusedClient = windowClients.some(
          (client) => client.visibilityState === 'visible' && client.focused
        )
        if (hasFocusedClient) return
        return self.registration.showNotification(title, options)
      })
  )
})

// Handle notification click - open or focus the PWA
self.addEventListener('notificationclick', (event) => {
  event.notification.close()

  const targetUrl = event.notification.data?.url || '/'

  event.waitUntil(
    clients
      .matchAll({ type: 'window', includeUncontrolled: true })
      .then((windowClients) => {
        for (const client of windowClients) {
          if (client.url.includes(self.location.origin) && 'focus' in client) {
            return client.focus()
          }
        }
        return clients.openWindow(targetUrl)
      })
  )
})
