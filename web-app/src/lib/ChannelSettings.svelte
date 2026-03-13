<script>
import { fetchChannelAgentsMd, fetchChannelDirectory, saveChannelAgentsMd, saveChannelDirectory } from "./api.ts";
import ChannelWorkflow from "./ChannelWorkflow.svelte";
import { activeChannel, channelSettings } from "./store.ts";

let inlineToolCalls = $derived($channelSettings[$activeChannel]?.inlineToolCalls ?? true);

function toggleInlineToolCalls() {
	channelSettings.update((s) => ({
		...s,
		[$activeChannel]: {
			...s[$activeChannel],
			inlineToolCalls: !inlineToolCalls,
		},
	}));
}

// Working directory editor state
let directoryValue = $state("");
let directoryOriginal = $state("");
let directoryLoading = $state(false);
let directorySaving = $state(false);
let directoryError = $state("");
let directorySuccess = $state("");

let directoryDirty = $derived(directoryValue !== directoryOriginal);

async function loadDirectory() {
	directoryLoading = true;
	directoryError = "";
	directorySuccess = "";
	const data = await fetchChannelDirectory($activeChannel);
	directoryValue = data.directory || "";
	directoryOriginal = data.directory || "";
	directoryLoading = false;
}

function validateDirectory(path) {
	if (!path) return null;
	if (path.startsWith("/") || path.startsWith("\\")) {
		return "Path must be relative (cannot start with / or \\)";
	}
	if (path.includes("..")) {
		return "Path cannot contain ..";
	}
	return null;
}

async function saveDirectory() {
	const trimmed = directoryValue.trim();
	const validationError = validateDirectory(trimmed);
	if (validationError) {
		directoryError = validationError;
		return;
	}
	directorySaving = true;
	directoryError = "";
	directorySuccess = "";
	const result = await saveChannelDirectory($activeChannel, trimmed || null);
	if (result.ok) {
		directoryOriginal = trimmed;
		directoryValue = trimmed;
		directorySuccess = "Saved";
		setTimeout(() => (directorySuccess = ""), 2000);
	} else {
		directoryError = result.error || "Failed to save";
	}
	directorySaving = false;
}

function discardDirectory() {
	directoryValue = directoryOriginal;
	directoryError = "";
	directorySuccess = "";
}

// AGENTS.md editor state
let agentsScope = $state("channel");
let agentsContent = $state("");
let agentsSource = $state("none");
let agentsOriginal = $state("");
let agentsLoading = $state(false);
let agentsSaving = $state(false);
let agentsError = $state("");
let agentsSuccess = $state("");

let agentsDirty = $derived(agentsContent !== agentsOriginal);

async function loadAgentsMd() {
	agentsLoading = true;
	agentsError = "";
	agentsSuccess = "";
	const data = await fetchChannelAgentsMd($activeChannel, agentsScope);
	if (!data) {
		// Request was aborted (e.g., rapid channel switch) — don't touch state
		agentsLoading = false;
		return;
	}
	if (data.error) {
		agentsError = data.error;
		agentsLoading = false;
		return;
	}
	agentsContent = data.content;
	agentsOriginal = data.content;
	agentsSource = data.source;
	agentsLoading = false;
}

async function saveAgentsMd() {
	agentsSaving = true;
	agentsError = "";
	agentsSuccess = "";
	const result = await saveChannelAgentsMd($activeChannel, agentsContent, agentsScope);
	if (result.ok) {
		agentsOriginal = agentsContent;
		agentsSuccess = "Saved";
		agentsSource = agentsContent.trim() ? (agentsScope === "project" ? "project-local" : "channel-local") : "none";
		setTimeout(() => (agentsSuccess = ""), 2000);
	} else {
		agentsError = result.error || "Failed to save";
	}
	agentsSaving = false;
}

function discardAgentsMd() {
	agentsContent = agentsOriginal;
	agentsError = "";
	agentsSuccess = "";
}

function switchScope(newScope) {
	if (newScope === agentsScope) return;
	if (agentsDirty && !confirm("You have unsaved changes. Switch scope and discard them?")) return;
	agentsScope = newScope;
	loadAgentsMd();
}

const sourceLabels = {
	"channel-repo": "In repo",
	"channel-local": "Local",
	"project-repo": "In repo",
	"project-local": "Local",
	none: "Not set",
};

// Load when channel changes
let lastChannel = $state("");
$effect(() => {
	if ($activeChannel && $activeChannel !== lastChannel) {
		lastChannel = $activeChannel;
		loadDirectory();
		loadAgentsMd();
	}
});
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
            Show tool calls inline in the message stream instead of grouped at the bottom.
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

    <!-- Working directory -->
    <section class="settings-section">
      <h2 class="section-title">Working Directory</h2>
      <span class="setting-description">
        Restrict this channel's coworkers to a subdirectory of the repo.
      </span>
      {#if directoryLoading}
        <div class="dir-loading">Loading...</div>
      {:else}
        <input
          type="text"
          class="dir-input"
          bind:value={directoryValue}
          placeholder="e.g. packages/auth"
          spellcheck="false"
          aria-label="Working directory path"
        />
        <div class="dir-actions">
          <div class="dir-status-area">
            {#if directoryError}
              <span class="dir-status error">{directoryError}</span>
            {/if}
            {#if directorySuccess}
              <span class="dir-status success">{directorySuccess}</span>
            {/if}
          </div>
          <div class="dir-buttons">
            {#if directoryDirty}
              <button class="dir-btn discard" onclick={discardDirectory}>Discard</button>
            {/if}
            <button
              class="dir-btn save"
              onclick={saveDirectory}
              disabled={!directoryDirty || directorySaving}
            >
              {directorySaving ? "Saving..." : "Save"}
            </button>
          </div>
        </div>
      {/if}
    </section>

    <!-- AGENTS.md editor -->
    <section class="settings-section">
      <h2 class="section-title">AGENTS.md</h2>
      <div class="agents-header">
        <span class="setting-description">
          Custom instructions for Claude Code sessions.
        </span>
        <div class="scope-selector">
          <button
            class="scope-btn"
            class:active={agentsScope === "channel"}
            onclick={() => switchScope("channel")}
          >Channel</button>
          <button
            class="scope-btn"
            class:active={agentsScope === "project"}
            onclick={() => switchScope("project")}
          >Project</button>
        </div>
      </div>
      {#if agentsLoading}
        <div class="agents-loading">Loading...</div>
      {:else}
        <textarea
          class="agents-editor"
          bind:value={agentsContent}
          placeholder={agentsScope === "channel"
            ? "# Channel Instructions\n\nAdd custom instructions for coworkers in this channel..."
            : "# Project Instructions\n\nAdd instructions shared across all channels..."}
          spellcheck="false"
          aria-label="AGENTS.md editor"
        ></textarea>
        <div class="agents-actions">
          <div class="agents-status-area">
            {#if agentsError}
              <span class="agents-status error">{agentsError}</span>
            {/if}
            {#if agentsSuccess}
              <span class="agents-status success">{agentsSuccess}</span>
            {/if}
            {#if agentsSource !== "none" && !agentsError && !agentsSuccess}
              <span class="agents-source">Source: {sourceLabels[agentsSource] || agentsSource}</span>
            {/if}
          </div>
          <div class="agents-buttons">
            {#if agentsDirty}
              <button class="agents-btn discard" onclick={discardAgentsMd}>Discard</button>
            {/if}
            <button
              class="agents-btn save"
              onclick={saveAgentsMd}
              disabled={!agentsDirty || agentsSaving}
            >
              {agentsSaving ? "Saving..." : "Save"}
            </button>
          </div>
        </div>
      {/if}
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

  /* Working directory styles */
  .dir-loading {
    font-size: 0.82rem;
    color: hsl(var(--muted-foreground));
    padding: 16px 0;
  }

  .dir-input {
    width: 100%;
    padding: 8px 12px;
    margin-top: 8px;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    background: hsl(var(--background));
    color: hsl(var(--foreground));
    font-family: "SF Mono", "Cascadia Code", "Fira Code", monospace;
    font-size: 0.82rem;
    box-sizing: border-box;
  }

  .dir-input:focus {
    outline: none;
    border-color: hsl(var(--primary));
  }

  .dir-input::placeholder {
    color: hsl(var(--muted-foreground) / 0.5);
  }

  .dir-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 8px;
    min-height: 32px;
  }

  .dir-status-area {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .dir-status {
    font-size: 0.78rem;
  }

  .dir-status.error {
    color: hsl(var(--destructive));
  }

  .dir-status.success {
    color: hsl(var(--accent-green));
  }

  .dir-buttons {
    display: flex;
    gap: 8px;
    margin-left: auto;
  }

  .dir-btn {
    padding: 5px 14px;
    border-radius: 5px;
    font-size: 0.82rem;
    cursor: pointer;
    border: 1px solid hsl(var(--border));
    transition: background 0.15s, border-color 0.15s;
  }

  .dir-btn.discard {
    background: transparent;
    color: hsl(var(--muted-foreground));
  }

  .dir-btn.discard:hover {
    background: hsl(var(--accent));
  }

  .dir-btn.save {
    background: hsl(var(--primary));
    color: hsl(var(--primary-foreground));
    border-color: hsl(var(--primary));
  }

  .dir-btn.save:hover:not(:disabled) {
    opacity: 0.9;
  }

  .dir-btn.save:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  /* AGENTS.md editor styles */
  .agents-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 10px;
  }

  .scope-selector {
    display: flex;
    border: 1px solid hsl(var(--border));
    border-radius: 5px;
    overflow: hidden;
    flex-shrink: 0;
  }

  .scope-btn {
    padding: 4px 12px;
    font-size: 0.78rem;
    border: none;
    background: transparent;
    color: hsl(var(--muted-foreground));
    cursor: pointer;
    transition: background 0.15s, color 0.15s;
  }

  .scope-btn:not(:last-child) {
    border-right: 1px solid hsl(var(--border));
  }

  .scope-btn.active {
    background: hsl(var(--accent));
    color: hsl(var(--foreground));
    font-weight: 500;
  }

  .scope-btn:hover:not(.active) {
    background: hsl(var(--accent) / 0.5);
  }

  .agents-source {
    font-size: 0.72rem;
    color: hsl(var(--muted-foreground));
    white-space: nowrap;
  }

  .agents-loading {
    font-size: 0.82rem;
    color: hsl(var(--muted-foreground));
    padding: 16px 0;
  }

  .agents-editor {
    width: 100%;
    min-height: 200px;
    padding: 12px;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    background: hsl(var(--background));
    color: hsl(var(--foreground));
    font-family: "SF Mono", "Cascadia Code", "Fira Code", monospace;
    font-size: 0.82rem;
    line-height: 1.5;
    resize: vertical;
    box-sizing: border-box;
  }

  .agents-editor:focus {
    outline: none;
    border-color: hsl(var(--primary));
  }

  .agents-editor::placeholder {
    color: hsl(var(--muted-foreground) / 0.5);
  }

  .agents-actions {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-top: 8px;
    min-height: 32px;
  }

  .agents-status-area {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .agents-status {
    font-size: 0.78rem;
  }

  .agents-status.error {
    color: hsl(var(--destructive));
  }

  .agents-status.success {
    color: hsl(var(--accent-green));
  }

  .agents-buttons {
    display: flex;
    gap: 8px;
    margin-left: auto;
  }

  .agents-btn {
    padding: 5px 14px;
    border-radius: 5px;
    font-size: 0.82rem;
    cursor: pointer;
    border: 1px solid hsl(var(--border));
    transition: background 0.15s, border-color 0.15s;
  }

  .agents-btn.discard {
    background: transparent;
    color: hsl(var(--muted-foreground));
  }

  .agents-btn.discard:hover {
    background: hsl(var(--accent));
  }

  .agents-btn.save {
    background: hsl(var(--primary));
    color: hsl(var(--primary-foreground));
    border-color: hsl(var(--primary));
  }

  .agents-btn.save:hover:not(:disabled) {
    opacity: 0.9;
  }

  .agents-btn.save:disabled {
    opacity: 0.5;
    cursor: not-allowed;
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
