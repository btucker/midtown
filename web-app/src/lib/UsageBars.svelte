<script>
  import { usageData } from './store.js'
  import { usageColor } from './usage-utils.js'
</script>

{#if $usageData && $usageData.length > 0}
  <div class="px-3 py-1 flex flex-col gap-0.5">
    {#each $usageData as account, index}
      {@const label = account.account_email
        ? `${account.provider.toUpperCase()} (${account.account_email})`
        : `${account.provider.toUpperCase()} (${account.profile})`
      }
      <div class="flex items-center gap-2 {index > 0 ? 'mt-1' : ''}">
        <span class="text-[0.65rem] text-accent-teal truncate flex-1 min-w-0">{label}</span>
        {#if account.session_resets || account.week_resets}
          <span class="text-[0.65rem] tabular-nums shrink-0" style="color: {usageColor(account.session_util)}">S {Math.round(account.session_util)}%</span>
          <span class="text-[0.65rem] tabular-nums shrink-0" style="color: {usageColor(account.week_util)}">W {Math.round(account.week_util)}%</span>
        {:else}
          <span class="text-[0.65rem] text-muted-foreground shrink-0">—</span>
        {/if}
      </div>
    {/each}
  </div>
{:else}
  <div class="px-3 py-1 text-[0.65rem] text-muted-foreground">Loading usage...</div>
{/if}
