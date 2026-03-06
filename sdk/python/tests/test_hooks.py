"""Tests for midtown.hooks and midtown.actions."""

from __future__ import annotations

from midtown.actions import Actions
from midtown.hooks import (
    DaemonAction,
    HookContext,
    TaskHooks,
    WorkflowHooks,
    get_plugin_manager,
    hookimpl,
)


class TestDaemonAction:
    def test_frozen(self) -> None:
        action = DaemonAction(method="channel.post", params={"message": "hi"})
        assert action.method == "channel.post"
        assert action.params == {"message": "hi"}

    def test_default_params(self) -> None:
        action = DaemonAction(method="daemon.check-pending")
        assert action.params == {}


class TestHookContext:
    def test_defaults(self) -> None:
        ctx = HookContext(event_type="pr.opened", event={"type": "pr.opened"})
        assert ctx.event_type == "pr.opened"
        assert ctx.task_id is None
        assert ctx.coworker == ""
        assert ctx.pr_number is None
        assert ctx.state == {}

    def test_with_task_context(self) -> None:
        ctx = HookContext(
            event_type="pr.opened",
            event={"type": "pr.opened"},
            task_id="42",
            task_state="in_review",
            prev_task_state="in_progress",
            pr_number=99,
            coworker="park",
        )
        assert ctx.task_id == "42"
        assert ctx.task_state == "in_review"
        assert ctx.prev_task_state == "in_progress"
        assert ctx.pr_number == 99


class TestActions:
    def test_post_to_channel(self) -> None:
        action = Actions.post_to_channel("hello")
        assert action == DaemonAction(method="channel.post", params={"message": "hello"})

    def test_post_to_channel_with_options(self) -> None:
        action = Actions.post_to_channel(
            "hello", channel="general", sender="bot", thread_parent_id="abc"
        )
        assert action.params == {
            "message": "hello",
            "channel": "general",
            "from": "bot",
            "thread_parent_id": "abc",
        }

    def test_nudge_coworker(self) -> None:
        action = Actions.nudge_coworker("park", "check this")
        assert action == DaemonAction(
            method="coworker.nudge", params={"name": "park", "message": "check this"}
        )

    def test_spawn_reviewer(self) -> None:
        action = Actions.spawn_reviewer(42)
        assert action == DaemonAction(method="pr.review", params={"pr": 42})

    def test_complete_task(self) -> None:
        action = Actions.complete_task("100")
        assert action == DaemonAction(method="task.done", params={"id": "100"})

    def test_check_pending(self) -> None:
        action = Actions.check_pending()
        assert action == DaemonAction(method="daemon.check-pending", params={})

    def test_enable_auto_merge(self) -> None:
        action = Actions.enable_auto_merge(55)
        assert action == DaemonAction(method="pr.auto-merge", params={"pr": 55})

    def test_create_task(self) -> None:
        action = Actions.create_task("Fix bug", description="Details", channel="dev")
        assert action.method == "task.create"
        assert action.params["subject"] == "Fix bug"
        assert action.params["description"] == "Details"
        assert action.params["channel"] == "dev"

    def test_update_task(self) -> None:
        action = Actions.update_task("42", owner="park", status="in_review", pr=99)
        assert action.method == "task.update"
        assert action.params["id"] == "42"
        assert action.params["owner"] == "park"
        assert action.params["status"] == "in_review"
        assert action.params["pr"] == 99
        # Unset params should be omitted
        assert "description" not in action.params
        assert "channel" not in action.params

    def test_spawn_coworker(self) -> None:
        action = Actions.spawn_coworker(prompt="do something")
        assert action == DaemonAction(
            method="coworker.spawn", params={"resume": False, "prompt": "do something"}
        )


class TestPreventDefault:
    """Tests for the prevent_default / is_default_prevented API on HookContext."""

    def test_default_not_prevented(self) -> None:
        ctx = HookContext(event_type="pr.opened", event={})
        assert ctx.is_default_prevented() is False

    def test_prevent_default(self) -> None:
        ctx = HookContext(event_type="pr.opened", event={})
        ctx.prevent_default()
        assert ctx.is_default_prevented() is True

    def test_prevent_default_idempotent(self) -> None:
        ctx = HookContext(event_type="pr.opened", event={})
        ctx.prevent_default()
        ctx.prevent_default()
        assert ctx.is_default_prevented() is True

    def test_no_camel_case_methods(self) -> None:
        """Regression guard: camelCase method names must not exist on HookContext.

        PR #1795 review explicitly renamed these from camelCase to snake_case
        per PEP 8 conventions. This test prevents re-introduction.
        """
        ctx = HookContext(event_type="test", event={})
        assert not hasattr(ctx, "preventDefault"), (
            "camelCase 'preventDefault' found on HookContext — "
            "use 'prevent_default' (snake_case per PEP 8)"
        )
        assert not hasattr(ctx, "isDefaultPrevented"), (
            "camelCase 'isDefaultPrevented' found on HookContext — "
            "use 'is_default_prevented' (snake_case per PEP 8)"
        )


class TestPluginManager:
    def test_creates_manager_with_specs(self) -> None:
        pm = get_plugin_manager()
        assert pm.project_name == "midtown_workflow"
        # Hook specs are registered
        assert hasattr(pm.hook, "on_pr_opened")
        assert hasattr(pm.hook, "on_coworker_idle")
        assert hasattr(pm.hook, "on_task_completed")
        assert hasattr(pm.hook, "workflow_started")

    def test_register_and_call_plugin(self) -> None:
        """A plugin with @hookimpl can be registered and called."""

        class MyPlugin:
            @hookimpl
            def on_pr_opened(self, ctx: HookContext) -> list[DaemonAction]:
                return [Actions.post_to_channel(f"PR #{ctx.pr_number} opened!")]

        pm = get_plugin_manager()
        pm.register(MyPlugin())

        ctx = HookContext(
            event_type="pr.opened",
            event={"type": "pr.opened"},
            pr_number=42,
            actions=Actions,
        )
        results = pm.hook.on_pr_opened(ctx=ctx)
        # pluggy returns a list of results (one per plugin)
        assert len(results) == 1
        assert results[0] == [
            DaemonAction(method="channel.post", params={"message": "PR #42 opened!"})
        ]

    def test_multiple_plugins_concat(self) -> None:
        """Multiple plugins' actions are collected (not firstresult)."""

        class PluginA:
            @hookimpl
            def on_pr_opened(self, ctx: HookContext) -> list[DaemonAction]:
                return [Actions.post_to_channel("A")]

        class PluginB:
            @hookimpl
            def on_pr_opened(self, ctx: HookContext) -> list[DaemonAction]:
                return [Actions.post_to_channel("B")]

        pm = get_plugin_manager()
        pm.register(PluginA())
        pm.register(PluginB())

        ctx = HookContext(
            event_type="pr.opened",
            event={"type": "pr.opened"},
            actions=Actions,
        )
        results = pm.hook.on_pr_opened(ctx=ctx)
        # Two plugins → two result lists
        assert len(results) == 2
        all_actions = [a for r in results for a in r]
        assert len(all_actions) == 2

    def test_empty_hook_returns_nothing(self) -> None:
        """Calling a hook with no registered plugins returns empty list."""
        pm = get_plugin_manager()
        ctx = HookContext(event_type="pr.opened", event={"type": "pr.opened"})
        results = pm.hook.on_pr_opened(ctx=ctx)
        assert results == []


class TestExports:
    """Verify that the public API is importable from the top-level package."""

    def test_import_from_midtown(self) -> None:
        from midtown import (  # noqa: F401
            Actions,
            DaemonAction,
            DispatchResult,
            HookContext,
            TaskHooks,
            WorkflowHooks,
            get_plugin_manager,
            hookimpl,
            hookspec,
        )

    def test_all_includes_hook_system(self) -> None:
        import midtown

        for name in [
            "Actions",
            "DaemonAction",
            "DispatchResult",
            "HookContext",
            "TaskHooks",
            "WorkflowHooks",
            "get_plugin_manager",
            "hookimpl",
            "hookspec",
            "MidtownRPC",
            "RpcError",
            "run",
            "run_loop",
        ]:
            assert name in midtown.__all__, f"{name} missing from __all__"
