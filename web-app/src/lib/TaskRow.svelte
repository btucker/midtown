<script>
  import { selectDm } from './api.js'
  import { getSenderColor } from './messageUtils.js'
  import Feather from '@lucide/svelte/icons/feather'
  import Search from '@lucide/svelte/icons/search'

  let { task, cw = null, reviewer = null, reviewPosted = false, onclick = null } = $props()

  const isActive = $derived(task.status === 'in_progress')
  const isBlocked = $derived(task.blocked_by?.length > 0)
  const hasProgress = $derived(cw?.progress != null)

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
</script>

<button
  class="task-row flex items-stretch gap-1.5 pr-2 py-[5px] border-none bg-transparent cursor-pointer rounded-[5px] transition-[background] duration-100 text-left font-mono text-[0.72rem] leading-[1.3] text-muted-foreground hover:bg-sidebar-accent {isActive ? 'text-sidebar-foreground' : ''} {isBlocked ? 'opacity-65' : ''}"
  {onclick}
>
  <span class="w-[3px] rounded-sm flex-shrink-0" style="background: {statusBarColor(task, cw)}"></span>
  <div class="flex-1 min-w-0 flex flex-col gap-[3px]">
    <div class="flex items-center gap-1.5">
      <span class="flex-shrink-0 font-semibold {isActive ? 'opacity-80' : 'opacity-60'}">!{task.id}</span>
      <span class="flex-1 min-w-0 overflow-hidden text-ellipsis whitespace-nowrap">{task.subject}</span>
      {#if isBlocked}
        <span class="flex-shrink-0 text-[0.62rem] text-[hsl(var(--status-amber))] opacity-85" title="Blocked by !{task.blocked_by[0]}">⧗ !{task.blocked_by[0]}</span>
      {/if}
      {#if task.owner}
        {@const ownerGlow = isActive && (!reviewer || reviewPosted)}
        <button
          class="avatar-chip"
          class:glowing={ownerGlow}
          style="background-color: {getSenderColor(task.owner)}{ownerGlow ? `; --glow-color: ${getSenderColor(task.owner)}` : ''}"
          title="{task.owner}{cw?.phase ? ` · ${cw.phase}` : ''}"
          onclick={(e) => { e.stopPropagation(); selectDm(task.owner) }}
        >{task.owner[0].toUpperCase()}<span class="chip-badge"><Feather size={11} strokeWidth={2.5} fill="white" /></span></button>
      {/if}
      {#if reviewer}
        {@const reviewerGlow = isActive && !reviewPosted}
        <button
          class="avatar-chip"
          class:glowing={reviewerGlow}
          style="background-color: {getSenderColor(reviewer)}{reviewerGlow ? `; --glow-color: ${getSenderColor(reviewer)}` : ''}"
          title="{reviewer} · {reviewPosted ? 'reviewed' : 'reviewing'}"
          onclick={(e) => { e.stopPropagation(); selectDm(reviewer) }}
        >{reviewer[0].toUpperCase()}<span class="chip-badge"><Search size={11} strokeWidth={2.5} fill="white" style="transform: scaleX(-1)" /></span></button>
      {/if}
    </div>
    {#if isActive && hasProgress && task.owner}
      {@const segments = lifecycleSegments(cw?.progress ?? 0, reviewer, reviewPosted, getSenderColor(task.owner), reviewer ? getSenderColor(reviewer) : null)}
      {@const totalPct = Math.round(segments.reduce((sum, s) => sum + s.width, 0))}
      <div class="flex items-center gap-1.5 pr-0.5">
        <div class="flex-1 h-[3px] bg-sidebar-accent rounded-sm overflow-hidden flex">
          {#each segments as seg}
            <div
              class="h-full transition-[width] duration-500 ease-in-out"
              style="width: {seg.width}%; background: {seg.color}"
            ></div>
          {/each}
        </div>
        <span class="flex-shrink-0 text-[0.6rem] text-[hsl(var(--accent-teal))] tabular-nums">{totalPct}%</span>
      </div>
    {/if}
  </div>
</button>

<style>
  .avatar-chip {
    position: relative;
    flex-shrink: 0;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    border: none;
    padding: 0;
    margin: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.55rem;
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
    box-shadow: 0 0 6px 1px var(--glow-color);
  }
</style>
