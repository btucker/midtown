import App from './App.svelte'
import { mount } from 'svelte'
import { registerSW } from 'virtual:pwa-register'

const app = mount(App, {
  target: document.getElementById('app'),
})

// Register service worker with auto-update
// When a new version is available, prompt user before reloading
registerSW({
  onNeedRefresh() {
    // Let user choose when to update to avoid losing unsaved work
    if (confirm('A new version is available. Reload to update?')) {
      window.location.reload()
    }
  },
  onOfflineReady() {
    console.log('App ready to work offline')
  },
})

export default app
