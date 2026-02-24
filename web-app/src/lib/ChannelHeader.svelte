<script>
  import { activeChannel, kanbanData, repoStatus, repoStatuses } from './store.js'
  import { getChannelTaskCount, getChannelPrs } from './channelUtils.js'
  import { formatRelativeTime } from './utils.js'

  let channelCounts = $derived(getChannelTaskCount($activeChannel, $kanbanData))
  let channelPrs = $derived(getChannelPrs($activeChannel, $kanbanData))
  let totalTasks = $derived(channelCounts.inProgress + channelCounts.pending + channelCounts.review)
  let isMultiRepo = $derived($repoStatuses.length > 1)

  function ciInfo(status) {
    switch (status) {
      case 'passed': return { char: '●', color: 'hsl(var(--status-green))' }
      case 'failed': return { char: '●', color: 'hsl(var(--status-red))' }
      case 'running':
      case 'pending': return { char: '●', color: 'hsl(var(--status-amber))' }
      default: return { char: '○', color: 'hsl(var(--muted-foreground))' }
    }
  }
</script>

<div class="hidden md:block bg-card border-b-2 border-border shrink-0">
  <div class="flex items-center justify-between px-4 py-3">
    <!-- Left: channel name + task/PR badges -->
    <div class="flex items-center gap-3 flex-1 min-w-0">
      <div class="flex items-baseline gap-1 shrink-0">
        <span class="text-[1.2rem] text-muted-foreground font-bold">#</span>
        <span class="text-[1.1rem] font-bold font-mono text-foreground">{$activeChannel}</span>
      </div>
      {#if totalTasks > 0 || channelPrs.length > 0}
        <div class="flex items-center gap-1.5 flex-wrap">
          {#if channelPrs.length > 0}
            <span class="text-[0.75rem] px-2 py-[3px] rounded-xl font-semibold whitespace-nowrap bg-blue-100 dark:bg-blue-950/80 text-link-pr" title="{channelPrs.length} active PR{channelPrs.length === 1 ? '' : 's'}">
              {channelPrs.length} PR{channelPrs.length === 1 ? '' : 's'}
            </span>
          {/if}
          {#if channelCounts.inProgress > 0}
            <span class="text-[0.75rem] px-2 py-[3px] rounded-xl font-semibold whitespace-nowrap bg-green-100 dark:bg-green-950/80 text-link-default" title="{channelCounts.inProgress} in progress">
              {channelCounts.inProgress} in progress
            </span>
          {/if}
          {#if channelCounts.pending > 0}
            <span class="text-[0.75rem] px-2 py-[3px] rounded-xl font-semibold whitespace-nowrap bg-purple-100 dark:bg-purple-950/80 text-link-task" title="{channelCounts.pending} pending">
              {channelCounts.pending} pending
            </span>
          {/if}
        </div>
      {/if}
    </div>

    <!-- Right: repo status (single repo) -->
    {#if !isMultiRepo && ($repoStatus.commitHash || $repoStatus.ciStatus)}
      {@const ci = ciInfo($repoStatus.ciStatus)}
      <div class="flex items-center gap-2 shrink-0 text-[0.75rem] font-mono">
        {#if $repoStatus.repoName}
          <span class="text-muted-foreground">{$repoStatus.repoName}</span>
        {/if}
        {#if $repoStatus.commitHash}
          <span class="text-link-default">{$repoStatus.commitHash}</span>
        {/if}
        {#if $repoStatus.commitTime}
          <span class="text-muted-foreground">{formatRelativeTime($repoStatus.commitTime)}</span>
        {/if}
        <span style="color: {ci.color}">{ci.char}</span>
        {#if $repoStatus.releaseTag}
          <span class="text-muted-foreground">Releases:</span>
          <span class="text-link-default">{$repoStatus.releaseTag}</span>
          {#if $repoStatus.releaseTime}
            <span class="text-muted-foreground">{formatRelativeTime($repoStatus.releaseTime)}</span>
          {/if}
        {/if}
      </div>
    {/if}
  </div>

  <!-- Multi-repo status rows (one row per repo) -->
  {#if isMultiRepo}
    {#each $repoStatuses as repo}
      {@const ci = ciInfo(repo.ciStatus)}
      <div class="flex items-center gap-2 px-4 pb-2 text-[0.7rem] font-mono border-t border-border">
        <span class="text-muted-foreground">{repo.label || repo.fullName || ''}</span>
        {#if repo.commitHash}
          <span class="text-link-default">{repo.commitHash}</span>
        {/if}
        {#if repo.commitTime}
          <span class="text-muted-foreground">{formatRelativeTime(repo.commitTime)}</span>
        {/if}
        {#if repo.commitHash || repo.ciStatus}
          <span style="color: {ci.color}">{ci.char}</span>
        {/if}
        {#if repo.releaseTag}
          <span class="text-muted-foreground">Releases:</span>
          <span class="text-link-default">{repo.releaseTag}</span>
          {#if repo.releaseTime}
            <span class="text-muted-foreground">{formatRelativeTime(repo.releaseTime)}</span>
          {/if}
        {/if}
      </div>
    {/each}
  {/if}
</div>
