<script>
  import { kanbanData, coworkers, repoStatus } from './store.js'
  import { selectDm } from './api.js'
  import { getSenderColor } from './messageUtils.js'
  import { renderContent } from './markdown.js'
  import Feather from '@lucide/svelte/icons/feather'
  import Search from '@lucide/svelte/icons/search'

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

  // Derived data for progress display
  const isActive = $derived(task.status === 'in_progress')
  const isBlocked = $derived(task.blocked_by?.length > 0)
  const cw = $derived(task.owner ? cwMap.get(task.owner) : null)
  const hasProgress = $derived(cw?.progress != null)
  const reviewer = $derived(relatedPr?.reviewer)
  const reviewPosted = $derived(relatedPr?.review_posted || false)

  function statusBarColor(task, cw) {
    if (task.status === 'done') return 'hsl(var(--accent-green, 142 71% 45%))'
    if (task.status !== 'in_progress') return 'hsl(var(--muted-foreground) / 0.3)'
    if (task.owner) return getSenderColor(task.owner)
    return 'hsl(var(--accent-teal))'
  }

  /**
   * Build progress bar segments: 70% dev, 20% review, 10% fix.
   * Returns array of { width, color } for each filled segment.
   */
  function lifecycleSegments(cwProgress, reviewer, reviewPosted, ownerColor, reviewerColor) {
    const segments = []
    if (!reviewer) {
      segments.push({ width: (cwProgress / 100) * 70, color: ownerColor })
    } else if (!reviewPosted) {
      segments.push({ width: 70, color: ownerColor })
      segments.push({ width: 20, color: reviewerColor })
    } else {
      segments.push({ width: 70, color: ownerColor })
      segments.push({ width: 20, color: reviewerColor })
    }
    return segments
  }

  // Render description markdown once reactively
  const descriptionHtml = $derived(task.description ? renderContent(task.description) : '')
</script>

<div class="task-card rounded-md border border-border bg-card mb-2" data-testid="task-card">
  <div class="flex items-stretch">
    <span class="status-bar rounded-l-md" style="background: {statusBarColor(task, cw)}"></span>
    <div class="flex-1 min-w-0 px-3 py-2.5">
      <div class="flex items-center gap-2">
        <span class="text-[hsl(var(--link-task))] font-bold text-[0.85rem] shrink-0">!{task.id}</span>
        <div class="flex-1 min-w-0 text-[0.88rem] text-foreground leading-snug">{task.subject}</div>
      </div>

      <div class="flex flex-wrap items-center gap-2 mt-1.5">
        {#if task.owner}
          {@const ownerGlow = isActive && (!reviewer || reviewPosted)}
          <button
            class="avatar-chip"
            class:glowing={ownerGlow}
            style="background-color: {getSenderColor(task.owner)}{ownerGlow ? `; --glow-color: ${getSenderColor(task.owner)}` : ''}"
            title="{task.owner}{cw?.phase ? ` · ${cw.phase}` : ''}"
            onclick={() => selectDm(task.owner)}
          >{task.owner[0].toUpperCase()}<span class="chip-badge"><Feather size={11} strokeWidth={2.5} fill="white" /></span></button>
          <button
            class="text-muted-foreground text-[0.75rem] bg-transparent border-none p-0 m-0 cursor-pointer hover:underline"
            onclick={() => selectDm(task.owner)}
            title="Open DM with {task.owner}"
          >{task.owner}</button>
        {/if}
        {#if reviewer}
          {@const reviewerGlow = isActive && !reviewPosted}
          <button
            class="avatar-chip"
            class:glowing={reviewerGlow}
            style="background-color: {getSenderColor(reviewer)}{reviewerGlow ? `; --glow-color: ${getSenderColor(reviewer)}` : ''}"
            title="{reviewer} · {reviewPosted ? 'reviewed' : 'reviewing'}"
            onclick={() => selectDm(reviewer)}
          >{reviewer[0].toUpperCase()}<span class="chip-badge"><Search size={11} strokeWidth={2.5} fill="white" style="transform: scaleX(-1)" /></span></button>
          <button
            class="text-muted-foreground text-[0.75rem] bg-transparent border-none p-0 m-0 cursor-pointer hover:underline"
            onclick={() => selectDm(reviewer)}
            title="Open DM with {reviewer}"
          >{reviewer} {reviewPosted ? 'reviewed' : 'reviewing'}</button>
        {/if}
        {#if relatedPr && prUrl}
          <a
            href={prUrl}
            target="_blank"
            rel="noopener"
            class="text-[hsl(var(--link-default))] text-[0.75rem] no-underline hover:underline"
          >PR #{relatedPr.number}</a>
        {/if}
        {#if isBlocked}
          <span class="text-[0.75rem] text-destructive">
            blocked by {task.blocked_by.map((b) => `!${b}`).join(', ')}
          </span>
        {/if}
      </div>

      {#if isActive && hasProgress && task.owner}
        {@const segments = lifecycleSegments(cw?.progress ?? 0, reviewer, reviewPosted, getSenderColor(task.owner), reviewer ? getSenderColor(reviewer) : null)}
        {@const totalPct = Math.round(segments.reduce((sum, s) => sum + s.width, 0))}
        <div class="flex items-center gap-2 mt-2">
          <div class="flex-1 h-[4px] bg-muted rounded-sm overflow-hidden flex">
            {#each segments as seg}
              <div
                class="h-full transition-[width] duration-500 ease-in-out"
                style="width: {seg.width}%; background: {seg.color}"
              ></div>
            {/each}
          </div>
          <span class="flex-shrink-0 text-[0.68rem] text-[hsl(var(--accent-teal))] tabular-nums font-medium">{totalPct}%</span>
        </div>
      {/if}

      {#if task.description}
        <details class="mt-2">
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
  </div>
</div>

<style>
  .status-bar {
    width: 4px;
    flex-shrink: 0;
  }

  .avatar-chip {
    position: relative;
    flex-shrink: 0;
    width: 20px;
    height: 20px;
    border-radius: 4px;
    border: none;
    padding: 0;
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.62rem;
    font-weight: 700;
    font-family: var(--font-sans);
    color: white;
    line-height: 1;
    cursor: pointer;
  }

  .avatar-chip:hover {
    opacity: 0.85;
  }

  .chip-badge {
    position: absolute;
    bottom: -4px;
    right: -4px;
    display: flex;
    align-items: center;
    justify-content: center;
    color: hsl(var(--sidebar-foreground));
  }

  .glowing {
    box-shadow: 0 0 8px 2px var(--glow-color);
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
