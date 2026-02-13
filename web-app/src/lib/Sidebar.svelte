<script>
  import ChannelList from './ChannelList.svelte'
  import CoworkerStatus from './CoworkerStatus.svelte'
  import UsageBars from './UsageBars.svelte'
  import AuthSwitcher from './AuthSwitcher.svelte'
  import { connected } from './store.js'
  import {
    pushSupported,
    pushPermission,
    pushSubscribed,
    subscribePush,
    unsubscribePush,
  } from './push.js'
  import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '$lib/components/ui/collapsible'
  import ChevronDown from '@lucide/svelte/icons/chevron-down'
  import ChevronRight from '@lucide/svelte/icons/chevron-right'

  let channelsExpanded = $state(true)

  async function togglePush() {
    if ($pushSubscribed) {
      await unsubscribePush()
    } else {
      await subscribePush()
    }
  }
</script>

<div class="flex h-full flex-col border-r-2 border-[#2a2a2a] bg-[#0f0f0f]">
  <!-- Channels section -->
  <Collapsible class="border-b border-[#1a1a1a]" bind:open={channelsExpanded}>
    <CollapsibleTrigger class="flex w-full items-center justify-between bg-transparent px-3.5 py-2.5 text-left text-[0.7rem] font-bold tracking-wide text-[#606060] transition-all duration-150 hover:bg-[#1a1a1a] hover:text-[#a0a0a0]">
      <span class="flex-1 text-left">CHANNELS</span>
      {#if channelsExpanded}
        <ChevronDown class="size-3 opacity-60" />
      {:else}
        <ChevronRight class="size-3 opacity-60" />
      {/if}
    </CollapsibleTrigger>
    <CollapsibleContent>
      <div class="p-0">
        <ChannelList />
      </div>
    </CollapsibleContent>
  </Collapsible>

  <!-- Footer section with coworker status, usage bars, and controls -->
  <div class="mt-auto flex flex-col gap-2 border-t-2 border-[#2a2a2a] bg-[#0a0a0a] p-2 pb-safe-offset-2">
    <CoworkerStatus />
    <UsageBars />
    <div class="flex items-center justify-end gap-2.5">
      <AuthSwitcher />
      {#if $pushSupported}
        <button
          class="cursor-pointer border-none bg-transparent p-1 text-base transition-opacity duration-200 {$pushSubscribed ? 'opacity-100' : 'opacity-50'} {$pushPermission === 'denied' ? 'cursor-not-allowed opacity-25' : ''} hover:opacity-100"
          onclick={togglePush}
          disabled={$pushPermission === 'denied'}
          title={$pushPermission === 'denied'
            ? 'Notifications blocked'
            : $pushSubscribed
              ? 'Disable notifications'
              : 'Enable notifications'}
        >
          {$pushSubscribed ? '🔔' : '🔕'}
        </button>
      {/if}
      <span
        class="size-2 shrink-0 rounded-full bg-[#af5f5f] shadow-[0_0_6px_rgba(175,95,95,0.4)] {$connected ? 'bg-[#5faf5f] shadow-[0_0_6px_rgba(95,175,95,0.5)]' : ''}"
        title={$connected ? 'Connected' : 'Disconnected'}
      ></span>
    </div>
  </div>
</div>
