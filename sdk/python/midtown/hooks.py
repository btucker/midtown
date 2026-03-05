"""Hook specifications for Midtown workflow plugins."""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Sequence

import pluggy

if TYPE_CHECKING:
    from midtown import MidtownRPC
    from midtown.actions import Action

hookspec = pluggy.HookspecMarker("midtown")
hookimpl = pluggy.HookimplMarker("midtown")


@dataclass
class DaemonAction:
    """What the daemon's compiled-in behavior would do."""

    kind: str
    args: dict[str, Any]


@dataclass
class HookContext:
    """Context provided to every hook invocation."""

    channel: str
    task_id: str | None
    thread_id: str | None
    message_id: str | None
    rpc: MidtownRPC | None
    daemon_actions: list[DaemonAction]

    _default_prevented: bool = field(default=False, repr=False)

    def prevent_default(self) -> None:
        """Block daemon's default behavior for this event."""
        self._default_prevented = True

    def is_default_prevented(self) -> bool:
        return self._default_prevented


class WorkflowHooks:
    """Specification for all workflow hooks."""

    # Task lifecycle
    @hookspec
    def on_task_created(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a new task is created."""

    @hookspec
    def on_task_assigned(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a coworker claims/is assigned a task."""

    @hookspec
    def on_task_completed(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a task is marked complete."""

    @hookspec
    def on_task_phase_complete(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a task phase completes."""

    # PR lifecycle
    @hookspec
    def on_pr_opened(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a PR is opened."""

    @hookspec
    def on_pr_approved(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a PR receives approval."""

    @hookspec
    def on_pr_changes_requested(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when changes are requested on a PR."""

    @hookspec
    def on_pr_merged(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a PR is merged."""

    @hookspec
    def on_pr_ci_passed(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when CI passes on a PR."""

    @hookspec
    def on_pr_ci_failed(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when CI fails on a PR."""

    @hookspec
    def on_pr_conflict(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a PR has merge conflicts."""

    @hookspec
    def on_pr_auto_merge(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a PR is eligible for auto-merge."""

    # Coworker
    @hookspec
    def on_coworker_spawned(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a coworker is spawned."""

    @hookspec
    def on_coworker_idle(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a coworker becomes idle."""

    @hookspec
    def on_coworker_stuck(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a coworker appears stuck."""

    @hookspec
    def on_coworker_message(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a coworker posts a message."""

    # Forked leads
    @hookspec
    def on_fork_lead_spawned(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a forked lead is spawned."""

    @hookspec
    def on_fork_lead_idle(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a forked lead goes idle."""

    # Channel
    @hookspec
    def on_channel_message(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called when a human posts to the channel."""

    # Timer
    @hookspec
    def on_timer_tick(self, event: dict, context: HookContext) -> Sequence[Action] | None:
        """Called on each dispatch tick."""


class TaskHooks:
    """Hooks for customizing task prompts."""

    @hookspec
    def get_system_prompt(self, task_id: str, task_metadata: dict) -> str | None:
        """Return a custom system prompt for this task."""

    @hookspec
    def get_author_prompt(self, task_id: str, task_metadata: dict) -> str | None:
        """Return a custom author prompt for this task."""

    @hookspec
    def get_reviewer_prompt(self, task_id: str, task_metadata: dict, pr_number: int) -> str | None:
        """Return a custom reviewer prompt for this task's PR."""
