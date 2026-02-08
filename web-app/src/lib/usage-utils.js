/**
 * Estimate time until utilization reaches 100% based on current consumption rate.
 * Uses the known window duration (5h session, 7d weekly) and current utilization.
 */
export function estimateTimeToFull(util, resetsAt, isSession) {
  if (util <= 0 || util >= 100) return null

  const now = Date.now()
  const resetTime = new Date(resetsAt).getTime()
  if (isNaN(resetTime)) return null
  const secsUntilReset = (resetTime - now) / 1000

  if (secsUntilReset <= 0) return null

  // Total window duration in seconds
  const windowSecs = isSession ? 5 * 3600 : 7 * 24 * 3600

  // Elapsed time in this window
  const elapsedSecs = windowSecs - secsUntilReset
  if (elapsedSecs <= 0) return null

  // Rate = utilization percentage per second
  const rate = util / elapsedSecs
  const remainingPct = 100 - util
  const secsToFull = remainingPct / rate

  return formatDurationEstimate(secsToFull)
}

/**
 * Format a duration in seconds as a human-readable estimate.
 */
export function formatDurationEstimate(secs) {
  const minutes = Math.round(secs / 60)
  if (minutes < 1) return '~<1m left'
  if (minutes < 60) return `~${minutes}m left`
  const hours = Math.floor(minutes / 60)
  const remainingMins = minutes % 60
  if (remainingMins === 0) return `~${hours}h left`
  return `~${hours}h${remainingMins}m left`
}

/**
 * Format reset time for display.
 * Session: "H:MMam/pm", Weekly: "Mon DD"
 */
export function formatResetTime(resetsAt, isSession) {
  const reset = new Date(resetsAt)
  if (reset <= new Date()) return 'now'

  if (isSession) {
    return reset.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
  }
  return reset.toLocaleDateString([], { month: 'short', day: 'numeric' })
}

/**
 * Get the color for a utilization percentage.
 * Matches TUI thresholds: green (<60%), yellow (60-80%), red (>=80%).
 */
export function usageColor(util) {
  if (util >= 80) return '#af5f5f'
  if (util >= 60) return '#d7af5f'
  return '#5faf5f'
}
