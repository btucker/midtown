// Lazy loader for the selkie WASM module.
// Caches the loaded module so init only runs once.
// On failure, clears the cached promise so subsequent calls can retry.

let selkie = null
let initPromise = null

export async function getSelkie() {
  if (selkie) return selkie

  if (!initPromise) {
    initPromise = (async () => {
      const { default: initWasm, initialize, render } = await import('selkie-rs')
      await initWasm()
      initialize({ startOnLoad: false })
      selkie = { render }
      return selkie
    })().catch((err) => {
      initPromise = null
      throw err
    })
  }

  return initPromise
}
