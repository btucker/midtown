<script>
  import { messages, messagesByChannel, activeChannel, channels as channelsStore, coworkers, kanbanData, repoStatus, repoStatuses, daemonStatus, isWideScreen, agentToolItems, threadData } from './store.js'
  import { sendMessage, uploadFile, closeThread, openThread, openTaskThread, getApiBase } from './api.js'
  import { AVENUE_COLORS, getSenderColor, isDimSender, formatTime, timeChanged, parseInsightSegments, dateChanged } from './messageUtils.js'
  import { tick, onMount, untrack } from 'svelte'
  import { fly } from 'svelte/transition'
  import ReplyIcon from '@lucide/svelte/icons/reply'
  import MermaidDiagram from './MermaidDiagram.svelte'
  import { parseSegments, hasMermaid, renderContent } from './markdown.js'
  import Autocomplete from './Autocomplete.svelte'
  import MessageRow from './MessageRow.svelte'
  import DayDivider from './DayDivider.svelte'
  import { clearMobileTextarea } from './mobileInput.js'

  // Windowed rendering: only render a slice of messages near the viewport.
  // Messages outside this window are not mounted in the DOM.
  const INITIAL_WINDOW_SIZE = 100  // messages to render on first load
  const LOAD_MORE_COUNT = 50       // messages to add when scrolling up

  let inputText = $state('')
  let scrollAreaViewport = $state(null)
  let autoScroll = $state(true)
  let pendingFile = $state(null)
  let uploading = $state(false)
  let textareaElement = $state(null)
  let formWrapperElement = $state(null)
  let channelLeadThinking = $state(false)
  let channelLeadThinkingTimeout = null
  let channelItemsActive = $state(false)
  let channelItemsActiveTimeout = null
  let topSentinel = $state(null)
  let topObserver = null

  // The index into channelMessages where rendering begins.
  // Messages before this index are not in the DOM.
  let renderStartIndex = $state(0)

  // Per-channel draft storage: saves inputText and pendingFile when switching channels
  let channelDrafts = new Map()
  let prevChannel = null

  // Autocomplete state
  let showAutocomplete = $state(false)
  let autocompleteType = $state(null) // '@' | '!' | '#'
  let autocompleteQuery = $state('')
  let autocompleteItems = $state([])
  let autocompletePosition = $state({ top: 0, left: 0 })
  let autocompleteSelectedIndex = $state(0)
  let autocompleteStartPos = $state(0)

  // DM channel detection: use is_dm field or dm- prefix fallback
  let activeChannelMeta = $derived($channelsStore.find((ch) => ch.name === $activeChannel) ?? null)
  let isDm = $derived(activeChannelMeta?.is_dm ?? $activeChannel.startsWith('dm-'))
  let dmPeerName = $derived($activeChannel.startsWith('dm-') ? $activeChannel.slice(3) : $activeChannel)

  // Filter messages by active channel
  let channelMessages = $derived($messagesByChannel[$activeChannel] || [])

  // Visible slice of messages for the DOM. Only these get rendered.
  let visibleMessages = $derived(channelMessages.slice(renderStartIndex))
  let hasMoreAbove = $derived(renderStartIndex > 0)

  // Track how many messages were present when each channel was first viewed.
  // Messages at or above this index are "new" and get the slide-up animation.
  // We use $state.raw so mutations don't trigger full reactive updates.
  let initialMessageCounts = $state.raw({})

  $effect(() => {
    // Reactive on both $activeChannel and channelMessages.length.
    // On first visit to a channel, channelMessages is empty (history not yet
    // loaded from WebSocket). We wait until messages actually arrive before
    // snapshotting the count. This prevents the race where we snapshot 0,
    // then history loads and every message animates as "new".
    const ch = $activeChannel
    const len = channelMessages.length
    if (!(ch in initialMessageCounts) && len > 0) {
      initialMessageCounts = { ...initialMessageCounts, [ch]: len }
    }
  })

  // Position the render window at the tail on channel switch or first history load.
  // Tracks $activeChannel and channelMessages.length, but uses prevRenderChannel
  // to distinguish channel switches from new-message arrivals. This avoids both:
  //  - stale counts (issue: window grows unbounded on revisit)
  //  - DOM flash (issue: renderStartIndex starts at 0 then jumps)
  let prevRenderChannel = null
  $effect(() => {
    const ch = $activeChannel
    const len = channelMessages.length
    if (ch !== prevRenderChannel) {
      // Channel switch — position at tail using current message count
      prevRenderChannel = ch
      renderStartIndex = Math.max(0, len - INITIAL_WINDOW_SIZE)
    } else if (len > 0 && renderStartIndex === 0 && len > INITIAL_WINDOW_SIZE) {
      // Same channel, history just loaded (was empty, now has messages).
      // Only fires once: after this, renderStartIndex > 0 so guard fails.
      renderStartIndex = len - INITIAL_WINDOW_SIZE
    }
    // New messages on current channel: no-op. visibleMessages is an
    // open-ended slice so new messages at the end render automatically.
  })

  // Save/restore drafts when switching channels
  $effect(() => {
    const ch = $activeChannel
    if (prevChannel !== null && prevChannel !== ch) {
      const currentText = untrack(() => inputText)
      const currentFile = untrack(() => pendingFile)
      if (currentText.trim() || currentFile) {
        channelDrafts.set(prevChannel, { text: currentText, file: currentFile })
      } else {
        channelDrafts.delete(prevChannel)
      }
    }
    if (prevChannel !== ch) {
      const draft = channelDrafts.get(ch)
      inputText = draft?.text ?? ''
      pendingFile = draft?.file ?? null
    }
    prevChannel = ch
    tick().then(() => resizeTextarea())
  })

  function isNewMessage(channelName, index) {
    // If we haven't recorded the initial count yet (effect hasn't fired),
    // treat all messages as old so they don't animate on first render.
    const threshold = initialMessageCounts[channelName] ?? Infinity
    return index >= threshold
  }

  // Tool call items for the active channel.
  // Main channel ('midtown') shows the lead's tool calls; topic channels show their channel lead's.
  let activeChannelToolItems = $derived($agentToolItems[$activeChannel] || [])

  // Activity strip computed values (always-rendered single line above input)
  let isLeadWorking = $derived($activeChannel === 'midtown' ? !!$daemonStatus?.lead_working : false)
  // Correlate InProgress tool calls with received ToolResults: a ToolUse item stays
  // InProgress in the store even after its ToolResult arrives in a later batch (items
  // are appended, not updated). Only count a call as truly in-progress if no matching
  // ToolResult exists in the store.
  let hasInProgressItems = $derived.by(() => {
    const completedCallIds = new Set()
    for (const item of activeChannelToolItems) {
      for (const part of item.content) {
        if (part.ToolResult) completedCallIds.add(part.ToolResult.call_id)
      }
    }
    return activeChannelToolItems.some((item) => {
      if (item.status !== 'InProgress') return false
      return item.content.some(
        (part) => part.ToolCall && !completedCallIds.has(part.ToolCall.call_id)
      )
    })
  })
  let showActivity = $derived(activeChannelToolItems.length > 0 || channelLeadThinking || isLeadWorking)
  let showDots = $derived(isLeadWorking || (hasInProgressItems && channelItemsActive) || channelLeadThinking)

  // Most recent tool call entry for inline display in the activity strip.
  // Replicates ToolActivity's merge logic but returns only the last call.
  let mostRecentToolCallEntry = $derived.by(() => {
    if (activeChannelToolItems.length === 0) return null
    const resultStatus = {}
    for (const item of activeChannelToolItems) {
      for (const part of item.content) {
        if (part.ToolResult) {
          resultStatus[part.ToolResult.call_id] = part.ToolResult.is_error ? 'error' : 'ok'
        }
      }
    }
    for (let i = activeChannelToolItems.length - 1; i >= 0; i--) {
      const item = activeChannelToolItems[i]
      if (item.content.some((p) => p.ToolCall)) {
        const callId = item.content.find((p) => p.ToolCall)?.ToolCall?.call_id
        const status = callId ? (resultStatus[callId] ?? null) : null
        return { item, status }
      }
    }
    return null
  })

  // Autocomplete filtering and data preparation
  function getAutocompleteItems(type, query) {
    const lowerQuery = query.toLowerCase()

    if (type === '@') {
      // Coworkers + lead
      const people = [
        { name: 'lead', type: 'lead' },
        ...$coworkers.map(cw => ({ name: cw.name, type: 'coworker', task: cw.current_task }))
      ]
      return people.filter(p => p.name.toLowerCase().startsWith(lowerQuery))
    }

    if (type === '!') {
      // Tasks from daemon status
      const tasks = $daemonStatus?.tasks || []
      return tasks
        .filter(t => {
          const idMatch = String(t.id).startsWith(query)
          const subjectMatch = t.subject?.toLowerCase().startsWith(lowerQuery)
          return idMatch || subjectMatch
        })
        .slice(0, 10) // Limit to 10 results
    }

    if (type === '#') {
      // PRs from kanban data + channels
      const prs = $kanbanData.review.map(pr => ({
        type: 'pr',
        number: pr.number,
        title: pr.title,
        status: pr.status
      }))
      const channelList = $channelsStore.map(ch => ({
        type: 'channel',
        name: ch.name
      }))
      const combined = [...prs, ...channelList]
      return combined.filter(item => {
        if (item.type === 'pr') {
          return String(item.number).startsWith(query) || item.title?.toLowerCase().startsWith(lowerQuery)
        }
        return item.name.toLowerCase().startsWith(lowerQuery)
      }).slice(0, 10)
    }

    return []
  }

  function getAutocompleteLabel(item) {
    if (typeof item === 'object' && item !== null) {
      if (item.type === 'coworker' || item.type === 'lead') return `@${item.name}`
      if (item.type === 'pr') return `#${item.number}`
      if (item.type === 'channel') return `#${item.name}`
      if (item.id !== undefined) return `!${item.id}` // task
    }
    return String(item)
  }

  function getAutocompleteValue(item) {
    if (typeof item === 'object' && item !== null) {
      if (item.type === 'coworker' || item.type === 'lead') return `@${item.name}`
      if (item.type === 'pr') return `#${item.number}`
      if (item.type === 'channel') return `#${item.name}`
      if (item.id !== undefined) return `!${item.id}` // task
    }
    return String(item)
  }

  function getAutocompleteDescription(item) {
    if (typeof item === 'object' && item !== null) {
      if ((item.type === 'coworker' || item.type === 'lead') && item.task) return item.task
      if (item.type === 'pr') return item.title
      if (item.subject) return item.subject // task
    }
    return null
  }

  function calculateAutocompletePosition() {
    if (!textareaElement || !formWrapperElement) return { top: 0, left: 0 }

    const textareaRect = textareaElement.getBoundingClientRect()
    const wrapperRect = formWrapperElement.getBoundingClientRect()

    // Position relative to the form wrapper (which has position: relative and
    // no overflow: hidden). Using position: absolute on the dropdown instead of
    // position: fixed avoids iOS visual/layout viewport split issues when the
    // virtual keyboard is open.
    //
    // top = textarea's top edge relative to the wrapper.
    // The dropdown uses translateY(-100% - 8px) to shift above the textarea.
    return {
      top: textareaRect.top - wrapperRect.top,
      left: textareaRect.left - wrapperRect.left,
      width: textareaRect.width
    }
  }

  function detectAutocompleteTrigger() {
    const cursorPos = textareaElement?.selectionStart || 0
    // Use textarea.value directly instead of inputText binding
    // because oninput fires before the binding updates
    const text = textareaElement?.value || inputText

    // Look backward from cursor to find trigger character
    let triggerPos = -1
    let triggerChar = null

    for (let i = cursorPos - 1; i >= 0; i--) {
      const char = text[i]
      const prevChar = i > 0 ? text[i - 1] : ' '

      // Check if this is a trigger character preceded by whitespace or start of line
      if (('@!#'.includes(char)) && (prevChar === ' ' || prevChar === '\n' || i === 0)) {
        triggerPos = i
        triggerChar = char
        break
      }

      // Stop if we hit whitespace (no trigger found in current word)
      if (char === ' ' || char === '\n') {
        break
      }
    }

    if (triggerPos >= 0 && triggerChar) {
      const query = text.slice(triggerPos + 1, cursorPos)
      autocompleteStartPos = triggerPos
      autocompleteType = triggerChar
      autocompleteQuery = query
      autocompleteItems = getAutocompleteItems(triggerChar, query)
      autocompletePosition = calculateAutocompletePosition()
      autocompleteSelectedIndex = 0
      showAutocomplete = autocompleteItems.length > 0
    } else {
      showAutocomplete = false
    }
  }

  function insertAutocompleteItem(item) {
    const value = getAutocompleteValue(item)
    const beforeTrigger = inputText.slice(0, autocompleteStartPos)
    const afterCursor = inputText.slice(textareaElement?.selectionStart || 0)

    inputText = beforeTrigger + value + ' ' + afterCursor
    showAutocomplete = false

    // Set cursor position after inserted text
    tick().then(() => {
      if (textareaElement) {
        const newPos = beforeTrigger.length + value.length + 1
        textareaElement.focus()
        textareaElement.setSelectionRange(newPos, newPos)
      }
    })
  }

  // Cache current tasks to avoid recalculating on every render
  let currentTasks = $derived(getCurrentTasks($coworkers))

  // Get PR status from kanban data
  function getPrStatus(prNum) {
    const pr = $kanbanData.review.find((p) => p.number === parseInt(prNum))
    return pr ? pr.status : null
  }

  // Find a PR by number across all kanban columns that contain PR data.
  // PRs appear in 'review' (open) and 'done' (merged) columns.
  function findPr(prNum) {
    const num = parseInt(prNum)
    return $kanbanData.review.find((p) => p.number === num)
      || $kanbanData.done.find((p) => p.number === num)
      || null
  }

  // Build GitHub PR URL (multi-repo aware).
  // Looks up the PR in kanbanData to find its repo, then resolves via
  // repoStatuses. Falls back to the primary repo if no match is found.
  // Returns null if repo full name is unavailable.
  function getPrUrl(prNum) {
    const pr = findPr(prNum)
    // If the PR has a repo label, resolve it via repoStatuses (multi-repo)
    if (pr?.repo && $repoStatuses.length > 0) {
      const info = $repoStatuses.find((r) => r.label === pr.repo)
      if (info?.fullName) {
        return `https://github.com/${info.fullName}/pull/${prNum}`
      }
    }
    // Fall back to the primary repo
    if ($repoStatus.fullName) {
      return `https://github.com/${$repoStatus.fullName}/pull/${prNum}`
    }
    return null
  }

  // Find a task by ID from the daemon status task list
  function findTask(taskId) {
    const tasks = $daemonStatus?.tasks || []
    return tasks.find((t) => String(t.id) === String(taskId)) || null
  }

  // Set up IntersectionObserver for the top sentinel (lazy load older messages)
  $effect(() => {
    const sentinel = topSentinel
    const viewport = scrollAreaViewport
    if (!sentinel || !viewport) return

    topObserver = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            loadMoreMessages()
          }
        }
      },
      { root: viewport, rootMargin: '200px 0px 0px 0px' }
    )
    topObserver.observe(sentinel)

    return () => {
      topObserver?.disconnect()
      topObserver = null
    }
  })

  // Handle clicks on channel links, task links, PR links, and coworker links
  onMount(() => {
    function handleLinkClick(e) {
      const target = e.target
      if (target.classList.contains('channel-link')) {
        e.preventDefault()
        const channelName = target.dataset.channel
        if ($channelsStore.some((ch) => ch.name === channelName)) {
          activeChannel.set(channelName)
        }
      } else if (target.classList.contains('task-link')) {
        e.preventDefault()
        const taskId = target.dataset.task
        const task = findTask(taskId)
        if (task) {
          openTaskThread(task, task.channel || $activeChannel)
        }
      } else if (target.classList.contains('pr-link')) {
        e.preventDefault()
        const prNum = target.dataset.pr
        const pr = findPr(prNum)
        const task = pr?.task_id ? findTask(pr.task_id) : null
        if (task) {
          openTaskThread(task, task.channel || $activeChannel)
        } else {
          const url = getPrUrl(prNum)
          if (url) window.open(url, '_blank', 'noopener')
        }
      } else if (target.classList.contains('coworker-link')) {
        // Prevent the browser from following the '#' href; no detail panel action.
        e.preventDefault()
      }
    }

    if (scrollAreaViewport) {
      scrollAreaViewport.addEventListener('click', handleLinkClick)
      return () => scrollAreaViewport.removeEventListener('click', handleLinkClick)
    }
  })

  function isAction(msg) {
    return msg.msg_type === 'action' || msg.content?.startsWith('/me ')
  }

  function getActionContent(msg) {
    return msg.content.replace(/^\/me\s*/, '')
  }

  // NOTE: Any new link type added to markdown.js (channel/task/PR/coworker/etc.) must be
  // handled in BOTH handleLinkClick (desktop — fires on the scroll viewport) AND here in
  // handleMessageTap (mobile — fires on the message row div). handleMessageTap calls
  // stopPropagation(), so handleLinkClick never runs on mobile. They are NOT redundant;
  // they are two separate entry points for the same click on different platforms.
  function handleMessageTap(event, msg) {
    // Mobile-only affordance: tap a top-level message to open its thread view.
    if ($isWideScreen || msg.thread_parent_id) return
    const target = event.target instanceof Element ? event.target : null
    // Block real interactive controls.
    if (target?.closest('button, input, textarea, select, label')) return
    // Block external links but not internal pseudo-links (channel/task/PR/coworker refs).
    // Internal refs use <a> tags from renderContent() and cover most message text on mobile,
    // so blocking all <a> elements effectively breaks tap-to-reply on nearly every message.
    const link = target?.closest('a')
    if (link && !link.dataset.channel && !link.dataset.task && !link.dataset.pr && !link.dataset.coworker) return
    // Task links open the task's thread (with task card); PR links open task thread or GitHub;
    // all other taps open the message thread.
    if (link?.dataset.task) {
      const task = findTask(link.dataset.task)
      if (task) openTaskThread(task, task.channel || $activeChannel)
    } else if (link?.dataset.pr) {
      const prNum = link.dataset.pr
      const pr = findPr(prNum)
      const task = pr?.task_id ? findTask(pr.task_id) : null
      if (task) {
        openTaskThread(task, task.channel || $activeChannel)
      } else {
        const url = getPrUrl(prNum)
        if (url) window.open(url, '_blank', 'noopener')
      }
    } else {
      openThread(msg, $activeChannel)
    }
    // Prevent the click from also triggering the internal link handler (handleLinkClick),
    // and prevent the browser from following href="#" which would scroll to page top.
    event.stopPropagation()
    event.preventDefault()
  }

  // Build a map of coworker name -> current task
  function getCurrentTasks(coworkerList) {
    const map = {}
    for (const cw of coworkerList) {
      if (cw.current_task) {
        map[cw.name.toLowerCase()] = cw.current_task
      }
    }
    return map
  }

  // Auto-scroll to bottom when new messages arrive
  $effect(() => {
    if (channelMessages.length > 0 && autoScroll && scrollAreaViewport) {
      tick().then(() => {
        scrollAreaViewport.scrollTop = scrollAreaViewport.scrollHeight
      })
    }
  })

  // Reset textarea height when input is cleared (after send)
  $effect(() => {
    inputText;
    tick().then(() => resizeTextarea())
  })

  // Clear optimistic thinking state when real tool activity arrives
  $effect(() => {
    if (activeChannelToolItems.some((item) => item.status === 'InProgress')) {
      channelLeadThinking = false
      if (channelLeadThinkingTimeout) {
        clearTimeout(channelLeadThinkingTimeout)
        channelLeadThinkingTimeout = null
      }
    }
  })

  // Clear optimistic thinking state when switching channels
  $effect(() => {
    $activeChannel // track dependency
    channelLeadThinking = false
    if (channelLeadThinkingTimeout) {
      clearTimeout(channelLeadThinkingTimeout)
      channelLeadThinkingTimeout = null
    }
  })

  // Track tool item activity freshness: mark active when new items arrive,
  // mark stale after 8s of silence. This catches channel leads that stop
  // mid-tool (no ToolResult or final message to clear InProgress items).
  $effect(() => {
    const items = activeChannelToolItems
    if (channelItemsActiveTimeout) {
      clearTimeout(channelItemsActiveTimeout)
      channelItemsActiveTimeout = null
    }
    if (items.length > 0) {
      channelItemsActive = true
      channelItemsActiveTimeout = setTimeout(() => {
        channelItemsActive = false
        channelItemsActiveTimeout = null
      }, 8000)
    } else {
      channelItemsActive = false
    }
  })

  // Ensure the timeouts are cleared when the component is destroyed
  $effect(() => {
    return () => {
      if (channelLeadThinkingTimeout) {
        clearTimeout(channelLeadThinkingTimeout)
        channelLeadThinkingTimeout = null
      }
      if (channelItemsActiveTimeout) {
        clearTimeout(channelItemsActiveTimeout)
        channelItemsActiveTimeout = null
      }
    }
  })

  async function handleSubmit(e) {
    e.preventDefault()

    // If there's a pending file, upload it first
    if (pendingFile && !uploading) {
      uploading = true
      const result = await uploadFile(pendingFile)
      uploading = false

      if (result.ok) {
        // Send message to lead with file path
        const message = inputText.trim()
          ? `${inputText.trim()}\n\n[Attached: ${result.path}]`
          : `[Attached file: ${result.filename}]\nPlease read: ${result.path}`

        sendMessage(message, $activeChannel)
        if ($activeChannel !== 'midtown' && $activeChannel !== 'main') {
          channelLeadThinking = true
          if (channelLeadThinkingTimeout) clearTimeout(channelLeadThinkingTimeout)
          channelLeadThinkingTimeout = setTimeout(() => {
            channelLeadThinking = false
            channelLeadThinkingTimeout = null
          }, 30000)
        }
        inputText = ''
        if (textareaElement) textareaElement.value = ''
        pendingFile = null
        channelDrafts.delete($activeChannel)
      } else {
        alert(`Upload failed: ${result.error}`)
        return
      }
    } else if (inputText.trim()) {
      sendMessage(inputText.trim(), $activeChannel)
      if ($activeChannel !== 'midtown' && $activeChannel !== 'main') {
        channelLeadThinking = true
        if (channelLeadThinkingTimeout) clearTimeout(channelLeadThinkingTimeout)
        channelLeadThinkingTimeout = setTimeout(() => {
          channelLeadThinking = false
          channelLeadThinkingTimeout = null
        }, 30000)
      }
      inputText = ''
      channelDrafts.delete($activeChannel)
      clearMobileTextarea(textareaElement, () => { inputText = '' })
    }
  }

  function handlePaste(e) {
    const items = e.clipboardData?.items
    if (!items) return

    for (const item of items) {
      // Check for image types
      if (item.type.startsWith('image/')) {
        e.preventDefault()
        const file = item.getAsFile()
        if (file) {
          pendingFile = file
        }
        return
      }
      // Check for files (PDFs, etc.)
      if (item.kind === 'file') {
        e.preventDefault()
        const file = item.getAsFile()
        if (file) {
          pendingFile = file
        }
        return
      }
    }
  }

  function clearPendingFile() {
    pendingFile = null
  }

  function handleKeyDown(e) {
    // Handle autocomplete navigation
    if (showAutocomplete) {
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        autocompleteSelectedIndex = (autocompleteSelectedIndex + 1) % autocompleteItems.length
        return
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault()
        autocompleteSelectedIndex = autocompleteSelectedIndex === 0
          ? autocompleteItems.length - 1
          : autocompleteSelectedIndex - 1
        return
      }
      if (e.key === 'Enter' || e.key === 'Tab') {
        e.preventDefault()
        if (autocompleteItems[autocompleteSelectedIndex]) {
          insertAutocompleteItem(autocompleteItems[autocompleteSelectedIndex])
        }
        return
      }
      if (e.key === 'Escape') {
        e.preventDefault()
        showAutocomplete = false
        return
      }
    }

    // Submit on Enter, allow Shift+Enter for new lines
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSubmit(e)
    }
  }

  // Load more messages when scrolling to the top of the visible window.
  // Preserves scroll position so the user doesn't jump.
  function loadMoreMessages() {
    if (renderStartIndex <= 0 || !scrollAreaViewport) return
    const prevScrollHeight = scrollAreaViewport.scrollHeight
    const prevScrollTop = scrollAreaViewport.scrollTop
    renderStartIndex = Math.max(0, renderStartIndex - LOAD_MORE_COUNT)
    // After Svelte renders the new messages, restore scroll position
    tick().then(() => {
      if (scrollAreaViewport) {
        const newScrollHeight = scrollAreaViewport.scrollHeight
        scrollAreaViewport.scrollTop = prevScrollTop + (newScrollHeight - prevScrollHeight)
      }
    })
  }

  function handleScroll() {
    if (!scrollAreaViewport) return
    const { scrollTop, scrollHeight, clientHeight } = scrollAreaViewport
    autoScroll = scrollHeight - scrollTop - clientHeight < 50
  }

  function scrollToBottom() {
    if (scrollAreaViewport) {
      scrollAreaViewport.scrollTop = scrollAreaViewport.scrollHeight
    }
  }

  function resizeTextarea() {
    if (!textareaElement) return
    textareaElement.style.height = 'auto'
    textareaElement.style.height = textareaElement.scrollHeight + 'px'
  }

  // Re-measure textarea height when its width changes (e.g., thread panel opens/closes,
  // window resize, sidebar toggle). Track previous width to avoid infinite loops —
  // without this guard, height changes from resizeTextarea() would re-trigger the observer.
  $effect(() => {
    if (!textareaElement) return
    let prevWidth = textareaElement.getBoundingClientRect().width
    const ro = new ResizeObserver((entries) => {
      const entry = entries[0]
      if (!entry) return
      const newWidth = entry.contentRect.width
      if (newWidth !== prevWidth) {
        prevWidth = newWidth
        resizeTextarea()
      }
    })
    ro.observe(textareaElement)
    return () => ro.disconnect()
  })

  function handleInput() {
    resizeTextarea()
    detectAutocompleteTrigger()
  }

  function describeToolCall(entry) {
    for (const part of entry.item.content) {
      if (part.ToolCall) {
        return part.ToolCall.semantic_header || part.ToolCall.name?.toLowerCase() || '?'
      }
    }
    return '?'
  }

  function getToolCallStatusIcon(entry) {
    if (entry.status === 'error') return '✗'
    if (entry.status === 'ok') return '✓'
    return '›'
  }
</script>

<div class="flex flex-col h-full min-h-0 overflow-hidden relative">
  <div
    class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden overscroll-contain text-[1rem] leading-[1.55] px-[18px] pt-[14px] pb-[18px]"
    bind:this={scrollAreaViewport}
    onscroll={handleScroll}
  >
      {#if channelMessages.length === 0}
        <div class="text-center text-muted-foreground py-[50px] px-[22px] font-sans">
          <p>No messages {isDm ? `with @${dmPeerName}` : `in #${$activeChannel}`}</p>
          <p class="text-[0.9rem] mt-[10px]">{isDm ? `Send a message to start a conversation` : `Messages posted to this channel will appear here`}</p>
        </div>
      {:else}
        {#if hasMoreAbove}
          <!-- Top sentinel: triggers loading older messages when scrolled into view.
               Only mounted when there are messages above the window, so the
               IntersectionObserver doesn't fire no-op callbacks on short channels. -->
          <div bind:this={topSentinel} class="h-[1px] w-full" aria-hidden="true"></div>
          <div class="text-center text-muted-foreground/50 text-[0.8rem] py-2 select-none">Loading earlier messages…</div>
        {/if}

        {#each visibleMessages as msg, i}
          {@const globalIndex = renderStartIndex + i}
          {@const dayLabel = dateChanged(channelMessages, globalIndex)}
          {#if dayLabel}
            <DayDivider label={dayLabel} />
          {/if}
          <div
            data-testid="message-row"
            in:fly={{ y: 16, duration: isNewMessage($activeChannel, globalIndex) ? 180 : 0, opacity: 0 }}
            class="group relative -mx-[18px] px-[18px] pb-[5px] rounded-sm hover:bg-accent/30"
            class:opacity-60={msg.pending}
            class:mobile-thread-tappable={!$isWideScreen && !msg.thread_parent_id}
            onclick={(event) => handleMessageTap(event, msg)}
          >
          {#if !msg.thread_parent_id}
            <button
              data-testid="thread-reply-button"
              class="hidden lg:flex absolute right-6 -top-3.5 items-center gap-2 px-3.5 py-1.5 rounded-lg border border-border bg-card text-[0.85rem] font-bold text-foreground cursor-pointer opacity-0 pointer-events-none transition-all duration-150 shadow-sm group-hover:opacity-100 group-hover:pointer-events-auto focus:opacity-100 focus:pointer-events-auto hover:border-primary hover:shadow-md"
              onclick={() => openThread(msg, $activeChannel)}
              aria-label="Reply in thread"
            >
              <ReplyIcon size={16} />
              <span>Reply</span>
            </button>
          {/if}

          <MessageRow
            {msg}
            msgs={channelMessages}
            index={globalIndex}
            senderClass="mt-1"
            currentTask={currentTasks[msg.from.toLowerCase()]}
            channelName={$activeChannel}
          >
            {#if isAction(msg) && !hasMermaid(msg.content)}
              <div class="flex gap-0 break-words">
                <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, undefined, $activeChannel)}">*</span>
                <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, undefined, $activeChannel)}">{@html renderContent(getActionContent(msg), getApiBase())}</span>
              </div>
            {:else if isAction(msg) && hasMermaid(msg.content)}
              {#each parseSegments(getActionContent(msg)) as segment, si}
                {#if segment.type === 'mermaid'}
                  <MermaidDiagram code={segment.content} />
                {:else}
                  <div class="flex gap-0 break-words">
                    {#if si === 0}
                      <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from, undefined, $activeChannel)}">*</span>
                    {:else}
                      <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                    {/if}
                    <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from, undefined, $activeChannel)}">{@html renderContent(segment.content, getApiBase())}</span>
                  </div>
                {/if}
              {/each}
            {:else}
              {#each parseInsightSegments(msg.content) as segment}
                {#if segment.type === 'insight'}
                  <div class="border-l-2 pl-3 max-w-[85%] my-0.5" style="border-color: {getSenderColor(msg.from, undefined, $activeChannel)}80">
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
          </MessageRow>

          <!-- Reply indicator for messages with thread replies -->
          {#if !msg.thread_parent_id && msg.reply_count}
            <div class="flex gap-0" style="padding-left: calc(2.4rem + 0.5rem);">
              <button
                data-testid="thread-summary"
                class="flex items-center gap-1.5 text-[0.75rem] text-link-default hover:text-link-hover cursor-pointer bg-transparent border-none p-0 mt-0.5"
                onclick={() => openThread(msg, $activeChannel)}
              >
                <span>{msg.reply_count} {msg.reply_count === 1 ? 'reply' : 'replies'}</span>
                {#if msg.last_reply}
                  <span class="text-muted-foreground/60">&middot;</span>
                  <span class="text-muted-foreground">{msg.last_reply.from}</span>
                  <span class="text-muted-foreground/60">&middot;</span>
                  <span class="text-muted-foreground">{formatTime(msg.last_reply.timestamp)}</span>
                {/if}
              </button>
            </div>
          {/if}
          </div>
        {/each}
      {/if}

  </div>

  <!-- Activity strip: always rendered at fixed height to prevent layout shift.
       Shows [dots?] [lead name] [icon] [tool description] on one line.
       In the main channel ('midtown'), dots are driven by lead_working (same signal as
       the TUI braille spinner). In topic channels, InProgress tool items drive the dots —
       channel leads don't have a separate lead_working signal. -->
  <div class="h-[1.5em] flex items-center gap-[6px] px-[18px] text-[0.82rem] overflow-hidden whitespace-nowrap shrink-0" data-testid="activity-strip">
    {#if showActivity}
      {#if showDots}
        <span class="typing-dots flex gap-[3px] items-center">
          <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {AVENUE_COLORS.lead}"></span>
          <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {AVENUE_COLORS.lead}"></span>
          <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {AVENUE_COLORS.lead}"></span>
        </span>
      {/if}
      <span class="font-mono font-semibold" style="color: {AVENUE_COLORS.lead}">{isDm ? `@${dmPeerName}` : $activeChannel}</span>
      {#if mostRecentToolCallEntry}
        <span class="text-muted-foreground/60 select-none">{getToolCallStatusIcon(mostRecentToolCallEntry)}</span>
        <span class="font-mono text-muted-foreground truncate">{describeToolCall(mostRecentToolCallEntry)}</span>
      {/if}
    {/if}
  </div>

  {#if !autoScroll}
    <button
      class="absolute bottom-[90px] right-[22px] w-[40px] h-[40px] rounded-full border-2 border-border bg-card text-foreground text-[1.2rem] cursor-pointer flex items-center justify-center transition-all duration-200 opacity-85 hover:opacity-100 hover:border-primary hover:text-primary z-10"
      onclick={scrollToBottom}
      aria-label="Scroll to bottom"
    >
      &#8595;
    </button>
  {/if}

  <!-- Input area wrapper: position: relative so the autocomplete can be
       absolutely positioned above the textarea without being clipped by the
       Channel root's overflow: hidden. The autocomplete uses translateY(-100% - 8px)
       to shift above the textarea's top edge. -->
  <div class="relative shrink-0" bind:this={formWrapperElement}>
    <!-- Autocomplete dropdown — positioned absolute within this wrapper -->
    <Autocomplete
      bind:show={showAutocomplete}
      bind:selectedIndex={autocompleteSelectedIndex}
      items={autocompleteItems}
      position={autocompletePosition}
      getLabel={getAutocompleteLabel}
      getValue={getAutocompleteValue}
      getDescription={getAutocompleteDescription}
      onSelect={insertAutocompleteItem}
    />
    <form class="flex flex-col gap-2 px-3 pt-2 pb-1 bg-card border-t border-border" onsubmit={handleSubmit}>
      {#if pendingFile}
        <div class="relative inline-block max-w-[200px] border border-border rounded-lg p-2 bg-card" data-testid="file-preview">
          {#if pendingFile.type.startsWith('image/')}
            <img src={URL.createObjectURL(pendingFile)} alt="Preview" class="max-w-full max-h-[120px] rounded block" />
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
          >
            &times;
          </button>
        </div>
      {/if}
      <div class="flex gap-2 w-full">
        <textarea
          data-testid="channel-input"
          bind:this={textareaElement}
          bind:value={inputText}
          placeholder={isDm ? `Message @${dmPeerName}...` : `Message to #${$activeChannel}...`}
          rows="1"
          class="flex-1 py-[13px] px-[17px] border-2 border-border rounded-[18px] bg-card text-foreground text-[1.02rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[50vh] overflow-y-auto focus:border-primary placeholder:text-muted-foreground"
          onkeydown={handleKeyDown}
          onpaste={handlePaste}
          oninput={handleInput}
        ></textarea>
        <button
          type="submit"
          disabled={!inputText.trim() && !pendingFile || uploading}
          data-testid="send-button"
          class="py-[13px] px-[22px] border-none rounded-[26px] bg-primary text-primary-foreground font-bold cursor-pointer transition-all duration-200 text-[0.95rem] tracking-[0.01em] disabled:opacity-40 disabled:cursor-not-allowed hover:bg-primary/90 hover:-translate-y-[1px] active:translate-y-0 not-disabled:hover:bg-primary/90"
        >
          {uploading ? 'Uploading...' : 'Send'}
        </button>
      </div>
    </form>
  </div>
</div>

<style>
  /* Inline image attachments */
  :global(.message-image) {
    max-width: 320px;
    max-height: 320px;
    border-radius: 6px;
    display: block;
    margin-top: 4px;
  }

  :global(.attachment-link) {
    display: inline-block;
    line-height: 0;
  }

  :global(.attachment-ref) {
    display: inline-block;
    font-size: 0.88em;
    color: hsl(var(--muted-foreground));
    background: hsl(var(--accent));
    border-radius: 4px;
    padding: 2px 8px;
    margin-top: 4px;
  }

  /* Link styles - applied globally within message content */
  :global(.message-text a),
  :global(.action-text a) {
    color: hsl(var(--link-default));
    text-decoration: none;
  }

  :global(.message-text a:hover),
  :global(.action-text a:hover) {
    text-decoration: underline;
  }

  :global(.message-text a.channel-link),
  :global(.action-text a.channel-link) {
    color: hsl(var(--link-default));
    font-weight: 600;
    cursor: pointer;
  }

  :global(.message-text a.task-link),
  :global(.action-text a.task-link) {
    color: hsl(var(--link-task));
    font-weight: 600;
    cursor: pointer;
  }

  :global(.message-text a.pr-link),
  :global(.action-text a.pr-link) {
    color: hsl(var(--link-pr));
    font-weight: 600;
    cursor: pointer;
  }

  :global(.message-text a.coworker-link),
  :global(.action-text a.coworker-link) {
    color: hsl(var(--link-coworker));
    font-weight: 600;
    cursor: pointer;
  }

  /* Inline code */
  :global(.message-text code),
  :global(.action-text code) {
    background: hsl(var(--accent));
    padding: 0.12em 0.45em;
    border-radius: 3px;
    font-size: 0.92em;
    font-family: 'IBM Plex Mono', 'SF Mono', 'Menlo', monospace;
  }

  /* Code blocks */
  :global(.message-text pre),
  :global(.action-text pre) {
    background: hsl(var(--accent));
    padding: 10px 14px;
    border-radius: 5px;
    overflow-x: auto;
    margin: 5px 0;
    border: none;
  }

  :global(.message-text pre code),
  :global(.action-text pre code) {
    background: none !important;
    padding: 0 !important;
    border: none;
    border-radius: 0;
    font-size: 0.88em;
  }

  /* Headings - scaled down for chat context */
  :global(.message-text h1),
  :global(.message-text h2),
  :global(.message-text h3),
  :global(.action-text h1),
  :global(.action-text h2),
  :global(.action-text h3) {
    font-size: 1em;
    font-weight: 700;
    margin: 5px 0 3px;
  }

  /* Lists — restore list-style-type stripped by Tailwind Preflight */
  :global(.message-text ul),
  :global(.action-text ul) {
    margin: 3px 0;
    padding-left: 1.6em;
    list-style-type: disc;
  }

  :global(.message-text ol),
  :global(.action-text ol) {
    margin: 3px 0;
    padding-left: 1.6em;
    list-style-type: decimal;
  }

  /* Blockquotes */
  :global(.message-text blockquote),
  :global(.action-text blockquote) {
    border-left: 2px solid hsl(var(--border));
    margin: 3px 0;
    padding-left: 10px;
    color: hsl(var(--muted-foreground));
  }

  /* Tables */
  :global(.message-text table),
  :global(.action-text table) {
    border-collapse: collapse;
    margin: 6px 0;
    font-size: 0.88em;
  }

  :global(.message-text th),
  :global(.action-text th) {
    background: hsl(var(--accent));
    color: hsl(var(--primary));
    padding: 4px 10px;
    text-align: left;
    border: 1px solid hsl(var(--border));
    font-weight: 600;
  }

  :global(.message-text td),
  :global(.action-text td) {
    padding: 3px 10px;
    border: 1px solid hsl(var(--border));
    color: hsl(var(--foreground));
  }

  :global(.message-text tr:nth-child(even) td),
  :global(.action-text tr:nth-child(even) td) {
    background: hsl(var(--secondary));
  }

  /* Paragraph spacing within messages */
  :global(.message-text p + p),
  :global(.action-text p + p) {
    margin-top: 0.5em;
  }

  /* Typing indicator bounce animation */
  @keyframes typing-bounce {
    0%, 80%, 100% {
      opacity: 0.3;
      transform: scale(0.8);
    }
    40% {
      opacity: 1;
      transform: scale(1);
    }
  }

  :global(.typing-dots .dot) {
    animation: typing-bounce 1.4s infinite ease-in-out both;
  }

  :global(.typing-dots .dot:nth-child(1)) {
    animation-delay: 0s;
  }

  :global(.typing-dots .dot:nth-child(2)) {
    animation-delay: 0.2s;
  }

  :global(.typing-dots .dot:nth-child(3)) {
    animation-delay: 0.4s;
  }

  @media (max-width: 1023px) {
    .mobile-thread-tappable {
      cursor: pointer;
    }
  }
</style>
