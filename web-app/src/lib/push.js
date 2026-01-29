import { writable } from 'svelte/store'

const API_BASE = '/api'

// Push notification state
export const pushSupported = writable(false)
export const pushPermission = writable('default') // 'default' | 'granted' | 'denied'
export const pushSubscribed = writable(false)

// Check if push notifications are supported
export function checkPushSupport() {
  const supported =
    'serviceWorker' in navigator &&
    'PushManager' in window &&
    'Notification' in window
  pushSupported.set(supported)
  if (supported) {
    pushPermission.set(Notification.permission)
  }
  return supported
}

// Fetch the VAPID public key from the server
async function getVapidPublicKey() {
  const res = await fetch(`${API_BASE}/push/vapid-key`)
  if (!res.ok) throw new Error('Failed to fetch VAPID key')
  const data = await res.json()
  return data.publicKey
}

// Convert base64url string to Uint8Array (for applicationServerKey)
function urlBase64ToUint8Array(base64String) {
  const padding = '='.repeat((4 - (base64String.length % 4)) % 4)
  const base64 = (base64String + padding).replace(/-/g, '+').replace(/_/g, '/')
  const rawData = atob(base64)
  const outputArray = new Uint8Array(rawData.length)
  for (let i = 0; i < rawData.length; ++i) {
    outputArray[i] = rawData.charCodeAt(i)
  }
  return outputArray
}

// Subscribe to push notifications
// Must be called from a user gesture (click/tap) for iOS compatibility
export async function subscribePush() {
  try {
    const permission = await Notification.requestPermission()
    pushPermission.set(permission)

    if (permission !== 'granted') {
      console.log('Push notification permission denied')
      return false
    }

    const registration = await navigator.serviceWorker.ready
    const vapidKey = await getVapidPublicKey()
    const applicationServerKey = urlBase64ToUint8Array(vapidKey)

    const subscription = await registration.pushManager.subscribe({
      userVisibleOnly: true,
      applicationServerKey,
    })

    // Extract keys and send to server
    const key = subscription.getKey('p256dh')
    const auth = subscription.getKey('auth')

    const p256dh = btoa(String.fromCharCode(...new Uint8Array(key)))
      .replace(/\+/g, '-')
      .replace(/\//g, '_')
      .replace(/=+$/, '')
    const authStr = btoa(String.fromCharCode(...new Uint8Array(auth)))
      .replace(/\+/g, '-')
      .replace(/\//g, '_')
      .replace(/=+$/, '')

    const res = await fetch(`${API_BASE}/push/subscribe`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        endpoint: subscription.endpoint,
        p256dh,
        auth: authStr,
      }),
    })

    if (!res.ok) throw new Error('Failed to register subscription')

    pushSubscribed.set(true)
    console.log('Push notification subscription successful')
    return true
  } catch (err) {
    console.error('Failed to subscribe to push:', err)
    return false
  }
}

// Unsubscribe from push notifications
export async function unsubscribePush() {
  try {
    const registration = await navigator.serviceWorker.ready
    const subscription = await registration.pushManager.getSubscription()

    if (subscription) {
      await subscription.unsubscribe()

      await fetch(`${API_BASE}/push/unsubscribe`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ endpoint: subscription.endpoint }),
      })
    }

    pushSubscribed.set(false)
    console.log('Push notification unsubscribed')
    return true
  } catch (err) {
    console.error('Failed to unsubscribe from push:', err)
    return false
  }
}

// Check current subscription status on load
export async function checkPushSubscription() {
  if (!checkPushSupport()) return

  try {
    const registration = await navigator.serviceWorker.ready
    const subscription = await registration.pushManager.getSubscription()
    pushSubscribed.set(subscription !== null)
  } catch (err) {
    console.error('Failed to check push subscription:', err)
  }
}
