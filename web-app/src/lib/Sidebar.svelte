<script>
  import ChannelList from './ChannelList.svelte'
  import CoworkerStatus from './CoworkerStatus.svelte'
  import OpsChannel from './OpsChannel.svelte'
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
  import Bell from '@lucide/svelte/icons/bell'
  import BellOff from '@lucide/svelte/icons/bell-off'

  let channelsExpanded = $state(true)

  async function togglePush() {
    if ($pushSubscribed) {
      await unsubscribePush()
    } else {
      await subscribePush()
    }
  }
</script>

<div class="flex h-full flex-col border-r-2 border-sidebar-border bg-sidebar">
  <!-- Channels section -->
  <Collapsible class="border-b border-sidebar-border" bind:open={channelsExpanded}>
    <CollapsibleTrigger class="flex w-full items-center justify-between bg-transparent px-3.5 py-2.5 text-left text-[0.7rem] font-bold tracking-wide text-muted-foreground transition-all duration-150 hover:bg-sidebar-accent hover:text-sidebar-foreground">
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

  <!-- Footer section with ops channel, coworker status, usage bars, and controls -->
  <div class="mt-auto flex flex-col gap-2 border-t-2 border-sidebar-border bg-sidebar p-2 pb-safe-offset-2">
    <OpsChannel />
    <CoworkerStatus />
    <UsageBars />
    <div class="flex items-center justify-end gap-2.5">
      <AuthSwitcher />
      {#if $pushSupported}
        <button
          class="push-toggle flex cursor-pointer items-center rounded border-none bg-transparent p-[5px] text-muted-foreground transition-all duration-200 hover:bg-accent hover:text-foreground {$pushSubscribed ? 'subscribed' : ''} {$pushPermission === 'denied' ? 'denied cursor-not-allowed opacity-25' : ''}"
          onclick={togglePush}
          disabled={$pushPermission === 'denied'}
          title={$pushPermission === 'denied'
            ? 'Notifications blocked in browser settings'
            : $pushSubscribed
              ? 'Disable push notifications'
              : 'Enable push notifications'}
        >
          {#if $pushSubscribed}
            <Bell size={16} />
          {:else}
            <BellOff size={16} />
          {/if}
        </button>
      {/if}
      <span
        class="size-2 shrink-0 rounded-full bg-[#af5f5f] shadow-[0_0_6px_rgba(175,95,95,0.4)] {$connected ? 'bg-[#5faf5f] shadow-[0_0_6px_rgba(95,175,95,0.5)]' : ''}"
        title={$connected ? 'Connected' : 'Disconnected'}
      ></span>
    </div>
  </div>
</div>
