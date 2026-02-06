// Lazy loader for the selkie WASM module.
// Caches the loaded module so init only runs once.

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
    })()
  }

  return initPromise
}
