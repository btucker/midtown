// Avenue colors for the web UI. Most match the terminal TUI palette (AVENUE_COLORS from ui.rs),
// but the lead/midtown color intentionally diverges: the TUI uses Color::LightYellow (#d7d787)
// while the web uses a richer gold (#E3BD3F) for better visual weight on screen.
export const AVENUE_COLORS = {
  lexington: '#5fafaf',   // Cyan
  park: '#5faf5f',        // Green
  madison: '#ff5f5f',     // LightRed
  broadway: '#af5faf',    // Magenta
  amsterdam: '#5f87af',   // Blue
  columbus: '#af5f5f',    // Red
  riverside: '#87d7d7',   // LightCyan
  york: '#87d787',        // LightGreen
  pleasant: '#d7afd7',    // LightMagenta
  vernon: '#87afd7',      // LightBlue
  bleecker: '#d7875f',    // orange (Indexed 208)
  houston: '#ff87d7',     // pink (Indexed 213)
  canal: '#87d7ff',       // light blue (Indexed 117)
  spring: '#afff87',      // light green (Indexed 156)
  prince: '#d7afff',      // lavender (Indexed 183)
  mercer: '#ffaf87',      // salmon (Indexed 216)
  lead: '#E3BD3F',         // Gold/Amber
  github: '#585858',      // DarkGray
  system: '#585858',      // DarkGray
  daemon: '#585858',      // DarkGray
  midtown: '#E3BD3F',     // Gold/Amber (project lead)
  user: 'hsl(var(--foreground))', // Human user — always use the foreground color (black in light, white in dark)
}

// Senders whose content is rendered in DarkGray (system infrastructure actors)
export const DIM_SENDERS = new Set(['daemon', 'github', 'system'])

function normalizeName(name) {
  return typeof name === 'string' ? name.toLowerCase() : ''
}

function getOverride(overrides, key) {
  if (!overrides || !key) return undefined
  if (overrides instanceof Map) {
    return overrides.get(key)
  }
  if (typeof overrides === 'object') {
    return overrides[key]
  }
  return undefined
}

export function getSenderColor(name, overrides) {
  const normalized = normalizeName(name)
  return getOverride(overrides, normalized) || AVENUE_COLORS[normalized] || '#d0d0d0'
}

function hasExtraDim(extraDimSenders, normalized) {
  if (!extraDimSenders || !normalized) return false
  if (extraDimSenders instanceof Set) {
    return extraDimSenders.has(normalized)
  }
  if (Array.isArray(extraDimSenders)) {
    return extraDimSenders.some((sender) => normalizeName(sender) === normalized)
  }
  if (typeof extraDimSenders === 'object') {
    return Boolean(extraDimSenders[normalized])
  }
  return false
}

export function isDimSender(sender, extraDimSenders) {
  const normalized = normalizeName(sender)
  if (!normalized) return false
  if (DIM_SENDERS.has(normalized)) return true
  return hasExtraDim(extraDimSenders, normalized)
}

export function formatTime(timestamp) {
  try {
    const date = new Date(timestamp)
    return date.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit', hour12: true })
  } catch {
    return ''
  }
}

export function formatTimeCompact(timestamp) {
  try {
    const date = new Date(timestamp)
    const h = date.getHours()
    const m = String(date.getMinutes()).padStart(2, '0')
    return `${h}:${m}`
  } catch {
    return ''
  }
}

// Returns true if the sender changed from the previous message in the list.
export function senderChanged(msgs, index) {
  if (index === 0) return true
  return msgs[index].from !== msgs[index - 1].from
}

/**
 * Parse message content into text and insight segments.
 *
 * Detects two insight formats:
 * 1. Coworker insights: content starts with 💡 — entire message is one insight segment
 * 2. Lead raw output: contains `★ Insight` blocks delimited by dash-lines
 *
 * Returns Array<{ type: 'text' | 'insight', content: string }>
 */
export function parseInsightSegments(content) {
  if (!content) return [{ type: 'text', content: content || '' }]

  // Format 1: whole-message insight (coworker 💡 prefix)
  if (content.trimStart().startsWith('💡')) {
    return [{ type: 'insight', content: content.trimStart().replace(/^💡\s*/, '') }]
  }

  // Format 2: ★ Insight blocks mixed with regular text
  if (!content.includes('★ Insight')) {
    return [{ type: 'text', content }]
  }

  const segments = []
  const lines = content.split('\n')
  let textBuf = []
  let insightBuf = []
  let inInsight = false

  for (const line of lines) {
    if (inInsight) {
      // End marker: line of 10+ dashes with optional backtick wrapping
      if (/^`?─{10,}`?$/.test(line.trim())) {
        if (insightBuf.length > 0) {
          segments.push({ type: 'insight', content: insightBuf.join('\n').trim() })
          insightBuf = []
        }
        inInsight = false
      } else {
        insightBuf.push(line)
      }
    } else if (line.includes('★ Insight')) {
      // Flush accumulated text
      if (textBuf.length > 0) {
        const text = textBuf.join('\n').trim()
        if (text) segments.push({ type: 'text', content: text })
        textBuf = []
      }
      inInsight = true
      insightBuf = []
    } else {
      textBuf.push(line)
    }
  }

  // Flush remaining buffers
  if (inInsight && insightBuf.length > 0) {
    segments.push({ type: 'insight', content: insightBuf.join('\n').trim() })
  }
  if (textBuf.length > 0) {
    const text = textBuf.join('\n').trim()
    if (text) segments.push({ type: 'text', content: text })
  }

  return segments.length > 0 ? segments : [{ type: 'text', content }]
}

// Returns true if the minute ticked over from the previous message in the same group.
// Used to conditionally show a gutter timestamp on continuation messages.
export function timeChanged(msgs, index) {
  if (index === 0 || senderChanged(msgs, index)) return false
  return formatTimeCompact(msgs[index].timestamp) !== formatTimeCompact(msgs[index - 1].timestamp)
}
