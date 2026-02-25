<script>
  import { activeChannel, kanbanData, repoStatus, repoStatuses } from './store.js'
  import { getChannelPrs } from './channelUtils.js'
  import { formatRelativeTime } from './utils.js'

  let openPrs = $derived(getChannelPrs($activeChannel, $kanbanData))
  // done PRs don't include task_id so they can't be channel-filtered; show all
  let mergedPrs = $derived($kanbanData.done || [])

  function getPrUrl(pr) {
    if (pr.repo && $repoStatuses.length > 0) {
      const info = $repoStatuses.find((s) => s.label === pr.repo)
      if (info?.fullName) return `https://github.com/${info.fullName}/pull/${pr.number}`
    }
    if ($repoStatus.fullName) return `https://github.com/${$repoStatus.fullName}/pull/${pr.number}`
    return null
  }

  function statusInfo(status) {
    switch (status) {
      case 'ci_passed': return { label: 'CI passed', color: 'hsl(var(--status-green))', char: '●' }
      case 'ci_failed': return { label: 'CI failed', color: 'hsl(var(--status-red))', char: '●' }
      case 'ci_pending': return { label: 'CI running', color: 'hsl(var(--status-amber))', char: '●' }
      case 'approved': return { label: 'Approved', color: 'hsl(var(--status-green))', char: '✓' }
      default: return { label: status || 'Unknown', color: 'hsl(var(--muted-foreground))', char: '○' }
    }
  }
</script>

<div class="pr-list">
  <!-- Open PRs -->
  {#if openPrs.length > 0}
    <table>
      <thead>
        <tr>
          <th>PR</th>
          <th>Title</th>
          <th>Status</th>
          <th>Author</th>
          <th>Reviewer</th>
          <th>Task</th>
          <th>Age</th>
        </tr>
      </thead>
      <tbody>
        {#each openPrs as pr (`${pr.repo || ''}#${pr.number}`)}
          {@const url = getPrUrl(pr)}
          {@const si = statusInfo(pr.status)}
          <tr>
            <td class="pr-number">
              {#if url}
                <a href={url} target="_blank" rel="noopener">#{pr.number}</a>
              {:else}
                #{pr.number}
              {/if}
            </td>
            <td class="pr-title">
              {#if url}
                <a href={url} target="_blank" rel="noopener">{pr.title}</a>
              {:else}
                {pr.title}
              {/if}
            </td>
            <td class="pr-status">
              <span class="status-dot" style="color: {si.color}" title={si.label}>{si.char}</span>
              <span class="status-label">{si.label}</span>
            </td>
            <td class="pr-meta">{pr.author || '—'}</td>
            <td class="pr-meta">{pr.reviewer || '—'}</td>
            <td class="pr-meta">
              {#if pr.task_id}
                <span class="task-id">!{pr.task_id}</span>
                {#if pr.task_name}
                  <span class="task-name">{pr.task_name}</span>
                {/if}
              {:else}
                —
              {/if}
            </td>
            <td class="pr-age">{pr.created_at ? formatRelativeTime(pr.created_at) : '—'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <p class="empty-state">No open PRs in this channel</p>
  {/if}

  <!-- Recently merged PRs -->
  {#if mergedPrs.length > 0}
    <div class="merged-section">
      <h3 class="merged-heading">Recently merged</h3>
      <table>
        <thead>
          <tr>
            <th>PR</th>
            <th>Title</th>
            <th>Merged</th>
          </tr>
        </thead>
        <tbody>
          {#each mergedPrs as pr (`${pr.repo || ''}#${pr.number}`)}
            {@const url = getPrUrl(pr)}
            <tr class="merged-row">
              <td class="pr-number">
                {#if url}
                  <a href={url} target="_blank" rel="noopener">#{pr.number}</a>
                {:else}
                  #{pr.number}
                {/if}
              </td>
              <td class="pr-title">
                {#if url}
                  <a href={url} target="_blank" rel="noopener">{pr.title}</a>
                {:else}
                  {pr.title}
                {/if}
              </td>
              <td class="pr-age">{pr.mergedAt ? formatRelativeTime(pr.mergedAt) : '—'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .pr-list {
    flex: 1;
    overflow-y: auto;
    padding: 16px;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
  }

  th {
    text-align: left;
    padding: 6px 10px;
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: hsl(var(--muted-foreground));
    border-bottom: 1px solid hsl(var(--border));
    white-space: nowrap;
  }

  td {
    padding: 7px 10px;
    border-bottom: 1px solid hsl(var(--border) / 0.5);
    vertical-align: middle;
  }

  tr:last-child td {
    border-bottom: none;
  }

  tr:hover td {
    background: hsl(var(--accent) / 0.4);
  }

  .pr-number {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.78rem;
    white-space: nowrap;
    color: hsl(var(--link-pr, var(--primary)));
  }

  .pr-number a,
  .pr-title a {
    color: inherit;
    text-decoration: none;
  }

  .pr-number a:hover,
  .pr-title a:hover {
    text-decoration: underline;
  }

  .pr-title {
    max-width: 32ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: hsl(var(--foreground));
  }

  .pr-status {
    white-space: nowrap;
  }

  .status-dot {
    font-size: 0.7rem;
    margin-right: 4px;
  }

  .status-label {
    font-size: 0.75rem;
    color: hsl(var(--muted-foreground));
  }

  .pr-meta {
    color: hsl(var(--muted-foreground));
    font-size: 0.78rem;
    white-space: nowrap;
    max-width: 14ch;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .task-id {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.75rem;
    color: hsl(var(--link-task, var(--primary)));
    margin-right: 4px;
  }

  .task-name {
    color: hsl(var(--muted-foreground));
    font-size: 0.75rem;
  }

  .pr-age {
    color: hsl(var(--muted-foreground));
    font-size: 0.75rem;
    white-space: nowrap;
  }

  .empty-state {
    color: hsl(var(--muted-foreground));
    font-size: 0.85rem;
    padding: 24px 0;
    text-align: center;
  }

  .merged-section {
    margin-top: 24px;
  }

  .merged-heading {
    font-size: 0.75rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: hsl(var(--muted-foreground));
    margin-bottom: 8px;
  }

  .merged-row td {
    opacity: 0.7;
  }
</style>
