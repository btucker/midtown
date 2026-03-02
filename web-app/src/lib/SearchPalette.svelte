<script>
  import { CommandDialog, CommandInput, CommandList, CommandEmpty, CommandGroup, CommandItem } from '$lib/components/ui/command'
  import HashIcon from '@lucide/svelte/icons/hash'
  import AtSignIcon from '@lucide/svelte/icons/at-sign'
  import { searchMessages, fetchHistory, selectDm } from './api.js'
  import { activeChannel, channels, messagesByChannel } from './store.js'
  import { getSenderColor } from './messageUtils.js'

  let { open = $bindable(false) } = $props()

  let query = $state('')
  let results = $state([])
  let loading = $state(false)
  let error = $state(false)
  // Plain `let` — timer IDs have no UI relevance and must not be reactive
  // (writing $state inside $effect re-triggers the effect infinitely).
  let debounceTimer = null

  // Debounced search: fire API call 300ms after the user stops typing.
  // Guards against stale responses: only apply results if the query hasn't
  // changed and the dialog is still open when the response arrives.
  $effect(() => {
    const q = query.trim()
    if (!q) {
      results = []
      loading = false
      error = false
      return
    }
    loading = true
    error = false
    debounceTimer = setTimeout(async () => {
      try {
        const response = await searchMessages(q)
        if (query.trim() === q && open) {
          results = response.results || []
          loading = false
          error = !!response.error
        }
      } catch {
        if (query.trim() === q && open) {
          results = []
          loading = false
          error = true
        }
      }
    }, 300)
    return () => clearTimeout(debounceTimer)
  })

  // Reset state when dialog closes
  $effect(() => {
    if (!open) {
      query = ''
      results = []
      loading = false
      error = false
    }
  })

  function selectResult(result) {
    if (result.channel.startsWith('dm-')) {
      // DM channels need selectDm() to ensure the channel appears in the sidebar
      const coworkerName = result.channel.replace(/^dm-/, '')
      selectDm(coworkerName)
    } else {
      activeChannel.set(result.channel)
      // Clear unread count for the navigated channel
      channels.update((list) =>
        list.map((ch) => (ch.name === result.channel ? { ...ch, unread: 0 } : ch))
      )
      // Ensure channel messages are loaded
      const existing = $messagesByChannel[result.channel]
      if (!existing || existing.length === 0) {
        fetchHistory(result.channel)
      }
    }
    open = false
  }

  function formatRelativeTime(timestamp) {
    try {
      const date = new Date(timestamp)
      const now = new Date()
      const diffMs = now - date
      const diffMins = Math.floor(diffMs / 60000)
      if (diffMins < 1) return 'now'
      if (diffMins < 60) return `${diffMins}m`
      const diffHours = Math.floor(diffMins / 60)
      if (diffHours < 24) return `${diffHours}h`
      const diffDays = Math.floor(diffHours / 24)
      if (diffDays < 7) return `${diffDays}d`
      return date.toLocaleDateString([], { month: 'short', day: 'numeric' })
    } catch {
      return ''
    }
  }
</script>

<CommandDialog bind:open shouldFilter={false} title="Search messages" description="Search across all channel messages">
  <CommandInput placeholder="Search messages..." bind:value={query} />
  <CommandList class="max-h-[min(400px,calc(60vh-env(safe-area-inset-bottom)))]">
    {#if loading}
      <div class="py-6 text-center text-sm text-muted-foreground">Searching...</div>
    {:else if error}
      <div class="py-6 text-center text-sm text-destructive">Search failed. Try again.</div>
    {:else if query.trim() && results.length === 0}
      <CommandEmpty>No results found.</CommandEmpty>
    {:else if results.length > 0}
      <CommandGroup>
        {#each results as result}
          <CommandItem value={result.id} onSelect={() => selectResult(result)}>
            <div class="flex w-full min-w-0 items-start gap-2">
              <div class="flex shrink-0 items-center gap-1 pt-0.5">
                {#if result.channel.startsWith('dm-')}
                  <AtSignIcon class="size-3 text-muted-foreground" />
                {:else}
                  <HashIcon class="size-3 text-muted-foreground" />
                {/if}
                <span class="text-xs text-muted-foreground">{result.channel}</span>
              </div>
              <div class="min-w-0 flex-1">
                <div class="flex items-baseline gap-1.5">
                  <span class="text-xs font-medium" style="color: {getSenderColor(result.from)}">{result.from}</span>
                  <span class="text-[0.65rem] text-muted-foreground/60">{formatRelativeTime(result.timestamp)}</span>
                </div>
                <p class="truncate text-xs text-muted-foreground">{result.snippet || result.content}</p>
              </div>
            </div>
          </CommandItem>
        {/each}
      </CommandGroup>
    {/if}
  </CommandList>
  {#if !loading && query.trim() && results.length > 0}
    <div class="border-t px-3 py-1.5 text-[0.65rem] text-muted-foreground/50">
      {results.length} result{results.length !== 1 ? 's' : ''}
    </div>
  {/if}
</CommandDialog>
