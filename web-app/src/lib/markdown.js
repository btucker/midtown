// Pure utility functions for markdown rendering and mermaid detection.
// Extracted from Channel.svelte for testability.

import snarkdown from 'snarkdown'

/**
 * Split text into segments of plain text and mermaid code blocks.
 * Returns array of {type: 'text'|'mermaid', content: string}.
 */
export function parseSegments(text) {
  const segments = []
  const regex = /```mermaid\s*\n([\s\S]*?)```/g
  let lastIndex = 0
  let match

  while ((match = regex.exec(text)) !== null) {
    if (match.index > lastIndex) {
      segments.push({ type: 'text', content: text.slice(lastIndex, match.index) })
    }
    segments.push({ type: 'mermaid', content: match[1].trim() })
    lastIndex = regex.lastIndex
  }

  if (lastIndex < text.length) {
    segments.push({ type: 'text', content: text.slice(lastIndex) })
  }

  return segments
}

/**
 * Check if message text contains any mermaid code blocks.
 */
export function hasMermaid(text) {
  return /```mermaid\s*\n/.test(text)
}

/**
 * Render markdown formatting via snarkdown.
 * Pre-escapes <, >, and & for XSS protection.
 * Restores > at line starts for blockquote support before markdown processing.
 * Auto-links bare URLs and ensures all links open in new tabs.
 * Disables underscore-based italic rendering (keeps asterisk-based italics).
 */
export function renderContent(text) {
  // Escape &, <, and > for XSS defense-in-depth.
  let safe = text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')

  // Restore > at line starts for markdown blockquotes.
  safe = safe.replace(/^(&gt;)+/gm, (m) => m.replace(/&gt;/g, '>'))

  // Escape underscores to prevent them from being interpreted as italic markers.
  // Use a placeholder that won't conflict with markdown syntax.
  safe = safe.replace(/_/g, '\x01UNDERSCORE\x01')

  // Auto-link bare URLs before markdown processing.
  // Protect existing markdown links and inline code from URL conversion.
  const preserved = []
  safe = safe.replace(/`[^`]+`/g, (m) => {
    preserved.push(m)
    return `\x00${preserved.length - 1}\x00`
  })
  safe = safe.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (m) => {
    preserved.push(m)
    return `\x00${preserved.length - 1}\x00`
  })
  safe = safe.replace(/(^|[\s(])(https?:\/\/[^\s)]+)/gm, '$1[$2]($2)')
  safe = safe.replace(/\x00(\d+)\x00/g, (_, i) => preserved[i])

  // Render markdown
  let html = snarkdown(safe)

  // Restore underscores after markdown processing
  html = html.replace(/\x01UNDERSCORE\x01/g, '_')

  // Ensure all links open in new tabs
  html = html.replace(/<a /g, '<a target="_blank" rel="noopener" ')

  return html
}
