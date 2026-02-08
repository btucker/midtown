<script>
  import { messages, messagesByChannel, activeChannel, channels, coworkers, leadTyping, kanbanData } from './store.js'
  import { sendMessage } from './api.js'
  import { tick, onMount } from 'svelte'
  import MermaidDiagram from './MermaidDiagram.svelte'
  import { parseSegments, hasMermaid, renderContent } from './markdown.js'

  let inputText = $state('')
  let messagesContainer = $state(null)
  let autoScroll = $state(true)

  // Filter messages by active channel
  let channelMessages = $derived($messagesByChannel[$activeChannel] || [])

  // Cache current tasks to avoid recalculating on every render
  let currentTasks = $derived(getCurrentTasks($coworkers))

  // Get PR status from kanban data
  function getPrStatus(prNum) {
    const pr = $kanbanData.review.find((p) => p.number === parseInt(prNum))
    return pr ? pr.status : null
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
        // TODO: Show task detail panel/modal
        console.log('Task link clicked:', taskId)
      } else if (target.classList.contains('pr-link')) {
        e.preventDefault()
        const prNum = target.dataset.pr
        // Open GitHub PR in new tab (assuming GitHub URL structure)
        // In real implementation, this should use the actual repo URL from config
        console.log('PR link clicked:', prNum)
        // window.open(`https://github.com/owner/repo/pull/${prNum}`, '_blank')
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

  function handleSubmit(e) {
    e.preventDefault()
    if (inputText.trim()) {
      sendMessage(inputText.trim())
      inputText = ''
    }
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
    <textarea
      bind:value={inputText}
      placeholder="Message to #{$activeChannel}..."
      rows="1"
      onkeydown={handleKeyDown}
    ></textarea>
    <button type="submit" disabled={!inputText.trim()}>Send</button>
  </form>
</div>

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
    gap: 8px;
    padding: 12px;
    background: #262626;
    border-top: 1px solid #3a3a3a;
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
</style>
