/**
 * Shared BiggerPicture lightbox instance
 *
 * BiggerPicture recommends using a single shared instance rather than
 * creating multiple instances. This module provides a singleton that can
 * be imported by any component that needs to display images/SVGs in a lightbox.
 */
import BiggerPicture from 'bigger-picture'

let bpInstance = null

/**
 * Get or create the shared BiggerPicture instance
 * @returns {Object|null} The BiggerPicture instance, or null if not in browser
 */
export function getBiggerPicture() {
  // Check if we're in a browser environment
  if (typeof window === 'undefined') {
    return null
  }

  if (!bpInstance) {
    bpInstance = BiggerPicture({
      target: document.body,
    })
  }

  return bpInstance
}

/**
 * Calculate the initial scale to fit an SVG to viewport width
 * Matches the 95% width fit-to-width behavior from the old MermaidModal
 *
 * @param {string} svgString - The SVG markup as a string
 * @returns {number} The scale factor to apply (default 1 if calculation fails)
 */
export function calculateFitToWidthScale(svgString) {
  try {
    // Parse SVG to get intrinsic dimensions
    const parser = new DOMParser()
    const doc = parser.parseFromString(svgString, 'image/svg+xml')
    const svg = doc.documentElement

    // Try to get dimensions from viewBox first, then width/height attributes
    let intrinsicWidth = 0
    let intrinsicHeight = 0

    if (svg.hasAttribute('viewBox')) {
      const viewBox = svg.getAttribute('viewBox').split(/\s+|,/)
      intrinsicWidth = parseFloat(viewBox[2])
      intrinsicHeight = parseFloat(viewBox[3])
    } else {
      intrinsicWidth = parseFloat(svg.getAttribute('width')) || 0
      intrinsicHeight = parseFloat(svg.getAttribute('height')) || 0
    }

    if (!intrinsicWidth || !intrinsicHeight) {
      // If we can't get dimensions, default to scale 1
      return 1
    }

    // Calculate scale to fit 95% of viewport width (matching old MermaidModal behavior)
    const viewportWidth = window.innerWidth
    const targetWidth = viewportWidth * 0.95
    const scale = targetWidth / intrinsicWidth

    // Clamp to reasonable bounds (0.1 to 5)
    return Math.min(5, Math.max(0.1, scale))
  } catch (e) {
    console.warn('Failed to calculate fit-to-width scale:', e)
    return 1
  }
}
