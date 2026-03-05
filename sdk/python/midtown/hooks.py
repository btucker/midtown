"""Pluggy hook specifications for the Midtown workflow plugin system.

Hook specs define the contract between the workflow daemon and user-supplied
plugins.  Each hook method receives a :class:`HookContext` and returns a list
of :class:`DaemonAction` objects describing side effects the daemon should
execute.

Two spec classes partition hooks by scope:

* :class:`WorkflowHooks` — project-wide lifecycle hooks (daemon start/stop,
  global event filtering).
* :class:`TaskHooks` — per-event hooks invoked when the daemon emits a
  workflow event (``pr.opened``, ``coworker.idle``, etc.).

Plugins implement hook specs via ``@hookimpl``::

    import pluggy
    from midtown.hooks import HookContext, TaskHooks, hookimpl

    class MyPlugin:
        @hookimpl
        def on_pr_opened(self, ctx: HookContext) -> list[DaemonAction]:
            return [ctx.actions.post_to_channel(f"PR #{ctx.pr_number} opened!")]
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import pluggy

PROJECT_NAME = "midtown_workflow"

hookspec = pluggy.HookspecMarker(PROJECT_NAME)
hookimpl = pluggy.HookimplMarker(PROJECT_NAME)


# ---------------------------------------------------------------------------
# Data classes
# ---------------------------------------------------------------------------


@dataclass(frozen=True)
class DaemonAction:
    """A side-effect command returned by hook implementations.

    Hooks do not perform I/O directly.  Instead they return ``DaemonAction``
    objects that the workflow daemon executes after all hooks have run.  This
    mirrors the Rust daemon's ``Effect`` pattern — decisions are pure, side
    effects are batched.

    Use the factory methods in :mod:`midtown.actions` (or on
    :class:`HookContext` via ``ctx.actions``) rather than constructing
    ``DaemonAction`` directly.
    """

    method: str
    """The RPC method or daemon command to invoke (e.g. ``"channel.post"``)."""

    params: dict[str, Any] = field(default_factory=dict)
    """Parameters for the command."""


@dataclass
class HookContext:
    """Context object passed to every hook invocation.

    Provides read-only access to the event, task state, and an ``actions``
    helper for constructing :class:`DaemonAction` return values.  Plugins
    should treat ``state`` as read-only; mutations are not persisted (use
    :meth:`actions.set_state` instead).
    """

    # Event data
    event_type: str
    """The event type string (e.g. ``"pr.opened"``, ``"coworker.idle"``)."""

    event: dict[str, Any]
    """The full event payload dict."""

    # Task context (may be None for non-task events)
    task_id: str | None = None
    """The task ID associated with this event, if any."""

    task_state: str | None = None
    """The workflow state of the task (e.g. ``"pending"``, ``"in_review"``)."""

    prev_task_state: str | None = None
    """The task state *before* the current event's transition, if any."""

    # Event fields (extracted for convenience)
    coworker: str = ""
    """The coworker name from the event, if present."""

    pr_number: int | None = None
    """The PR number from the event, if present."""

    channel: str = ""
    """The channel from the event."""

    # Workflow state (read-only snapshot)
    state: dict[str, Any] = field(default_factory=dict)
    """The current workflow state dict (read-only snapshot)."""

    # Action builder
    actions: Any = None
    """An :class:`~midtown.actions.Actions` instance for building return values."""


# ---------------------------------------------------------------------------
# Hook specifications
# ---------------------------------------------------------------------------


class WorkflowHooks:
    """Project-wide lifecycle hooks.

    These hooks fire once per daemon lifecycle event, not per-task.
    Implementations are registered via ``@hookimpl``.
    """

    @hookspec
    def workflow_started(self, ctx: HookContext) -> list[DaemonAction]:
        """Called when the workflow daemon starts up.

        Use this for one-time initialisation (e.g. posting a startup message
        to the channel).
        """

    @hookspec
    def workflow_stopped(self, ctx: HookContext) -> list[DaemonAction]:
        """Called when the workflow daemon is shutting down."""

    @hookspec
    def on_event(self, ctx: HookContext) -> list[DaemonAction]:
        """Called for every workflow event, before the event-specific hook.

        Useful for logging, metrics, or cross-cutting concerns.  Runs
        regardless of event type.
        """


class TaskHooks:
    """Per-event hooks invoked when the daemon emits a workflow event.

    Each method corresponds to one event type.  The method name is the event
    type with dots replaced by underscores and prefixed with ``on_``
    (e.g. ``pr.opened`` → ``on_pr_opened``).

    Hooks receive a :class:`HookContext` populated with event data and return
    a list of :class:`DaemonAction` objects.  Return an empty list for no
    side effects.

    All hooks use ``firstresult=False`` (the default) so multiple plugins can
    contribute actions for the same event — actions are concatenated.
    """

    # -- Task lifecycle -----------------------------------------------------

    @hookspec
    def on_task_created(self, ctx: HookContext) -> list[DaemonAction]:
        """A new task was created."""

    @hookspec
    def on_task_assigned(self, ctx: HookContext) -> list[DaemonAction]:
        """A task was assigned to a coworker."""

    # -- PR lifecycle -------------------------------------------------------

    @hookspec
    def on_pr_opened(self, ctx: HookContext) -> list[DaemonAction]:
        """A PR was opened for a task."""

    @hookspec
    def on_pr_approved(self, ctx: HookContext) -> list[DaemonAction]:
        """A PR was approved by a reviewer."""

    @hookspec
    def on_pr_changes_requested(self, ctx: HookContext) -> list[DaemonAction]:
        """A reviewer requested changes on a PR."""

    @hookspec
    def on_pr_merged(self, ctx: HookContext) -> list[DaemonAction]:
        """A PR was merged."""

    @hookspec
    def on_pr_ci_passed(self, ctx: HookContext) -> list[DaemonAction]:
        """CI checks passed on a PR."""

    @hookspec
    def on_pr_ci_failed(self, ctx: HookContext) -> list[DaemonAction]:
        """CI checks failed on a PR."""

    @hookspec
    def on_pr_conflict(self, ctx: HookContext) -> list[DaemonAction]:
        """A PR has a merge conflict."""

    @hookspec
    def on_pr_auto_merge(self, ctx: HookContext) -> list[DaemonAction]:
        """A PR is eligible for auto-merge (approved + CI green, no active reviewer)."""

    # -- Coworker lifecycle -------------------------------------------------

    @hookspec
    def on_coworker_idle(self, ctx: HookContext) -> list[DaemonAction]:
        """A coworker went idle (finished its current work)."""

    @hookspec
    def on_coworker_stuck(self, ctx: HookContext) -> list[DaemonAction]:
        """A coworker appears stuck and will be restarted."""

    # -- Catch-all ----------------------------------------------------------

    @hookspec
    def on_unhandled_event(self, ctx: HookContext) -> list[DaemonAction]:
        """Called for events that have no specific ``on_<type>`` hook.

        Useful for plugins that want to handle custom event types without
        modifying the hook spec.
        """


def get_plugin_manager() -> pluggy.PluginManager:
    """Create and return a :class:`pluggy.PluginManager` with hook specs registered.

    The caller is responsible for registering plugin implementations via
    ``pm.register(plugin_instance)``.
    """
    pm = pluggy.PluginManager(PROJECT_NAME)
    pm.add_hookspecs(WorkflowHooks)
    pm.add_hookspecs(TaskHooks)
    return pm
