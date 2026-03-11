<script>
/**
 * TodoBlock — renders a TodoWrite tool call as a checkbox list.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 *           input.todos — array of { content, status }
 *   timestamp — ISO 8601 timestamp of the parent message (for auto-collapse)
 */
import { createAutoCollapse } from "./useAutoCollapse.js";

let { block, timestamp = null } = $props();

let todos = $derived(block.input?.todos || []);
let doneCount = $derived(todos.filter((t) => t.status === "completed").length);
let totalCount = $derived(todos.length);
let summaryText = $derived(`Todos (${doneCount}/${totalCount} done)`);

let displayState = $state("collapsed");
let userOverride = $state(false);

const ac = $derived.by(() => createAutoCollapse(timestamp));

$effect.pre(() => {
	if (!userOverride) displayState = ac.initial;
});

$effect(() => {
	if (userOverride) return;
	const currentAc = ac;
	currentAc.startTimer(() => {
		displayState = "collapsed";
	});
	return () => currentAc.clearTimer();
});

function toggle() {
	userOverride = true;
	ac.clearTimer();
	displayState = displayState === "expanded" ? "collapsed" : "expanded";
}
</script>

{#if todos.length > 0}
  <div class="todo-block">
    <button class="todo-header" onclick={toggle} aria-expanded={displayState !== 'collapsed'}>
      <span class="todo-chevron">{displayState === 'collapsed' ? '▸' : '▾'}</span>
      <span>{displayState === 'collapsed' ? summaryText : 'Todos'}</span>
    </button>
    {#if displayState !== 'collapsed'}
      <ul class="todo-list">
        {#each todos as todo}
          <li class="todo-item" class:todo-done={todo.status === 'completed'}>
            <span class="todo-check">{todo.status === 'completed' ? '☑' : todo.status === 'in_progress' ? '▶' : '☐'}</span>
            <span class="todo-text">{todo.content}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </div>
{/if}

<style>
  .todo-block {
    margin: 6px 0;
    border: 1px solid hsl(var(--border));
    border-radius: 6px;
    overflow: hidden;
    font-size: 0.82rem;
    line-height: 1.5;
  }

  .todo-header {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 4px 10px;
    background: hsl(var(--accent));
    border: none;
    cursor: pointer;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    font-weight: 600;
    color: hsl(var(--muted-foreground));
    text-align: left;
  }

  .todo-header:hover {
    background: hsl(var(--accent) / 0.8);
  }

  .todo-chevron {
    flex-shrink: 0;
    width: 1em;
    font-size: 0.7rem;
  }

  .todo-list {
    list-style: none;
    margin: 0;
    padding: 4px 10px;
  }

  .todo-item {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 1px 0;
    color: hsl(var(--foreground));
  }

  .todo-done {
    color: hsl(var(--muted-foreground));
  }

  .todo-done .todo-text {
    text-decoration: line-through;
    opacity: 0.7;
  }

  .todo-check {
    flex-shrink: 0;
    font-size: 0.85em;
  }

  .todo-text {
    min-width: 0;
  }
</style>
