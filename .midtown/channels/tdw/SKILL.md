---
name: tdw
description: Test-Driven Writing. Use for writing tasks with criteria-based iteration.
compatibility: Midtown daemon + Claude Code channel-lead
metadata:
  midtown_hooks: scripts/hooks.py
  midtown_order: 50
---

# Test-Driven Writing (TDW)

TDW applies TDD principles to writing. Criteria act as tests. AI drafts. Critique checks. Human reviews critiques, not drafts. Edits become new criteria, creating a learning loop.

## The SDOH Cycle

| Command | Phase | What happens |
|---------|-------|--------------|
| `/study` | Study | Research, gather sources, propose outline |
| `/do` | Do | Write or revise the draft |
| `/observe` | Observe | Critique draft against criteria |
| `/hone` | Hone | Improve criteria/patterns from session |

## Stage Progression

```
research -> outline -> draft -> critique ----> revise ----> critique (loop)
                                   |                           |
                                   +-- all pass --> final      +-- all pass --> final
```

## Criteria vs Patterns

- **Criteria** = pass/fail tests. "Lead appears in first two paragraphs." Either it does or it doesn't.
- **Patterns** = guidance. "Short sentences for emphasis." Helps but doesn't block.

## When to use

- User says `/study`, `/do`, `/observe`, `/hone`
- Task is a writing task (blog, essay, documentation)
- User mentions "criteria" or "test-driven writing"

## State Tracking

The daemon tracks per-task:
- `tasks.<id>.stage` — Current stage (research, outline, draft, critique, revise, final)
- `tasks.<id>.criteria` — List of pass/fail criteria
- `tasks.<id>.patterns` — List of guidance patterns
- `tasks.<id>.revision_count` — Number of critique-revise cycles

## Human Learning Loop

Humans can add criteria and patterns in real-time during any active stage:

- `add criterion: <text>` or `new rule: <text>` — add a pass/fail assertion
- `add pattern: <text>` — add non-blocking guidance

Every human edit teaches the system. New criteria are checked in all future critiques.

## Default Criteria

- Lead appears in first two paragraphs
- No AI-isms (delve, it's important to note, in summary)
- Claims grounded in specifics (numbers, names, examples)
- No throat-clearing (basically, actually, just)
- 'So what' is clear by the end
- No wasted scenes — physical settings use what's available

## Default Patterns

- Short sentences for emphasis
- Contrast for punch
- Practitioner voice over academic voice
- Specific > abstract
- Mundane setting amplifies profound content
