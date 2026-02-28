<script>
  import { repoStatus, repoStatuses } from './store.js'

  let isMultiRepo = $derived($repoStatuses.length > 1)

  function ciInfo(status) {
    switch (status) {
      case 'passed': return { color: 'hsl(var(--status-green))' }
      case 'failed': return { color: 'hsl(var(--status-red))' }
      case 'running':
      case 'pending': return { color: 'hsl(var(--status-amber))' }
      default: return { color: 'hsl(var(--muted-foreground))' }
    }
  }

  function commitUrl(fullName, hash) {
    if (fullName && hash) return `https://github.com/${fullName}/commit/${hash}`
    return null
  }
</script>

{#if isMultiRepo && $repoStatuses.some(r => r.ciStatus)}
  <!-- Multi-repo: row of colored CI dots (only when CI data exists) -->
  <div class="mini-repo-status" title={$repoStatuses.map(r => `${r.label}: ${r.ciStatus || 'unknown'}`).join(', ')}>
    {#each $repoStatuses as repo}
      {@const ci = ciInfo(repo.ciStatus)}
      <span class="ci-dot" style="background: {ci.color}" title="{repo.label}: {repo.ciStatus || 'unknown'}"></span>
    {/each}
  </div>
{:else if $repoStatus.commitHash || $repoStatus.ciStatus}
  <!-- Single repo: commit hash + CI dot -->
  {@const ci = ciInfo($repoStatus.ciStatus)}
  {@const url = commitUrl($repoStatus.fullName, $repoStatus.commitHash)}
  <div class="mini-repo-status">
    {#if $repoStatus.commitHash}
      {#if url}
        <a href={url} target="_blank" rel="noopener" class="commit-hash">{$repoStatus.commitHash}</a>
      {:else}
        <span class="commit-hash">{$repoStatus.commitHash}</span>
      {/if}
    {/if}
    <span class="ci-dot" style="background: {ci.color}" title="CI: {$repoStatus.ciStatus || 'unknown'}"></span>
  </div>
{/if}

<style>
  .mini-repo-status {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-left: auto;
    padding-left: 8px;
    padding-right: env(safe-area-inset-right, 0px);
    flex-shrink: 0;
  }

  .commit-hash {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.7rem;
    color: hsl(var(--muted-foreground));
    text-decoration: none;
  }

  .commit-hash:hover {
    color: hsl(var(--foreground));
  }

  .ci-dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    flex-shrink: 0;
  }
</style>
