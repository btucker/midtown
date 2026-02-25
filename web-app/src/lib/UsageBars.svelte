<script>
  import { usageData } from './store.js'
  import { estimateTimeToFull, formatResetTime, usageColor } from './usage-utils.js'

  let expandedAccount = $state(null)

  function toggleAccount(label) {
    expandedAccount = expandedAccount === label ? null : label
  }
</script>

{#if $usageData && $usageData.length > 0}
  <div class="px-3 py-1 flex flex-col gap-0.5">
    {#each $usageData as account, index}
      {@const label = account.account_email
        ? `${account.provider.toUpperCase()} (${account.account_email})`
        : `${account.provider.toUpperCase()} (${account.profile})`
      }
      {@const isExpanded = expandedAccount === label}
      {@const hasData = account.session_resets || account.week_resets}

      <div class="{index > 0 ? 'mt-1' : ''}">
        <button
          type="button"
          class="w-full flex items-center gap-2 bg-transparent border-none p-0 cursor-pointer text-left"
          onclick={() => hasData && toggleAccount(label)}
        >
          <span class="text-[0.65rem] text-accent-teal truncate flex-1 min-w-0">{label}</span>
          {#if hasData}
            <span class="text-[0.65rem] tabular-nums shrink-0" style="color: {usageColor(account.session_util)}">S {Math.round(account.session_util)}%</span>
            <span class="text-[0.65rem] tabular-nums shrink-0" style="color: {usageColor(account.week_util)}">W {Math.round(account.week_util)}%</span>
          {:else}
            <span class="text-[0.65rem] text-muted-foreground shrink-0">—</span>
          {/if}
        </button>

        {#if isExpanded && hasData}
          <div class="mt-1 ml-2 flex flex-col gap-0.5">
            {#each [
              { label: 'Session', util: account.session_util, resets: account.session_resets, isSession: true },
              { label: 'Week', util: account.week_util, resets: account.week_resets, isSession: false },
            ] as bar}
              {@const estimate = estimateTimeToFull(bar.util, bar.resets, bar.isSession)}
              {@const resetText = formatResetTime(bar.resets, bar.isSession)}
              <div class="flex gap-1.5 text-[0.62rem] text-muted-foreground">
                <span class="shrink-0" style="color: {usageColor(bar.util)}">{bar.label}</span>
                {#if estimate}<span>{estimate}</span>{/if}
                <span>· resets {resetText}</span>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    {/each}
  </div>
{:else}
  <div class="px-3 py-1 text-[0.65rem] text-muted-foreground">Loading usage...</div>
{/if}
