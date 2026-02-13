<script>
  import { usageData } from './store.js'
  import { estimateTimeToFull, formatResetTime, usageColor } from './usage-utils.js'
</script>

{#if $usageData && $usageData.length > 0}
  <div class="py-2.5 px-3 border-t border-[#3a3a3a]">
    {#each $usageData as account, index}
      {@const label = account.account_email
        ? `${account.provider.toUpperCase()} (${account.account_email})`
        : `${account.provider.toUpperCase()} (${account.profile})`
      }

      <div class="mt-0 first:mt-0 first:pt-0 first:border-t-0 {index > 0 ? 'mt-3 pt-3 border-t border-[#2a2a2a]' : ''}">
        <div class="text-[0.7rem] text-[#7ec4cf] mb-2 truncate">{label}</div>

        {#if account.session_resets || account.week_resets}
          {#each [
            { label: 'Session', util: account.session_util, resets: account.session_resets, isSession: true },
            { label: 'Week', util: account.week_util, resets: account.week_resets, isSession: false },
          ] as bar}
            {@const pct = Math.round(bar.util)}
            {@const color = usageColor(bar.util)}
            {@const estimate = estimateTimeToFull(bar.util, bar.resets, bar.isSession)}
            {@const resetText = formatResetTime(bar.resets, bar.isSession)}

            <div class="flex items-center gap-1.5">
              <span class="text-[0.7rem] text-[#585858] min-w-[42px]">{bar.label}</span>
              <div class="flex-1 h-1.5 bg-[#303030] rounded overflow-hidden">
                <div
                  class="h-full rounded transition-all duration-500 ease-in-out"
                  style="width: {Math.min(bar.util, 100)}%; background: {color}"
                ></div>
              </div>
              <span class="text-[0.7rem] min-w-[28px] text-right tabular-nums" style="color: {color}">{pct}%</span>
            </div>
            <div class="flex gap-2 text-[0.65rem] text-[#484848] pl-12 mb-1">
              {#if estimate}
                <span class="text-[#585858]">{estimate}</span>
              {/if}
              <span>resets {resetText}</span>
            </div>
          {/each}
        {:else}
          <div class="text-[0.7rem] text-[#484848] py-1">No usage data available</div>
        {/if}
      </div>
    {/each}
  </div>
{:else}
  <div class="py-2.5 px-3 border-t border-[#3a3a3a]">
    <div class="text-[0.7rem] text-[#7ec4cf] mb-2">Usage</div>
    <div class="text-[0.7rem] text-[#484848]">Loading...</div>
  </div>
{/if}
