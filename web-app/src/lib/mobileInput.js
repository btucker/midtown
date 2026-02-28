/**
 * Clear a textarea and guard against mobile keyboard composition buffer
 * repopulating the value after it was cleared.
 *
 * On iOS Safari and mobile Chrome, the virtual keyboard maintains a composition
 * buffer for autocorrect and predictive text. When Svelte clears the binding
 * value, the browser may repopulate the textarea when the composition session
 * finalizes (compositionend). This function registers a one-shot listener to
 * re-clear if that happens — no blur/focus cycle, no keyboard dismissal.
 *
 * @param {HTMLTextAreaElement | null} element
 * @param {() => void} syncBinding - re-sets the Svelte state binding to ''
 */
export function clearMobileTextarea(element, syncBinding) {
  if (!element) return
  element.value = ''
  element.addEventListener('compositionend', () => {
    element.value = ''
    syncBinding()
  }, { once: true })
}
