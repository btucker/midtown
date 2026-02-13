import { writable } from 'svelte/store'
import { getApiBase } from './api.js'

// Push notification state
export const pushSupported = writable(false)
export const pushPermission = writable('default') // 'default' | 'granted' | 'denied'
export const pushSubscribed = writable(false)
export const pushError = writable(null) // string | null — user-visible error message

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
  const res = await fetch(`${getApiBase()}/push/vapid-key`)
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
  pushError.set(null)
  try {
    const permission = await Notification.requestPermission()
    pushPermission.set(permission)

    if (permission !== 'granted') {
      pushError.set('Notification permission denied')
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

    const res = await fetch(`${getApiBase()}/push/subscribe`, {
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
    return true
  } catch (err) {
    console.error('Failed to subscribe to push:', err)
    pushError.set(err.message || 'Failed to subscribe')
    return false
  }
}

// Unsubscribe from push notifications
export async function unsubscribePush() {
  pushError.set(null)
  try {
    const registration = await navigator.serviceWorker.ready
    const subscription = await registration.pushManager.getSubscription()

    if (subscription) {
      await subscription.unsubscribe()

      await fetch(`${getApiBase()}/push/unsubscribe`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ endpoint: subscription.endpoint }),
      })
    }

    pushSubscribed.set(false)
    return true
  } catch (err) {
    console.error('Failed to unsubscribe from push:', err)
    pushError.set(err.message || 'Failed to unsubscribe')
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
