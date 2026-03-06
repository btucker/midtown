# /// script
# requires-python = ">=3.9"
# dependencies = [
#   "midtown-sdk",
# ]
# ///
"""TDW (Test-Driven Writing) workflow — SDOH cycle with criteria-based iteration.

Implements a writing workflow where drafts are checked against pass/fail
criteria (like tests), and human reviewers evaluate critiques rather than
reading drafts directly.  Edits become new criteria, creating a learning loop.

Stage progression::

    research → outline → draft → critique ──► revise ──► critique (loop)
                                      │                        │
                                      └── all pass ──► final   └── all pass ──► final

The SDOH (Study → Do → Observe → Hone) cycle maps to:

- **Study**: research stage — gather sources, quotes, key points
- **Do**: outline + draft + revise stages — produce the writing
- **Observe**: critique stage — check draft against criteria
- **Hone**: final stage — human reviews for voice and creative opportunities

Channel commands (human learning loop):

- ``add criterion: <text>`` or ``new rule: <text>`` — add a pass/fail assertion
- ``add pattern: <text>`` — add non-blocking guidance

Usage::

    # As a channel-specific workflow:
    cp tdw_workflow.py .midtown/channels/tdw/workflow.py
"""

from __future__ import annotations

import re

from midtown import MidtownRPC, run

# ── Criteria (pass/fail assertions) ────────────────────────────────
# These are tests.  Either the draft passes or it doesn't.
# Keep minimal — too many rules box in the prose.

DEFAULT_CRITERIA = [
    "Lead appears in first two paragraphs",
    "No AI-isms (delve, it's important to note, in summary)",
    "Claims grounded in specifics (numbers, names, examples)",
    "No throat-clearing (basically, actually, just)",
    "'So what' is clear by the end",
    "No wasted scenes — physical settings use what's available",
]

# ── Patterns (guidance, not blocking) ──────────────────────────────
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
# State helpers
# ---------------------------------------------------------------------------


def _get_task(state: dict, task_id: str) -> dict | None:
    """Return the TDW task data dict, or None if unknown."""
    return state.get("tdw_tasks", {}).get(task_id)


def _get_or_create_task(state: dict, task_id: str) -> dict:
    """Return the TDW task data dict, creating it if absent."""
    return state.setdefault("tdw_tasks", {}).setdefault(task_id, {})


def _init_task(state: dict, task_id: str) -> dict:
    """Initialise TDW state for a new writing task."""
    task = _get_or_create_task(state, task_id)
    task["stage"] = "research"
    task["criteria"] = list(DEFAULT_CRITERIA)
    task["patterns"] = list(DEFAULT_PATTERNS)
    task["revision_count"] = 0
    return task


def _find_active_task(state: dict) -> tuple[str | None, dict]:
    """Find the first active TDW task (in a writable stage)."""
    for task_id, task_data in state.get("tdw_tasks", {}).items():
        if task_data.get("stage") in ("draft", "critique", "revise"):
            return task_id, task_data
    return None, {}


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
        "Phase 1: Read as a suspicious reader — flag feel issues.\n"
        "Phase 2: Check each criterion systematically.\n"
        "Phase 3: Read as a reader — honest reaction, no checklist.\n"
        "\n"
        "Post your critique to the channel. Be specific. Quote exact phrases.\n"
        "End with: CRITIQUE COMPLETE - [N] criteria failed"
    )


# ---------------------------------------------------------------------------
# Main handler
# ---------------------------------------------------------------------------


def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:  # noqa: C901
    """Route TDW events to stage transitions and side effects."""
    event_type: str = event["type"]
    task_id: str | None = event.get("task_id")
    message: str = event.get("message", "")
    msg_lower = message.lower()

    # ------------------------------------------------------------------
    # task.created — initialise TDW state
    # ------------------------------------------------------------------
    if event_type == "task.created" and task_id:
        _init_task(state, task_id)
        subject = event.get("subject", "untitled")
        rpc.post_to_channel(
            f"Writing task: {subject}\n"
            "\n"
            "**Stage 1/5: Research** (Study)\n"
            "Gather sources, quotes, and key points.\n"
            "\n"
            "Post 'research complete' when ready to outline."
        )

    # ------------------------------------------------------------------
    # coworker.message — detect stage transitions
    # ------------------------------------------------------------------
    elif event_type == "coworker.message" and task_id:
        task = _get_task(state, task_id)
        if task is None:
            return
        stage = task.get("stage")

        if not stage:
            return

        # Study → Do: research → outline
        if stage == "research" and "research complete" in msg_lower:
            task["stage"] = "outline"
            rpc.post_to_channel(
                "**Stage 2/5: Outline** (Do)\n"
                "Create the structure. Post 'outline ready' when done."
            )

        # Do: outline → draft
        elif stage == "outline" and "outline ready" in msg_lower:
            task["stage"] = "draft"
            rpc.post_to_channel(
                "**Stage 3/5: Draft** (Do)\n"
                "Write the first draft. Don't edit — just get it down.\n"
                "Post 'draft complete' when finished."
            )

        # Do → Observe: draft → critique
        elif stage == "draft" and "draft complete" in msg_lower:
            criteria = task.get("criteria", DEFAULT_CRITERIA)
            patterns = task.get("patterns", DEFAULT_PATTERNS)
            task["stage"] = "critique"

            criteria_display = "\n".join(f"- [ ] {c}" for c in criteria)
            patterns_display = "\n".join(f"- {p}" for p in patterns)
            rpc.post_to_channel(
                "**Stage 4/5: Critique** (Observe)\n"
                f"Running {len(criteria)} criteria against the draft...\n"
                "\n"
                f"**Criteria** (must pass):\n{criteria_display}\n"
                "\n"
                f"**Patterns** (guidance):\n{patterns_display}"
            )
            rpc.spawn_coworker(prompt=_build_critique_prompt(criteria, patterns))

        # Observe → Hone or Revise: critique results
        elif stage == "critique" and "critique complete" in msg_lower:
            match = re.search(r"(\d+) criteria failed", msg_lower)
            if not match:
                rpc.post_to_channel(
                    "**Unable to parse critique result.**\n"
                    "Expected format: 'CRITIQUE COMPLETE - N criteria failed'\n"
                    "Staying in critique stage. Please re-run the critique."
                )
                return
            failures = int(match.group(1))

            if failures == 0:
                task["stage"] = "final"
                rpc.post_to_channel(
                    "**All criteria passed!**\n"
                    "**Stage 5/5: Final Review** (Hone)\n"
                    "Human: Review for voice and creative opportunities.\n"
                    "The system caught what's wrong. You catch what could be better.\n"
                    "When a passage sets a scene, ask: is it using what's available?"
                )
            else:
                task["stage"] = "revise"
                rpc.post_to_channel(
                    f"**{failures} criteria failed.**\n"
                    "**Stage: Revise** (Do)\n"
                    "Human: Review the critique. Are these valid criticisms?\n"
                    "\n"
                    "When ready, post 'revision complete' for re-critique."
                )

        # Do → Observe loop: revise → re-critique
        elif stage == "revise" and "revision complete" in msg_lower:
            count = task.get("revision_count", 0)
            task["revision_count"] = count + 1
            task["stage"] = "critique"

            criteria = task.get("criteria", DEFAULT_CRITERIA)
            patterns = task.get("patterns", DEFAULT_PATTERNS)

            rpc.post_to_channel(
                f"**Back to critique** (revision #{count + 1})\n"
                "Running criteria against revised draft..."
            )
            rpc.spawn_coworker(prompt=_build_critique_prompt(criteria, patterns))

    # ------------------------------------------------------------------
    # channel.message — human learning loop (add criteria/patterns)
    # ------------------------------------------------------------------
    elif event_type == "channel.message":
        _handle_channel_message(msg_lower, message, state, rpc)


def _handle_channel_message(
    msg_lower: str, message: str, state: dict, rpc: MidtownRPC
) -> None:
    """Process channel messages for criterion/pattern additions."""
    # Detect "add criterion:" or "new rule:" patterns
    if "add criterion:" in msg_lower or "new rule:" in msg_lower:
        match = re.search(
            r"(?:add criterion|new rule):\s*(.+)", message, re.IGNORECASE
        )
        if match:
            new_criterion = match.group(1).strip()
            task_id, task_data = _find_active_task(state)
            if task_id:
                task_data.setdefault("criteria", []).append(new_criterion)
                rpc.post_to_channel(
                    f'Added criterion: "{new_criterion}"\n'
                    "Every human edit teaches the system. "
                    "This will be checked in future critiques."
                )

    # Detect "add pattern:" for guidance (not blocking)
    elif "add pattern:" in msg_lower:
        match = re.search(r"add pattern:\s*(.+)", message, re.IGNORECASE)
        if match:
            new_pattern = match.group(1).strip()
            task_id, task_data = _find_active_task(state)
            if task_id:
                task_data.setdefault("patterns", []).append(new_pattern)
                rpc.post_to_channel(
                    f'Added pattern: "{new_pattern}"\n'
                    "This is guidance, not a requirement. "
                    "It helps without blocking."
                )


if __name__ == "__main__":
    run(handle)
