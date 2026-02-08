<script>
  import { usageData } from './store.js'
  import { estimateTimeToFull, formatResetTime, usageColor } from './usage-utils.js'
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
{:else}
  <div class="usage-container">
    <div class="usage-header">Usage</div>
    <div class="usage-placeholder">Loading...</div>
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

  .usage-placeholder {
    font-size: 0.7rem;
    color: #484848;
  }
</style>
