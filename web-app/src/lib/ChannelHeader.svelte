<script>
  import { activeChannel } from './store.js'
  import Kanban from './Kanban.svelte'

  let kanbanExpanded = $state(true)

  function toggleKanban() {
    kanbanExpanded = !kanbanExpanded
  }
</script>

<div class="channel-header">
  <div class="header-top">
    <div class="channel-title">
      <span class="channel-hash">#</span>
      <span class="channel-name">{$activeChannel}</span>
    </div>
    <button class="kanban-toggle" onclick={toggleKanban} title={kanbanExpanded ? 'Hide kanban' : 'Show kanban'}>
      {kanbanExpanded ? '\u25BC' : '\u25B6'}
    </button>
  </div>
  {#if kanbanExpanded}
    <div class="kanban-strip">
      <Kanban />
    </div>
  {/if}
</div>

<style>
  .channel-header {
    background: #1a1a1a;
    border-bottom: 2px solid #2a2a2a;
    flex-shrink: 0;
  }

  .header-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
  }

  .channel-title {
    display: flex;
    align-items: baseline;
    gap: 4px;
  }

  .channel-hash {
    font-size: 1.2rem;
    color: #606060;
    font-weight: 700;
  }

  .channel-name {
    font-size: 1.1rem;
    font-weight: 700;
    color: #d0d0d0;
  }

  .kanban-toggle {
    background: transparent;
    border: 1px solid #2a2a2a;
    color: #606060;
    font-size: 0.75rem;
    cursor: pointer;
    padding: 4px 8px;
    border-radius: 4px;
    transition: all 0.15s;
  }

  .kanban-toggle:hover {
    background: #252525;
    border-color: #5faf5f;
    color: #5faf5f;
  }

  .kanban-strip {
    border-top: 1px solid #2a2a2a;
  }

  /* Hide on mobile */
  @media (max-width: 768px) {
    .channel-header {
      display: none;
    }
  }
</style>
