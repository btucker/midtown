<script>
  import { kanbanData, coworkers, repoStatus, repoStatuses, channels, activeChannel, daemonStatus } from './store.js'
  import { openTaskThread } from './api.js'
  import { getPrUrl as getPrUrlUtil } from './channelUtils.js'
  import { renderContent } from './markdown.js'
  import TaskRow from './TaskRow.svelte'

  let { task } = $props()

  function getPrUrl(prNumber) {
    if (!prNumber || !$repoStatus.fullName) return null
    return `https://github.com/${$repoStatus.fullName}/pull/${prNumber}`
  }

  // Map coworker name → coworker object for progress/phase lookup
  const cwMap = $derived(new Map($coworkers.map(cw => [cw.name, cw])))

  // Find the related open PR for this task
  const relatedPr = $derived(
    $kanbanData.review.find((pr) => String(pr.task_id) === String(task.id))
  )

  const prUrl = $derived(relatedPr ? getPrUrl(relatedPr.number) : null)
  const cw = $derived(task.owner ? cwMap.get(task.owner) : null)
  const reviewer = $derived(relatedPr?.reviewer)
  const reviewPosted = $derived(relatedPr?.review_posted || false)

  // Render description markdown once reactively
  const descriptionHtml = $derived(task.description ? renderContent(task.description) : '')

  function handleDescriptionClick(e) {
    const target = e.target
    if (target.classList.contains('channel-link')) {
      e.preventDefault()
      const name = target.dataset.channel
      if ($channels.some((ch) => ch.name === name)) {
        $activeChannel = name
      }
    } else if (target.classList.contains('task-link')) {
      e.preventDefault()
      const taskId = target.dataset.task
      const tasks = $daemonStatus?.tasks || []
      const found = tasks.find((t) => String(t.id) === String(taskId))
      if (found) {
        openTaskThread(found, found.channel || $activeChannel)
      }
    } else if (target.classList.contains('pr-link')) {
      e.preventDefault()
      const prNum = target.dataset.pr
      const url = getPrUrlUtil(prNum, $kanbanData, $repoStatuses, $repoStatus.fullName)
      if (url) window.open(url, '_blank', 'noopener')
    } else if (target.classList.contains('coworker-link')) {
      e.preventDefault()
    }
  }
</script>

<div class="task-card" data-testid="task-card">
  <TaskRow
    {task}
    {cw}
    {reviewer}
    {reviewPosted}
  />

  {#if relatedPr && prUrl}
    <div class="px-3 pb-1.5">
      <a
        href={prUrl}
        target="_blank"
        rel="noopener"
        class="text-[hsl(var(--link-default))] text-[0.72rem] no-underline hover:underline"
      >PR #{relatedPr.number}</a>
    </div>
  {/if}

  {#if task.description}
    <details class="px-3 pb-2">
      <summary class="text-[0.72rem] text-muted-foreground/60 cursor-pointer select-none list-none flex items-center gap-1">
        <span class="disclosure-triangle">▶</span>
        <span>Description</span>
      </summary>
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="task-description mt-1.5" onclick={handleDescriptionClick}>
        {@html descriptionHtml}
      </div>
    </details>
  {/if}
</div>

<style>
  .task-card {
    border: 1px solid hsl(var(--border));
    border-radius: 0.375rem;
    background: hsl(var(--card));
    margin-bottom: 0.5rem;
  }

  details[open] .disclosure-triangle {
    display: inline-block;
    transform: rotate(90deg);
  }

  .task-description {
    font-size: 0.78rem;
    line-height: 1.5;
    color: hsl(var(--muted-foreground));
  }

  .task-description :global(p) {
    margin: 0.3em 0;
  }

  .task-description :global(p:first-child) {
    margin-top: 0;
  }

  .task-description :global(p:last-child) {
    margin-bottom: 0;
  }

  .task-description :global(ul),
  .task-description :global(ol) {
    margin: 0.3em 0;
    padding-left: 1.5em;
  }

  .task-description :global(li) {
    margin: 0.15em 0;
  }

  .task-description :global(code) {
    font-size: 0.85em;
    background: hsl(var(--muted));
    padding: 0.1em 0.35em;
    border-radius: 3px;
  }

  .task-description :global(pre) {
    margin: 0.5em 0;
    padding: 0.5em;
    border-radius: 4px;
    background: hsl(var(--muted));
    overflow-x: auto;
  }

  .task-description :global(pre code) {
    background: none;
    padding: 0;
  }

  .task-description :global(a) {
    color: hsl(var(--link-default));
  }

  .task-description :global(a.task-link) {
    color: hsl(var(--link-task));
    font-weight: 600;
    cursor: pointer;
  }

  .task-description :global(a.pr-link) {
    color: hsl(var(--link-pr));
    font-weight: 600;
    cursor: pointer;
  }

  .task-description :global(a.channel-link) {
    color: hsl(var(--link-default));
    font-weight: 600;
    cursor: pointer;
  }

  .task-description :global(a.coworker-link) {
    color: hsl(var(--link-coworker));
    font-weight: 600;
    cursor: pointer;
  }

  .task-description :global(strong) {
    color: hsl(var(--foreground));
    font-weight: 600;
  }

  .task-description :global(blockquote) {
    border-left: 3px solid hsl(var(--border));
    margin: 0.3em 0;
    padding-left: 0.75em;
    color: hsl(var(--muted-foreground) / 0.8);
  }
</style>
