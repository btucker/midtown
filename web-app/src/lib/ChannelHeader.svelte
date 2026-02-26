<script>
  import { onDestroy } from 'svelte'
  import { fade } from 'svelte/transition'
  import { activeChannel, channels, kanbanData, daemonStatus, repoStatus, repoStatuses } from './store.js'
  import { formatRelativeTime } from './utils.js'

  let isMultiRepo = $derived($repoStatuses.length > 1)

  // DM channel detection: use is_dm field from store, or dm- name prefix as fallback
  let activeChannelMeta = $derived($channels.find((ch) => ch.name === $activeChannel) ?? null)
  let isDm = $derived(activeChannelMeta?.is_dm ?? $activeChannel.startsWith('dm-'))
  let dmPeerName = $derived($activeChannel.startsWith('dm-') ? $activeChannel.slice(3) : $activeChannel)

  function ciInfo(status) {
    switch (status) {
      case 'passed': return { char: '●', color: 'hsl(var(--status-green))' }
      case 'failed': return { char: '●', color: 'hsl(var(--status-red))' }
      case 'running':
      case 'pending': return { char: '●', color: 'hsl(var(--status-amber))' }
      default: return { char: '○', color: 'hsl(var(--muted-foreground))' }
    }
  }

  // "Just merged" banner — mirrors CelebrationEffects hydration-guard pattern
  const BANNER_DURATION_MS = 5 * 60 * 1000 // 5 minutes
  const seenPrs = new Set()
  const bannerTimers = new Map()
  let hydrated = false
  let recentMerges = $state([])

  function prKey(pr) {
    return `${pr?.repo || 'default'}#${pr?.number ?? 'unknown'}`
  }

  function getPrUrl(pr) {
    if (pr.repo && $repoStatuses.length > 0) {
      const info = $repoStatuses.find((s) => s.label === pr.repo)
      if (info?.fullName) return `https://github.com/${info.fullName}/pull/${pr.number}`
    }
    if ($repoStatus.fullName) return `https://github.com/${$repoStatus.fullName}/pull/${pr.number}`
    return null
  }

  function addMergeBanner(pr) {
    const key = prKey(pr)
    if (bannerTimers.has(key)) return
    const url = getPrUrl(pr)
    recentMerges = [...recentMerges, { key, pr, url }]
    const timer = setTimeout(() => {
      recentMerges = recentMerges.filter((m) => m.key !== key)
      bannerTimers.delete(key)
    }, BANNER_DURATION_MS)
    bannerTimers.set(key, timer)
  }

  $effect(() => {
    const ready = Boolean($daemonStatus)
    if (!ready) return
    const done = $kanbanData.done || []
    if (!hydrated) {
      done.forEach((pr) => seenPrs.add(prKey(pr)))
      hydrated = true
      return
    }
    for (const pr of done) {
      const key = prKey(pr)
      if (!seenPrs.has(key)) {
        seenPrs.add(key)
        addMergeBanner(pr)
      }
    }
  })

  $effect(() => {
    if (!$daemonStatus) {
      recentMerges = []
      bannerTimers.forEach((t) => clearTimeout(t))
      bannerTimers.clear()
      seenPrs.clear()
      hydrated = false
    }
  })

  onDestroy(() => {
    bannerTimers.forEach((t) => clearTimeout(t))
    bannerTimers.clear()
  })
</script>

<div class="hidden md:block bg-card shrink-0">
  <div class="flex items-center justify-between px-4 py-2">
    <!-- Left: channel name -->
    <div class="flex items-baseline gap-1 shrink-0">
      {#if isDm}
        <span class="text-[1.2rem] text-muted-foreground font-bold">@</span>
        <span class="text-[1.1rem] font-bold font-mono text-foreground">{dmPeerName}</span>
        <span class="text-[0.7rem] text-muted-foreground font-normal ml-1">Direct Message</span>
      {:else}
        <span class="text-[1.2rem] text-muted-foreground font-bold">#</span>
        <span class="text-[1.1rem] font-bold font-mono text-foreground">{$activeChannel}</span>
      {/if}
    </div>

    <!-- Center: just-merged banner (fades out after 5 min) -->
    {#each recentMerges as merge (merge.key)}
      <div class="just-merged shrink-0" transition:fade={{ duration: 400 }}>
        <span class="label">Just merged:</span>
        {#if merge.url}
          <a href={merge.url} target="_blank" rel="noopener">{merge.pr.title}</a>
        {:else}
          <span>#{merge.pr.number} {merge.pr.title}</span>
        {/if}
      </div>
    {/each}

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

<style>
  .just-merged {
    font-size: 0.75rem;
    font-weight: 600;
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 36ch;
    margin-right: 0.75rem;
    padding: 3px 0.5rem;
    border-radius: 0.75rem;
    background: light-dark(#f3e8ff, rgba(88, 28, 135, 0.25));
    color: light-dark(#7c3aed, #c4b5fd);
  }

  .just-merged .label {
    margin-right: 0.35em;
    opacity: 0.7;
  }

  .just-merged a {
    color: inherit;
    text-decoration: none;
  }

  .just-merged a:hover {
    text-decoration: underline;
  }

</style>
