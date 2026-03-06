"""Tests for WorkflowDaemon."""

from __future__ import annotations

import asyncio
import json
import tempfile
import time
from pathlib import Path

import pytest

from midtown.daemon import WorkflowDaemon


# Helper to create a plugin that implements on_pr_opened via the new API.
# Plugins use @hookimpl and receive a single `ctx` argument.
def _plugin_source(message: str) -> str:
    return (
        "from midtown.hooks import hookimpl\n"
        "\n"
        "@hookimpl\n"
        "def on_pr_opened(ctx):\n"
        f'    return [ctx.actions.post_to_channel("{message}")]\n'
    )


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
            plugin_file.write_text(_plugin_source("hello"))

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
                "def on_timer_tick(ctx):\n"
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

            (dir_a / "plugin_a.py").write_text(_plugin_source("from A"))
            (dir_b / "plugin_b.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_merged(ctx):\n"
                "    return []\n"
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
            (plugin_dir / "good.py").write_text(_plugin_source("ok"))

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
            plugin_file.write_text(_plugin_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )
            assert len(daemon._loaded_plugins) == 1

            daemon.unload_plugin(plugin_file)
            assert len(daemon._loaded_plugins) == 0
            assert plugin_file not in daemon._mtimes

    def test_unload_nonexistent_is_noop(self) -> None:
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
            plugin_file.write_text(_plugin_source("v1"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            # No changes yet
            assert daemon.check_for_changes() == []

            # Touch the file to update mtime
            time.sleep(0.05)
            plugin_file.write_text(_plugin_source("v2"))

            changed = daemon.check_for_changes()
            assert plugin_file in changed

    def test_reload_changed_updates_plugin(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(_plugin_source("v1"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event("pr.opened", {"pr_number": 1})
            assert actions[0].params["message"] == "v1"

            # Update plugin
            time.sleep(0.05)
            plugin_file.write_text(_plugin_source("v2"))

            daemon.reload_changed()

            actions = daemon.dispatch_event("pr.opened", {"pr_number": 1})
            assert actions[0].params["message"] == "v2"

    def test_reload_preserves_tracking_on_failure(self) -> None:
        """A temporary import error should not permanently disable a plugin."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(_plugin_source("v1"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            # Introduce a syntax error
            time.sleep(0.05)
            plugin_file.write_text("raise SyntaxError('broken')\n")
            daemon.reload_changed()

            # Plugin should still be tracked so next fix is picked up
            assert plugin_file in daemon._mtimes

            # Fix the plugin
            time.sleep(0.05)
            plugin_file.write_text(_plugin_source("v2"))

            daemon.reload_changed()

            actions = daemon.dispatch_event("pr.opened", {"pr_number": 1})
            assert len(actions) == 1
            assert actions[0].params["message"] == "v2"

    def test_deleted_plugin_is_unloaded(self) -> None:
        """Deleting a plugin file should unregister its hooks."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(_plugin_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            assert len(daemon._loaded_plugins) == 1

            # Delete the plugin file
            plugin_file.unlink()
            daemon.reload_changed()

            assert len(daemon._loaded_plugins) == 0
            assert plugin_file not in daemon._mtimes

            # Hooks should no longer fire
            actions = daemon.dispatch_event("pr.opened", {"pr_number": 1})
            assert actions == []


class TestEventDispatch:
    """Tests for event dispatch to plugins."""

    def test_dispatch_to_matching_hook(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "my_plugin.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(ctx):\n"
                "    return [ctx.actions.post_to_channel(\n"
                "        f\"PR #{ctx.pr_number} opened\"\n"
                "    )]\n"
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event("pr.opened", {"pr_number": 123})

            assert len(actions) == 1
            assert actions[0].method == "channel.post"
            assert "123" in actions[0].params["message"]

    def test_dispatch_unknown_event_returns_empty(self) -> None:
        daemon = WorkflowDaemon(
            socket_path="/tmp/test.sock",
            plugin_dirs=[],
        )

        actions = daemon.dispatch_event("unknown.event", {})
        assert actions == []

    def test_dispatch_no_matching_hook_returns_empty(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "my_plugin.py").write_text(_plugin_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event("pr.merged", {})
            assert actions == []

    def test_multiple_plugins_dispatch(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "plugin_a.py").write_text(_plugin_source("from A"))
            (plugin_dir / "plugin_b.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(ctx):\n"
                '    return [ctx.actions.nudge_coworker("bob", "from B")]\n'
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event("pr.opened", {"pr_number": 1})

            assert len(actions) == 2
            methods = {a.method for a in actions}
            assert "channel.post" in methods
            assert "coworker.nudge" in methods

    def test_plugin_returning_none_is_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "silent.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(ctx):\n"
                "    return None\n"
            )
            (plugin_dir / "talker.py").write_text(_plugin_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event("pr.opened", {"pr_number": 1})

            assert len(actions) == 1
            assert actions[0].params["message"] == "hello"

    def test_action_serialization(self) -> None:
        """Dispatch returns DaemonAction objects with method and params."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "multi_action.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_task_completed(ctx):\n"
                "    return [\n"
                '        ctx.actions.post_to_channel("done!"),\n'
                "        ctx.actions.check_pending(),\n"
                "    ]\n"
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event(
                "task.completed", {"task_id": "42"}, task_id="42"
            )

            assert len(actions) == 2
            assert actions[0].method == "channel.post"
            assert actions[0].params == {"message": "done!"}
            assert actions[1].method == "daemon.check-pending"
            assert actions[1].params == {}

    def test_on_event_hook_fires_for_all_events(self) -> None:
        """The global on_event hook should fire alongside specific hooks."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "global_logger.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_event(ctx):\n"
                '    return [ctx.actions.post_to_channel("global")]\n'
            )
            (plugin_dir / "pr_handler.py").write_text(_plugin_source("specific"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event("pr.opened", {"pr_number": 1})

            assert len(actions) == 2
            messages = {a.params["message"] for a in actions}
            assert "global" in messages
            assert "specific" in messages

    def test_context_populated_from_event(self) -> None:
        """HookContext fields should be populated from event and kwargs."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "inspector.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(ctx):\n"
                "    msg = f'{ctx.event_type}:{ctx.pr_number}:{ctx.task_id}'\n"
                "    return [ctx.actions.post_to_channel(msg)]\n"
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                plugin_dirs=[plugin_dir],
            )

            actions = daemon.dispatch_event(
                "pr.opened",
                {"pr_number": 42},
                task_id="7",
            )

            assert len(actions) == 1
            assert actions[0].params["message"] == "pr.opened:42:7"


# ---------------------------------------------------------------------------
# Helpers for socket server tests
# ---------------------------------------------------------------------------


async def _send_request(socket_path: str, request: dict) -> dict:
    """Connect to the daemon socket, send a request, and return the response."""
    reader, writer = await asyncio.open_unix_connection(socket_path)
    try:
        writer.write(json.dumps(request, separators=(",", ":")).encode() + b"\n")
        await writer.drain()
        line = await asyncio.wait_for(reader.readline(), timeout=5.0)
        return json.loads(line)
    finally:
        writer.close()
        await writer.wait_closed()


async def _start_daemon(daemon: WorkflowDaemon, timeout: float = 5.0) -> asyncio.Task:
    """Start the daemon server and wait for it to begin listening."""
    import stat

    task = asyncio.create_task(daemon.run())
    # Wait for the socket file to appear (must be an actual socket, not a
    # regular file — important for the stale-socket-cleanup test).
    deadline = asyncio.get_event_loop().time() + timeout
    while True:
        if asyncio.get_event_loop().time() > deadline:
            raise TimeoutError("Daemon did not start in time")
        p = Path(daemon.socket_path)
        if p.exists() and stat.S_ISSOCK(p.stat().st_mode):
            break
        await asyncio.sleep(0.01)
    return task


# ---------------------------------------------------------------------------
# Socket server tests
# ---------------------------------------------------------------------------


class TestSocketServer:
    """Tests for the Unix socket server."""

    @pytest.mark.asyncio
    async def test_server_starts_and_accepts_connections(self) -> None:
        """The server should start, listen, and handle a basic request."""
        with tempfile.TemporaryDirectory() as tmpdir:
            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(socket_path=sock_path, plugin_dirs=[])

            server_task = await _start_daemon(daemon)
            try:
                response = await _send_request(
                    sock_path,
                    {"type": "pr.opened", "event": {"pr_number": 1}},
                )
                assert response["ok"] is True
                assert response["actions"] == []
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_event_dispatch_round_trip(self) -> None:
        """Events dispatched over the socket should return plugin actions."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "my_plugin.py").write_text(_plugin_source("socket works"))

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, plugin_dirs=[plugin_dir]
            )

            server_task = await _start_daemon(daemon)
            try:
                response = await _send_request(
                    sock_path,
                    {"type": "pr.opened", "event": {"pr_number": 42}},
                )
                assert response["ok"] is True
                assert len(response["actions"]) == 1
                assert response["actions"][0]["method"] == "channel.post"
                assert response["actions"][0]["params"]["message"] == "socket works"
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_missing_type_field_returns_error(self) -> None:
        """A request without a 'type' field should return an error."""
        with tempfile.TemporaryDirectory() as tmpdir:
            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(socket_path=sock_path, plugin_dirs=[])

            server_task = await _start_daemon(daemon)
            try:
                response = await _send_request(sock_path, {"event": {}})
                assert response["ok"] is False
                assert "type" in response["error"]
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_invalid_json_returns_error(self) -> None:
        """Sending invalid JSON should return an error, not crash."""
        with tempfile.TemporaryDirectory() as tmpdir:
            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(socket_path=sock_path, plugin_dirs=[])

            server_task = await _start_daemon(daemon)
            try:
                reader, writer = await asyncio.open_unix_connection(sock_path)
                writer.write(b"not valid json\n")
                await writer.drain()
                line = await asyncio.wait_for(reader.readline(), timeout=5.0)
                response = json.loads(line)
                assert response["ok"] is False
                assert "invalid JSON" in response["error"]
                writer.close()
                await writer.wait_closed()
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_multiple_sequential_requests(self) -> None:
        """The server should handle multiple sequential connections."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()
            (plugin_dir / "counter.py").write_text(_plugin_source("counted"))

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, plugin_dirs=[plugin_dir]
            )

            server_task = await _start_daemon(daemon)
            try:
                for i in range(3):
                    response = await _send_request(
                        sock_path,
                        {"type": "pr.opened", "event": {"pr_number": i}},
                    )
                    assert response["ok"] is True
                    assert len(response["actions"]) == 1
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_plugin_error_isolation(self) -> None:
        """A plugin that raises should not crash the server or block others."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            # Plugin that raises on dispatch
            (plugin_dir / "bad_plugin.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(ctx):\n"
                '    raise RuntimeError("plugin exploded")\n'
            )
            # Good plugin that should still work
            (plugin_dir / "good_plugin.py").write_text(_plugin_source("still works"))

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, plugin_dirs=[plugin_dir]
            )

            server_task = await _start_daemon(daemon)
            try:
                # The request should not crash the server — pluggy calls all
                # hooks and propagates the first exception. The server catches
                # it and returns an error response.
                response = await _send_request(
                    sock_path,
                    {"type": "pr.opened", "event": {"pr_number": 1}},
                )
                # Server should still be alive for next request
                response2 = await _send_request(
                    sock_path,
                    {"type": "pr.merged", "event": {}},
                )
                assert response2["ok"] is True
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_hot_reload_during_socket_operation(self) -> None:
        """Plugins modified while the server is running should be reloaded."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            plugin_file = plugin_dir / "my_plugin.py"
            plugin_file.write_text(_plugin_source("v1"))

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, plugin_dirs=[plugin_dir]
            )

            server_task = await _start_daemon(daemon)
            try:
                # First request should see v1
                response = await _send_request(
                    sock_path,
                    {"type": "pr.opened", "event": {"pr_number": 1}},
                )
                assert response["actions"][0]["params"]["message"] == "v1"

                # Update plugin file
                time.sleep(0.05)
                plugin_file.write_text(_plugin_source("v2"))

                # Next request should trigger hot-reload and see v2
                response = await _send_request(
                    sock_path,
                    {"type": "pr.opened", "event": {"pr_number": 2}},
                )
                assert response["actions"][0]["params"]["message"] == "v2"
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_stale_socket_cleanup(self) -> None:
        """Starting the daemon should clean up a stale socket file."""
        with tempfile.TemporaryDirectory() as tmpdir:
            sock_path = str(Path(tmpdir) / "daemon.sock")

            # Create a stale socket file
            Path(sock_path).touch()

            daemon = WorkflowDaemon(socket_path=sock_path, plugin_dirs=[])

            server_task = await _start_daemon(daemon)
            try:
                response = await _send_request(
                    sock_path,
                    {"type": "pr.opened", "event": {}},
                )
                assert response["ok"] is True
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_task_context_forwarded_over_socket(self) -> None:
        """Task context fields should be forwarded through the socket protocol."""
        with tempfile.TemporaryDirectory() as tmpdir:
            plugin_dir = Path(tmpdir) / "plugins"
            plugin_dir.mkdir()

            (plugin_dir / "inspector.py").write_text(
                "from midtown.hooks import hookimpl\n"
                "\n"
                "@hookimpl\n"
                "def on_pr_opened(ctx):\n"
                "    msg = f'{ctx.task_id}:{ctx.task_state}'\n"
                "    return [ctx.actions.post_to_channel(msg)]\n"
            )

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, plugin_dirs=[plugin_dir]
            )

            server_task = await _start_daemon(daemon)
            try:
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 42},
                        "task_id": "7",
                        "task_state": "in_review",
                    },
                )
                assert response["ok"] is True
                assert response["actions"][0]["params"]["message"] == "7:in_review"
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass
