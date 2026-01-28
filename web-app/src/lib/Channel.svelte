<script>
  import { messages } from './store.js'
  import { sendMessage } from './api.js'
  import { onMount, tick } from 'svelte'
  import snarkdown from 'snarkdown'

  let inputText = $state('')
  let messagesContainer = $state(null)
  let autoScroll = $state(true)

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
    // Enable auto-scroll when user scrolls near bottom
    autoScroll = scrollHeight - scrollTop - clientHeight < 50
  }

  function formatTime(timestamp) {
    try {
      const date = new Date(timestamp)
      return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    } catch {
      return ''
    }
  }

  // Muted avenue colors matching terminal TUI palette (AVENUE_COLORS from ui.rs)
  const AVENUE_COLORS = {
    lexington: '#5fafaf',   // Cyan
    park: '#5faf5f',        // Green
    madison: '#d7af5f',     // Yellow
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
  }

  function getSenderColor(name) {
    return AVENUE_COLORS[name?.toLowerCase()] || '#d0d0d0'
  }

  function getMessageClass(msg) {
    if (msg.from === 'mobile') return 'message-self'
    if (msg.from === 'github') return 'message-github'
    if (msg.from === 'lead') return 'message-lead'
    if (msg.msg_type === 'action') return 'message-action'
    return ''
  }

  function isAction(msg) {
    return msg.msg_type === 'action' || msg.content.startsWith('/me ')
  }

  function escapeHtml(text) {
    return text
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
  }

  function formatContent(msg) {
    if (isAction(msg)) {
      const content = msg.content.replace(/^\/me\s*/, '')
      return `* ${msg.from} ${content}`
    }
    return msg.content
  }

  function renderMarkdown(msg) {
    return snarkdown(escapeHtml(formatContent(msg)))
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
      {#each $messages as msg}
        <div class="message {getMessageClass(msg)}" class:action={isAction(msg)}>
          {#if !isAction(msg)}
            <div class="message-header">
              <span class="from" style="color: {getSenderColor(msg.from)}">{msg.from}</span>
              <span class="time">{formatTime(msg.timestamp)}</span>
            </div>
          {/if}
          <div class="content markdown" class:action-content={isAction(msg)}>
            {@html renderMarkdown(msg)}
          </div>
        </div>
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
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .empty-state {
    text-align: center;
    color: #585858;
    padding: 40px 20px;
  }

  .empty-state .hint {
    font-size: 0.875rem;
    margin-top: 8px;
  }

  .message {
    padding: 8px 12px;
    background: #262626;
    border-radius: 8px;
    max-width: 85%;
  }

  .message.action {
    background: transparent;
    padding: 4px 12px;
  }

  .message-self {
    align-self: flex-end;
    background: #303030;
    border: 1px solid #5fafaf;
  }

  .message-github {
    border-left: 3px solid #585858;
  }

  .message-lead {
    border-left: 3px solid #d7d787;
  }

  .message-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 4px;
  }

  .from {
    font-weight: 600;
    font-size: 0.8rem;
  }

  .time {
    font-size: 0.7rem;
    color: #585858;
  }

  .content {
    font-size: 0.9rem;
    line-height: 1.4;
    word-break: break-word;
  }

  .action-content {
    font-style: italic;
    color: #585858;
  }

  /* Markdown content styles */
  .markdown :global(p) {
    margin: 0;
  }

  .markdown :global(p + p) {
    margin-top: 0.5em;
  }

  .markdown :global(code) {
    background: rgba(255, 255, 255, 0.1);
    padding: 0.1em 0.35em;
    border-radius: 3px;
    font-size: 0.85em;
    font-family: 'SF Mono', 'Fira Code', 'Cascadia Code', monospace;
  }

  .markdown :global(pre) {
    background: rgba(0, 0, 0, 0.3);
    padding: 8px 12px;
    border-radius: 6px;
    overflow-x: auto;
    margin: 4px 0;
  }

  .markdown :global(pre code) {
    background: none;
    padding: 0;
  }

  .markdown :global(a) {
    color: #5fafaf;
    text-decoration: none;
  }

  .markdown :global(a:hover) {
    text-decoration: underline;
  }

  .markdown :global(strong) {
    color: #fff;
  }

  .markdown :global(blockquote) {
    border-left: 3px solid #444;
    margin: 4px 0;
    padding: 2px 0 2px 10px;
    color: #aaa;
  }

  .markdown :global(ul),
  .markdown :global(ol) {
    margin: 4px 0;
    padding-left: 1.5em;
  }

  .markdown :global(li) {
    margin: 2px 0;
  }

  .markdown :global(h1),
  .markdown :global(h2),
  .markdown :global(h3),
  .markdown :global(h4) {
    margin: 4px 0 2px;
    font-size: 1em;
    font-weight: 600;
    color: #fff;
  }

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
