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


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _state_key(task_id: str, field: str) -> str:
    """Build a namespaced state key for a TDW task."""
    return f"tasks.{task_id}.{field}"


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


def _find_active_task_id(context: HookContext) -> str | None:
    """Find the first active TDW task (in a writable stage).

    Returns the task_id or None.
    """
    tasks = context.rpc.get_state("tasks") or {}
    for task_id, task_data in tasks.items():
        if isinstance(task_data, dict) and task_data.get("stage") in (
            "draft",
            "critique",
            "revise",
        ):
            return task_id
    return None


# ---------------------------------------------------------------------------
# Hook implementations
# ---------------------------------------------------------------------------


@hookimpl
def on_task_created(event: object, context: HookContext) -> Sequence[DaemonAction] | None:
    """Initialize TDW state for new writing tasks."""
    task_id = getattr(event, "task_id", None)
    if not task_id:
        return None

    subject = getattr(event, "subject", "untitled")

    context.rpc.set_state(_state_key(task_id, "stage"), "research")
    context.rpc.set_state(_state_key(task_id, "criteria"), list(DEFAULT_CRITERIA))
    context.rpc.set_state(_state_key(task_id, "patterns"), list(DEFAULT_PATTERNS))
    context.rpc.set_state(_state_key(task_id, "revision_count"), 0)

    return [
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
def on_coworker_message(event: object, context: HookContext) -> Sequence[DaemonAction] | None:
    """Detect stage transitions and advance the SDOH cycle."""
    task_id = getattr(event, "task_id", None)
    if not task_id:
        return None

    stage = context.rpc.get_state(_state_key(task_id, "stage"))
    if not stage:
        return None

    message = getattr(event, "message", "")
    msg_lower = message.lower()

    # Study -> Do: research -> outline
    if stage == "research" and "research complete" in msg_lower:
        context.rpc.set_state(_state_key(task_id, "stage"), "outline")
        return [
            Actions.post_to_channel(
                "**Stage 2/5: Outline** (Do)\n"
                "Create the structure. Post 'outline ready' when done."
            ),
        ]

    # Do: outline -> draft
    if stage == "outline" and "outline ready" in msg_lower:
        context.rpc.set_state(_state_key(task_id, "stage"), "draft")
        return [
            Actions.post_to_channel(
                "**Stage 3/5: Draft** (Do)\n"
                "Write the first draft. Don't edit -- just get it down.\n"
                "Post 'draft complete' when finished."
            ),
        ]

    # Do -> Observe: draft -> critique
    if stage == "draft" and "draft complete" in msg_lower:
        criteria = context.rpc.get_state(_state_key(task_id, "criteria")) or DEFAULT_CRITERIA
        patterns = context.rpc.get_state(_state_key(task_id, "patterns")) or DEFAULT_PATTERNS
        context.rpc.set_state(_state_key(task_id, "stage"), "critique")

        criteria_display = "\n".join(f"- [ ] {c}" for c in criteria)
        patterns_display = "\n".join(f"- {p}" for p in patterns)
        return [
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
            context.rpc.set_state(_state_key(task_id, "stage"), "final")
            return [
                Actions.post_to_channel(
                    "**All criteria passed!**\n"
                    "**Stage 5/5: Final Review** (Hone)\n"
                    "Human: Review for voice and creative opportunities.\n"
                    "The system caught what's wrong. You catch what could be better.\n"
                    "When a passage sets a scene, ask: is it using what's available?"
                ),
            ]
        else:
            context.rpc.set_state(_state_key(task_id, "stage"), "revise")
            return [
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
        count = context.rpc.get_state(_state_key(task_id, "revision_count")) or 0
        context.rpc.set_state(_state_key(task_id, "revision_count"), count + 1)
        context.rpc.set_state(_state_key(task_id, "stage"), "critique")

        criteria = context.rpc.get_state(_state_key(task_id, "criteria")) or DEFAULT_CRITERIA
        return [
            Actions.post_to_channel(
                f"**Back to critique** (revision #{count + 1})\n"
                "Running criteria against revised draft..."
            ),
            Actions.spawn_coworker(prompt=_build_critique_prompt(
                criteria,
                context.rpc.get_state(_state_key(task_id, "patterns")) or DEFAULT_PATTERNS,
            )),
        ]

    return None


@hookimpl
def on_channel_message(event: object, context: HookContext) -> Sequence[DaemonAction] | None:
    """Handle the human learning loop -- add criteria/patterns in real-time."""
    message = getattr(event, "message", "")
    msg_lower = message.lower()

    # Detect "add criterion:" or "new rule:" patterns
    if "add criterion:" in msg_lower or "new rule:" in msg_lower:
        match = re.search(
            r"(?:add criterion|new rule):\s*(.+)", message, re.IGNORECASE
        )
        if match:
            new_criterion = match.group(1).strip()
            task_id = _find_active_task_id(context)
            if task_id:
                criteria = context.rpc.get_state(_state_key(task_id, "criteria")) or []
                criteria.append(new_criterion)
                context.rpc.set_state(_state_key(task_id, "criteria"), criteria)
                return [
                    Actions.post_to_channel(
                        f'Added criterion: "{new_criterion}"\n'
                        "Every human edit teaches the system. "
                        "This will be checked in future critiques."
                    ),
                ]

    # Detect "add pattern:" for guidance (not blocking)
    elif "add pattern:" in msg_lower:
        match = re.search(r"add pattern:\s*(.+)", message, re.IGNORECASE)
        if match:
            new_pattern = match.group(1).strip()
            task_id = _find_active_task_id(context)
            if task_id:
                patterns = context.rpc.get_state(_state_key(task_id, "patterns")) or []
                patterns.append(new_pattern)
                context.rpc.set_state(_state_key(task_id, "patterns"), patterns)
                return [
                    Actions.post_to_channel(
                        f'Added pattern: "{new_pattern}"\n'
                        "This is guidance, not a requirement. "
                        "It helps without blocking."
                    ),
                ]

    return None
