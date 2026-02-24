// Muted avenue colors matching terminal TUI palette (AVENUE_COLORS from ui.rs)
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
  lead: '#d7d787',        // LightYellow
  github: '#585858',      // DarkGray
  system: '#585858',      // DarkGray
  daemon: '#585858',      // DarkGray
  midtown: '#d7d787',     // LightYellow (project lead)
}

// Senders whose content is rendered in DarkGray (system infrastructure actors)
export const DIM_SENDERS = new Set(['daemon', 'github', 'system'])

export function getSenderColor(name) {
  return AVENUE_COLORS[name?.toLowerCase()] || '#d0d0d0'
}

export function isDimSender(sender) {
  return DIM_SENDERS.has(sender?.toLowerCase())
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
