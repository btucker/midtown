// Pure utility functions for markdown rendering and mermaid detection.
// Extracted from Channel.svelte for testability.

import { marked, Renderer } from 'marked'

// Configure marked with a custom renderer that suppresses underscore-based
// italic rendering. Identifiers like `function_name` should not become italic.
const renderer = new Renderer()
renderer.em = ({ raw }) => {
  if (raw.startsWith('_')) {
    return raw
  }
  return false // fall through to default rendering
}

marked.use({ renderer, gfm: true, breaks: false })

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
 * Render markdown formatting via marked (GFM).
 * Pre-escapes <, >, and & for XSS protection.
 * Restores > at line starts for blockquote support before markdown processing.
 * Auto-links bare URLs (handled natively by marked GFM mode).
 * Disables underscore-based italic rendering (keeps asterisk-based italics).
 * Converts #channel references to clickable channel-switch links.
 * Converts !N task references to clickable task-detail links.
 */
export function renderContent(text) {
  // Escape &, <, and > for XSS defense-in-depth.
  let safe = text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')

  // Restore > at line starts for markdown blockquotes.
  safe = safe.replace(/^(&gt;)+/gm, (m) => m.replace(/&gt;/g, '>'))

  // Protect existing markdown links and inline code before converting special references
  // This prevents conflicts when special references appear inside markdown syntax
  const preservedItems = []

  // Preserve fenced code blocks first (before inline code spans)
  safe = safe.replace(/```[\s\S]*?```/g, (m) => {
    preservedItems.push(m)
    return `\x02PRESERVE${preservedItems.length - 1}\x02`
  })

  // Preserve inline code spans
  safe = safe.replace(/`[^`]+`/g, (m) => {
    preservedItems.push(m)
    return `\x02PRESERVE${preservedItems.length - 1}\x02`
  })

  // Preserve markdown links
  safe = safe.replace(/\[([^\]]+)\]\(([^)]+)\)/g, (m) => {
    preservedItems.push(m)
    return `\x02PRESERVE${preservedItems.length - 1}\x02`
  })

  // Now convert special references to markdown-style links (processed BEFORE marked)
  // Order matters: we must protect already-converted links from being re-matched

  // Helper function to preserve and restore conversions between each pattern
  function preserveAndReplace(text, pattern, replacement) {
    const tempPreserved = []
    // First, protect already-converted markdown links (our special URL schemes)
    text = text.replace(/\[([^\]]+)\]\((channel|task|pr|coworker):[^)]+\)/g, (m) => {
      tempPreserved.push(m)
      return `\x03TEMP${tempPreserved.length - 1}\x03`
    })
    // Apply the new pattern
    text = text.replace(pattern, replacement)
    // Restore protected links
    text = text.replace(/\x03TEMP(\d+)\x03/g, (_, i) => tempPreserved[i])
    return text
  }

  // PR #N references (must come before bare #N to avoid conflicts)
  safe = preserveAndReplace(safe, /\b(PR|pull request)\s+#(\d+)\b/gi, (match, prefix, prNum) => {
    return `[${prefix} #${prNum}](pr:${prNum})`
  })

  // Bare #N PR references (numbers only, not letters = PR not channel)
  safe = preserveAndReplace(safe, /\B#(\d+)\b/g, (match, prNum) => {
    return `[#${prNum}](pr:${prNum})`
  })

  // #channel references (letters/hyphens = channel not PR)
  safe = preserveAndReplace(safe, /#([a-z][a-z0-9-]*)\b/gi, (match, channelName) => {
    return `[#${channelName}](channel:${channelName})`
  })

  // !N task references
  safe = preserveAndReplace(safe, /!(\d+)\b/g, (match, taskId) => {
    return `[!${taskId}](task:${taskId})`
  })

  // @coworker mentions (only on desktop where we can show detail panel)
  safe = preserveAndReplace(safe, /@([a-z][a-z0-9-]*)\b/gi, (match, name) => {
    return `[@${name}](coworker:${name})`
  })

  // Restore preserved user-written markdown links and code blocks
  safe = safe.replace(/\x02PRESERVE(\d+)\x02/g, (_, i) => preservedItems[i])

  // Render markdown via marked (GFM: tables, strikethrough, auto-links)
  let html = marked.parse(safe)

  // Convert our special URL schemes to proper links with classes and data attributes
  html = html.replace(/<a href="channel:([^"]+)">([^<]*)<\/a>/g, (match, channelName, text) => {
    return `<a href="#" class="channel-link" data-channel="${channelName}">${text}</a>`
  })

  html = html.replace(/<a href="task:([^"]+)">([^<]*)<\/a>/g, (match, taskId, text) => {
    return `<a href="#" class="task-link" data-task="${taskId}">${text}</a>`
  })

  html = html.replace(/<a href="pr:([^"]+)">(.*?)<\/a>/g, (match, prNum, text) => {
    return `<a href="#" class="pr-link" data-pr="${prNum}">${text}</a>`
  })

  html = html.replace(/<a href="coworker:([^"]+)">([^<]*)<\/a>/g, (match, name, text) => {
    return `<a href="#" class="coworker-link" data-coworker="${name}">${text}</a>`
  })

  // Ensure all links open in new tabs
  html = html.replace(/<a /g, '<a target="_blank" rel="noopener" ')

  // Restore target for internal channel/task/pr/coworker links (they shouldn't open new tabs)
  html = html.replace(/<a target="_blank" rel="noopener" (href="#" class="channel-link")/g, '<a $1')
  html = html.replace(/<a target="_blank" rel="noopener" (href="#" class="task-link")/g, '<a $1')
  html = html.replace(/<a target="_blank" rel="noopener" (href="#" class="pr-link")/g, '<a $1')
  html = html.replace(/<a target="_blank" rel="noopener" (href="#" class="coworker-link")/g, '<a $1')

  return html.trim()
}
