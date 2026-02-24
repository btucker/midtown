<script>
  import { activeChannel, kanbanData } from './store.js'
  import { getChannelTaskCount, getChannelPrs } from './channelUtils.js'

  let channelCounts = $derived(getChannelTaskCount($activeChannel, $kanbanData))
  let channelPrs = $derived(getChannelPrs($activeChannel, $kanbanData))
  let totalTasks = $derived(channelCounts.inProgress + channelCounts.pending + channelCounts.review)
</script>

<div class="hidden md:block bg-[#1a1a1a] border-b-2 border-[#2a2a2a] shrink-0">
  <div class="flex items-center justify-between px-4 py-3">
    <div class="flex items-center gap-3 flex-1 min-w-0">
      <div class="flex items-baseline gap-1 shrink-0">
        <span class="text-[1.2rem] text-[#606060] font-bold">#</span>
        <span class="text-[1.1rem] font-bold font-mono text-[#d0d0d0]">{$activeChannel}</span>
      </div>
      {#if totalTasks > 0 || channelPrs.length > 0}
        <div class="flex items-center gap-1.5 flex-wrap">
          {#if channelPrs.length > 0}
            <span class="text-[0.75rem] px-2 py-[3px] rounded-xl font-semibold whitespace-nowrap bg-[#2a3a5a] text-[#5f87af]" title="{channelPrs.length} active PR{channelPrs.length === 1 ? '' : 's'}">
              {channelPrs.length} PR{channelPrs.length === 1 ? '' : 's'}
            </span>
          {/if}
          {#if channelCounts.inProgress > 0}
            <span class="text-[0.75rem] px-2 py-[3px] rounded-xl font-semibold whitespace-nowrap bg-[#2a3a2a] text-[#5faf5f]" title="{channelCounts.inProgress} in progress">
              {channelCounts.inProgress} in progress
            </span>
          {/if}
          {#if channelCounts.pending > 0}
            <span class="text-[0.75rem] px-2 py-[3px] rounded-xl font-semibold whitespace-nowrap bg-[#3a3a2a] text-[#af5faf]" title="{channelCounts.pending} pending">
              {channelCounts.pending} pending
            </span>
          {/if}
        </div>
      {/if}
    </div>
  </div>
</div>
