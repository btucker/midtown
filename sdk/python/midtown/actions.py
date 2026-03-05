"""Factory methods for creating :class:`~midtown.hooks.DaemonAction` objects.

Plugins use the :class:`Actions` helper (available as ``ctx.actions`` in hook
implementations) to build return values without constructing raw
``DaemonAction`` dicts::

    @hookimpl
    def on_pr_opened(self, ctx: HookContext) -> list[DaemonAction]:
        return [
            ctx.actions.post_to_channel(f"PR #{ctx.pr_number} opened!"),
            ctx.actions.spawn_reviewer(ctx.pr_number),
        ]

Each method mirrors an :class:`~midtown.MidtownRPC` method but returns a
``DaemonAction`` instead of performing I/O.
"""

from __future__ import annotations

from typing import Any

from midtown.hooks import DaemonAction


class Actions:
    """Factory for building :class:`DaemonAction` objects.

    Methods mirror :class:`~midtown.MidtownRPC` for a familiar API.
    """

    # -- Channel ------------------------------------------------------------

    @staticmethod
    def post_to_channel(
        message: str,
        *,
        channel: str | None = None,
        sender: str | None = None,
        thread_parent_id: str | None = None,
    ) -> DaemonAction:
        """Post a message to a channel."""
        params: dict[str, Any] = {"message": message}
        if channel is not None:
            params["channel"] = channel
        if sender is not None:
            params["from"] = sender
        if thread_parent_id is not None:
            params["thread_parent_id"] = thread_parent_id
        return DaemonAction(method="channel.post", params=params)

    # -- Task ---------------------------------------------------------------

    @staticmethod
    def create_task(
        subject: str,
        *,
        description: str = "",
        channel: str | None = None,
        blocked_by: list[str] | None = None,
        model: str | None = None,
    ) -> DaemonAction:
        """Create a new task."""
        params: dict[str, Any] = {"subject": subject}
        if description:
            params["description"] = description
        if channel is not None:
            params["channel"] = channel
        if blocked_by is not None:
            params["blocked_by"] = blocked_by
        if model is not None:
            params["model"] = model
        return DaemonAction(method="task.create", params=params)

    @staticmethod
    def complete_task(task_id: str) -> DaemonAction:
        """Mark a task as done."""
        return DaemonAction(method="task.done", params={"id": task_id})

    # -- Coworker -----------------------------------------------------------

    @staticmethod
    def nudge_coworker(
        name: str,
        message: str,
        *,
        sender: str | None = None,
    ) -> DaemonAction:
        """Send a nudge message to a coworker."""
        params: dict[str, Any] = {"name": name, "message": message}
        if sender is not None:
            params["from"] = sender
        return DaemonAction(method="coworker.nudge", params=params)

    @staticmethod
    def spawn_coworker(
        *,
        prompt: str | None = None,
        resume: bool = False,
    ) -> DaemonAction:
        """Spawn a new coworker session."""
        params: dict[str, Any] = {"resume": resume}
        if prompt is not None:
            params["prompt"] = prompt
        return DaemonAction(method="coworker.spawn", params=params)

    # -- PR -----------------------------------------------------------------

    @staticmethod
    def spawn_reviewer(pr_number: int) -> DaemonAction:
        """Spawn a reviewer for a pull request."""
        return DaemonAction(method="pr.review", params={"pr": pr_number})

    @staticmethod
    def enable_auto_merge(pr_number: int) -> DaemonAction:
        """Enable GitHub auto-merge on a pull request."""
        return DaemonAction(method="pr.auto-merge", params={"pr": pr_number})

    # -- Daemon -------------------------------------------------------------

    @staticmethod
    def check_pending() -> DaemonAction:
        """Trigger immediate dispatch of pending tasks."""
        return DaemonAction(method="daemon.check-pending", params={})
