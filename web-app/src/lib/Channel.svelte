<script>
  import { messages, coworkers } from './store.js'
  import { sendMessage } from './api.js'
  import { tick } from 'svelte'

  let inputText = $state('')
  let messagesContainer = $state(null)
  let autoScroll = $state(true)

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

  // System-like senders are grouped together without blank lines between them
  // (matches TUI's is_system_like_sender: daemon/system only, NOT github)
  const SYSTEM_LIKE_SENDERS = new Set(['daemon', 'system', 'midtown'])

  // Dim senders have their content rendered in DarkGray
  // (matches TUI's is_dim_sender: daemon/github/system)
  const DIM_SENDERS = new Set(['daemon', 'midtown', 'github', 'system'])

  function getSenderColor(name) {
    return AVENUE_COLORS[name?.toLowerCase()] || '#d0d0d0'
  }

  function isSystemLike(sender) {
    return SYSTEM_LIKE_SENDERS.has(sender?.toLowerCase())
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

  // Check if we need a blank line before this message
  // Matches TUI: blank line on sender change, except between consecutive system-like senders
  function needsBlankLine(msgs, index) {
    if (index === 0) return false
    if (!senderChanged(msgs, index)) return false
    const prev = msgs[index - 1].from
    const curr = msgs[index].from
    // No blank line between consecutive system-like senders (e.g., daemon → system)
    if (isSystemLike(prev) && isSystemLike(curr)) return false
    return true
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

  function handleScroll() {
    if (!messagesContainer) return
    const { scrollTop, scrollHeight, clientHeight } = messagesContainer
    autoScroll = scrollHeight - scrollTop - clientHeight < 50
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
      {@const currentTasks = getCurrentTasks($coworkers)}
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

        {#if isAction(msg)}
          <!-- Action message: HH:MM * content -->
          <div class="message-line">
            <span class="time-gutter">{formatTime(msg.timestamp)}</span>
            <span class="action-star" style="color: {getSenderColor(msg.from)}">*</span>
            <span class="action-text" style="color: {getSenderColor(msg.from)}">{@html renderContent(getActionContent(msg))}</span>
          </div>
        {:else}
          <!-- Regular message: HH:MM content -->
          <div class="message-line">
            <span class="time-gutter">{formatTime(msg.timestamp)}</span>
            <span class="message-text" class:dim-text={isDimSender(msg.from)}>{@html renderContent(msg.content)}</span>
          </div>
        {/if}
      {/each}
    {/if}
  </div>

  <form class="input-area" onsubmit={handleSubmit}>
    <input
      type="text"
      bind:value={inputText}
      placeholder="Message to lead..."
      autocomplete="off"
    />
    <button type="submit" disabled={!inputText.trim()}>Send</button>
  </form>
</div>

<style>
  .channel-container {
    display: flex;
    flex-direction: column;
    height: 100%;
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

  /* Input area */
  .input-area {
    display: flex;
    gap: 8px;
    padding: 12px;
    background: #262626;
    border-top: 1px solid #3a3a3a;
  }

  input {
    flex: 1;
    padding: 12px 16px;
    border: 1px solid #3a3a3a;
    border-radius: 24px;
    background: #1c1c1c;
    color: #d0d0d0;
    font-size: 1rem;
    outline: none;
  }

  input:focus {
    border-color: #5fafaf;
  }

  input::placeholder {
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
