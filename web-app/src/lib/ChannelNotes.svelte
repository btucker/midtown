<script>
  import { onMount } from 'svelte'
  import { activeChannel, activeProject } from './store.js'
  import { renderContent } from './markdown.js'

  /** @type {Array<{filename: string, title: string, content: string}>} */
  let notes = $state([])
  let loading = $state(false)
  let selectedIndex = $state(0)
  /** On mobile, whether we're showing the content pane (true) or the list (false). */
  let mobileShowContent = $state(false)

  async function loadNotes(project, channel) {
    if (!project || !channel) return
    loading = true
    notes = []
    selectedIndex = 0
    mobileShowContent = false
    try {
      const res = await fetch(`/api/projects/${encodeURIComponent(project)}/channels/${encodeURIComponent(channel)}/notes`)
      if (res.ok) {
        notes = await res.json()
      }
    } catch (e) {
      console.error('Failed to fetch channel notes:', e)
    } finally {
      loading = false
    }
  }

  // Reload whenever the active project or channel changes.
  $effect(() => {
    loadNotes($activeProject, $activeChannel)
  })

  function selectNote(index) {
    selectedIndex = index
    mobileShowContent = true
  }

  function backToList() {
    mobileShowContent = false
  }

  let selectedNote = $derived(notes[selectedIndex] ?? null)
</script>

<div class="notes-layout">
  <!-- Sidebar: list of notes -->
  <nav class="notes-sidebar" class:mobile-hidden={mobileShowContent}>
    <div class="sidebar-header">Notes</div>
    {#if loading}
      <div class="status-message">Loading…</div>
    {:else if notes.length === 0}
      <div class="status-message empty">No notes yet for this channel</div>
    {:else}
      <ul class="note-list">
        {#each notes as note, i}
          <li>
            <button
              class="note-item"
              class:active={i === selectedIndex}
              onclick={() => selectNote(i)}
            >
              {note.title}
            </button>
          </li>
        {/each}
      </ul>
    {/if}
  </nav>

  <!-- Content pane -->
  <div class="notes-content" class:mobile-hidden={!mobileShowContent && notes.length > 0}>
    {#if loading}
      <div class="content-placeholder">Loading…</div>
    {:else if notes.length === 0}
      <div class="content-placeholder">
        <p>No notes yet for this channel.</p>
        <p class="hint">Add <code>.md</code> files to <code>channels/{$activeChannel}/notes/</code> to get started.</p>
      </div>
    {:else if selectedNote}
      <!-- Mobile back button -->
      <button class="back-button md:hidden" onclick={backToList}>← Notes</button>
      <article class="note-body prose">
        {@html renderContent(selectedNote.content)}
      </article>
    {/if}
  </div>
</div>

<style>
  .notes-layout {
    display: flex;
    flex: 1;
    min-height: 0;
    overflow: hidden;
    height: 100%;
  }

  /* ── Sidebar ── */
  .notes-sidebar {
    width: 220px;
    flex-shrink: 0;
    border-right: 1px solid hsl(var(--border));
    display: flex;
    flex-direction: column;
    overflow: hidden;
    background: hsl(var(--card));
  }

  .sidebar-header {
    padding: 10px 14px 8px;
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: hsl(var(--muted-foreground));
    border-bottom: 1px solid hsl(var(--border));
    flex-shrink: 0;
  }

  .note-list {
    list-style: none;
    margin: 0;
    padding: 6px 0;
    overflow-y: auto;
    flex: 1;
  }

  .note-item {
    display: block;
    width: 100%;
    padding: 7px 14px;
    font-size: 0.82rem;
    text-align: left;
    background: none;
    border: none;
    color: hsl(var(--foreground) / 0.75);
    cursor: pointer;
    border-left: 2px solid transparent;
    transition: background 0.1s, color 0.1s;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .note-item:hover {
    background: hsl(var(--accent));
    color: hsl(var(--foreground));
  }

  .note-item.active {
    color: hsl(var(--primary));
    border-left-color: hsl(var(--primary));
    background: hsl(var(--accent) / 0.5);
    font-weight: 500;
  }

  .status-message {
    padding: 16px 14px;
    font-size: 0.82rem;
    color: hsl(var(--muted-foreground));
  }

  .status-message.empty {
    font-style: italic;
  }

  /* ── Content pane ── */
  .notes-content {
    flex: 1;
    overflow-y: auto;
    padding: 24px 28px;
    min-width: 0;
  }

  .content-placeholder {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    height: 100%;
    gap: 8px;
    color: hsl(var(--muted-foreground));
    text-align: center;
    padding: 40px 24px;
  }

  .content-placeholder p {
    margin: 0;
    font-size: 0.9rem;
  }

  .hint {
    font-size: 0.78rem !important;
    opacity: 0.7;
  }

  .back-button {
    display: none;
    align-items: center;
    gap: 6px;
    background: none;
    border: none;
    color: hsl(var(--primary));
    font-size: 0.82rem;
    cursor: pointer;
    padding: 0 0 12px;
    font-weight: 500;
  }

  /* ── Markdown / prose styles ── */
  .note-body :global(h1) {
    font-size: 1.4rem;
    font-weight: 700;
    margin: 0 0 16px;
    color: hsl(var(--foreground));
    border-bottom: 1px solid hsl(var(--border));
    padding-bottom: 8px;
  }

  .note-body :global(h2) {
    font-size: 1.1rem;
    font-weight: 600;
    margin: 24px 0 10px;
    color: hsl(var(--foreground));
  }

  .note-body :global(h3) {
    font-size: 0.95rem;
    font-weight: 600;
    margin: 18px 0 8px;
    color: hsl(var(--foreground));
  }

  .note-body :global(p) {
    margin: 0 0 12px;
    font-size: 0.88rem;
    line-height: 1.65;
    color: hsl(var(--foreground) / 0.85);
  }

  .note-body :global(ul),
  .note-body :global(ol) {
    margin: 0 0 12px 18px;
    font-size: 0.88rem;
    line-height: 1.65;
    color: hsl(var(--foreground) / 0.85);
  }

  .note-body :global(li) {
    margin-bottom: 4px;
  }

  .note-body :global(code) {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.82em;
    background: hsl(var(--accent));
    padding: 1px 5px;
    border-radius: 3px;
  }

  .note-body :global(pre) {
    background: hsl(var(--accent));
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    padding: 14px 16px;
    overflow-x: auto;
    margin: 0 0 16px;
  }

  .note-body :global(pre code) {
    background: none;
    padding: 0;
    font-size: 0.82rem;
    line-height: 1.55;
  }

  .note-body :global(blockquote) {
    border-left: 3px solid hsl(var(--primary) / 0.5);
    margin: 0 0 12px;
    padding: 6px 14px;
    color: hsl(var(--muted-foreground));
    font-style: italic;
  }

  .note-body :global(hr) {
    border: none;
    border-top: 1px solid hsl(var(--border));
    margin: 20px 0;
  }

  .note-body :global(a) {
    color: hsl(var(--primary));
    text-decoration: none;
  }

  .note-body :global(a:hover) {
    text-decoration: underline;
  }

  .note-body :global(table) {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
    margin-bottom: 16px;
  }

  .note-body :global(th) {
    text-align: left;
    padding: 6px 10px;
    font-weight: 600;
    border-bottom: 2px solid hsl(var(--border));
    color: hsl(var(--muted-foreground));
    font-size: 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }

  .note-body :global(td) {
    padding: 6px 10px;
    border-bottom: 1px solid hsl(var(--border) / 0.5);
  }

  /* ── Mobile layout ── */
  @media (max-width: 767px) {
    .notes-layout {
      position: relative;
    }

    .notes-sidebar {
      width: 100%;
      border-right: none;
    }

    .notes-content {
      position: absolute;
      inset: 0;
      background: hsl(var(--background));
      padding: 16px;
    }

    .back-button {
      display: flex;
    }

    .mobile-hidden {
      display: none;
    }
  }
</style>
