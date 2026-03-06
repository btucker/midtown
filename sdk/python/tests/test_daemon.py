"""Tests for WorkflowDaemon."""

from __future__ import annotations

import tempfile
import time
from pathlib import Path

from midtown.daemon import WorkflowDaemon
from midtown.hooks import HookContext


def _make_context(**kwargs: object) -> HookContext:
    """Create a HookContext with sensible defaults for testing."""
    defaults = {
        "channel": "test",
        "task_id": None,
        "thread_id": None,
        "message_id": None,
        "rpc": None,
        "daemon_actions": [],
    }
    defaults.update(kwargs)
    return HookContext(**defaults)  # type: ignore[arg-type]


class TestPluginLoading:
    """Tests for plugin discovery and loading."""

    def test_load_from_nonexistent_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[Path(tmpdir) / "nonexistent"],
            )
            assert len(daemon._loaded_plugins) == 0

    def test_load_from_empty_directory(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()
            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )
            assert len(daemon._loaded_plugins) == 0

    def test_load_single_plugin(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.post_to_channel("PR opened!")]\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            assert len(daemon._loaded_plugins) == 1
            assert plugin_file in daemon._loaded_plugins

    def test_skips_underscore_files(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "__init__.py").write_text("")
            (plugin_dir / "_private.py").write_text("x = 1\n")
            (plugin_dir / "good_plugin.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_timer_tick(event, context):\n"
                "    return None\n"
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            assert len(daemon._loaded_plugins) == 1

    def test_load_multiple_directories(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            dir_a = Path(tmpdir) / "a"
            dir_b = Path(tmpdir) / "b"
            dir_a.mkdir()
            dir_b.mkdir()

            (dir_a / "plugin_a.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return []\n'
            )
            (dir_b / "plugin_b.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_merged(event, context):\n"
                '    return []\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[dir_a, dir_b],
            )

            assert len(daemon._loaded_plugins) == 2

    def test_bad_plugin_does_not_crash(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "bad.py").write_text("raise RuntimeError('boom')\n")
            (plugin_dir / "good.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return []\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            # Bad plugin failed to load, but good plugin succeeded
            assert len(daemon._loaded_plugins) == 1


class TestPluginUnloading:
    """Tests for plugin unloading."""

    def test_unload_plugin(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return []\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )
            assert len(daemon._loaded_plugins) == 1

            daemon.unload_plugin(plugin_file)
            assert len(daemon._loaded_plugins) == 0
            assert plugin_file not in daemon._mtimes

    def test_unload_nonexistent_is_noop(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[],
            )
            # Should not raise
            daemon.unload_plugin(Path("/nonexistent/plugin.py"))


class TestHotReload:
    """Tests for mtime-based hot-reload."""

    def test_detect_changed_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return []\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            # No changes yet
            assert daemon.check_for_changes() == []

            # Touch the file to update mtime
            time.sleep(0.05)
            plugin_file.write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.post_to_channel("v2")]\n'
            )

            changed = daemon.check_for_changes()
            assert plugin_file in changed

    def test_reload_changed_updates_plugin(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.post_to_channel("v1")]\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            ctx = _make_context(task_id="1")
            actions, _ = daemon.dispatch_event("pr.opened", {"pr_number": 1}, ctx)
            assert actions[0]["args"]["message"] == "v1"

            # Update plugin
            time.sleep(0.05)
            plugin_file.write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.post_to_channel("v2")]\n'
            )

            daemon.reload_changed()

            ctx = _make_context(task_id="1")
            actions, _ = daemon.dispatch_event("pr.opened", {"pr_number": 1}, ctx)
            assert actions[0]["args"]["message"] == "v2"


class TestEventDispatch:
    """Tests for event dispatch to plugins."""

    def test_dispatch_to_matching_hook(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "my_plugin.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.post_to_channel(f"PR #{event[\'pr_number\']} opened")]\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            ctx = _make_context(task_id="1")
            actions, prevented = daemon.dispatch_event(
                "pr.opened", {"pr_number": 123}, ctx
            )

            assert not prevented
            assert len(actions) == 1
            assert actions[0]["type"] == "post_to_channel"
            assert "123" in actions[0]["args"]["message"]

    def test_dispatch_unknown_event_returns_empty(self) -> None:
        daemon = WorkflowDaemon(
            socket_path="/tmp/test.sock",
            plugin_dirs=[],
        )

        ctx = _make_context()
        actions, prevented = daemon.dispatch_event(
            "unknown.event", {}, ctx
        )

        assert actions == []
        assert not prevented

    def test_dispatch_no_matching_hook_returns_empty(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "my_plugin.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                "    return []\n"
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            ctx = _make_context()
            actions, prevented = daemon.dispatch_event(
                "pr.merged", {}, ctx
            )

            assert actions == []
            assert not prevented

    def test_prevent_default(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "veto_plugin.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_auto_merge(event, context):\n"
                "    context.prevent_default()\n"
                '    return [Action.enable_auto_merge(event["pr_number"])]\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            ctx = _make_context(task_id="1")
            actions, prevented = daemon.dispatch_event(
                "pr.auto_merge", {"pr_number": 123}, ctx
            )

            assert prevented
            assert len(actions) == 1
            assert actions[0]["type"] == "enable_auto_merge"

    def test_multiple_plugins_dispatch(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "plugin_a.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.post_to_channel("from A")]\n'
            )
            (plugin_dir / "plugin_b.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.nudge_coworker("bob", "from B")]\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            ctx = _make_context()
            actions, _ = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, ctx
            )

            assert len(actions) == 2
            types = {a["type"] for a in actions}
            assert "post_to_channel" in types
            assert "nudge_coworker" in types

    def test_plugin_returning_none_is_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "silent.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                "    return None\n"
            )
            (plugin_dir / "talker.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(event, context):\n"
                '    return [Action.post_to_channel("hello")]\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            ctx = _make_context()
            actions, _ = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, ctx
            )

            assert len(actions) == 1
            assert actions[0]["args"]["message"] == "hello"

    def test_action_serialization(self) -> None:
        """Dispatch returns dicts with type and args, not Action objects."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "multi_action.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "from midtown.actions import Action\n"
                "\n"
                "@hookimpl\n"
                "def on_task_completed(event, context):\n"
                "    return [\n"
                '        Action.post_to_channel("done!"),\n'
                "        Action.check_pending(),\n"
                "    ]\n"
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            ctx = _make_context(task_id="42")
            actions, _ = daemon.dispatch_event(
                "task.completed", {"task_id": "42"}, ctx
            )

            assert len(actions) == 2
            assert isinstance(actions[0], dict)
            assert actions[0] == {
                "type": "post_to_channel",
                "args": {"message": "done!", "thread_id": None},
            }
            assert actions[1] == {"type": "check_pending", "args": {}}
