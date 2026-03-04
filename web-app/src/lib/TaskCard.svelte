<script>
  import { kanbanData, coworkers, repoStatus } from './store.js'
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
      <div class="task-description mt-1.5">
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
