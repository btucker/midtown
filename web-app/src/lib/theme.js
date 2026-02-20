import { writable } from 'svelte/store'

const STORAGE_KEY = 'midtown-theme'

function getInitialTheme() {
  const stored = typeof localStorage !== 'undefined' ? localStorage.getItem(STORAGE_KEY) : null
  return stored === 'light' ? 'light' : 'dark'
}

export const theme = writable(getInitialTheme())

export function toggleTheme() {
  theme.update(t => {
    const next = t === 'dark' ? 'light' : 'dark'
    localStorage.setItem(STORAGE_KEY, next)
    return next
  })
}
