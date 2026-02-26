<script>
  import { kanbanData, repoStatus } from './store.js'

  let { task } = $props()

  function getStatusColor(status) {
    switch (status) {
      case 'in_progress': return '#5fafaf'
      case 'pending': return '#d7d787'
      case 'done': return '#5faf5f'
      default: return 'hsl(var(--muted-foreground))'
    }
  }

  function getPrUrl(prNumber) {
    if (!prNumber || !$repoStatus.fullName) return null
    return `https://github.com/${$repoStatus.fullName}/pull/${prNumber}`
  }

  // Find the related open PR for this task
  const relatedPr = $derived(
    $kanbanData.review.find((pr) => String(pr.task_id) === String(task.id))
  )

  const prUrl = $derived(relatedPr ? getPrUrl(relatedPr.number) : null)
</script>

<div class="rounded-md border border-border bg-card px-3 py-2.5 mb-2" data-testid="task-card">
  <div class="flex items-start gap-2">
    <span class="text-[hsl(var(--link-task))] font-bold text-[0.85rem] shrink-0">!{task.id}</span>
    <div class="flex-1 min-w-0">
      <div class="text-[0.88rem] text-foreground leading-snug">{task.subject}</div>
      <div class="flex flex-wrap items-center gap-1.5 mt-1.5">
        {#if task.status}
          <span
            class="inline-block px-1.5 py-0.5 rounded text-[0.68rem] font-semibold text-[#111] capitalize leading-none"
            style="background: {getStatusColor(task.status)}"
          >{task.status.replace('_', ' ')}</span>
        {/if}
        {#if task.owner}
          <span class="text-muted-foreground text-[0.75rem]">{task.owner}</span>
        {/if}
        {#if relatedPr && prUrl}
          <a
            href={prUrl}
            target="_blank"
            rel="noopener"
            class="text-[hsl(var(--link-default))] text-[0.75rem] no-underline hover:underline"
          >PR #{relatedPr.number}</a>
        {/if}
        {#if task.blocked_by?.length > 0}
          <span class="text-[0.75rem] text-destructive">
            blocked by {task.blocked_by.map((b) => `!${b}`).join(', ')}
          </span>
        {/if}
      </div>
      {#if task.description}
        <details class="mt-1.5">
          <summary class="text-[0.72rem] text-muted-foreground/60 cursor-pointer select-none list-none flex items-center gap-1">
            <span class="disclosure-triangle">▶</span>
            <span>Description</span>
          </summary>
          <p class="text-[0.75rem] text-muted-foreground mt-1 whitespace-pre-wrap break-words leading-snug">
            {task.description}
          </p>
        </details>
      {/if}
    </div>
  </div>
</div>

<style>
  details[open] .disclosure-triangle {
    display: inline-block;
    transform: rotate(90deg);
  }
</style>
