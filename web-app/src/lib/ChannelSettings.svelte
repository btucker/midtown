<script>
import ChannelWorkflow from "./ChannelWorkflow.svelte";
import { activeChannel, channelSettings } from "./store.js";

let inlineToolCalls = $derived($channelSettings[$activeChannel]?.inlineToolCalls ?? false);

function toggleInlineToolCalls() {
	channelSettings.update((s) => ({
		...s,
		[$activeChannel]: {
			...s[$activeChannel],
			inlineToolCalls: !inlineToolCalls,
		},
	}));
}
</script>

<div class="settings-layout">
  <div class="settings-content">
    <!-- Tool-call display toggle -->
    <section class="settings-section">
      <h2 class="section-title">Tool Call Display</h2>
      <div class="setting-row">
        <div class="setting-info">
          <span class="setting-label">Inline tool calls</span>
          <span class="setting-description">
            Show Edit/Write tool calls inline in the message stream instead of grouped at the bottom.
          </span>
        </div>
        <button
          class="toggle-switch"
          class:active={inlineToolCalls}
          onclick={toggleInlineToolCalls}
          role="switch"
          aria-checked={inlineToolCalls}
          aria-label="Toggle inline tool calls"
        >
          <span class="toggle-knob"></span>
        </button>
      </div>
    </section>

    <!-- Workflow section (embedded) -->
    <section class="settings-section workflow-embed">
      <ChannelWorkflow />
    </section>
  </div>
</div>

<style>
  .settings-layout {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    height: 100%;
  }

  .settings-content {
    padding: 20px 24px;
    max-width: 800px;
  }

  .settings-section {
    margin-bottom: 28px;
  }

  .section-title {
    font-size: 0.72rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: hsl(var(--muted-foreground));
    margin: 0 0 10px;
    padding-bottom: 6px;
    border-bottom: 1px solid hsl(var(--border));
  }

  .setting-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 8px 0;
  }

  .setting-info {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .setting-label {
    font-size: 0.88rem;
    font-weight: 500;
    color: hsl(var(--foreground));
  }

  .setting-description {
    font-size: 0.78rem;
    color: hsl(var(--muted-foreground));
  }

  .toggle-switch {
    position: relative;
    width: 40px;
    height: 22px;
    border-radius: 11px;
    border: 1px solid hsl(var(--border));
    background: hsl(var(--accent));
    cursor: pointer;
    flex-shrink: 0;
    transition: background 0.2s, border-color 0.2s;
    padding: 0;
  }

  .toggle-switch.active {
    background: hsl(var(--primary));
    border-color: hsl(var(--primary));
  }

  .toggle-knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: hsl(var(--background));
    transition: transform 0.2s;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.2);
  }

  .toggle-switch.active .toggle-knob {
    transform: translateX(18px);
  }

  /* The embedded workflow already has its own padding — remove duplicate */
  .workflow-embed :global(.workflow-layout) {
    overflow: visible;
  }

  .workflow-embed :global(.workflow-content) {
    padding: 0;
  }

  @media (max-width: 767px) {
    .settings-content {
      padding: 16px;
    }
  }
</style>
