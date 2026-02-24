<script>
  import { messages, messagesByChannel, activeChannel, channels, coworkers, kanbanData, repoStatus, repoStatuses, daemonStatus, detailPanelData, isWideScreen, agentToolItems, threadData } from './store.js'
  import { sendMessage, uploadFile, closeThread, openThread } from './api.js'
  import { tick, onMount } from 'svelte'
  import { fly } from 'svelte/transition'
  import MermaidDiagram from './MermaidDiagram.svelte'
  import { parseSegments, hasMermaid, renderContent } from './markdown.js'
  import Autocomplete from './Autocomplete.svelte'
  import ToolActivity from './ToolActivity.svelte'
  import * as Dialog from '$lib/components/ui/dialog'

  let inputText = $state('')
  let scrollAreaViewport = $state(null)
  let autoScroll = $state(true)
  let selectedTask = $state(null)
  let pendingFile = $state(null)
  let uploading = $state(false)
  let textareaElement = $state(null)
  let formWrapperElement = $state(null)
  let channelLeadThinking = $state(false)
  let channelLeadThinkingTimeout = null

  // Autocomplete state
  let showAutocomplete = $state(false)
  let autocompleteType = $state(null) // '@' | '!' | '#'
  let autocompleteQuery = $state('')
  let autocompleteItems = $state([])
  let autocompletePosition = $state({ top: 0, left: 0 })
  let autocompleteSelectedIndex = $state(0)
  let autocompleteStartPos = $state(0)

  // Filter messages by active channel
  let channelMessages = $derived($messagesByChannel[$activeChannel] || [])

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

  function isNewMessage(channelName, index) {
    // If we haven't recorded the initial count yet (effect hasn't fired),
    // treat all messages as old so they don't animate on first render.
    const threshold = initialMessageCounts[channelName] ?? Infinity
    return index >= threshold
  }

  // Tool call items for the active channel.
  // Main channel ('midtown') shows the lead's tool calls; topic channels show their channel lead's.
  let activeChannelToolItems = $derived($agentToolItems[$activeChannel] || [])

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
      const channelList = $channels.map(ch => ({
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

  function closeTaskModal() {
    selectedTask = null
  }

  // Handle clicks on channel links, task links, PR links, and coworker links
  onMount(() => {
    function handleLinkClick(e) {
      const target = e.target
      if (target.classList.contains('channel-link')) {
        e.preventDefault()
        const channelName = target.dataset.channel
        if ($channels.some((ch) => ch.name === channelName)) {
          activeChannel.set(channelName)
        }
      } else if (target.classList.contains('task-link')) {
        e.preventDefault()
        const taskId = target.dataset.task
        const task = findTask(taskId)
        if (task) {
          // Desktop (>= 1025px): use DetailPanel; Mobile/tablet: use modal
          if ($isWideScreen) {
            closeThread()
            detailPanelData.set({ type: 'task', data: task })
          } else {
            selectedTask = task
          }
        }
      } else if (target.classList.contains('pr-link')) {
        e.preventDefault()
        const prNum = target.dataset.pr
        const url = getPrUrl(prNum)
        if (url) {
          // Desktop (>= 1025px): use DetailPanel if PR data available, else open GitHub
          if ($isWideScreen) {
            const pr = findPr(prNum)
            if (pr) {
              closeThread()
              detailPanelData.set({
                type: 'pr',
                data: {
                  number: pr.number,
                  title: pr.title,
                  author: pr.author,
                  reviewer: pr.reviewer,
                  status: pr.status,
                  url: url,
                },
              })
            } else {
              // PR not in kanban data — fall back to opening GitHub
              window.open(url, '_blank', 'noopener')
            }
          } else {
            window.open(url, '_blank', 'noopener')
          }
        }
      } else if (target.classList.contains('coworker-link')) {
        e.preventDefault()
        const coworkerName = target.dataset.coworker
        // Find the coworker in the store
        const coworker = $coworkers.find((cw) => cw.name.toLowerCase() === coworkerName.toLowerCase())
        if (coworker && $isWideScreen) {
          closeThread()
          detailPanelData.set({
            type: 'coworker',
            data: {
              name: coworker.name,
              status: coworker.status,
              current_task: coworker.current_task,
              model: coworker.model,
              started_at: coworker.started_at,
            },
          })
        }
      }
    }

    if (scrollAreaViewport) {
      scrollAreaViewport.addEventListener('click', handleLinkClick)
      return () => scrollAreaViewport.removeEventListener('click', handleLinkClick)
    }
  })

  // Muted avenue colors matching terminal TUI palette (AVENUE_COLORS from ui.rs)
  const AVENUE_COLORS = {
    lexington: '#5fafaf',   // Cyan
    park: '#5faf5f',        // Green
    madison: '#ff5f5f',     // LightRed
    broadway: '#af5faf',    // Magenta
    amsterdam: '#5f87af',   // Blue
    columbus: '#af5f5f',    // Red
    riverside: '#87d7d7',   // LightCyan
    york: '#87d787',        // LightGreen
    pleasant: '#d7afd7',    // LightMagenta
    vernon: '#87afd7',      // LightBlue
    bleecker: '#d7875f',    // orange (Indexed 208)
    houston: '#ff87d7',     // pink (Indexed 213)
    canal: '#87d7ff',       // light blue (Indexed 117)
    spring: '#afff87',      // light green (Indexed 156)
    prince: '#d7afff',      // lavender (Indexed 183)
    mercer: '#ffaf87',      // salmon (Indexed 216)
    lead: '#d7d787',        // LightYellow
    github: '#585858',      // DarkGray
    system: '#585858',      // DarkGray
    midtown: '#d7d787',     // LightYellow (project lead)
  }

  // Senders whose content is rendered in DarkGray (system infrastructure actors)
  const DIM_SENDERS = new Set(['daemon', 'github', 'system'])

  function getSenderColor(name) {
    return AVENUE_COLORS[name?.toLowerCase()] || '#d0d0d0'
  }

  function isDimSender(sender) {
    return DIM_SENDERS.has(sender?.toLowerCase())
  }

  function isAction(msg) {
    return msg.msg_type === 'action' || msg.content?.startsWith('/me ')
  }

  function isInsight(msg) {
    return msg.msg_type === 'insight' || msg.type === 'insight'
  }

  function formatTime(timestamp) {
    try {
      const date = new Date(timestamp)
      return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
    } catch {
      return ''
    }
  }

  function getActionContent(msg) {
    return msg.content.replace(/^\/me\s*/, '')
  }

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
    openThread(msg, $activeChannel)
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

  // Check if sender changed from the previous message
  function senderChanged(msgs, index) {
    if (index === 0) return true
    return msgs[index].from !== msgs[index - 1].from
  }

  // Blank line before every sender change for consistent visual separation
  function needsBlankLine(msgs, index) {
    if (index === 0) return false
    return senderChanged(msgs, index)
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

  // Ensure the timeout is cleared when the component is destroyed
  $effect(() => {
    return () => {
      if (channelLeadThinkingTimeout) {
        clearTimeout(channelLeadThinkingTimeout)
        channelLeadThinkingTimeout = null
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
        pendingFile = null
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

  function handleInput() {
    resizeTextarea()
    detectAutocompleteTrigger()
  }
</script>

<div class="flex flex-col h-full min-h-0 overflow-hidden relative">
  <div
    class="flex-1 min-h-0 overflow-y-auto overflow-x-hidden overscroll-contain font-mono text-[1rem] leading-[1.55] px-[18px] pt-[14px] pb-[18px]"
    bind:this={scrollAreaViewport}
    onscroll={handleScroll}
  >
      {#if channelMessages.length === 0}
        <div class="text-center text-muted-foreground py-[50px] px-[22px] font-sans">
          <p>No messages in #{$activeChannel}</p>
          <p class="text-[0.9rem] mt-[10px]">Messages posted to this channel will appear here</p>
        </div>
      {:else}
        {#each channelMessages as msg, i}
          <div
            in:fly={{ y: 16, duration: isNewMessage($activeChannel, i) ? 180 : 0, opacity: 0 }}
            class="group relative"
            class:mobile-thread-tappable={!$isWideScreen && !msg.thread_parent_id}
            onclick={(event) => handleMessageTap(event, msg)}
          >
          {#if !msg.thread_parent_id}
            <button
              class="hidden lg:flex absolute right-0 top-[1px] items-center gap-1 px-2 py-[1px] rounded border border-border bg-card/95 text-[0.68rem] text-muted-foreground cursor-pointer opacity-0 pointer-events-none transition-opacity duration-150 group-hover:opacity-100 group-hover:pointer-events-auto focus:opacity-100 focus:pointer-events-auto hover:text-foreground hover:border-ring"
              onclick={() => openThread(msg, $activeChannel)}
              aria-label="Reply in thread"
            >
              Reply
            </button>
          {/if}
          {#if needsBlankLine(channelMessages, i)}
            <div class="h-[1.4em]"></div>
          {/if}
          {#if senderChanged(channelMessages, i)}
            <!-- Author line: bold name + current task -->
            <div
              class="mt-1 whitespace-nowrap overflow-hidden text-ellipsis flex items-center gap-[7px]"
            >
              <span class="font-mono font-semibold text-[0.82rem]" style="color: {getSenderColor(msg.from)}">{msg.from}</span>
              {#if currentTasks[msg.from.toLowerCase()]}
                <span class="text-muted-foreground"> - {currentTasks[msg.from.toLowerCase()]}</span>
              {/if}
            </div>
          {/if}

          {#if isAction(msg) && !hasMermaid(msg.content)}
            <!-- Action message: HH:MM * content -->
            <div class="flex gap-0 break-words">
              <span class="text-muted-foreground/60 flex-shrink-0 w-[3.7em] text-right mr-[0.5em] select-none text-[0.78rem]">{formatTime(msg.timestamp)}</span>
              <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from)}">*</span>
              <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from)}">{@html renderContent(getActionContent(msg))}</span>
            </div>
          {:else if isAction(msg) && hasMermaid(msg.content)}
            <!-- Action message with mermaid diagram(s) -->
            {#each parseSegments(getActionContent(msg)) as segment, si}
              {#if segment.type === 'mermaid'}
                <div class="ml-[4.2em]">
                  <MermaidDiagram code={segment.content} />
                </div>
              {:else}
                <div class="flex gap-0 break-words">
                  {#if si === 0}
                    <span class="text-muted-foreground/60 flex-shrink-0 w-[3.7em] text-right mr-[0.5em] select-none text-[0.78rem]">{formatTime(msg.timestamp)}</span>
                    <span class="flex-shrink-0 mr-[0.3em]" style="color: {getSenderColor(msg.from)}">*</span>
                  {:else}
                    <span class="text-muted-foreground/60 flex-shrink-0 w-[3.7em] text-right mr-[0.5em] select-none text-[0.78rem]"></span>
                    <span class="flex-shrink-0 mr-[0.3em] invisible">*</span>
                  {/if}
                  <span class="action-text flex-1 min-w-0" style="color: {getSenderColor(msg.from)}">{@html renderContent(segment.content)}</span>
                </div>
              {/if}
            {/each}
          {:else if hasMermaid(msg.content)}
            <!-- Message with mermaid diagram(s) -->
            {#each parseSegments(msg.content) as segment, si}
              {#if segment.type === 'mermaid'}
                <div class="ml-[4.2em]">
                  <MermaidDiagram code={segment.content} />
                </div>
              {:else}
                <div class="flex gap-0 break-words">
                  {#if si === 0}
                    <span class="text-muted-foreground/60 flex-shrink-0 w-[3.7em] text-right mr-[0.5em] select-none text-[0.78rem]">{formatTime(msg.timestamp)}</span>
                  {:else}
                    <span class="text-muted-foreground/60 flex-shrink-0 w-[3.7em] text-right mr-[0.5em] select-none text-[0.78rem]"></span>
                  {/if}
                  <span class="message-text flex-1 min-w-0 {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(segment.content)}</span>
                </div>
              {/if}
            {/each}
          {:else}
            <!-- Regular message: HH:MM content -->
            <div class="flex gap-0 break-words">
              <span class="text-muted-foreground/60 flex-shrink-0 w-[3.7em] text-right mr-[0.5em] select-none text-[0.78rem]">{formatTime(msg.timestamp)}</span>
              <span class="message-text flex-1 min-w-0 {isDimSender(msg.from) ? 'text-muted-foreground' : 'text-foreground'}">{@html renderContent(msg.content)}</span>
            </div>
          {/if}

          <!-- Reply indicator for messages with thread replies -->
          {#if !msg.thread_parent_id && msg.reply_count}
            <div class="flex gap-0">
              <span class="flex-shrink-0 w-[3.7em] mr-[0.5em]"></span>
              <button
                class="flex items-center gap-1.5 text-[0.75rem] text-[hsl(var(--link-default))] hover:text-[hsl(var(--link-hover))] cursor-pointer bg-transparent border-none p-0 mt-0.5"
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

      <!-- Unified activity strip: shows the active channel's lead name, bouncing dots,
           and tool call activity. In the main channel, uses lead_working (same signal as
           the TUI braille spinner) to drive the dots. In topic channels, InProgress tool
           items drive the dots (channel leads don't have a separate lead_working signal). -->
      {#if activeChannelToolItems.length > 0 || channelLeadThinking || ($activeChannel === 'midtown' && !!$daemonStatus?.lead_working)}
        {@const agentName = $activeChannel}
        {@const stripColor = agentName === 'midtown' ? AVENUE_COLORS.lead : getSenderColor(agentName)}
        {@const isLeadWorking = $activeChannel === 'midtown' ? !!$daemonStatus?.lead_working : false}
        {@const hasInProgressItems = activeChannelToolItems.some((item) => item.status === 'InProgress')}
        {@const showDots = isLeadWorking || hasInProgressItems || channelLeadThinking}
        <div class="mt-[3px]">
          {#if activeChannelToolItems.length > 0}
            <ToolActivity {agentName} items={activeChannelToolItems} />
          {/if}
          <div class="flex items-center gap-[7px] whitespace-nowrap overflow-hidden text-ellipsis">
            {#if showDots}
              <span class="typing-dots flex gap-[3px] items-center">
                <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {stripColor}"></span>
                <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {stripColor}"></span>
                <span class="dot w-[5px] h-[5px] rounded-full" style="background-color: {stripColor}"></span>
              </span>
            {/if}
            <span class="font-mono font-semibold text-[0.85rem]" style="color: {stripColor}">{agentName}</span>
          </div>
        </div>
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
        <div class="relative inline-block max-w-[200px] border border-border rounded-lg p-2 bg-card">
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
            class="absolute top-1 right-1 w-6 h-6 p-0 rounded-full bg-[rgba(0,0,0,0.7)] text-white text-[1.2rem] leading-none flex items-center justify-center cursor-pointer border border-[#3a3a3a] hover:bg-[rgba(255,87,87,0.8)] hover:border-[#ff5f5f]"
            onclick={clearPendingFile}
            aria-label="Remove file"
          >
            &times;
          </button>
        </div>
      {/if}
      <div class="flex gap-2 w-full">
        <textarea
          bind:this={textareaElement}
          bind:value={inputText}
          placeholder="Message to #{$activeChannel}..."
          rows="1"
          class="flex-1 py-[13px] px-[17px] border-2 border-border rounded-[18px] bg-card text-foreground text-[1.02rem] font-inherit outline-none resize-none min-h-[1.6em] max-h-[9em] overflow-y-auto focus:border-primary placeholder:text-muted-foreground"
          onkeydown={handleKeyDown}
          onpaste={handlePaste}
          oninput={handleInput}
        ></textarea>
        <button
          type="submit"
          disabled={!inputText.trim() && !pendingFile || uploading}
          class="py-[13px] px-[22px] border-none rounded-[26px] bg-[#5faf5f] text-[#0a0a0a] font-bold cursor-pointer transition-all duration-200 text-[0.95rem] tracking-[0.01em] disabled:opacity-40 disabled:cursor-not-allowed hover:bg-[#6fc57f] hover:-translate-y-[1px] active:translate-y-0 not-disabled:hover:bg-[#6fc57f]"
        >
          {uploading ? 'Uploading...' : 'Send'}
        </button>
      </div>
    </form>
  </div>
</div>

<!-- Task detail modal (opened by clicking !N task links in chat) -->
<Dialog.Root open={selectedTask != null} onOpenChange={(open) => { if (!open) selectedTask = null }}>
  <Dialog.Content class="bg-card rounded-[9px] p-[18px] max-w-[420px] max-h-[80vh] overflow-y-auto border border-border">
    <Dialog.Header>
      <div class="flex items-center gap-[9px]">
        <span class="text-primary font-mono text-[0.88rem]">!{selectedTask?.id}</span>
        <span class="text-[0.72rem] py-[3px] px-[9px] rounded-[13px] bg-accent text-muted-foreground capitalize">{selectedTask?.status}</span>
      </div>
      <Dialog.Title class="text-foreground text-[1.05rem] font-semibold m-0 leading-[1.45]">
        {selectedTask?.subject}
      </Dialog.Title>
    </Dialog.Header>

    {#if selectedTask?.description}
      <p class="text-muted-foreground text-[0.88rem] leading-[1.55] m-0 mb-[13px] whitespace-pre-wrap">{selectedTask.description}</p>
    {:else}
      <p class="text-muted-foreground/60 text-[0.88rem] italic leading-[1.55] m-0 mb-[13px]">No description</p>
    {/if}

    {#if selectedTask?.owner}
      <div class="flex gap-[9px] text-[0.85rem] mb-[5px]">
        <span class="text-muted-foreground/60">Owner:</span>
        <span class="text-foreground">{selectedTask.owner}</span>
      </div>
    {/if}
  </Dialog.Content>
</Dialog.Root>

<style>
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
  }

  :global(.message-text pre code),
  :global(.action-text pre code) {
    background: none;
    padding: 0;
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

  /* Lists */
  :global(.message-text ul),
  :global(.message-text ol),
  :global(.action-text ul),
  :global(.action-text ol) {
    margin: 3px 0;
    padding-left: 1.6em;
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
