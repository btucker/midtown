<script>
  import { usageData } from './store.js'
  import { estimateTimeToFull, formatResetTime, usageColor } from './usage-utils.js'
</script>

{#if $usageData && $usageData.length > 0}
  <div class="usage-container">
    {#each $usageData as account, index}
      {@const label = account.account_email
        ? `${account.provider.toUpperCase()} (${account.account_email})`
        : `${account.provider.toUpperCase()} (${account.profile})`
      }

      <div class="account-section" class:not-first={index > 0}>
        <div class="usage-header">{label}</div>

        {#if account.session_resets || account.week_resets}
          {#each [
            { label: 'Session', util: account.session_util, resets: account.session_resets, isSession: true },
            { label: 'Week', util: account.week_util, resets: account.week_resets, isSession: false },
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
        {:else}
          <div class="no-usage">No usage data available</div>
        {/if}
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

  .account-section {
    margin-bottom: 0;
  }

  .account-section.not-first {
    margin-top: 12px;
    padding-top: 12px;
    border-top: 1px solid #2a2a2a;
  }

  .usage-header {
    font-size: 0.7rem;
    color: #7ec4cf;
    margin-bottom: 8px;
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

  .no-usage {
    font-size: 0.7rem;
    color: #484848;
    padding: 4px 0;
  }

  .usage-placeholder {
    font-size: 0.7rem;
    color: #484848;
  }
</style>
