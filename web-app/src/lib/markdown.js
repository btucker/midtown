// Pure utility functions for markdown-like rendering and mermaid detection.
// Extracted from Channel.svelte for testability.

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
 * Render markdown-like formatting (bold, links, bare URLs).
 * HTML-escapes first, then applies formatting.
 */
export function renderContent(text) {
  // Escape HTML first
  let html = text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
  // Bold: **text**
  html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
  // Links: [text](url)
  html = html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
  // Bare URLs
  html = html.replace(/(^|[\s(])(https?:\/\/[^\s)]+)/g, '$1<a href="$2" target="_blank" rel="noopener">$2</a>')
  return html
}
