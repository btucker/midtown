# /// script
# requires-python = ">=3.9"
# dependencies = [
#   "transitions>=0.9",
#   "midtown-sdk",
# ]
# ///
"""Default PR workflow — replicates the compiled-in Midtown PR lifecycle.

This is the reference ``workflow.py`` that ships with the Midtown SDK.  Copy it
into your project (or a channel subdirectory) and customise to taste:

.. code-block:: bash

    cp $(python -c "import midtown; import os; print(os.path.dirname(midtown.__file__))")/default_workflow.py .midtown/workflow.py

State machine
-------------

Each task moves through five states driven by daemon events:

    pending ──task.assigned──► in_progress ──pr.opened──► in_review
                                    ▲                           │
                                    │                    pr.changes_requested
                              pr.changes_requested              │
                                    │                           ▼
                                    └──────────────────── approved
                                                               │
                                             pr.approved / pr.merged
                                                               ▼
                                                            merged

Side effects
------------

* ``pr.opened``          — post to channel that review is needed; spawn a reviewer
* ``pr.approved``        — nudge author: PR approved, please merge
* ``pr.changes_requested``— nudge author: please address review feedback
* ``pr.ci_failed``       — nudge author: CI failed, please investigate
* ``pr.conflict``        — nudge author: merge conflict, please rebase
* ``pr.ci_passed``       — nudge author if in ``in_review`` or ``approved``: CI green, please merge
* ``pr.merged``          — complete the associated task
* ``coworker.stuck``     — post a warning to the channel

Events without registered transitions (``task.created``, ``channel.message``,
``coworker.message``, ``timer.tick``, etc.) are silently ignored because the
machine is created with ``ignore_invalid_triggers=True``.
"""

from __future__ import annotations

from transitions import Machine

from midtown import MidtownRPC, run

# ---------------------------------------------------------------------------
# State machine definition
# ---------------------------------------------------------------------------

STATES = ["pending", "in_progress", "in_review", "approved", "merged"]

#: Each entry maps a WorkflowEvent type to a state transition.
#: Trigger names are event types with ``"."`` replaced by ``"_"``.
TRANSITIONS = [
    # Task lifecycle
    {"trigger": "task_assigned", "source": "pending", "dest": "in_progress"},
    # PR opened: coworker pushed their branch and opened a PR
    {"trigger": "pr_opened", "source": "in_progress", "dest": "in_review"},
    # Review outcomes
    {"trigger": "pr_approved", "source": "in_review", "dest": "approved"},
    {
        "trigger": "pr_changes_requested",
        "source": ["in_review", "approved"],
        "dest": "in_progress",
    },
    # Merged (from either state — reviewer may merge directly)
    {
        "trigger": "pr_merged",
        "source": ["in_progress", "in_review", "approved"],
        "dest": "merged",
    },
    # Task explicitly completed (e.g. non-PR tasks)
    {"trigger": "task_completed", "source": "*", "dest": "merged"},
]


class _TaskWorkflow:
    """Thin wrapper so ``transitions.Machine`` has a model object to operate on.

    ``transitions.Machine`` dynamically injects a ``state`` attribute and one
    method per trigger onto the model instance at construction time.
    """

    # Declared here for static-analysis tools; populated at runtime by Machine.
    state: str

    def __init__(self, initial: str) -> None:
        self.machine = Machine(
            model=self,
            states=STATES,
            transitions=TRANSITIONS,
            initial=initial,
            # Silently ignore triggers that have no transition from the current
            # state (e.g. ``pr_opened`` when already ``in_review``).
            ignore_invalid_triggers=True,
        )


# ---------------------------------------------------------------------------
# State persistence helpers
# ---------------------------------------------------------------------------


def _load_task(state: dict, task_id: str) -> _TaskWorkflow:
    """Restore a task's workflow from ``state``, defaulting to ``"pending"``."""
    task_data = state.setdefault("tasks", {}).get(task_id, {})
    return _TaskWorkflow(initial=task_data.get("state", "pending"))


def _save_task(state: dict, task_id: str, wf: _TaskWorkflow, **extra: object) -> None:
    """Persist the workflow's current state (and any extra metadata) into ``state``."""
    task_data = state.setdefault("tasks", {}).setdefault(task_id, {})
    task_data["state"] = wf.state
    task_data.update(extra)


def _get_task_data(state: dict, task_id: str) -> dict:
    return state.get("tasks", {}).get(task_id, {})


# ---------------------------------------------------------------------------
# Main handler
# ---------------------------------------------------------------------------


def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:  # noqa: C901
    """Route a WorkflowEvent to the appropriate state transition and side effects."""
    event_type: str = event["type"]
    trigger: str = event_type.replace(".", "_")
    task_id: str | None = event.get("task_id")
    coworker: str = event.get("coworker", "")
    pr_number: int | None = event.get("pr_number")

    # ------------------------------------------------------------------
    # State transition (task-scoped events only)
    # ------------------------------------------------------------------
    if task_id:
        wf = _load_task(state, task_id)
        prev_state = wf.state

        # transitions.Machine attaches one method per defined trigger directly
        # onto the model.  Events that have no registered transition (e.g.
        # ``pr_ci_failed``) won't have a corresponding method, so guard with
        # ``hasattr`` before calling.  ``ignore_invalid_triggers=True`` already
        # suppresses errors for valid trigger names that have no transition *from
        # the current state*, but cannot help with undefined trigger names.
        if hasattr(wf, trigger):
            getattr(wf, trigger)()

        # Persist state; also capture author when a PR is opened.
        extra: dict = {}
        if event_type == "pr.opened" and coworker:
            extra["pr_author"] = coworker
        elif event_type == "task.assigned" and coworker:
            extra["coworker"] = coworker

        _save_task(state, task_id, wf, **extra)
        new_state = wf.state
    else:
        prev_state = new_state = None

    # ------------------------------------------------------------------
    # Side effects
    # ------------------------------------------------------------------

    if event_type == "pr.opened" and pr_number:
        # Post to channel so the team knows a review is needed, then spawn a
        # reviewer coworker.  The spawned coworker will receive the review task
        # via normal task dispatch once it connects to the daemon.
        rpc.post_to_channel(
            f"PR #{pr_number} opened by {coworker} — assigning reviewer"
        )
        rpc.spawn_coworker(
            prompt=(
                f"Please review PR #{pr_number} opened by {coworker}. "
                "Use the `code-review` skill to analyze it, then post your "
                "review as a GitHub comment on the PR."
            )
        )

    elif event_type == "pr.approved" and pr_number:
        # The reviewer approved.  Nudge the PR author so they can decide to merge.
        author = _get_task_data(state, task_id or "").get("pr_author", coworker)
        if author:
            rpc.nudge_coworker(
                author,
                f"PR #{pr_number} is approved — please address any remaining "
                "feedback and merge when ready",
            )

    elif event_type == "pr.changes_requested" and pr_number:
        author = _get_task_data(state, task_id or "").get("pr_author", coworker)
        if author:
            rpc.nudge_coworker(
                author,
                f"PR #{pr_number}: changes requested — please address review feedback",
            )

    elif event_type == "pr.ci_failed" and pr_number:
        author = _get_task_data(state, task_id or "").get("pr_author", coworker)
        check_name: str | None = event.get("check_name")
        detail = f" ({check_name})" if check_name else ""
        if author:
            rpc.nudge_coworker(
                author,
                f"PR #{pr_number}: CI failed{detail} — please investigate",
            )

    elif event_type == "pr.conflict" and pr_number:
        author = _get_task_data(state, task_id or "").get("pr_author", coworker)
        if author:
            rpc.nudge_coworker(
                author,
                f"PR #{pr_number} has a merge conflict — please rebase",
            )

    elif event_type == "pr.ci_passed" and pr_number:
        # CI just went green.  If the PR is already reviewed (in_review) or
        # approved, nudge the author to merge.
        if new_state in ("in_review", "approved"):
            author = _get_task_data(state, task_id or "").get("pr_author", coworker)
            if author:
                rpc.nudge_coworker(
                    author,
                    f"PR #{pr_number}: CI is green — please address any review "
                    "feedback and merge when ready",
                )

    elif event_type == "pr.merged" and task_id:
        # Mark the task done so downstream blocked tasks become unblocked.
        rpc.complete_task(task_id)

    elif event_type == "coworker.stuck":
        rpc.post_to_channel(
            f"⚠️ {coworker} appears stuck"
            + (f" on task !{task_id}" if task_id else "")
            + " — may need intervention"
        )

    # timer.tick and other events are deliberately handled by state transitions
    # only; no additional side effects needed in the reference implementation.

    _ = prev_state  # available for custom extensions


if __name__ == "__main__":
    run(handle)
