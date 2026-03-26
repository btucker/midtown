<script module lang="ts">
// Module-level draft storage persists across mount/unmount cycles.
// ThreadPanel is conditionally rendered ({#if $threadData}), so instance-level
// state would be lost when the thread panel closes and reopens.
const threadDrafts = new Map();
let prevThreadId = null;
</script>

<script lang="ts">
  import { threadData, deepLinkMsgId, threadOwnership, threadForkParents, threadForkOwners, activeProject, channels as channelsStore, activeChannel, daemonStatus, kanbanData, repoStatus, repoStatuses, channelSettings } from './store.ts'
  import { sendMessage, closeThread, forkThread, unforkThread, clearErrorCallback, openTaskThread, selectDm } from './api.ts'
  import { handleCodePaste } from './codePaste.ts'
  import { extractPastedFile, updatePreviewUrl, uploadAndSend } from './filePaste.ts'
  import { getPrUrl as getPrUrlUtil } from './channelUtils.ts'
  import { tick, onMount, onDestroy, untrack } from 'svelte'
  import { dateChanged } from './messageUtils.ts'
  import SendHorizontal from '@lucide/svelte/icons/send-horizontal'
  import { openImageLightbox } from './biggerPicture.ts'
  import MessageRow from './MessageRow.svelte'
  import DayDivider from './DayDivider.svelte'
  import ThreadActivityDrawer from './ThreadActivityDrawer.svelte'
  import TaskRow from './TaskRow.svelte'
  import DiffView from './DiffView.svelte'
  import ToolRunSummary from './ToolRunSummary.svelte'
  import { groupTimelineToolRuns, isToolOnly } from './toolRunGrouping.ts'
  import { clearMobileTextarea } from './mobileInput.ts'

  // Thread panel resize state (desktop only)
  const THREAD_PANEL_WIDTH_KEY = 'thread-panel:width'
  const THREAD_PANEL_DEFAULT_WIDTH = 380
  const THREAD_PANEL_MIN_WIDTH = 280
  const THREAD_PANEL_MAX_WIDTH = 600

  let panelWidth = $state((() => {
    if (typeof localStorage === 'undefined') return THREAD_PANEL_DEFAULT_WIDTH
    const saved = localStorage.getItem(THREAD_PANEL_WIDTH_KEY)
    if (saved) {
      const parsed = parseInt(saved, 10)
      if (!isNaN(parsed) && parsed >= THREAD_PANEL_MIN_WIDTH && parsed <= THREAD_PANEL_MAX_WIDTH) {
        return parsed
      }
    }
    return THREAD_PANEL_DEFAULT_WIDTH
  })())

  let isResizing = $state(false)
  let resizeStartX = 0
  let resizeStartWidth = 0

  function handleResizeMouseDown(e) {
    e.preventDefault()
    isResizing = true
    resizeStartX = e.clientX
    resizeStartWidth = panelWidth
    document.addEventListener('mousemove', handleResizeMouseMove)
    document.addEventListener('mouseup', handleResizeMouseUp)
    document.body.style.cursor = 'ew-resize'
    document.body.style.userSelect = 'none'
  }

  function handleResizeMouseMove(e) {
    // Panel is on the right: drag left (negative delta) = wider
    const delta = resizeStartX - e.clientX
    const newWidth = Math.max(THREAD_PANEL_MIN_WIDTH, Math.min(THREAD_PANEL_MAX_WIDTH, resizeStartWidth + delta))
    panelWidth = newWidth
  }

  function handleResizeMouseUp() {
    isResizing = false
    document.removeEventListener('mousemove', handleResizeMouseMove)
    document.removeEventListener('mouseup', handleResizeMouseUp)
    document.body.style.cursor = ''
    document.body.style.userSelect = ''
    if (typeof localStorage !== 'undefined') {
      localStorage.setItem(THREAD_PANEL_WIDTH_KEY, String(panelWidth))
    }
  }

  // Cleanup resize listeners if component is destroyed mid-drag
  $effect(() => {
    return () => {
      document.removeEventListener('mousemove', handleResizeMouseMove)
      document.removeEventListener('mouseup', handleResizeMouseUp)
      document.body.style.cursor = ''
      document.body.style.userSelect = ''
    }
  })

  let replyText = $state('')
  let pendingFile = $state(null)
  let pendingFileUrl = $state(null)
  let uploading = $state(false)
  let desktopScrollArea = $state(null)
  let mobileScrollArea = $state(null)
  let autoScroll = $state(true)
  let desktopTextareaEl = $state(null)
  let mobileTextareaEl = $state(null)
  let isDesktop = $state(typeof window !== 'undefined' && window.matchMedia('(min-width: 1024px)').matches)

  // Manage blob URL for file preview — create once per file, revoke on change/unmount.
  $effect(() => {
    const file = pendingFile
    if (file) {
      const url = URL.createObjectURL(file)
      pendingFileUrl = url
      return () => URL.revokeObjectURL(url)
    } else {
      pendingFileUrl = null
    }
  })

  // Optimistic thinking state: true from the moment the user sends a reply until
  // real InProgress tool items arrive (or 30s timeout).
  let thinking = $state(false)
  let thinkingTimeout = null

  // Whether this thread has a dedicated (forked) session.
  // Only applicable to topic channels (not main or DM channels).
  let isTopicChannel = $derived(
    $threadData?.channelName
      && $threadData.channelName !== ($activeProject || 'midtown')
      && !$threadData.channelName.startsWith('dm-')
  )
  let hasDedicatedSession = $derived(
    $threadData?.parentMessage?.id
      ? ($threadOwnership[$threadData.parentMessage.id] ?? false)
      : false
  )
  // The parent channel lead's name for the active fork (e.g., "web").
  // Used to display fork messages with the parent lead's name/color.
  let forkParentLead = $derived(
    $threadData?.parentMessage?.id
      ? ($threadForkParents[$threadData.parentMessage.id] ?? null)
      : null
  )
  // The fork session's agent name (e.g., "web-discuss-ab12").
  // Used to verify msg.from actually belongs to the fork session.
  let forkOwner = $derived(
    $threadData?.parentMessage?.id
      ? ($threadForkOwners[$threadData.parentMessage.id] ?? null)
      : null
  )
  let forkPending = $state(false)
  let forkErrorCallbackId = $state(null)

  // Clear forkPending and stale error callback when ownership state updates (success path).
  $effect(() => {
    if ($threadData?.parentMessage?.id) {
      const _ownership = $threadOwnership[$threadData.parentMessage.id]
      untrack(() => {
        forkPending = false
        if (forkTimeout) { clearTimeout(forkTimeout); forkTimeout = null }
        // Clear the error callback registered by forkThread/unforkThread so it
        // doesn't fire on the next unrelated server error.
        if (forkErrorCallbackId != null) {
          clearErrorCallback(forkErrorCallbackId)
          forkErrorCallbackId = null
        }
      })
    }
  })

  let forkError = $state(null)
  let forkTimeout = null

  function handleForkToggle() {
    if (!$threadData?.parentMessage?.id || !$threadData?.channelName) return
    forkPending = true
    forkError = null
    if (forkTimeout) clearTimeout(forkTimeout)
    const onError = (msg) => {
      if (!forkPending) return // Timeout already handled this request
      if (forkTimeout) { clearTimeout(forkTimeout); forkTimeout = null }
      forkPending = false
      forkErrorCallbackId = null
      forkError = msg
      // Auto-clear error after 5 seconds
      setTimeout(() => { forkError = null }, 5000)
    }
    // Safety net: if no ownership update arrives within 10s, auto-clear pending state
    forkTimeout = setTimeout(() => {
      forkTimeout = null
      if (forkPending) {
        forkPending = false
        forkError = 'Request timed out'
        setTimeout(() => { forkError = null }, 5000)
      }
    }, 10000)
    if (hasDedicatedSession) {
      forkErrorCallbackId = unforkThread($threadData.parentMessage.id, $threadData.channelName, onError)
    } else {
      forkErrorCallbackId = forkThread($threadData.parentMessage.id, $threadData.channelName, onError)
    }
  }

  // Stable thread identity: changes only when a different thread is opened or closed.
  // Using $derived ensures the clearing effect below re-runs only on actual thread switches,
  // not on every message-array update (which reassigns $threadData but keeps the same id).
  // Falls back to a task-based key for task threads without a parent message (openTaskThread
  // with no message_id), so drafts and thinking state still work for card-only threads.
  let currentThreadId = $derived(
    $threadData?.parentMessage?.id
      ?? ($threadData?.tasks?.[0]?.id != null ? `task-${$threadData.tasks[0].id}` : null)
  )

  // Clear thinking, fork state, and reset autoScroll when thread is closed or switched.
  // Fork state must be cleared here (not just in the ownership effect) because switching
  // to a task-only thread (no parentMessage.id) would leave stale forkTimeout running.
  $effect(() => {
    currentThreadId // track dependency — re-runs only when thread identity changes
    untrack(() => {
      thinking = false
      autoScroll = true
      forkPending = false
      forkError = null
    })
    if (thinkingTimeout) {
      clearTimeout(thinkingTimeout)
      thinkingTimeout = null
    }
    if (forkTimeout) {
      clearTimeout(forkTimeout)
      forkTimeout = null
    }
  })

  // Save/restore drafts when switching threads
  $effect(() => {
    const tid = currentThreadId
    if (prevThreadId !== null && prevThreadId !== tid) {
      const currentReply = untrack(() => replyText)
      const currentFile = untrack(() => pendingFile)
      if (currentReply.trim() || currentFile) {
        threadDrafts.set(prevThreadId, { text: currentReply, file: currentFile })
      } else {
        threadDrafts.delete(prevThreadId)
      }
    }
    if (prevThreadId !== tid) {
      const draft = tid !== null ? threadDrafts.get(tid) : null
      untrack(() => {
        replyText = draft?.text ?? ''
        pendingFile = draft?.file ?? null
      })
      // resizeTextarea must run after the DOM reflects the new draft content
      tick().then(() => resizeTextarea())
    }
    prevThreadId = tid
  })

  // Clear thinking when in-progress tool blocks appear on thread messages.
  // A tool block is in-progress when output === null and no later block with
  // the same call_id has output set (i.e. completed). We only clear thinking
  // when genuinely new work is happening, not just because historical messages
  // have completed tool_data.
  $effect(() => {
    const msgs = $threadData?.messages ?? []
    const completedCallIds = new Set()
    for (const msg of msgs) {
      for (const block of msg.tool_data ?? []) {
        if (block.call_id && block.output != null) completedCallIds.add(block.call_id)
      }
    }
    const hasInProgress = msgs.some((m) =>
      m.tool_data?.some((b) => b.output == null && b.call_id && !completedCallIds.has(b.call_id))
    )
    if (hasInProgress) {
      untrack(() => { thinking = false })
      if (thinkingTimeout) {
        clearTimeout(thinkingTimeout)
        thinkingTimeout = null
      }
    }
  })

  onDestroy(() => {
    // Save current draft before component unmounts (thread panel closes)
    if (prevThreadId !== null && (replyText.trim() || pendingFile)) {
      threadDrafts.set(prevThreadId, { text: replyText, file: pendingFile })
    }
    prevThreadId = null  // Reset so the next mount triggers a restore
    if (thinkingTimeout) {
      clearTimeout(thinkingTimeout)
      thinkingTimeout = null
    }
    if (forkTimeout) {
      clearTimeout(forkTimeout)
      forkTimeout = null
    }
  })

  // Extract Edit/Write tool calls to render as inline diffs.
  // Enabled by default for all channels; can be disabled per-channel via inlineToolCalls setting.
  // Tool items are keyed by channel name in the store; we pull the items for
  // that channel and filter for Edit/Write calls.
  let isDmChannel = $derived($threadData?.channelName?.startsWith('dm-') ?? false)
  let showInlineDiffs = $derived(
    isDmChannel || ($channelSettings[$threadData?.channelName]?.inlineToolCalls ?? true)
  )

  let editDiffs = $derived.by(() => {
    if (!showInlineDiffs || !$threadData) return []
    // Unified path: extract Edit/Write diffs from msg.tool_data on thread messages.
    // Works identically for DM and topic channels now that both carry tool_data.
    const allMessages = [...($threadData.parentMessage ? [$threadData.parentMessage] : []), ...($threadData.messages ?? [])]
    const diffs = []
    for (const msg of allMessages) {
      if (!msg.tool_data?.length) continue
      for (const block of msg.tool_data) {
        if (block.tool_name !== 'Edit' && block.tool_name !== 'Write') continue
        // Skip failed tool calls
        if (block.error) continue
        const input = block.input || {}
        if (block.tool_name === 'Edit' && (input.old_string || input.new_string)) {
          diffs.push({
            type: 'edit',
            timestamp: msg.timestamp,
            itemId: block.call_id || msg.id,
            filePath: input.file_path || '',
            oldString: input.old_string || '',
            newString: input.new_string || '',
          })
        } else if (block.tool_name === 'Write' && input.content) {
          diffs.push({
            type: 'edit',
            timestamp: msg.timestamp,
            itemId: block.call_id || msg.id,
            filePath: input.file_path || '',
            oldString: '',
            newString: input.content || '',
          })
        }
      }
    }
    return diffs
  })

  // Build a merged timeline of messages + edit diffs for DM threads.
  // Non-DM threads just use messages as-is.
  // Each message entry gets a precomputed `msgIndex` — its position in the
  // messages-only sublist — so the template can pass it to MessageRow in O(1)
  // instead of using indexOf (which would be O(N) per call, O(N^2) total).
  let mergedTimeline = $derived.by(() => {
    if (!$threadData) return []
    const msgs = ($threadData.messages ?? []).map((m, i) => ({ type: 'message', data: m, timestamp: m.timestamp, msgIndex: i }))
    if (!showInlineDiffs || editDiffs.length === 0) return msgs
    const edits = editDiffs.map((d) => ({ type: 'edit', data: d, timestamp: d.timestamp, msgIndex: -1 }))
    const sorted = [...msgs, ...edits].sort((a, b) => (a.timestamp || '').localeCompare(b.timestamp || ''))
    // Recompute message indices after sort — interleaved edits shift positions
    let idx = 0
    for (const entry of sorted) {
      if (entry.type === 'message') entry.msgIndex = idx++
    }
    return sorted
  })

  // Pre-compute the messages-only list from the merged timeline, for MessageRow's
  // senderChanged/timeChanged logic. Avoids recomputing in every iteration.
  let timelineMessages = $derived(mergedTimeline.filter((e) => e.type === 'message').map((e) => e.data))
  let groupedTimeline = $derived(groupTimelineToolRuns(mergedTimeline))

  // Reply count excluding tool-only messages (tool runs are visual noise, not conversation)
  let visibleReplyCount = $derived(($threadData?.messages ?? []).filter(m => !isToolOnly(m)).length)

  // Track viewport changes to know which panel is active
  onMount(() => {
    const mql = window.matchMedia('(min-width: 1024px)')
    function onChange(e) { isDesktop = e.matches }
    mql.addEventListener('change', onChange)
    return () => mql.removeEventListener('change', onChange)
  })

  // Handle clicks on PR links, task links, channel links in thread messages
  function handleThreadLinkClick(e) {
    if (e.defaultPrevented) return
    const target = e.target
    if (target.classList.contains('pr-link')) {
      e.preventDefault()
      const prNum = target.dataset.pr
      const url = getPrUrlUtil(prNum, $kanbanData, $repoStatuses, $repoStatus.fullName)
      if (url) window.open(url, '_blank', 'noopener')
    } else if (target.classList.contains('task-link')) {
      e.preventDefault()
      const taskId = target.dataset.task
      const tasks = $daemonStatus?.tasks || []
      const task = tasks.find((t) => String(t.id) === String(taskId))
      if (task) openTaskThread(task, task.channel || $activeChannel)
    } else if (target.classList.contains('channel-link')) {
      e.preventDefault()
      const name = target.dataset.channel
      if ($channelsStore.some((ch) => ch.name === name)) $activeChannel = name
    } else if (target.classList.contains('coworker-link')) {
      e.preventDefault()
      const name = target.dataset.coworker
      if (name) selectDm(name)
    } else if (target.classList.contains('message-image')) {
      e.preventDefault()
      openImageLightbox(target.dataset.fullSrc || target.src)
    }
  }

  $effect(() => {
    const el = desktopScrollArea
    if (!el) return
    el.addEventListener('click', handleThreadLinkClick)
    return () => el.removeEventListener('click', handleThreadLinkClick)
  })

  $effect(() => {
    const el = mobileScrollArea
    if (!el) return
    el.addEventListener('click', handleThreadLinkClick)
    return () => el.removeEventListener('click', handleThreadLinkClick)
  })

  // Derive the active elements based on viewport
  let scrollArea = $derived(isDesktop ? desktopScrollArea : mobileScrollArea)
  let textareaEl = $derived(isDesktop ? desktopTextareaEl : mobileTextareaEl)

  // pushState: true (default) — user-initiated close should create a history entry
  // so the back button can reopen the thread.
  function handleClose() { closeThread() }
  function handleWindowKeydown(event) {
    if (event.key === 'Escape' && !event.defaultPrevented) handleClose()
  }

  async function handleSubmit(e) {
    e.preventDefault()
    if (!$threadData) return
    const parentId = $threadData.parentMessage?.id ?? null

    if (uploading) return
    if (pendingFile) {
      uploading = true
      const submittingThreadId = currentThreadId
      const result = await uploadAndSend(pendingFile, replyText, $threadData.channelName, parentId)
      uploading = false
      if (result.ok) {
        replyText = ''
        pendingFile = null
        if (submittingThreadId) threadDrafts.delete(submittingThreadId)
        clearMobileTextarea(textareaEl, () => { replyText = '' })
      } else {
        alert(`Upload failed: ${result.error}`)
        return
      }
    } else if (replyText.trim()) {
      sendMessage(replyText.trim(), $threadData.channelName, parentId)
      replyText = ''
      if (currentThreadId) threadDrafts.delete(currentThreadId)
      clearMobileTextarea(textareaEl, () => { replyText = '' })
    } else {
      return
    }

    // Optimistic: show the drawer immediately while waiting for tool calls to arrive
    thinking = true
    if (thinkingTimeout) clearTimeout(thinkingTimeout)
    thinkingTimeout = setTimeout(() => {
      thinking = false
      thinkingTimeout = null
    }, 30000)
  }

  function handleTextareaKeyDown(e) {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSubmit(e)
    }
  }

  function handlePaste(e) {
    const file = extractPastedFile(e)
    if (file) {
      pendingFile = file
      return
    }
    const cursorPos = handleCodePaste(e, textareaEl, () => replyText, (t) => { replyText = t })
    if (cursorPos !== false) {
      tick().then(() => {
        textareaEl.selectionStart = cursorPos
        textareaEl.selectionEnd = cursorPos
      })
    }
  }

  function clearPendingFile() {
    pendingFile = null
  }

  // Manage blob preview URL: create on file change, revoke old URL to prevent memory leaks.
  $effect(() => {
    const file = pendingFile
    pendingFileUrl = updatePreviewUrl(untrack(() => pendingFileUrl), file)
    return () => {
      if (pendingFileUrl) {
        URL.revokeObjectURL(pendingFileUrl)
        pendingFileUrl = null
      }
    }
  })

  // Auto-scroll when new messages or edit diffs arrive (only if user is near bottom)
  $effect(() => {
    if ((mergedTimeline.length > 0) && scrollArea && untrack(() => autoScroll)) {
      // Skip auto-scroll-to-bottom when a deep-link target is pending —
      // the deep-link effect below will handle scrolling to the right message.
      // Read via untrack() so clearing the deep-link store doesn't re-trigger this effect.
      if (untrack(() => $deepLinkMsgId)) return
      tick().then(() => {
        scrollArea.scrollTop = scrollArea.scrollHeight
      })
    }
  })

  // Deep-link: scroll to and highlight a specific message when deepLinkMsgId is set.
  // Waits until thread messages are loaded, then finds the target element.
  $effect(() => {
    const targetId = $deepLinkMsgId
    if (!targetId || !scrollArea || mergedTimeline.length === 0) return
    tick().then(() => {
      const el = scrollArea.querySelector(`[data-msg-id="${CSS.escape(targetId)}"]`)
      if (el) {
        el.scrollIntoView({ behavior: 'smooth', block: 'center' })
        el.classList.add('deep-link-highlight')
        setTimeout(() => el.classList.remove('deep-link-highlight'), 2000)
        untrack(() => deepLinkMsgId.set(null))
      }
    })
  })

  // Focus textarea only when a *new* thread opens — not on every message append.
  // Uses currentThreadId (stable thread identity) instead of $threadData to avoid
  // re-firing when the message array grows.
  let lastFocusedThreadId = null
  $effect(() => {
    const threadId = currentThreadId
    const lastId = lastFocusedThreadId
    if (threadId && threadId !== lastId && textareaEl) {
      lastFocusedThreadId = threadId
      tick().then(() => { if (isDesktop) textareaEl.focus() })
    }
    if (!threadId) lastFocusedThreadId = null
  })

  function handleScroll() {
    if (!scrollArea) return
    const { scrollTop, scrollHeight, clientHeight } = scrollArea
    autoScroll = scrollHeight - scrollTop - clientHeight < 50
  }

  function scrollToBottom() {
    if (scrollArea) {
      scrollArea.scrollTop = scrollArea.scrollHeight
      autoScroll = true
    }
  }

  function resizeTextarea() {
    if (!textareaEl) return
    textareaEl.style.overflowY = 'hidden'
    textareaEl.style.height = 'auto'
    textareaEl.style.height = textareaEl.scrollHeight + 'px'
    textareaEl.style.overflowY =
      textareaEl.scrollHeight > textareaEl.clientHeight ? 'auto' : 'hidden'
  }

  // Re-measure textarea height when its width changes (e.g., thread panel resized,
  // window resize). Track previous width to avoid infinite loops.
  $effect(() => {
    if (!textareaEl) return
    let prevWidth = textareaEl.getBoundingClientRect().width
    const ro = new ResizeObserver((entries) => {
      const entry = entries[0]
      if (!entry) return
      const newWidth = entry.contentRect.width
      if (newWidth !== prevWidth) {
        prevWidth = newWidth
        resizeTextarea()
      }
    })
    ro.observe(textareaEl)
    return () => ro.disconnect()
  })

  $effect(() => {
    replyText;
    tick().then(() => resizeTextarea())
  })
</script>

<svelte:window onkeydown={handleWindowKeydown} />

{#if $threadData}
  <!-- Desktop: side panel with resize handle -->
  <div
    class="hidden lg:flex flex-col h-full bg-background border-l-2 border-border shrink-0 relative shadow-[-8px_0_8px_-8px_rgba(0,0,0,0.15)] dark:shadow-[-8px_0_8px_-8px_rgba(0,0,0,0.4)]"
    style="width: {panelWidth}px"
    data-testid="thread-panel"
  >
    <!-- Resize handle on left edge -->
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      role="separator"
      aria-label="Resize thread panel"
      onmousedown={handleResizeMouseDown}
      class="absolute inset-y-0 -left-1 w-2 cursor-ew-resize hover:bg-primary/20 transition-colors z-30 hidden md:block"
      class:bg-primary={isResizing}
    >
      <div class="absolute inset-y-0 left-0 w-full flex items-center justify-center transition-opacity {isResizing ? 'opacity-100' : 'opacity-0 hover:opacity-100'}">
        <div class="w-0.5 h-12 bg-primary/60 rounded-full"></div>
      </div>
    </div>

    <!-- Header -->
    <div class="flex items-center justify-between px-4 py-2 min-h-[var(--header-height)] bg-card border-b-2 border-border shrink-0">
      <h2 class="text-[1.1rem] font-bold font-mono text-foreground m-0">Thread</h2>
      <div class="flex items-center gap-1">
        {#if isTopicChannel && $threadData?.parentMessage?.id}
          <button
            class="px-2 py-1 text-[0.72rem] rounded-md border transition-all duration-150 {hasDedicatedSession ? 'border-destructive/40 text-destructive hover:bg-destructive/10' : 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'}"
            onclick={handleForkToggle}
            disabled={forkPending}
            title={hasDedicatedSession ? 'End the dedicated fork session for this thread' : 'Fork the channel lead to dedicate a session to this thread'}
          >
            {#if forkPending}
              ...
            {:else if hasDedicatedSession}
              End fork
            {:else}
              Fork lead
            {/if}
          </button>
        {/if}
        <button
          class="w-8 h-8 flex items-center justify-center bg-transparent border border-border rounded-md text-muted-foreground text-[1.3rem] cursor-pointer transition-all duration-150 leading-none hover:bg-accent hover:border-destructive hover:text-destructive ml-1 shrink-0"
          onclick={handleClose}
          aria-label="Close thread"
          data-testid="thread-close-button"
        >&times;</button>
      </div>
    </div>
    {#if forkError}
      <div class="px-4 py-1.5 bg-destructive/10 text-destructive text-[0.72rem] border-b border-destructive/20 shrink-0">{forkError}</div>
    {/if}

    <!-- Scrollable content: task cards + parent message + replies -->
    <div
      class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden text-[1rem] leading-[1.55] px-[18px] pt-[10px] pb-[10px]"
      bind:this={desktopScrollArea}
      onscroll={handleScroll}
    >
      <!-- Task cards at top (above parent message) -->
      {#if $threadData.tasks?.length > 0}
        {#each $threadData.tasks as task}
          <TaskRow {task} variant="card" />
        {/each}
      {/if}

      <!-- Parent message as first item in stream -->
      {#if $threadData.parentMessage}
          <MessageRow
            msg={$threadData.parentMessage}
            msgs={[$threadData.parentMessage]}
            index={0}
            senderClass="mt-1"
            channelName={$threadData?.channelName}
            showToolData={showInlineDiffs}
            class={$threadData.parentMessage.auto_output ? 'auto-output' : ''}
          />

        <!-- Separator with reply count -->
        <div class="flex items-center gap-2 py-3 text-muted-foreground/70 text-[0.72rem]">
          <div class="flex-1 h-px bg-border/60"></div>
          <span>{visibleReplyCount === 0 ? 'no replies yet' : visibleReplyCount === 1 ? '1 reply' : `${visibleReplyCount} replies`}</span>
          <div class="flex-1 h-px bg-border/60"></div>
        </div>
      {/if}

      <!-- Thread replies (interleaved with edit diffs for DM channels) -->
      {#each groupedTimeline as entry, i}
        {#if entry.type === 'tool-run'}
          <ToolRunSummary
            messages={entry.entries.map(e => e.data)}
            lastTimestamp={entry.lastTimestamp}
            allMessages={timelineMessages}
            startIndex={entry.entries[0].msgIndex}
            channelName={$threadData?.channelName}
            showToolData={showInlineDiffs}
          />
        {:else if entry.type === 'edit'}
          <DiffView
            filePath={entry.data.filePath}
            oldString={entry.data.oldString}
            newString={entry.data.newString}
          />
        {:else}
          {@const msg = entry.data}
          {@const dayLabel = dateChanged(timelineMessages, entry.msgIndex)}
          {#if dayLabel}
            <DayDivider label={dayLabel} />
          {/if}
          <MessageRow
            {msg}
            msgs={timelineMessages}
            index={entry.msgIndex}
            senderClass="mt-1"
            channelName={$threadData?.channelName}
            threadParentId={$threadData?.parentMessage?.id}
            isDedicatedSession={hasDedicatedSession && forkOwner != null && msg.from === forkOwner}
            {forkParentLead}
            showToolData={showInlineDiffs}
            class="{msg.pending ? 'opacity-60' : ''} {msg.auto_output ? 'auto-output' : ''}"
          />
        {/if}
      {/each}
    </div>

    {#if !autoScroll}
      <button
        class="absolute bottom-[90px] right-[18px] w-[36px] h-[36px] rounded-full border-2 border-border bg-card text-foreground text-[1.1rem] cursor-pointer flex items-center justify-center transition-all duration-200 opacity-85 hover:opacity-100 hover:border-primary hover:text-primary z-10"
        onclick={scrollToBottom}
        aria-label="Scroll to bottom"
      >↓</button>
    {/if}

    <!-- Activity drawer: slides up from the input when lead is working -->
    <ThreadActivityDrawer messages={$threadData?.messages ?? []} channelName={$threadData?.channelName} threadParentId={$threadData?.parentMessage?.id} {thinking} inlineMode={showInlineDiffs} />

    <!-- Input -->
    <form class="flex flex-col gap-2 px-3 py-1.5 bg-card border-t border-border shrink-0" onsubmit={handleSubmit}>
      {#if pendingFile}
        <div class="relative inline-block max-w-[200px] border border-border rounded-lg p-2 bg-card" data-testid="thread-file-preview">
          {#if pendingFile.type.startsWith('image/')}
            <img src={pendingFileUrl} alt="Preview" class="max-w-full max-h-[120px] rounded block" />
          {:else}
            <div class="flex items-center gap-2 text-foreground">
              <span class="text-[1.5rem]">&#128196;</span>
              <span class="text-[0.85rem] overflow-hidden text-ellipsis whitespace-nowrap">{pendingFile.name}</span>
            </div>
          {/if}
          <button
            type="button"
            class="absolute top-1 right-1 w-6 h-6 p-0 rounded-full bg-[rgba(0,0,0,0.7)] text-white text-[1.2rem] leading-none flex items-center justify-center cursor-pointer border border-border hover:bg-[rgba(255,87,87,0.8)] hover:border-destructive"
            onclick={clearPendingFile}
            aria-label="Remove file"
          >&times;</button>
        </div>
      {/if}
      <div class="relative">
        <textarea
          data-testid="thread-input"
          bind:this={desktopTextareaEl}
          bind:value={replyText}
          placeholder="Reply in thread..."
          rows="1"
          class="block w-full py-[13px] px-[17px] pr-[48px] border-2 border-input rounded-[18px] bg-background text-foreground text-[1.02rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[50vh] overflow-y-hidden focus:border-primary placeholder:text-muted-foreground"
          onkeydown={handleTextareaKeyDown}
          onpaste={handlePaste}
          oninput={resizeTextarea}
        ></textarea>
        <button
          type="submit"
          disabled={(!replyText.trim() && !pendingFile) || uploading}
          data-testid="thread-send-button"
          class="absolute right-[12px] bottom-[10px] p-1.5 rounded-full border-none bg-primary text-primary-foreground cursor-pointer transition-all duration-200 disabled:opacity-30 disabled:cursor-not-allowed hover:bg-primary/90"
        ><SendHorizontal size={18} /></button>
      </div>
    </form>
  </div>

  <!-- Mobile: slide-in pane (inside board content area) -->
  <div class="lg:hidden absolute inset-0 z-20 bg-background flex flex-col thread-mobile-pane" data-testid="thread-panel-mobile">
    <!-- Mobile header with back button -->
    <!-- No top safe-area padding here: this pane is inside board content, below
         App.svelte's mobile header which already applies pt-safe-offset-* -->
    <div class="flex items-center gap-2 px-3 py-3 bg-card border-b-2 border-border shrink-0">
      <button
        class="w-8 h-8 flex items-center justify-center bg-transparent border border-border rounded-md text-muted-foreground text-[1.1rem] cursor-pointer transition-all duration-150 leading-none hover:text-foreground shrink-0"
        onclick={handleClose}
        aria-label="Back to channel"
        data-testid="thread-back-button"
      >&larr;</button>
      <h2 class="text-[1.1rem] font-bold font-mono text-foreground m-0 flex-1">Thread</h2>
      {#if isTopicChannel && $threadData?.parentMessage?.id}
        <button
          class="px-2 py-1 text-[0.72rem] rounded-md border transition-all duration-150 {hasDedicatedSession ? 'border-destructive/40 text-destructive hover:bg-destructive/10' : 'border-border text-muted-foreground hover:bg-accent hover:text-foreground'}"
          onclick={handleForkToggle}
          disabled={forkPending}
          title={hasDedicatedSession ? 'End the dedicated fork session for this thread' : 'Fork the channel lead to dedicate a session to this thread'}
        >
          {#if forkPending}
            ...
          {:else if hasDedicatedSession}
            End fork
          {:else}
            Fork lead
          {/if}
        </button>
      {/if}
    </div>
    {#if forkError}
      <div class="px-3 py-1.5 bg-destructive/10 text-destructive text-[0.72rem] border-b border-destructive/20 shrink-0">{forkError}</div>
    {/if}

    <!-- Mobile messages -->
    <div
      class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden text-[1rem] leading-[1.55] px-[18px] pt-[10px] pb-[10px]"
      bind:this={mobileScrollArea}
      onscroll={handleScroll}
    >
      <!-- Task cards at top -->
      {#if $threadData.tasks?.length > 0}
        {#each $threadData.tasks as task}
          <TaskRow {task} variant="card" />
        {/each}
      {/if}

      <!-- Parent message as first item in stream -->
      {#if $threadData.parentMessage}
        <MessageRow
          msg={$threadData.parentMessage}
          msgs={[$threadData.parentMessage]}
          index={0}
          senderClass="mt-1"
          channelName={$threadData?.channelName}
          showToolData={showInlineDiffs}
          class={$threadData.parentMessage.auto_output ? 'auto-output' : ''}
        />

        <!-- Separator with reply count -->
        <div class="flex items-center gap-2 py-3 text-muted-foreground/70 text-[0.72rem]">
          <div class="flex-1 h-px bg-border/60"></div>
          <span>{visibleReplyCount === 0 ? 'no replies yet' : visibleReplyCount === 1 ? '1 reply' : `${visibleReplyCount} replies`}</span>
          <div class="flex-1 h-px bg-border/60"></div>
        </div>
      {/if}

      <!-- Replies (interleaved with edit diffs for DM channels) -->
      {#each groupedTimeline as entry, i}
        {#if entry.type === 'tool-run'}
          <ToolRunSummary
            messages={entry.entries.map(e => e.data)}
            lastTimestamp={entry.lastTimestamp}
            allMessages={timelineMessages}
            startIndex={entry.entries[0].msgIndex}
            channelName={$threadData?.channelName}
            showToolData={showInlineDiffs}
          />
        {:else if entry.type === 'edit'}
          <DiffView
            filePath={entry.data.filePath}
            oldString={entry.data.oldString}
            newString={entry.data.newString}
          />
        {:else}
          {@const msg = entry.data}
          {@const dayLabel = dateChanged(timelineMessages, entry.msgIndex)}
          {#if dayLabel}
            <DayDivider label={dayLabel} />
          {/if}
          <MessageRow
            {msg}
            msgs={timelineMessages}
            index={entry.msgIndex}
            senderClass="mt-1"
            channelName={$threadData?.channelName}
            threadParentId={$threadData?.parentMessage?.id}
            isDedicatedSession={hasDedicatedSession && forkOwner != null && msg.from === forkOwner}
            {forkParentLead}
            showToolData={showInlineDiffs}
            class="{msg.pending ? 'opacity-60' : ''} {msg.auto_output ? 'auto-output' : ''}"
          />
        {/if}
      {/each}
    </div>

    {#if !autoScroll}
      <button
        class="absolute bottom-[90px] right-[18px] w-[36px] h-[36px] rounded-full border-2 border-border bg-card text-foreground text-[1.1rem] cursor-pointer flex items-center justify-center transition-all duration-200 opacity-85 hover:opacity-100 hover:border-primary hover:text-primary z-10"
        onclick={scrollToBottom}
        aria-label="Scroll to bottom"
      >↓</button>
    {/if}

    <!-- Activity drawer: slides up from the input when lead is working -->
    <ThreadActivityDrawer messages={$threadData?.messages ?? []} channelName={$threadData?.channelName} threadParentId={$threadData?.parentMessage?.id} {thinking} inlineMode={showInlineDiffs} />

    <!-- Mobile input -->
    <form class="flex flex-col gap-2 px-3 pt-2 pb-safe-offset-2 bg-card border-t border-border shrink-0" onsubmit={handleSubmit}>
      {#if pendingFile}
        <div class="relative inline-block max-w-[200px] border border-border rounded-lg p-2 bg-card" data-testid="thread-file-preview">
          {#if pendingFile.type.startsWith('image/')}
            <img src={pendingFileUrl} alt="Preview" class="max-w-full max-h-[120px] rounded block" />
          {:else}
            <div class="flex items-center gap-2 text-foreground">
              <span class="text-[1.5rem]">&#128196;</span>
              <span class="text-[0.85rem] overflow-hidden text-ellipsis whitespace-nowrap">{pendingFile.name}</span>
            </div>
          {/if}
          <button
            type="button"
            class="absolute top-1 right-1 w-6 h-6 p-0 rounded-full bg-[rgba(0,0,0,0.7)] text-white text-[1.2rem] leading-none flex items-center justify-center cursor-pointer border border-border hover:bg-[rgba(255,87,87,0.8)] hover:border-destructive"
            onclick={clearPendingFile}
            aria-label="Remove file"
          >&times;</button>
        </div>
      {/if}
      <div class="relative">
        <textarea
          data-testid="thread-input"
          bind:this={mobileTextareaEl}
          bind:value={replyText}
          placeholder="Reply in thread..."
          rows="1"
          class="block w-full py-[10px] px-[14px] pr-[42px] border-2 border-input rounded-[14px] bg-background text-foreground text-[0.9rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[50vh] overflow-y-hidden focus:border-primary placeholder:text-muted-foreground"
          onkeydown={handleTextareaKeyDown}
          onpaste={handlePaste}
          oninput={resizeTextarea}
        ></textarea>
        <button
          type="submit"
          disabled={(!replyText.trim() && !pendingFile) || uploading}
          data-testid="thread-send-button"
          class="absolute right-[12px] bottom-[6px] p-1.5 rounded-full border-none bg-primary text-primary-foreground cursor-pointer transition-all duration-200 disabled:opacity-30 disabled:cursor-not-allowed hover:bg-primary/90"
        ><SendHorizontal size={18} /></button>
      </div>
    </form>
  </div>
{/if}

<style>
  .thread-mobile-pane {
    animation: thread-slide-in 0.24s ease-out;
  }

  @keyframes thread-slide-in {
    from {
      transform: translateX(100%);
    }
    to {
      transform: translateX(0);
    }
  }

  :global(.deep-link-highlight) {
    animation: deep-link-flash 2s ease-out;
  }

  @keyframes deep-link-flash {
    0% { background-color: hsl(var(--primary) / 0.2); }
    70% { background-color: hsl(var(--primary) / 0.2); }
    100% { background-color: transparent; }
  }

</style>
