# /// script
# requires-python = ">=3.9"
# dependencies = [
#   "midtown-sdk",
# ]
# ///
"""TDW (Test-Driven Writing) workflow hooks — SDOH cycle with criteria-based iteration.

Implements a writing workflow where drafts are checked against pass/fail
criteria (like tests), and human reviewers evaluate critiques rather than
reading drafts directly.  Edits become new criteria, creating a learning loop.

Stage progression::

    research -> outline -> draft -> critique --> revise --> critique (loop)
                                      |                        |
                                      +-- all pass --> final   +-- all pass --> final

The SDOH (Study -> Do -> Observe -> Hone) cycle maps to:

- **Study**: research stage -- gather sources, quotes, key points
- **Do**: outline + draft + revise stages -- produce the writing
- **Observe**: critique stage -- check draft against criteria
- **Hone**: final stage -- human reviews for voice and creative opportunities

Channel commands (human learning loop):

- ``add criterion: <text>`` or ``new rule: <text>`` -- add a pass/fail assertion
- ``add pattern: <text>`` -- add non-blocking guidance
"""

from __future__ import annotations

import re
from typing import Sequence

from midtown.actions import Actions
from midtown.hooks import DaemonAction, HookContext, hookimpl

# -- Criteria (pass/fail assertions) ----------------------------------------
# These are tests.  Either the draft passes or it doesn't.
# Keep minimal -- too many rules box in the prose.

DEFAULT_CRITERIA = [
    "Lead appears in first two paragraphs",
    "No AI-isms (delve, it's important to note, in summary)",
    "Claims grounded in specifics (numbers, names, examples)",
    "No throat-clearing (basically, actually, just)",
    "'So what' is clear by the end",
    "No wasted scenes -- physical settings use what's available",
]

# -- Patterns (guidance, not blocking) --------------------------------------
# Techniques that help but don't block publication.

DEFAULT_PATTERNS = [
    "Short sentences for emphasis",
    "Contrast for punch",
    "Practitioner voice over academic voice",
    "Specific > abstract",
    "Mundane setting amplifies profound content",
]

STAGES = ["research", "outline", "draft", "critique", "revise", "final"]
ACTIVE_STAGES = ("draft", "critique", "revise")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _build_critique_prompt(criteria: list[str], patterns: list[str]) -> str:
    """Build the prompt for the critique agent."""
    criteria_list = "\n".join(f"- {c}" for c in criteria)
    patterns_list = "\n".join(f"- {p}" for p in patterns)
    return (
        "You are a writing critic using the TDW methodology.\n"
        "\n"
        "Read the draft and check each criterion. For each:\n"
        "1. State whether it PASSES or FAILS\n"
        "2. Quote the specific text (if failing)\n"
        "3. Suggest a fix (if failing)\n"
        "\n"
        f"CRITERIA (pass/fail):\n{criteria_list}\n"
        "\n"
        f"PATTERNS (guidance to consider):\n{patterns_list}\n"
        "\n"
        "Phase 1: Read as a suspicious reader -- flag feel issues.\n"
        "Phase 2: Check each criterion systematically.\n"
        "Phase 3: Read as a reader -- honest reaction, no checklist.\n"
        "\n"
        "Post your critique to the channel. Be specific. Quote exact phrases.\n"
        "End with: CRITIQUE COMPLETE - [N] criteria failed"
    )


def _find_active_task_id(ctx: HookContext) -> str | None:
    """Find the sole active TDW task (in a writable stage).

    Returns the task_id when exactly one task is active, or ``None`` when
    there are zero or multiple active tasks.  Callers that need to resolve
    ambiguity should use ``ctx.task_id`` from the event context instead.
    """
    tasks = ctx.state.get("tasks", {})
    active: list[str] = [
        tid
        for tid, td in tasks.items()
        if isinstance(td, dict) and td.get("stage") in ACTIVE_STAGES
    ]
    if len(active) == 1:
        return active[0]
    return None


def _resolve_target_task(
    ctx: HookContext,
) -> tuple[str | None, dict]:
    """Resolve the target task for a channel command.

    When ``ctx.task_id`` is provided (e.g. the message was posted with
    ``--task``), return that task directly — but only if it's in an active
    stage.  Otherwise fall back to ``_find_active_task_id`` which requires
    exactly one active task to avoid ambiguity.
    """
    tasks = ctx.state.get("tasks", {})
    if ctx.task_id:
        task_data = tasks.get(ctx.task_id, {})
        if isinstance(task_data, dict) and task_data.get("stage") in ACTIVE_STAGES:
            return ctx.task_id, task_data
        return None, {}
    tid = _find_active_task_id(ctx)
    if tid:
        return tid, tasks.get(tid, {})
    return None, {}


def _warn_no_target(ctx: HookContext) -> list[DaemonAction]:
    """Return a user-facing warning when no target task could be resolved."""
    tasks = ctx.state.get("tasks", {})
    active = [
        tid
        for tid, td in tasks.items()
        if isinstance(td, dict) and td.get("stage") in ACTIVE_STAGES
    ]
    if ctx.task_id:
        return [
            Actions.post_to_channel(
                f"Task {ctx.task_id} is not in an active stage "
                "(draft/critique/revise). Criterion or pattern not added."
            )
        ]
    elif len(active) > 1:
        return [
            Actions.post_to_channel(
                f"Multiple active tasks ({', '.join(active)}). "
                "Use --task <id> to specify which task to update."
            )
        ]
    return []


# ---------------------------------------------------------------------------
# Hook implementations
# ---------------------------------------------------------------------------


@hookimpl
def on_task_created(ctx: HookContext) -> Sequence[DaemonAction] | None:
    """Initialize TDW state for new writing tasks."""
    task_id = ctx.task_id
    if not task_id:
        return None

    subject = ctx.event.get("subject", "untitled")

    return [
        Actions.set_state(f"tasks.{task_id}.stage", "research"),
        Actions.set_state(f"tasks.{task_id}.criteria", list(DEFAULT_CRITERIA)),
        Actions.set_state(f"tasks.{task_id}.patterns", list(DEFAULT_PATTERNS)),
        Actions.set_state(f"tasks.{task_id}.revision_count", 0),
        Actions.post_to_channel(
            f"Writing task: {subject}\n"
            "\n"
            "**Stage 1/5: Research** (Study)\n"
            "Gather sources, quotes, and key points.\n"
            "\n"
            "Post 'research complete' when ready to outline."
        ),
    ]


@hookimpl
def on_coworker_message(ctx: HookContext) -> Sequence[DaemonAction] | None:
    """Detect stage transitions and advance the SDOH cycle."""
    task_id = ctx.task_id
    if not task_id:
        return None

    tasks = ctx.state.get("tasks", {})
    task_data = tasks.get(task_id, {})
    stage = task_data.get("stage") if isinstance(task_data, dict) else None
    if not stage:
        return None

    message = ctx.event.get("message", "")
    msg_lower = message.lower()

    # Study -> Do: research -> outline
    if stage == "research" and "research complete" in msg_lower:
        return [
            Actions.set_state(f"tasks.{task_id}.stage", "outline"),
            Actions.post_to_channel(
                "**Stage 2/5: Outline** (Do)\n"
                "Create the structure. Post 'outline ready' when done."
            ),
        ]

    # Do: outline -> draft
    if stage == "outline" and "outline ready" in msg_lower:
        return [
            Actions.set_state(f"tasks.{task_id}.stage", "draft"),
            Actions.post_to_channel(
                "**Stage 3/5: Draft** (Do)\n"
                "Write the first draft. Don't edit -- just get it down.\n"
                "Post 'draft complete' when finished."
            ),
        ]

    # Do -> Observe: draft -> critique
    if stage == "draft" and "draft complete" in msg_lower:
        criteria = task_data.get("criteria") or DEFAULT_CRITERIA
        patterns = task_data.get("patterns") or DEFAULT_PATTERNS

        criteria_display = "\n".join(f"- [ ] {c}" for c in criteria)
        patterns_display = "\n".join(f"- {p}" for p in patterns)
        return [
            Actions.set_state(f"tasks.{task_id}.stage", "critique"),
            Actions.post_to_channel(
                f"**Stage 4/5: Critique** (Observe)\n"
                f"Running {len(criteria)} criteria against the draft...\n"
                f"\n"
                f"**Criteria** (must pass):\n{criteria_display}\n"
                f"\n"
                f"**Patterns** (guidance):\n{patterns_display}"
            ),
            Actions.spawn_coworker(prompt=_build_critique_prompt(criteria, patterns)),
        ]

    # Observe -> Hone or Revise: critique results
    if stage == "critique" and "critique complete" in msg_lower:
        match = re.search(r"(\d+) criteria failed", msg_lower)
        if not match:
            return [
                Actions.post_to_channel(
                    "**Unable to parse critique result.**\n"
                    "Expected format: 'CRITIQUE COMPLETE - N criteria failed'\n"
                    "Staying in critique stage. Please re-run the critique."
                ),
            ]

        failures = int(match.group(1))

        if failures == 0:
            return [
                Actions.set_state(f"tasks.{task_id}.stage", "final"),
                Actions.post_to_channel(
                    "**All criteria passed!**\n"
                    "**Stage 5/5: Final Review** (Hone)\n"
                    "Human: Review for voice and creative opportunities.\n"
                    "The system caught what's wrong. You catch what could be better.\n"
                    "When a passage sets a scene, ask: is it using what's available?"
                ),
            ]
        else:
            return [
                Actions.set_state(f"tasks.{task_id}.stage", "revise"),
                Actions.post_to_channel(
                    f"**{failures} criteria failed.**\n"
                    "**Stage: Revise** (Do)\n"
                    "Human: Review the critique. Are these valid criticisms?\n"
                    "\n"
                    "When ready, post 'revision complete' for re-critique."
                ),
            ]

    # Do -> Observe loop: revise -> re-critique
    if stage == "revise" and "revision complete" in msg_lower:
        count = task_data.get("revision_count") or 0

        criteria = task_data.get("criteria") or DEFAULT_CRITERIA
        return [
            Actions.set_state(f"tasks.{task_id}.revision_count", count + 1),
            Actions.set_state(f"tasks.{task_id}.stage", "critique"),
            Actions.post_to_channel(
                f"**Back to critique** (revision #{count + 1})\n"
                "Running criteria against revised draft..."
            ),
            Actions.spawn_coworker(prompt=_build_critique_prompt(
                criteria,
                task_data.get("patterns") or DEFAULT_PATTERNS,
            )),
        ]

    return None


@hookimpl
def on_channel_message(ctx: HookContext) -> Sequence[DaemonAction] | None:
    """Handle the human learning loop -- add criteria/patterns in real-time."""
    message = ctx.event.get("message", "")
    msg_lower = message.lower()

    # Detect "add criterion:" or "new rule:" patterns
    if "add criterion:" in msg_lower or "new rule:" in msg_lower:
        match = re.search(
            r"(?:add criterion|new rule):\s*(.+)", message, re.IGNORECASE
        )
        if match:
            new_criterion = match.group(1).strip()
            tid, task_data = _resolve_target_task(ctx)
            if tid:
                criteria = list(task_data.get("criteria") or [])
                criteria.append(new_criterion)
                return [
                    Actions.set_state(f"tasks.{tid}.criteria", criteria),
                    Actions.post_to_channel(
                        f'Added criterion: "{new_criterion}"\n'
                        "Every human edit teaches the system. "
                        "This will be checked in future critiques."
                    ),
                ]
            else:
                return _warn_no_target(ctx) or None

    # Detect "add pattern:" for guidance (not blocking)
    elif "add pattern:" in msg_lower:
        match = re.search(r"add pattern:\s*(.+)", message, re.IGNORECASE)
        if match:
            new_pattern = match.group(1).strip()
            tid, task_data = _resolve_target_task(ctx)
            if tid:
                patterns = list(task_data.get("patterns") or [])
                patterns.append(new_pattern)
                return [
                    Actions.set_state(f"tasks.{tid}.patterns", patterns),
                    Actions.post_to_channel(
                        f'Added pattern: "{new_pattern}"\n'
                        "This is guidance, not a requirement. "
                        "It helps without blocking."
                    ),
                ]
            else:
                return _warn_no_target(ctx) or None

    return None
