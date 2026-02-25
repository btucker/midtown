// Pure utility functions for markdown rendering and mermaid detection.
// Extracted from Channel.svelte for testability.

import { marked, Renderer } from 'marked'
import { markedHighlight } from 'marked-highlight'
import hljs from 'highlight.js/lib/core'

// Register only languages relevant to this project (tree-shakeable imports)
import rust from 'highlight.js/lib/languages/rust'
import javascript from 'highlight.js/lib/languages/javascript'
import typescript from 'highlight.js/lib/languages/typescript'
import python from 'highlight.js/lib/languages/python'
import bash from 'highlight.js/lib/languages/bash'
import json from 'highlight.js/lib/languages/json'
import toml from 'highlight.js/lib/languages/ini' // hljs uses 'ini' for TOML
import yaml from 'highlight.js/lib/languages/yaml'
import css from 'highlight.js/lib/languages/css'
import xml from 'highlight.js/lib/languages/xml'
import diff from 'highlight.js/lib/languages/diff'

hljs.registerLanguage('rust', rust)
hljs.registerLanguage('javascript', javascript)
hljs.registerLanguage('js', javascript)
hljs.registerLanguage('typescript', typescript)
hljs.registerLanguage('ts', typescript)
hljs.registerLanguage('python', python)
hljs.registerLanguage('py', python)
hljs.registerLanguage('bash', bash)
hljs.registerLanguage('sh', bash)
hljs.registerLanguage('shell', bash)
hljs.registerLanguage('json', json)
hljs.registerLanguage('toml', toml)
hljs.registerLanguage('yaml', yaml)
hljs.registerLanguage('yml', yaml)
hljs.registerLanguage('css', css)
hljs.registerLanguage('xml', xml)
hljs.registerLanguage('html', xml)
hljs.registerLanguage('diff', diff)

// Configure marked with syntax highlighting via marked-highlight
marked.use(
  markedHighlight({
    langPrefix: 'hljs language-',
    highlight(code, lang) {
      if (lang && hljs.getLanguage(lang)) {
        return hljs.highlight(code, { language: lang }).value
      }
      // For unknown/unspecified languages, try auto-detection
      return hljs.highlightAuto(code).value
    },
  })
)

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

const IMAGE_EXTENSIONS = /\.(png|jpg|jpeg|gif|webp)$/i

/**
 * Generate HTML for an inline attachment reference.
 * Image files render as a clickable thumbnail; other files show a file badge.
 */
function renderAttachmentHtml(path, apiBase) {
  const filename = path.split('/').pop()
  if (IMAGE_EXTENSIONS.test(filename) && apiBase) {
    const url = `${apiBase}/uploads/${encodeURIComponent(filename)}`
    const safeAlt = filename.replace(/"/g, '&quot;')
    return `<a href="${url}" target="_blank" rel="noopener" class="attachment-link"><img src="${url}" alt="${safeAlt}" class="message-image" loading="lazy" /></a>`
  }
  const safeName = filename.replace(/</g, '&lt;').replace(/>/g, '&gt;')
  return `<span class="attachment-ref">📎 ${safeName}</span>`
}

/**
 * Render markdown formatting via marked (GFM).
 * Pre-escapes <, >, and & for XSS protection.
 * Restores > at line starts for blockquote support before markdown processing.
 * Auto-links bare URLs (handled natively by marked GFM mode).
 * Disables underscore-based italic rendering (keeps asterisk-based italics).
 * Converts #channel references to clickable channel-switch links.
 * Converts !N task references to clickable task-detail links.
 * Converts [Attached: /path] and [Attached file: name]\nPlease read: /path patterns
 * to inline images (for image files) or file badges (for other types).
 * @param {string} text - Raw message content
 * @param {string} [apiBase] - Base URL for the project daemon API (e.g. http://host:47023/api)
 */
export function renderContent(text, apiBase = '') {
  // Trim leading/trailing whitespace so messages don't render with blank lines at the top.
  text = text.trim()

  // Extract attachment references before XSS escaping so we can inject image HTML later.
  // Pattern 1: [Attached: /full/path/to/file.ext]  (appears when user text accompanies file)
  // Pattern 2: [Attached file: name]\nPlease read: /full/path  (standalone file message)
  // Both forms are replaced with a control-char placeholder; final HTML is injected after
  // all markdown transformations so the image tags aren't escaped or double-processed.
  const attachments = []
  text = text.replace(/\[Attached file:[^\]]*\]\nPlease read:\s*(.+)/g, (_, path) => {
    attachments.push(path)
    return `\x01ATTACH${attachments.length - 1}\x01`
  })
  text = text.replace(/\[Attached:\s*([^\]\n]+)\]/g, (_, path) => {
    attachments.push(path.trim())
    return `\x01ATTACH${attachments.length - 1}\x01`
  })

  // Protect code blocks and inline code BEFORE XSS escaping.
  // This prevents double-escaping: code containing <div> would otherwise become
  // &lt;div&gt; after escaping, then render as literal "&lt;div&gt;" in the output.
  // marked-highlight handles its own escaping for code content.
  const preservedItems = []

  // Preserve fenced code blocks first (before inline code spans)
  text = text.replace(/```[\s\S]*?```/g, (m) => {
    preservedItems.push(m)
    return `\x02PRESERVE${preservedItems.length - 1}\x02`
  })

  // Preserve inline code spans
  text = text.replace(/`[^`]+`/g, (m) => {
    preservedItems.push(m)
    return `\x02PRESERVE${preservedItems.length - 1}\x02`
  })

  // Escape &, <, and > for XSS defense-in-depth (on non-code text only).
  let safe = text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')

  // Restore > at line starts for markdown blockquotes.
  safe = safe.replace(/^(&gt;)+/gm, (m) => m.replace(/&gt;/g, '>'))

  // Preserve markdown links before converting special references
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

  // Replace attachment placeholders with final HTML (images or file badges).
  // Strip surrounding <p>…</p> when the placeholder is the sole paragraph content
  // so the image renders as a block element rather than inside a paragraph.
  if (attachments.length > 0) {
    html = html.replace(/<p>\s*\x01ATTACH(\d+)\x01\s*<\/p>/g, (_, i) =>
      renderAttachmentHtml(attachments[parseInt(i)], apiBase)
    )
    html = html.replace(/\x01ATTACH(\d+)\x01/g, (_, i) =>
      renderAttachmentHtml(attachments[parseInt(i)], apiBase)
    )
  }

  return html.trim()
}
