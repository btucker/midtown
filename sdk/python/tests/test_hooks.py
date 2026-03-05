"""Tests for hook specifications and actions."""

from __future__ import annotations

import pluggy

from midtown.actions import Action
from midtown.hooks import (
    DaemonAction,
    HookContext,
    TaskHooks,
    WorkflowHooks,
    hookimpl,
)


class TestHookContext:
    """Tests for HookContext."""

    def test_prevent_default(self) -> None:
        ctx = HookContext(
            channel="test",
            task_id="1",
            thread_id=None,
            message_id=None,
            rpc=None,
            daemon_actions=[],
        )
        assert not ctx.is_default_prevented()
        ctx.prevent_default()
        assert ctx.is_default_prevented()

    def test_daemon_actions_stored(self) -> None:
        actions = [DaemonAction(kind="spawn", args={"name": "alice"})]
        ctx = HookContext(
            channel="test",
            task_id=None,
            thread_id=None,
            message_id=None,
            rpc=None,
            daemon_actions=actions,
        )
        assert len(ctx.daemon_actions) == 1
        assert ctx.daemon_actions[0].kind == "spawn"


class TestActions:
    """Tests for Action factory methods."""

    def test_post_to_channel(self) -> None:
        action = Action.post_to_channel("Hello")
        assert action.type == "post_to_channel"
        assert action.args == {"message": "Hello", "thread_id": None}

    def test_post_to_channel_with_thread(self) -> None:
        action = Action.post_to_channel("Hello", thread_id="t123")
        assert action.args["thread_id"] == "t123"

    def test_nudge_coworker(self) -> None:
        action = Action.nudge_coworker("alice", "Check this")
        assert action.type == "nudge_coworker"
        assert action.args == {"name": "alice", "message": "Check this"}

    def test_spawn_reviewer(self) -> None:
        action = Action.spawn_reviewer(123)
        assert action.type == "spawn_reviewer"
        assert action.args == {"pr_number": 123}

    def test_spawn_coworker(self) -> None:
        action = Action.spawn_coworker("Do work", different_from="bob")
        assert action.type == "spawn_coworker"
        assert action.args == {"prompt": "Do work", "different_from": "bob"}

    def test_spawn_coworker_default(self) -> None:
        action = Action.spawn_coworker("Do work")
        assert action.args["different_from"] is None

    def test_fork_lead(self) -> None:
        action = Action.fork_lead("research", "Research X")
        assert action.type == "fork_lead"
        assert action.args == {"role": "research", "prompt": "Research X"}

    def test_complete_task(self) -> None:
        action = Action.complete_task("42")
        assert action.type == "complete_task"
        assert action.args == {"task_id": "42"}

    def test_enable_auto_merge(self) -> None:
        action = Action.enable_auto_merge(123)
        assert action.type == "enable_auto_merge"
        assert action.args == {"pr_number": 123}

    def test_check_pending(self) -> None:
        action = Action.check_pending()
        assert action.type == "check_pending"
        assert action.args == {}

    def test_http_post(self) -> None:
        action = Action.http_post("https://example.com", {"key": "value"})
        assert action.type == "http_post"
        assert action.args == {"url": "https://example.com", "body": {"key": "value"}}


class TestPluggyIntegration:
    """Tests that hook specs work with pluggy's plugin manager."""

    def test_register_workflow_hooks(self) -> None:
        pm = pluggy.PluginManager("midtown")
        pm.add_hookspecs(WorkflowHooks)
        # Should be able to register and call hooks
        assert hasattr(pm.hook, "on_pr_opened")
        assert hasattr(pm.hook, "on_task_created")
        assert hasattr(pm.hook, "on_timer_tick")

    def test_register_task_hooks(self) -> None:
        pm = pluggy.PluginManager("midtown")
        pm.add_hookspecs(TaskHooks)
        assert hasattr(pm.hook, "get_system_prompt")
        assert hasattr(pm.hook, "get_author_prompt")
        assert hasattr(pm.hook, "get_reviewer_prompt")

    def test_plugin_implementation(self) -> None:
        """A plugin can implement a hook and return actions."""
        pm = pluggy.PluginManager("midtown")
        pm.add_hookspecs(WorkflowHooks)

        class MyPlugin:
            @hookimpl
            def on_pr_opened(self, event: dict, context: HookContext):
                return [Action.post_to_channel(f"PR #{event['pr_number']} opened")]

        pm.register(MyPlugin())

        ctx = HookContext(
            channel="test",
            task_id="1",
            thread_id=None,
            message_id=None,
            rpc=None,
            daemon_actions=[],
        )
        results = pm.hook.on_pr_opened(event={"pr_number": 42}, context=ctx)
        assert len(results) == 1
        assert len(results[0]) == 1
        assert results[0][0].type == "post_to_channel"
        assert "42" in results[0][0].args["message"]

    def test_plugin_prevent_default(self) -> None:
        """A plugin can call preventDefault on the context."""
        pm = pluggy.PluginManager("midtown")
        pm.add_hookspecs(WorkflowHooks)

        class VetoPlugin:
            @hookimpl
            def on_pr_auto_merge(self, event: dict, context: HookContext):
                context.prevent_default()
                return [Action.enable_auto_merge(event["pr_number"])]

        pm.register(VetoPlugin())

        ctx = HookContext(
            channel="test",
            task_id="1",
            thread_id=None,
            message_id=None,
            rpc=None,
            daemon_actions=[],
        )
        results = pm.hook.on_pr_auto_merge(event={"pr_number": 99}, context=ctx)
        assert ctx.is_default_prevented()
        assert len(results) == 1

    def test_multiple_plugins(self) -> None:
        """Multiple plugins can respond to the same hook."""
        pm = pluggy.PluginManager("midtown")
        pm.add_hookspecs(WorkflowHooks)

        class PluginA:
            @hookimpl
            def on_pr_opened(self, event: dict, context: HookContext):
                return [Action.post_to_channel("Plugin A")]

        class PluginB:
            @hookimpl
            def on_pr_opened(self, event: dict, context: HookContext):
                return [Action.nudge_coworker("bob", "check PR")]

        pm.register(PluginA())
        pm.register(PluginB())

        ctx = HookContext(
            channel="test",
            task_id=None,
            thread_id=None,
            message_id=None,
            rpc=None,
            daemon_actions=[],
        )
        results = pm.hook.on_pr_opened(event={"pr_number": 1}, context=ctx)
        assert len(results) == 2

    def test_task_hooks_implementation(self) -> None:
        """TaskHooks can return custom prompts."""
        pm = pluggy.PluginManager("midtown")
        pm.add_hookspecs(TaskHooks)

        class PromptPlugin:
            @hookimpl
            def get_system_prompt(self, task_id: str, task_metadata: dict):
                if task_metadata.get("type") == "review":
                    return "You are a code reviewer."
                return None

        pm.register(PromptPlugin())

        results = pm.hook.get_system_prompt(
            task_id="1", task_metadata={"type": "review"}
        )
        assert results == ["You are a code reviewer."]

        # pluggy filters out None returns, so no-match returns empty list
        results = pm.hook.get_system_prompt(
            task_id="2", task_metadata={"type": "implement"}
        )
        assert results == []


class TestExports:
    """Tests that the public API is properly exported from midtown package."""

    def test_imports_from_package(self) -> None:
        from midtown import (
            Action,
            DaemonAction,
            HookContext,
            TaskHooks,
            WorkflowHooks,
            hookimpl,
            hookspec,
        )
        assert Action is not None
        assert HookContext is not None
        assert hookimpl is not None
        assert hookspec is not None
        assert WorkflowHooks is not None
        assert TaskHooks is not None
        assert DaemonAction is not None
