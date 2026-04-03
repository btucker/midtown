<script lang="ts">
import { getApiBase } from "./api.ts";
import MermaidDiagram from "./MermaidDiagram.svelte";
import { activeChannel, activeProject } from "./store.ts";

interface WorkflowData {
	state?: { tasks?: Record<string, { phase: string }> };
	available_workflows?: Array<{ name: string; description?: string }>;
	assigned_workflow?: string | null;
	lead_driven?: boolean;
	mermaid?: string;
}

let data = $state<WorkflowData | null>(null);
let loading = $state(false);
let error = $state("");
let assigning = $state(false);

let fetchVersion = 0;

function fetchWorkflow() {
	const project = $activeProject;
	const channel = $activeChannel;
	if (!project || !channel) return;

	const controller = new AbortController();
	const version = ++fetchVersion;

	loading = true;
	data = null;
	error = "";

	fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/workflow`, {
		signal: controller.signal,
	})
		.then((res) => {
			if (!res.ok) throw new Error(`HTTP ${res.status}`);
			return res.json();
		})
		.then((d) => {
			if (version === fetchVersion) data = d;
		})
		.catch((e) => {
			if (version === fetchVersion && e.name !== "AbortError") {
				console.error("Failed to fetch workflow:", e);
				error = e.message;
			}
		})
		.finally(() => {
			if (version === fetchVersion) loading = false;
		});

	return () => controller.abort();
}

$effect(() => {
	// Track reactive dependencies
	void $activeProject;
	void $activeChannel;
	return fetchWorkflow();
});

async function handleWorkflowChange(event: Event | { target: { value: string } }) {
	const channel = $activeChannel;
	if (!channel) return;

	const value = (event.target as HTMLSelectElement).value;
	const workflow = value === "" ? null : value;

	assigning = true;
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/workflow`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ workflow }),
		});
		if (!res.ok) {
			const body = await res.json().catch(() => ({}));
			throw new Error(body.error || `HTTP ${res.status}`);
		}
		// Refetch to update diagram and state
		fetchWorkflow();
	} catch (e) {
		console.error("Failed to assign workflow:", e);
		error = e instanceof Error ? e.message : String(e);
	} finally {
		assigning = false;
	}
}

let togglingLeadDriven = $state(false);

async function handleLeadDrivenToggle(event: Event) {
	const channel = $activeChannel;
	if (!channel) return;

	const enabled = (event.target as HTMLInputElement).checked;
	togglingLeadDriven = true;
	try {
		const res = await fetch(`${getApiBase()}/channels/${encodeURIComponent(channel)}/workflow`, {
			method: "PUT",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ lead_driven: enabled }),
		});
		if (!res.ok) {
			const body = await res.json().catch(() => ({}));
			throw new Error(body.error || `HTTP ${res.status}`);
		}
		fetchWorkflow();
	} catch (e) {
		console.error("Failed to toggle lead-driven:", e);
		error = e instanceof Error ? e.message : String(e);
	} finally {
		togglingLeadDriven = false;
	}
}

let taskEntries = $derived(data?.state?.tasks ? Object.entries(data.state.tasks) : []);
</script>

<div class="workflow-layout">
  {#if loading}
    <div class="placeholder">Loading...</div>
  {:else if error}
    <div class="placeholder">
      <p>Failed to load workflow</p>
      <p class="hint">{error}</p>
    </div>
  {:else if data}
    <div class="workflow-content">
      <!-- Workflow selector -->
      <section class="workflow-section">
        <h2 class="section-title">Workflow</h2>
        {#if data.available_workflows && data.available_workflows.length > 0}
          <div class="selector-row">
            <select
              class="workflow-select"
              value={data.assigned_workflow ?? ""}
              onchange={handleWorkflowChange}
              disabled={assigning}
            >
              <option value="">None &mdash; daemon defaults</option>
              {#each data.available_workflows as wf}
                <option value={wf.name}>
                  {wf.name}{wf.description ? ` — ${wf.description}` : ""}
                </option>
              {/each}
            </select>
            {#if assigning}
              <span class="assigning-hint">Saving...</span>
            {/if}
          </div>
        {:else if data.assigned_workflow}
          <div class="selector-row">
            <span class="stale-hint">
              Assigned workflow <strong>{data.assigned_workflow}</strong> is no longer available.
            </span>
            <button
              class="unassign-btn"
              onclick={() => handleWorkflowChange({ target: { value: "" } })}
              disabled={assigning}
            >
              {assigning ? "Removing..." : "Remove assignment"}
            </button>
          </div>
        {:else}
          <p class="empty-hint">
            No workflows available. Create a workflow directory in
            <code class="mono">~/.midtown/projects/&lt;project&gt;/workflows/</code>
          </p>
        {/if}
      </section>

      <!-- Lead-driven toggle -->
      <section class="workflow-section">
        <h2 class="section-title">Lead-Driven Mode</h2>
        <label class="toggle-row">
          <input
            type="checkbox"
            class="toggle-checkbox"
            checked={data.lead_driven}
            onchange={handleLeadDrivenToggle}
            disabled={togglingLeadDriven}
          />
          <span class="toggle-label">
            Lead controls task dispatch
            {#if togglingLeadDriven}
              <span class="assigning-hint">Saving...</span>
            {/if}
          </span>
        </label>
        <p class="toggle-description">
          When enabled, the channel lead decides when to create and assign tasks instead of the daemon dispatching automatically.
        </p>
      </section>

      <!-- Mermaid diagram -->
      {#if data.mermaid}
        <section class="workflow-section">
          <h2 class="section-title">{data.assigned_workflow ? 'State Machine' : 'Default State Machine'}</h2>
          <div class="diagram-container">
            <MermaidDiagram code={data.mermaid} />
          </div>
        </section>
      {/if}

      <!-- Task positions -->
      {#if taskEntries.length > 0}
        <section class="workflow-section">
          <h2 class="section-title">Task Positions</h2>
          <div class="info-card">
            {#each taskEntries as [taskId, taskState]}
              <div class="task-row">
                <span class="task-id">!{taskId}</span>
                <span class="task-arrow">&rarr;</span>
                <span class="task-phase">{(taskState as { phase: string }).phase}</span>
              </div>
            {/each}
          </div>
        </section>
      {/if}
    </div>
  {/if}
</div>

<style>
  .workflow-layout {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    height: 100%;
  }

  .workflow-content {
    padding: 20px 24px;
    max-width: 800px;
  }

  .placeholder {
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

  .placeholder p {
    margin: 0;
    font-size: 0.9rem;
  }

  .hint {
    font-size: 0.78rem !important;
    opacity: 0.7;
  }

  .workflow-section {
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

  .selector-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .workflow-select {
    flex: 1;
    max-width: 400px;
    background: hsl(var(--card));
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    padding: 8px 12px;
    font-size: 0.84rem;
    color: hsl(var(--foreground));
    cursor: pointer;
    outline: none;
  }

  .workflow-select:focus {
    border-color: hsl(var(--primary));
  }

  .workflow-select:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .assigning-hint {
    font-size: 0.78rem;
    color: hsl(var(--muted-foreground));
    font-style: italic;
  }

  .diagram-container {
    margin-bottom: 8px;
  }

  .info-card {
    background: hsl(var(--card));
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    padding: 10px 14px;
  }

  .task-row {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding: 4px 0;
    font-size: 0.84rem;
  }

  .task-id {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.8rem;
    font-weight: 600;
    color: hsl(var(--primary));
  }

  .task-arrow {
    color: hsl(var(--muted-foreground));
  }

  .task-phase {
    color: hsl(var(--foreground));
    font-weight: 500;
  }

  .stale-hint {
    font-size: 0.82rem;
    color: hsl(var(--muted-foreground));
  }

  .unassign-btn {
    background: hsl(var(--card));
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    padding: 6px 12px;
    font-size: 0.8rem;
    color: hsl(var(--foreground));
    cursor: pointer;
    white-space: nowrap;
  }

  .unassign-btn:hover {
    border-color: hsl(var(--primary));
  }

  .unassign-btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .toggle-row {
    display: flex;
    align-items: center;
    gap: 10px;
    cursor: pointer;
  }

  .toggle-checkbox {
    width: 16px;
    height: 16px;
    accent-color: hsl(var(--primary));
    cursor: pointer;
  }

  .toggle-checkbox:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .toggle-label {
    font-size: 0.84rem;
    color: hsl(var(--foreground));
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .toggle-description {
    font-size: 0.78rem;
    color: hsl(var(--muted-foreground));
    margin: 6px 0 0;
    line-height: 1.4;
  }

  .empty-hint {
    font-size: 0.82rem;
    color: hsl(var(--muted-foreground));
    font-style: italic;
    margin: 0;
  }

  .mono {
    font-family: 'SF Mono', Menlo, Consolas, Monaco, 'Courier New', monospace;
    font-size: 0.78rem;
  }

  @media (max-width: 767px) {
    .workflow-content {
      padding: 16px;
    }
  }
</style>
