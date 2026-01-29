import App from './App.svelte'
import { mount } from 'svelte'
import { registerSW } from 'virtual:pwa-register'

const app = mount(App, {
  target: document.getElementById('app'),
})

// Register service worker with auto-update
// When a new version is available, it will automatically reload
registerSW({
  onNeedRefresh() {
    // New content available - reload to get latest version
    window.location.reload()
  },
  onOfflineReady() {
    console.log('App ready to work offline')
  },
})

export default app
