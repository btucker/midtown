<script>
  import { messages, messagesByChannel, activeChannel, channels, coworkers, leadTyping, kanbanData, repoStatus, repoStatuses, daemonStatus } from './store.js'
  import { sendMessage, uploadFile } from './api.js'
  import { tick, onMount } from 'svelte'
  import MermaidDiagram from './MermaidDiagram.svelte'
  import { parseSegments, hasMermaid, renderContent } from './markdown.js'

  let inputText = $state('')
  let messagesContainer = $state(null)
  let autoScroll = $state(true)
  let selectedTask = $state(null)
  let pendingFile = $state(null)
  let uploading = $state(false)

  // Filter messages by active channel
  let channelMessages = $derived($messagesByChannel[$activeChannel] || [])

  // Cache current tasks to avoid recalculating on every render
  let currentTasks = $derived(getCurrentTasks($coworkers))

  // Get PR status from kanban data
  function getPrStatus(prNum) {
    const pr = $kanbanData.review.find((p) => p.number === parseInt(prNum))
    return pr ? pr.status : null
  }

  // Build GitHub PR URL (multi-repo aware).
  // Looks up the PR in kanbanData to find its repo, then resolves via
  // repoStatuses. Falls back to the primary repo if no match is found.
  // Returns null if repo full name is unavailable.
  function getPrUrl(prNum) {
    const num = parseInt(prNum)
    // Search open and merged PRs for this number
    const pr = $kanbanData.review.find((p) => p.number === num)
      || $kanbanData.done.find((p) => p.number === num)
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

  function handleModalKeydown(event) {
    if (event.key === 'Escape' && selectedTask) {
      closeTaskModal()
    }
  }

  // Handle clicks on channel links, task links, and PR links
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
          selectedTask = task
        }
      } else if (target.classList.contains('pr-link')) {
        e.preventDefault()
        const prNum = target.dataset.pr
        const url = getPrUrl(prNum)
        if (url) {
          window.open(url, '_blank', 'noopener')
        }
      }
    }

    if (messagesContainer) {
      messagesContainer.addEventListener('click', handleLinkClick)
      return () => messagesContainer.removeEventListener('click', handleLinkClick)
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
    midtown: '#585858',     // DarkGray (daemon renamed to midtown)
  }

  // Senders whose content is rendered in DarkGray (system infrastructure actors)
  const DIM_SENDERS = new Set(['daemon', 'midtown', 'github', 'system'])

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

  function isCrossPost(msg) {
    return msg.source_channel && msg.source_channel !== msg.channel
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
    if (channelMessages.length > 0 && autoScroll && messagesContainer) {
      tick().then(() => {
        messagesContainer.scrollTop = messagesContainer.scrollHeight
      })
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

        sendMessage(message)
        inputText = ''
        pendingFile = null
      } else {
        alert(`Upload failed: ${result.error}`)
        uploading = false
        return
      }
    } else if (inputText.trim()) {
      sendMessage(inputText.trim())
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
    // Submit on Enter, allow Shift+Enter for new lines
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault()
      handleSubmit(e)
    }
  }

  function handleScroll() {
    if (!messagesContainer) return
    const { scrollTop, scrollHeight, clientHeight } = messagesContainer
    autoScroll = scrollHeight - scrollTop - clientHeight < 50
  }

  function scrollToBottom() {
    if (messagesContainer) {
      messagesContainer.scrollTop = messagesContainer.scrollHeight
    }
  }
</script>

<svelte:window onkeydown={handleModalKeydown} />

<div class="channel-container">
  <div class="messages" bind:this={messagesContainer} onscroll={handleScroll}>
    {#if channelMessages.length === 0}
      <div class="empty-state">
        <p>No messages in #{$activeChannel}</p>
        <p class="hint">Messages posted to this channel will appear here</p>
      </div>
    {:else}
      {#each channelMessages as msg, i}
        {#if needsBlankLine(channelMessages, i)}
          <div class="blank-line"></div>
        {/if}

        {#if senderChanged(channelMessages, i)}
          <!-- Author line: bold name + current task + cross-post indicator -->
          <div class="sender-line" class:cross-post={isInsight(msg) && isCrossPost(msg)}>
            {#if isInsight(msg) && isCrossPost(msg)}
              <span class="insight-star">★</span>
              <span class="cross-post-source">from #{msg.source_channel}</span>
              <span class="sender-divider">|</span>
            {/if}
            <span class="sender-name" style="color: {getSenderColor(msg.from)}">{msg.from}</span>
            {#if currentTasks[msg.from.toLowerCase()]}
              <span class="sender-task"> - {currentTasks[msg.from.toLowerCase()]}</span>
            {/if}
          </div>
        {/if}

        {#if isAction(msg) && !hasMermaid(msg.content)}
          <!-- Action message: HH:MM * content -->
          <div class="message-line">
            <span class="time-gutter">{formatTime(msg.timestamp)}</span>
            <span class="action-star" style="color: {getSenderColor(msg.from)}">*</span>
            <span class="action-text" style="color: {getSenderColor(msg.from)}">{@html renderContent(getActionContent(msg))}</span>
          </div>
        {:else if isAction(msg) && hasMermaid(msg.content)}
          <!-- Action message with mermaid diagram(s) -->
          {#each parseSegments(getActionContent(msg)) as segment, si}
            {#if segment.type === 'mermaid'}
              <div class="mermaid-block">
                <MermaidDiagram code={segment.content} />
              </div>
            {:else}
              <div class="message-line">
                {#if si === 0}
                  <span class="time-gutter">{formatTime(msg.timestamp)}</span>
                  <span class="action-star" style="color: {getSenderColor(msg.from)}">*</span>
                {:else}
                  <span class="time-gutter"></span>
                  <span class="action-star" style="visibility: hidden">*</span>
                {/if}
                <span class="action-text" style="color: {getSenderColor(msg.from)}">{@html renderContent(segment.content)}</span>
              </div>
            {/if}
          {/each}
        {:else if hasMermaid(msg.content)}
          <!-- Message with mermaid diagram(s) -->
          {#each parseSegments(msg.content) as segment, si}
            {#if segment.type === 'mermaid'}
              <div class="mermaid-block">
                <MermaidDiagram code={segment.content} />
              </div>
            {:else}
              <div class="message-line">
                {#if si === 0}
                  <span class="time-gutter">{formatTime(msg.timestamp)}</span>
                {:else}
                  <span class="time-gutter"></span>
                {/if}
                <span class="message-text" class:dim-text={isDimSender(msg.from)}>{@html renderContent(segment.content)}</span>
              </div>
            {/if}
          {/each}
        {:else}
          <!-- Regular message: HH:MM content -->
          <div class="message-line">
            <span class="time-gutter">{formatTime(msg.timestamp)}</span>
            <span class="message-text" class:dim-text={isDimSender(msg.from)}>{@html renderContent(msg.content)}</span>
          </div>
        {/if}
      {/each}
    {/if}

    {#if $leadTyping}
      <div class="typing-indicator">
        <span class="typing-name" style="color: {AVENUE_COLORS.lead}">lead</span>
        <span class="typing-dots">
          <span class="dot"></span>
          <span class="dot"></span>
          <span class="dot"></span>
        </span>
      </div>
    {/if}
  </div>

  {#if !autoScroll}
    <button class="scroll-to-bottom" onclick={scrollToBottom} aria-label="Scroll to bottom">
      ↓
    </button>
  {/if}

  <form class="input-area" onsubmit={handleSubmit}>
    {#if pendingFile}
      <div class="file-preview">
        {#if pendingFile.type.startsWith('image/')}
          <img src={URL.createObjectURL(pendingFile)} alt="Preview" class="preview-image" />
        {:else}
          <div class="preview-file">
            <span class="file-icon">📄</span>
            <span class="file-name">{pendingFile.name}</span>
          </div>
        {/if}
        <button type="button" class="remove-file" onclick={clearPendingFile} aria-label="Remove file">×</button>
      </div>
    {/if}
    <div class="input-row">
      <textarea
        bind:value={inputText}
        placeholder="Message to #{$activeChannel}..."
        rows="1"
        onkeydown={handleKeyDown}
        onpaste={handlePaste}
      ></textarea>
      <button type="submit" disabled={!inputText.trim() && !pendingFile || uploading}>
        {uploading ? 'Uploading...' : 'Send'}
      </button>
    </div>
  </form>
</div>

<!-- Task detail modal (opened by clicking !N task links in chat) -->
{#if selectedTask}
  <!-- svelte-ignore a11y_click_events_have_key_events a11y_interactive_supports_focus -->
  <div class="task-modal-overlay" onclick={closeTaskModal} role="dialog" aria-modal="true" tabindex="-1">
    <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_noninteractive_element_interactions -->
    <div class="task-modal-content" role="document" onclick={(e) => e.stopPropagation()}>
      <button class="task-modal-close" onclick={closeTaskModal} aria-label="Close">×</button>

      <div class="task-modal-header">
        <span class="task-modal-id">!{selectedTask.id}</span>
        <span class="task-modal-status">{selectedTask.status}</span>
      </div>
      <h4 class="task-modal-title">{selectedTask.subject}</h4>
      {#if selectedTask.description}
        <p class="task-modal-description">{selectedTask.description}</p>
      {:else}
        <p class="task-modal-description empty">No description</p>
      {/if}
      {#if selectedTask.owner}
        <div class="task-modal-meta">
          <span class="task-meta-label">Owner:</span>
          <span class="task-meta-value">{selectedTask.owner}</span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .channel-container {
    display: flex;
    flex-direction: column;
    height: 100%;
    position: relative;
  }

  .messages {
    flex: 1;
    overflow-y: auto;
    padding: 12px 16px;
    font-family: 'SF Mono', 'Menlo', 'Consolas', monospace;
    font-size: 0.85rem;
    line-height: 1.5;
  }

  .empty-state {
    text-align: center;
    color: #585858;
    padding: 40px 20px;
    font-family: system-ui, -apple-system, sans-serif;
  }

  .empty-state .hint {
    font-size: 0.875rem;
    margin-top: 8px;
  }

  /* Blank line separator between different senders */
  .blank-line {
    height: 0.75em;
  }

  /* Author/sender header line */
  .sender-line {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .sender-line.cross-post {
    padding: 4px 8px;
    margin: -4px -8px 4px;
    border-left: 3px solid #af5faf;
    background: rgba(175, 95, 175, 0.1);
    border-radius: 4px;
  }

  .insight-star {
    color: #ffaf5f;
    font-size: 1rem;
  }

  .cross-post-source {
    color: #5fafaf;
    font-size: 0.85rem;
    font-weight: 600;
  }

  .sender-divider {
    color: #585858;
  }

  .sender-name {
    font-weight: 700;
  }

  .sender-task {
    color: #585858;
  }

  /* Mermaid diagram block (indented to match message gutter) */
  .mermaid-block {
    margin-left: 4em;
  }

  /* Message line with timestamp gutter */
  .message-line {
    display: flex;
    gap: 0;
    word-break: break-word;
  }

  .time-gutter {
    color: #4a4a4a;
    flex-shrink: 0;
    width: 3.5em;
    text-align: right;
    margin-right: 0.5em;
    user-select: none;
  }

  .message-text {
    color: #d0d0d0;
    flex: 1;
    min-width: 0;
  }

  .message-text.dim-text {
    color: #585858;
  }

  .message-text :global(a),
  .action-text :global(a) {
    color: #5fafaf;
    text-decoration: none;
  }

  .message-text :global(a:hover),
  .action-text :global(a:hover) {
    text-decoration: underline;
  }

  .message-text :global(a.channel-link),
  .action-text :global(a.channel-link) {
    color: #5fafaf;
    font-weight: 600;
    cursor: pointer;
  }

  .message-text :global(a.task-link),
  .action-text :global(a.task-link) {
    color: #af5faf;
    font-weight: 600;
    cursor: pointer;
  }

  .message-text :global(a.pr-link),
  .action-text :global(a.pr-link) {
    color: #5f87af;
    font-weight: 600;
    cursor: pointer;
  }

  /* Inline code */
  .message-text :global(code),
  .action-text :global(code) {
    background: #2a2a2a;
    padding: 0.1em 0.4em;
    border-radius: 3px;
    font-size: 0.9em;
  }

  /* Code blocks */
  .message-text :global(pre),
  .action-text :global(pre) {
    background: #2a2a2a;
    padding: 8px 12px;
    border-radius: 4px;
    overflow-x: auto;
    margin: 4px 0;
  }

  .message-text :global(pre code),
  .action-text :global(pre code) {
    background: none;
    padding: 0;
    border-radius: 0;
    font-size: 0.85em;
  }

  /* Headings - scaled down for chat context */
  .message-text :global(h1),
  .message-text :global(h2),
  .message-text :global(h3),
  .action-text :global(h1),
  .action-text :global(h2),
  .action-text :global(h3) {
    font-size: 1em;
    font-weight: 700;
    margin: 4px 0 2px;
  }

  /* Lists */
  .message-text :global(ul),
  .message-text :global(ol),
  .action-text :global(ul),
  .action-text :global(ol) {
    margin: 2px 0;
    padding-left: 1.5em;
  }

  /* Blockquotes */
  .message-text :global(blockquote),
  .action-text :global(blockquote) {
    border-left: 2px solid #4a4a4a;
    margin: 2px 0;
    padding-left: 8px;
    color: #888;
  }

  /* Action messages (in sender's color) */
  .action-star {
    flex-shrink: 0;
    margin-right: 0.25em;
  }

  .action-text {
    flex: 1;
    min-width: 0;
  }

  /* Typing indicator */
  .typing-indicator {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 0;
    margin-top: 4px;
    opacity: 0.7;
  }

  .typing-name {
    font-weight: 700;
    font-size: 0.8rem;
  }

  .typing-dots {
    display: flex;
    gap: 3px;
    align-items: center;
  }

  .typing-dots .dot {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: #d7d787;
    animation: typing-bounce 1.4s infinite ease-in-out both;
  }

  .typing-dots .dot:nth-child(1) {
    animation-delay: 0s;
  }

  .typing-dots .dot:nth-child(2) {
    animation-delay: 0.2s;
  }

  .typing-dots .dot:nth-child(3) {
    animation-delay: 0.4s;
  }

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

  /* Scroll-to-bottom button */
  .scroll-to-bottom {
    position: absolute;
    bottom: 80px;
    right: 20px;
    width: 36px;
    height: 36px;
    border-radius: 50%;
    border: 1px solid #3a3a3a;
    background: #262626;
    color: #d0d0d0;
    font-size: 1.1rem;
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    transition: opacity 0.2s;
    opacity: 0.8;
    z-index: 10;
  }

  .scroll-to-bottom:hover {
    opacity: 1;
    border-color: #5fafaf;
    color: #5fafaf;
  }

  /* Input area */
  .input-area {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    padding-bottom: calc(12px + env(safe-area-inset-bottom, 0px));
    background: #262626;
    border-top: 1px solid #3a3a3a;
  }

  /* File preview */
  .file-preview {
    position: relative;
    display: inline-block;
    max-width: 200px;
    border: 1px solid #3a3a3a;
    border-radius: 8px;
    padding: 8px;
    background: #1c1c1c;
  }

  .preview-image {
    max-width: 100%;
    max-height: 120px;
    border-radius: 4px;
    display: block;
  }

  .preview-file {
    display: flex;
    align-items: center;
    gap: 8px;
    color: #d0d0d0;
  }

  .file-icon {
    font-size: 1.5rem;
  }

  .file-name {
    font-size: 0.85rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .remove-file {
    position: absolute;
    top: 4px;
    right: 4px;
    width: 24px;
    height: 24px;
    padding: 0;
    border-radius: 50%;
    background: rgba(0, 0, 0, 0.7);
    color: #fff;
    font-size: 1.2rem;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
    border: 1px solid #3a3a3a;
  }

  .remove-file:hover {
    background: rgba(255, 87, 87, 0.8);
    border-color: #ff5f5f;
  }

  .input-row {
    display: flex;
    gap: 8px;
    width: 100%;
  }

  textarea {
    flex: 1;
    padding: 12px 16px;
    border: 1px solid #3a3a3a;
    border-radius: 16px;
    background: #1c1c1c;
    color: #d0d0d0;
    font-size: 1rem;
    font-family: inherit;
    outline: none;
    resize: none;
    min-height: 1.5em;
    max-height: 8em;
    overflow-y: auto;
    field-sizing: content;
  }

  textarea:focus {
    border-color: #5fafaf;
  }

  textarea::placeholder {
    color: #585858;
  }

  button {
    padding: 12px 20px;
    border: none;
    border-radius: 24px;
    background: #5fafaf;
    color: #1c1c1c;
    font-weight: 600;
    cursor: pointer;
    transition: opacity 0.2s;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  button:hover:not(:disabled) {
    opacity: 0.9;
  }

  /* Task detail modal (matches Kanban modal style) */
  .task-modal-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    padding: 16px;
  }

  .task-modal-content {
    background: #16213e;
    border-radius: 8px;
    padding: 16px;
    max-width: 400px;
    width: 100%;
    max-height: 80vh;
    overflow-y: auto;
    position: relative;
    border: 1px solid #0f3460;
  }

  .task-modal-close {
    position: absolute;
    top: 8px;
    right: 8px;
    background: transparent;
    border: none;
    color: #666;
    font-size: 1.5rem;
    cursor: pointer;
    padding: 4px 8px;
    line-height: 1;
    border-radius: 0;
  }

  .task-modal-close:hover {
    color: #00d9ff;
  }

  .task-modal-header {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
  }

  .task-modal-id {
    color: #00d9ff;
    font-family: ui-monospace, monospace;
    font-size: 0.85rem;
  }

  .task-modal-status {
    font-size: 0.7rem;
    padding: 2px 8px;
    border-radius: 12px;
    background: #0f3460;
    color: #888;
    text-transform: capitalize;
  }

  .task-modal-title {
    color: #eee;
    font-size: 1rem;
    font-weight: 600;
    margin: 0 0 12px 0;
    line-height: 1.4;
  }

  .task-modal-description {
    color: #aaa;
    font-size: 0.85rem;
    line-height: 1.5;
    margin: 0 0 12px 0;
    white-space: pre-wrap;
  }

  .task-modal-description.empty {
    color: #666;
    font-style: italic;
  }

  .task-modal-meta {
    display: flex;
    gap: 8px;
    font-size: 0.8rem;
    margin-bottom: 4px;
  }

  .task-meta-label {
    color: #666;
  }

  .task-meta-value {
    color: #ccc;
  }
</style>
