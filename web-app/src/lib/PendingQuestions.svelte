<script>
  import { pendingQuestions } from './store.js'
  import { sendAnswer } from './api.js'

  let answers = {}

  function handleAnswer(coworkerName) {
    const answer = answers[coworkerName]?.trim()
    if (!answer) return
    sendAnswer(coworkerName, answer)
    delete answers[coworkerName]
    answers = { ...answers }
  }

  function handleKeydown(event, coworkerName) {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault()
      handleAnswer(coworkerName)
    }
  }
</script>

{#if $pendingQuestions.length > 0}
  <div class="pending-questions">
    {#each $pendingQuestions as q (q.id)}
      <div class="question-card">
        <div class="question-header">
          <span class="coworker-name">{q.coworker_name}</span>
          <span class="question-label"> is asking:</span>
        </div>
        <div class="question-text">{q.question}</div>
        <div class="answer-row">
          <input
            class="answer-input"
            type="text"
            placeholder="Type your answer..."
            bind:value={answers[q.coworker_name]}
            onkeydown={(e) => handleKeydown(e, q.coworker_name)}
          />
          <button
            class="answer-btn"
            onclick={() => handleAnswer(q.coworker_name)}
            disabled={!answers[q.coworker_name]?.trim()}
          >
            Answer
          </button>
        </div>
      </div>
    {/each}
  </div>
{/if}

<style>
  .pending-questions {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 12px;
  }

  .question-card {
    background: var(--color-surface-1, #1e2030);
    border: 1px solid var(--color-accent, #7aa2f7);
    border-left: 3px solid var(--color-accent, #7aa2f7);
    border-radius: 6px;
    padding: 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .question-header {
    font-size: 0.85rem;
  }

  .coworker-name {
    font-weight: 600;
    color: var(--color-accent, #7aa2f7);
  }

  .question-label {
    color: var(--color-text-muted, #a9b1d6);
  }

  .question-text {
    font-size: 0.9rem;
    color: var(--color-text, #c0caf5);
    line-height: 1.4;
  }

  .answer-row {
    display: flex;
    gap: 6px;
    margin-top: 4px;
  }

  .answer-input {
    flex: 1;
    background: var(--color-surface-0, #13141e);
    border: 1px solid var(--color-border, #3b4261);
    border-radius: 4px;
    color: var(--color-text, #c0caf5);
    padding: 4px 8px;
    font-size: 0.85rem;
  }

  .answer-input:focus {
    outline: none;
    border-color: var(--color-accent, #7aa2f7);
  }

  .answer-btn {
    background: var(--color-accent, #7aa2f7);
    color: #1a1b26;
    border: none;
    border-radius: 4px;
    padding: 4px 12px;
    font-size: 0.85rem;
    font-weight: 600;
    cursor: pointer;
  }

  .answer-btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .answer-btn:hover:not(:disabled) {
    filter: brightness(1.1);
  }
</style>
