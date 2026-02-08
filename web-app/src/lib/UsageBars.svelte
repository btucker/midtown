<script>
  import { usageData } from './store.js'

  /**
   * Get the color for a utilization percentage.
   * Matches TUI thresholds: green (<60%), yellow (60-80%), red (>=80%).
   */
  function usageColor(util) {
    if (util >= 80) return '#af5f5f'
    if (util >= 60) return '#d7af5f'
    return '#5faf5f'
  }

  /**
   * Estimate time until utilization reaches 100% based on current consumption rate.
   * Uses the known window duration (5h session, 7d weekly) and current utilization.
   */
  function estimateTimeToFull(util, resetsAt, isSession) {
    if (util <= 0 || util >= 100) return null

    const now = Date.now()
    const resetTime = new Date(resetsAt).getTime()
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
  function formatDurationEstimate(secs) {
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
  function formatResetTime(resetsAt, isSession) {
    const reset = new Date(resetsAt)
    if (reset <= new Date()) return 'now'

    if (isSession) {
      return reset.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })
    }
    return reset.toLocaleDateString([], { month: 'short', day: 'numeric' })
  }
</script>

{#if $usageData}
  <div class="usage-container">
    <div class="usage-header">
      Usage{$usageData.account_email ? ` (${$usageData.account_email})` : ''}
    </div>

    {#each [
      { label: 'Session', util: $usageData.session_util, resets: $usageData.session_resets, isSession: true },
      { label: 'Week', util: $usageData.week_util, resets: $usageData.week_resets, isSession: false },
    ] as bar}
      {@const pct = Math.round(bar.util)}
      {@const color = usageColor(bar.util)}
      {@const estimate = estimateTimeToFull(bar.util, bar.resets, bar.isSession)}
      {@const resetText = formatResetTime(bar.resets, bar.isSession)}

      <div class="usage-row">
        <span class="usage-label">{bar.label}</span>
        <div class="bar-track">
          <div
            class="bar-fill"
            style="width: {Math.min(bar.util, 100)}%; background: {color}"
          ></div>
        </div>
        <span class="usage-pct" style="color: {color}">{pct}%</span>
      </div>
      <div class="usage-detail">
        {#if estimate}
          <span class="usage-estimate">{estimate}</span>
        {/if}
        <span class="usage-reset">resets {resetText}</span>
      </div>
    {/each}
  </div>
{/if}

<style>
  .usage-container {
    padding: 10px 12px;
    border-top: 1px solid #3a3a3a;
  }

  .usage-header {
    font-size: 0.7rem;
    color: #585858;
    margin-bottom: 8px;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .usage-row {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .usage-label {
    font-size: 0.7rem;
    color: #585858;
    min-width: 42px;
  }

  .bar-track {
    flex: 1;
    height: 6px;
    background: #303030;
    border-radius: 3px;
    overflow: hidden;
  }

  .bar-fill {
    height: 100%;
    border-radius: 3px;
    transition: width 0.5s ease, background 0.5s ease;
  }

  .usage-pct {
    font-size: 0.7rem;
    min-width: 28px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .usage-detail {
    display: flex;
    gap: 8px;
    font-size: 0.65rem;
    color: #484848;
    padding-left: 48px;
    margin-bottom: 4px;
  }

  .usage-estimate {
    color: #585858;
  }
</style>
