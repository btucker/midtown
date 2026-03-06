<script>
/**
 * TodoBlock — renders a TodoWrite tool call as a checkbox list.
 *
 * Props:
 *   block — ToolBlock { tool_name, input, output, error }
 *           input.todos — array of { content, status }
 */
let { block } = $props();

let todos = $derived(block.input?.todos || []);
</script>

{#if todos.length > 0}
  <div class="todo-block">
    <div class="todo-header">Todos</div>
    <ul class="todo-list">
      {#each todos as todo}
        <li class="todo-item" class:todo-done={todo.status === 'completed'}>
          <span class="todo-check">{todo.status === 'completed' ? '☑' : todo.status === 'in_progress' ? '▶' : '☐'}</span>
          <span class="todo-text">{todo.content}</span>
        </li>
      {/each}
    </ul>
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
    padding: 4px 10px;
    background: hsl(var(--accent));
    font-family: var(--font-mono);
    font-size: 0.75rem;
    font-weight: 600;
    color: hsl(var(--muted-foreground));
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
