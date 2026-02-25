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
    return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
  } catch {
    return ''
  }
}

// Returns true if the sender changed from the previous message in the list.
export function senderChanged(msgs, index) {
  if (index === 0) return true
  return msgs[index].from !== msgs[index - 1].from
}

// Returns true if the minute ticked over from the previous message in the same group.
// Used to conditionally show a gutter timestamp on continuation messages.
export function timeChanged(msgs, index) {
  if (index === 0 || senderChanged(msgs, index)) return false
  return formatTime(msgs[index].timestamp) !== formatTime(msgs[index - 1].timestamp)
}
