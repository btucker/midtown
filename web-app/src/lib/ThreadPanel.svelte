<script module>
// Module-level draft storage persists across mount/unmount cycles.
// ThreadPanel is conditionally rendered ({#if $threadData}), so instance-level
// state would be lost when the thread panel closes and reopens.
const threadDrafts = new Map();
let prevThreadId = null;
</script>

<script>
  import { threadData, agentToolItems, threadToolItems, deepLinkMsgId, threadOwnership, threadForkParents, threadForkOwners, activeProject, channels as channelsStore, activeChannel, daemonStatus, kanbanData, repoStatus, repoStatuses } from './store.js'
  import { sendMessage, closeThread, getApiBase, forkThread, unforkThread, openTaskThread, selectDm } from './api.js'
  import { extractPastedFile, uploadAndSend } from './filePaste.js'
  import { getPrUrl as getPrUrlUtil } from './channelUtils.js'
  import { tick, onMount, onDestroy, untrack } from 'svelte'
  import { getSenderColor, isDimSender, parseInsightSegments, dateChanged } from './messageUtils.js'
  import SendHorizontal from '@lucide/svelte/icons/send-horizontal'
  import { openImageLightbox } from './biggerPicture.js'
  import MermaidDiagram from './MermaidDiagram.svelte'
  import { parseSegments, hasMermaid, renderContent } from './markdown.js'
  import MessageRow from './MessageRow.svelte'
  import ToolDataBlocks from './ToolDataBlocks.svelte'
  import DayDivider from './DayDivider.svelte'
  import ThreadActivityDrawer from './ThreadActivityDrawer.svelte'
  import TaskRow from './TaskRow.svelte'
  import DiffView from './DiffView.svelte'
  import { clearMobileTextarea } from './mobileInput.js'

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

  function isAction(msg) {
    return msg.msg_type === 'action' || msg.content?.startsWith('/me ')
  }

  function getActionContent(msg) {
    return msg.content.replace(/^\/me\s*/, '')
  }

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

  // Clear forkPending when ownership state updates.
  $effect(() => {
    if ($threadData?.parentMessage?.id) {
      const _ownership = $threadOwnership[$threadData.parentMessage.id]
      untrack(() => { forkPending = false })
    }
  })

  let forkError = $state(null)

  function handleForkToggle() {
    if (!$threadData?.parentMessage?.id || !$threadData?.channelName) return
    forkPending = true
    forkError = null
    const onError = (msg) => {
      forkPending = false
      forkError = msg
      // Auto-clear error after 5 seconds
      setTimeout(() => { forkError = null }, 5000)
    }
    if (hasDedicatedSession) {
      unforkThread($threadData.parentMessage.id, $threadData.channelName, onError)
    } else {
      forkThread($threadData.parentMessage.id, $threadData.channelName, onError)
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

  // Clear thinking and reset autoScroll when thread is closed or switched.
  $effect(() => {
    currentThreadId // track dependency — re-runs only when thread identity changes
    untrack(() => {
      thinking = false
      autoScroll = true
    })
    if (thinkingTimeout) {
      clearTimeout(thinkingTimeout)
      thinkingTimeout = null
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

  // Clear thinking when real InProgress tool items arrive.
  // When a fork is handling the thread, its tool calls land in threadToolItems — use
  // those exclusively to avoid channel-lead crosstalk (the original bug). When no fork
  // exists (threadToolItems empty), the channel lead handles the reply and its tool
  // events go to agentToolItems — fall back to those so the thinking indicator clears.
  $effect(() => {
    const parentId = $threadData?.parentMessage?.id
    const channelName = $threadData?.channelName ?? 'midtown'
    const threadItems = parentId ? ($threadToolItems[parentId] || []) : []
    const channelItems = $agentToolItems[channelName] || []
    // Prefer thread-scoped items when a fork is active (any items present);
    // fall back to channel items when no fork has produced thread tool calls.
    const items = threadItems.length > 0 ? threadItems : channelItems
    const hasInProgress = items.some((item) => item.status === 'InProgress')
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
  })

  // Extract Edit/Write tool calls for DM channels to render as inline diffs.
  // Tool items are keyed by channel name in the store; for DM threads we pull
  // the items for that DM channel and filter for Edit/Write calls.
  let isDmChannel = $derived($threadData?.channelName?.startsWith('dm-') ?? false)

  let editDiffs = $derived.by(() => {
    if (!isDmChannel || !$threadData) return []
    const channelName = $threadData.channelName
    const items = $agentToolItems[channelName] || []
    // Build result status map: call_id → 'error' | 'ok'
    // so we can skip diffs for failed Edit/Write calls.
    const resultStatus = {}
    for (const item of items) {
      if (!item.content) continue
      for (const part of item.content) {
        if (part.ToolResult) {
          resultStatus[part.ToolResult.call_id] = part.ToolResult.is_error ? 'error' : 'ok'
        }
      }
    }
    const diffs = []
    for (const item of items) {
      if (!item.content) continue
      for (const part of item.content) {
        if (part.ToolCall && (part.ToolCall.name === 'Edit' || part.ToolCall.name === 'Write')) {
          const callId = part.ToolCall.call_id
          if (resultStatus[callId] === 'error') continue
          const input = part.ToolCall.input || {}
          // Edit has file_path + old_string + new_string; Write has file_path + content
          if (part.ToolCall.name === 'Edit' && (input.old_string || input.new_string)) {
            diffs.push({
              type: 'edit',
              timestamp: item.timestamp,
              itemId: item.item_id,
              filePath: input.file_path || '',
              oldString: input.old_string || '',
              newString: input.new_string || '',
            })
          } else if (part.ToolCall.name === 'Write' && input.content) {
            diffs.push({
              type: 'edit',
              timestamp: item.timestamp,
              itemId: item.item_id,
              filePath: input.file_path || '',
              oldString: '',
              newString: input.content || '',
            })
          }
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
    if (!isDmChannel || editDiffs.length === 0) return msgs
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
    if (file) pendingFile = file
  }

  function clearPendingFile() {
    pendingFile = null
  }

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
            title={hasDedicatedSession ? 'Return this thread to the channel lead' : 'Create a dedicated session for this thread'}
          >
            {#if forkPending}
              ...
            {:else if hasDedicatedSession}
              Return to main
            {:else}
              Dedicate session
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
          class={$threadData.parentMessage.auto_output ? 'auto-output' : ''}
        >
          {#if isAction($threadData.parentMessage) && !hasMermaid($threadData.parentMessage.content)}
            <div class="flex gap-0 break-words">
              <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">*</span>
              <span class="action-text flex-1 min-w-0" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">{@html renderContent(getActionContent($threadData.parentMessage), getApiBase())}</span>
            </div>
          {:else if isAction($threadData.parentMessage) && hasMermaid($threadData.parentMessage.content)}
            {#each parseSegments(getActionContent($threadData.parentMessage)) as segment, si}
              {#if segment.type === 'mermaid'}
                <MermaidDiagram code={segment.content} />
              {:else}
                <div class="flex gap-0 break-words">
                  {#if si === 0}
                    <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">*</span>
                  {:else}
                    <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                  {/if}
                  <span class="action-text flex-1 min-w-0" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">{@html renderContent(segment.content, getApiBase())}</span>
                </div>
              {/if}
            {/each}
          {:else}
            {#each parseInsightSegments($threadData.parentMessage.content) as segment}
              {#if segment.type === 'insight'}
                <div class="border-l-2 pl-3 max-w-[85%] my-0.5" style="border-color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}80">
                  {#if hasMermaid(segment.content)}
                    {#each parseSegments(segment.content) as mseg}
                      {#if mseg.type === 'mermaid'}
                        <MermaidDiagram code={mseg.content} />
                      {:else}
                        <div class="message-text text-foreground">{@html renderContent(mseg.content, getApiBase())}</div>
                      {/if}
                    {/each}
                  {:else}
                    <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
                  {/if}
                </div>
              {:else if hasMermaid(segment.content)}
                {#each parseSegments(segment.content) as mseg}
                  {#if mseg.type === 'mermaid'}
                    <MermaidDiagram code={mseg.content} />
                  {:else}
                    <div class="break-words message-text {isDimSender($threadData.parentMessage.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(mseg.content, getApiBase())}</div>
                  {/if}
                {/each}
              {:else}
                <div class="break-words message-text {isDimSender($threadData.parentMessage.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
              {/if}
            {/each}
          {/if}
          {#if $threadData.parentMessage.tool_data?.length}
            <ToolDataBlocks blocks={$threadData.parentMessage.tool_data} />
          {/if}
        </MessageRow>

        <!-- Separator with reply count -->
        <div class="flex items-center gap-2 py-3 text-muted-foreground/50 text-[0.72rem]">
          <div class="flex-1 h-px bg-border/60"></div>
          <span>{($threadData.messages?.length ?? 0) === 0 ? 'no replies yet' : $threadData.messages.length === 1 ? '1 reply' : `${$threadData.messages.length} replies`}</span>
          <div class="flex-1 h-px bg-border/60"></div>
        </div>
      {/if}

      <!-- Thread replies (interleaved with edit diffs for DM channels) -->
      {#each mergedTimeline as entry, i}
        {#if entry.type === 'edit'}
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
            class="{msg.pending ? 'opacity-60' : ''} {msg.auto_output ? 'auto-output' : ''}"
          >
            {#if isAction(msg) && !hasMermaid(msg.content)}
              <div class="flex gap-0 break-words">
                <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">*</span>
                <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">{@html renderContent(getActionContent(msg), getApiBase())}</span>
              </div>
            {:else if isAction(msg) && hasMermaid(msg.content)}
              {#each parseSegments(getActionContent(msg)) as segment, si}
                {#if segment.type === 'mermaid'}
                  <MermaidDiagram code={segment.content} />
                {:else}
                  <div class="flex gap-0 break-words">
                    {#if si === 0}
                      <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">*</span>
                    {:else}
                      <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                    {/if}
                    <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">{@html renderContent(segment.content, getApiBase())}</span>
                  </div>
                {/if}
              {/each}
            {:else}
              {#each parseInsightSegments(msg.content) as segment}
                {#if segment.type === 'insight'}
                  <div class="border-l-2 pl-3 max-w-[85%] my-0.5" style="border-color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}80">
                    {#if hasMermaid(segment.content)}
                      {#each parseSegments(segment.content) as mseg}
                        {#if mseg.type === 'mermaid'}
                          <MermaidDiagram code={mseg.content} />
                        {:else}
                          <div class="message-text text-foreground">{@html renderContent(mseg.content, getApiBase())}</div>
                        {/if}
                      {/each}
                    {:else}
                      <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
                    {/if}
                  </div>
                {:else if hasMermaid(segment.content)}
                  {#each parseSegments(segment.content) as mseg}
                    {#if mseg.type === 'mermaid'}
                      <MermaidDiagram code={mseg.content} />
                    {:else}
                      <div class="break-words message-text {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(mseg.content, getApiBase())}</div>
                    {/if}
                  {/each}
                {:else}
                  <div class="break-words message-text {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
                {/if}
              {/each}
            {/if}
            {#if msg.tool_data?.length}
              <ToolDataBlocks blocks={msg.tool_data} />
            {/if}
          </MessageRow>
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
    <ThreadActivityDrawer channelName={$threadData?.channelName} threadParentId={$threadData?.parentMessage?.id} {thinking} />

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
          title={hasDedicatedSession ? 'Return this thread to the channel lead' : 'Create a dedicated session for this thread'}
        >
          {#if forkPending}
            ...
          {:else if hasDedicatedSession}
            Return to main
          {:else}
            Dedicate session
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
          class={$threadData.parentMessage.auto_output ? 'auto-output' : ''}
        >
          {#if isAction($threadData.parentMessage) && !hasMermaid($threadData.parentMessage.content)}
            <div class="flex gap-0 break-words">
              <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">*</span>
              <span class="action-text flex-1 min-w-0" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">{@html renderContent(getActionContent($threadData.parentMessage), getApiBase())}</span>
            </div>
          {:else if isAction($threadData.parentMessage) && hasMermaid($threadData.parentMessage.content)}
            {#each parseSegments(getActionContent($threadData.parentMessage)) as segment, si}
              {#if segment.type === 'mermaid'}
                <MermaidDiagram code={segment.content} />
              {:else}
                <div class="flex gap-0 break-words">
                  {#if si === 0}
                    <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">*</span>
                  {:else}
                    <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                  {/if}
                  <span class="action-text flex-1 min-w-0" style="color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}">{@html renderContent(segment.content, getApiBase())}</span>
                </div>
              {/if}
            {/each}
          {:else}
            {#each parseInsightSegments($threadData.parentMessage.content) as segment}
              {#if segment.type === 'insight'}
                <div class="border-l-2 pl-3 max-w-[85%] my-0.5" style="border-color: {getSenderColor($threadData.parentMessage.from, undefined, $threadData?.channelName)}80">
                  {#if hasMermaid(segment.content)}
                    {#each parseSegments(segment.content) as mseg}
                      {#if mseg.type === 'mermaid'}
                        <MermaidDiagram code={mseg.content} />
                      {:else}
                        <div class="message-text text-foreground">{@html renderContent(mseg.content, getApiBase())}</div>
                      {/if}
                    {/each}
                  {:else}
                    <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
                  {/if}
                </div>
              {:else if hasMermaid(segment.content)}
                {#each parseSegments(segment.content) as mseg}
                  {#if mseg.type === 'mermaid'}
                    <MermaidDiagram code={mseg.content} />
                  {:else}
                    <div class="break-words message-text {isDimSender($threadData.parentMessage.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(mseg.content, getApiBase())}</div>
                  {/if}
                {/each}
              {:else}
                <div class="break-words message-text {isDimSender($threadData.parentMessage.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
              {/if}
            {/each}
          {/if}
          {#if $threadData.parentMessage.tool_data?.length}
            <ToolDataBlocks blocks={$threadData.parentMessage.tool_data} />
          {/if}
        </MessageRow>

        <!-- Separator with reply count -->
        <div class="flex items-center gap-2 py-3 text-muted-foreground/50 text-[0.72rem]">
          <div class="flex-1 h-px bg-border/60"></div>
          <span>{($threadData.messages?.length ?? 0) === 0 ? 'no replies yet' : $threadData.messages.length === 1 ? '1 reply' : `${$threadData.messages.length} replies`}</span>
          <div class="flex-1 h-px bg-border/60"></div>
        </div>
      {/if}

      <!-- Replies (interleaved with edit diffs for DM channels) -->
      {#each mergedTimeline as entry, i}
        {#if entry.type === 'edit'}
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
            class="{msg.pending ? 'opacity-60' : ''} {msg.auto_output ? 'auto-output' : ''}"
          >
            {#if isAction(msg) && !hasMermaid(msg.content)}
              <div class="flex gap-0 break-words">
                <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">*</span>
                <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">{@html renderContent(getActionContent(msg), getApiBase())}</span>
              </div>
            {:else if isAction(msg) && hasMermaid(msg.content)}
              {#each parseSegments(getActionContent(msg)) as segment, si}
                {#if segment.type === 'mermaid'}
                  <MermaidDiagram code={segment.content} />
                {:else}
                  <div class="flex gap-0 break-words">
                    {#if si === 0}
                      <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">*</span>
                    {:else}
                      <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                    {/if}
                    <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}">{@html renderContent(segment.content, getApiBase())}</span>
                  </div>
                {/if}
              {/each}
            {:else}
              {#each parseInsightSegments(msg.content) as segment}
                {#if segment.type === 'insight'}
                  <div class="border-l-2 pl-3 max-w-[85%] my-0.5" style="border-color: {getSenderColor(msg.from, undefined, $threadData?.channelName)}80">
                    {#if hasMermaid(segment.content)}
                      {#each parseSegments(segment.content) as mseg}
                        {#if mseg.type === 'mermaid'}
                          <MermaidDiagram code={mseg.content} />
                        {:else}
                          <div class="message-text text-foreground">{@html renderContent(mseg.content, getApiBase())}</div>
                        {/if}
                      {/each}
                    {:else}
                      <div class="message-text text-foreground">{@html renderContent(segment.content, getApiBase())}</div>
                    {/if}
                  </div>
                {:else if hasMermaid(segment.content)}
                  {#each parseSegments(segment.content) as mseg}
                    {#if mseg.type === 'mermaid'}
                      <MermaidDiagram code={mseg.content} />
                    {:else}
                      <div class="break-words message-text {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(mseg.content, getApiBase())}</div>
                    {/if}
                  {/each}
                {:else}
                  <div class="break-words message-text {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content, getApiBase())}</div>
                {/if}
              {/each}
            {/if}
            {#if msg.tool_data?.length}
              <ToolDataBlocks blocks={msg.tool_data} />
            {/if}
          </MessageRow>
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
    <ThreadActivityDrawer channelName={$threadData?.channelName} threadParentId={$threadData?.parentMessage?.id} {thinking} />

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
