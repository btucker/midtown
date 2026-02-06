<script>
  import { messages, coworkers, leadTyping } from './store.js'
  import { sendMessage } from './api.js'
  import { tick } from 'svelte'
  import MermaidDiagram from './MermaidDiagram.svelte'

  let inputText = $state('')
  let messagesContainer = $state(null)
  let autoScroll = $state(true)

  // Cache current tasks to avoid recalculating on every render
  let currentTasks = $derived(getCurrentTasks($coworkers))

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

  // Split text into segments of plain text and mermaid code blocks.
  // Returns array of {type: 'text'|'mermaid', content: string}.
  function parseSegments(text) {
    const segments = []
    const regex = /```mermaid\s*\n([\s\S]*?)```/g
    let lastIndex = 0
    let match

    while ((match = regex.exec(text)) !== null) {
      if (match.index > lastIndex) {
        segments.push({ type: 'text', content: text.slice(lastIndex, match.index) })
      }
      segments.push({ type: 'mermaid', content: match[1].trim() })
      lastIndex = regex.lastIndex
    }

    if (lastIndex < text.length) {
      segments.push({ type: 'text', content: text.slice(lastIndex) })
    }

    return segments
  }

  // Check if message text contains any mermaid code blocks
  function hasMermaid(text) {
    return /```mermaid\s*\n/.test(text)
  }

  // Render markdown-like formatting (bold, links)
  function renderContent(text) {
    // Escape HTML first
    let html = text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
    // Bold: **text**
    html = html.replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    // Links: [text](url)
    html = html.replace(/\[([^\]]+)\]\(([^)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
    // Bare URLs
    html = html.replace(/(^|[\s(])(https?:\/\/[^\s)]+)/g, '$1<a href="$2" target="_blank" rel="noopener">$2</a>')
    return html
  }

  // Auto-scroll to bottom when new messages arrive
  $effect(() => {
    if ($messages.length > 0 && autoScroll && messagesContainer) {
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
    {#if $messages.length === 0}
      <div class="empty-state">
        <p>No messages yet</p>
        <p class="hint">Messages from the team channel will appear here</p>
      </div>
    {:else}
      {#each $messages as msg, i}
        {#if needsBlankLine($messages, i)}
          <div class="blank-line"></div>
        {/if}

        {#if senderChanged($messages, i)}
          <!-- Author line: bold name + current task -->
          <div class="sender-line">
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
      placeholder="Message to lead..."
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
