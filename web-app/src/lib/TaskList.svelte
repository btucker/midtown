<script>
  import { kanbanData, coworkers } from './store.js'
  import { openTaskThread, selectDm } from './api.js'
  import { getSenderColor } from './messageUtils.js'
  import Feather from '@lucide/svelte/icons/feather'
  import Search from '@lucide/svelte/icons/search'

  let { channelName = '' } = $props()

  /**
   * Filter tasks by channel field.
   * For 'midtown' channel, return all tasks.
   * For topic channels, only return tasks explicitly assigned to that channel.
   *
   * This matches the TUI implementation in src/bin/midtown/cli/chat/ui/board.rs
   * which groups tasks by `task.channel.as_deref().unwrap_or(main_channel)`.
   */
  function filterTasksByChannel(tasks, channel) {
    if (channel === 'midtown') {
      // Main channel shows only tasks with no channel (or channel='midtown').
      // Tasks assigned to other channels appear there only, not duplicated here.
      // Matches TUI's unwrap_or(main_channel) grouping.
      return tasks.filter(task => !task.channel || task.channel === 'midtown')
    }

    // Topic channels only show tasks explicitly assigned to that channel via the channel field
    return tasks.filter(task => task.channel === channel)
  }

  // Derived: tasks for this channel
  const channelTasks = $derived.by(() => {
    const allTasks = [
      ...$kanbanData.inProgress.map(t => ({ ...t, status: 'in_progress' })),
      ...$kanbanData.backlog.map(t => ({ ...t, status: 'pending' }))
    ]
    return filterTasksByChannel(allTasks, channelName)
  })

  // Map coworker name → coworker object for progress/phase lookup
  const cwMap = $derived(new Map($coworkers.map(cw => [cw.name, cw])))

  // Map task_id → { reviewer, reviewPosted } for showing reviewer avatar + glow state
  const taskReviewerMap = $derived.by(() => {
    const map = new Map()
    for (const pr of $kanbanData.review) {
      if (pr.task_id != null && pr.reviewer) {
        map.set(String(pr.task_id), { reviewer: pr.reviewer, reviewPosted: pr.review_posted || false })
      }
    }
    return map
  })

  function statusBarColor(task, cw) {
    if (task.status !== 'in_progress') return 'hsl(var(--muted-foreground) / 0.3)'
    // Use the owner's avatar color for the status bar when in-progress
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
      // Dev phase: author filling 0-70%
      segments.push({ width: (cwProgress / 100) * 70, color: ownerColor })
    } else if (!reviewPosted) {
      // Review phase: dev complete (70%), reviewer filling the 20% block
      segments.push({ width: 70, color: ownerColor })
      segments.push({ width: 20, color: reviewerColor })
    } else {
      // Fix phase: dev (70%) + review (20%) complete, fix segment not filled
      // (coworker progress is cumulative and not reset between phases, so we
      // can't derive fix-specific progress from it — show 90% until task completes)
      segments.push({ width: 70, color: ownerColor })
      segments.push({ width: 20, color: reviewerColor })
    }
    return segments
  }

  function handleTaskClick(task) {
    openTaskThread(task, task.channel || channelName)
  }
</script>

<div class="flex flex-col gap-0.5 py-1 pb-1.5">
  {#each channelTasks as task}
    {@const isActive = task.status === 'in_progress'}
    {@const isBlocked = task.blocked_by?.length > 0}
    {@const cw = task.owner ? cwMap.get(task.owner) : null}
    {@const hasProgress = cw?.progress != null}
    {@const reviewInfo = taskReviewerMap.get(String(task.id))}
    {@const reviewer = reviewInfo?.reviewer}
    {@const reviewPosted = reviewInfo?.reviewPosted}
    <button
      class="flex items-stretch gap-1.5 pr-2 py-[5px] border-none bg-transparent cursor-pointer rounded-[5px] transition-[background] duration-100 text-left font-mono text-[0.72rem] leading-[1.3] text-muted-foreground hover:bg-sidebar-accent {isActive ? 'text-sidebar-foreground' : ''} {isBlocked ? 'opacity-65' : ''}"
      onclick={() => handleTaskClick(task)}
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
  {/each}

  {#if channelTasks.length === 0}
    <div class="px-3 py-2 text-[0.72rem] text-muted-foreground italic text-center">No active tasks</div>
  {/if}
</div>

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
