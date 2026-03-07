"""Tests for WorkflowDaemon."""

from __future__ import annotations

import asyncio
import json
import tempfile
import time
from pathlib import Path

import pytest

from midtown.daemon import DispatchResult, WorkflowDaemon


# Helper to create a workflow.py that implements on_pr_opened.
def _workflow_source(message: str) -> str:
    return (
        "from midtown.hooks import hookimpl\n"
        "\n"
        "@hookimpl\n"
        "def on_pr_opened(ctx):\n"
        f'    return [ctx.actions.post_to_channel("{message}")]\n'
    )


def _create_workflow(workflows_dir: Path, name: str, source: str) -> Path:
    """Create a workflow at workflows_dir/<name>/workflow.py and return the file path."""
    workflow_dir = workflows_dir / name
    workflow_dir.mkdir(parents=True, exist_ok=True)
    workflow_file = workflow_dir / "workflow.py"
    workflow_file.write_text(source)
    return workflow_file


class TestWorkflowLoading:
    """Tests for workflow discovery and loading."""

    def test_nonexistent_workflows_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=Path(tmpdir) / "nonexistent",
            )
            assert len(daemon._workflows) == 0

    def test_empty_workflows_dir(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflows_dir.mkdir()
            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )
            assert len(daemon._workflows) == 0

    def test_lazy_loading_on_dispatch(self) -> None:
        """Workflows are not loaded at construction time — only on dispatch."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            # Not loaded yet
            assert len(daemon._workflows) == 0

            # Dispatch triggers lazy load
            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert len(daemon._workflows) == 1
            assert len(result.actions) == 1
            assert result.actions[0].params["message"] == "hello"

    def test_bad_workflow_does_not_crash(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "bad", "raise RuntimeError('boom')\n")

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="bad"
            )
            assert result.actions == []
            assert not result.default_prevented


class TestSingleWorkflowDispatch:
    """Tests for dispatching to a single workflow by name."""

    def test_single_workflow_dispatch(self) -> None:
        """Loading a workflow by name and dispatching to it works."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("tdw response"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 42}, channel_workflow="tdw"
            )

            assert len(result.actions) == 1
            assert result.actions[0].method == "channel.post"
            assert result.actions[0].params["message"] == "tdw response"

    def test_dispatch_without_workflow_returns_empty(self) -> None:
        """channel_workflow that doesn't exist returns empty DispatchResult."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflows_dir.mkdir()

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="nonexistent"
            )
            assert result.actions == []
            assert not result.default_prevented

    def test_dispatch_empty_workflow_returns_empty(self) -> None:
        """Empty channel_workflow string returns empty DispatchResult."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("tdw"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event("pr.opened", {"pr_number": 1})
            assert result.actions == []
            assert not result.default_prevented

    def test_two_workflows_isolated(self) -> None:
        """Each workflow has its own PluginManager — they don't share hooks."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("from tdw"))
            _create_workflow(
                workflows_dir,
                "review",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_merged(ctx):\n"
                    '    return [ctx.actions.post_to_channel("review merged")]\n'
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            # Dispatch to tdw
            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert len(result.actions) == 1
            assert result.actions[0].params["message"] == "from tdw"

            # Dispatch to review — different event, different workflow
            result = daemon.dispatch_event(
                "pr.merged", {}, channel_workflow="review"
            )
            assert len(result.actions) == 1
            assert result.actions[0].params["message"] == "review merged"

            # tdw has no on_pr_merged
            result = daemon.dispatch_event(
                "pr.merged", {}, channel_workflow="tdw"
            )
            assert result.actions == []


class TestWorkflowUnloading:
    """Tests for workflow unloading."""

    def test_unload_workflow(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            # Load it first
            daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert "tdw" in daemon._workflows

            daemon.unload_workflow("tdw")
            assert "tdw" not in daemon._workflows

    def test_unload_nonexistent_is_noop(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=Path(tmpdir),
            )
            # Should not raise
            daemon.unload_workflow("nonexistent")


class TestHotReload:
    """Tests for mtime-based hot-reload."""

    def test_detect_changed_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflow_file = _create_workflow(
                workflows_dir, "tdw", _workflow_source("v1")
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            # Load workflow
            daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )

            # No changes yet
            assert daemon.check_for_changes() == []

            # Touch the file to update mtime
            time.sleep(0.05)
            workflow_file.write_text(_workflow_source("v2"))

            changed = daemon.check_for_changes()
            assert "tdw" in changed

    def test_reload_changed_updates_workflow(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflow_file = _create_workflow(
                workflows_dir, "tdw", _workflow_source("v1")
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions[0].params["message"] == "v1"

            # Update workflow
            time.sleep(0.05)
            workflow_file.write_text(_workflow_source("v2"))

            daemon.reload_changed()

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions[0].params["message"] == "v2"

    def test_auto_reload_on_dispatch(self) -> None:
        """_ensure_loaded detects mtime changes and reloads automatically."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflow_file = _create_workflow(
                workflows_dir, "tdw", _workflow_source("v1")
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions[0].params["message"] == "v1"

            # Update workflow file
            time.sleep(0.05)
            workflow_file.write_text(_workflow_source("v2"))

            # Next dispatch auto-reloads via _ensure_loaded
            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions[0].params["message"] == "v2"

    def test_reload_preserves_tracking_on_failure(self) -> None:
        """A temporary error should not permanently disable a workflow."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflow_file = _create_workflow(
                workflows_dir, "tdw", _workflow_source("v1")
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            # Load workflow
            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions[0].params["message"] == "v1"

            # Introduce a syntax error
            time.sleep(0.05)
            workflow_file.write_text("raise SyntaxError('broken')\n")

            # _ensure_loaded should keep the old version on failure
            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            # Old version still works
            assert result.actions[0].params["message"] == "v1"

            # Fix the workflow
            time.sleep(0.05)
            workflow_file.write_text(_workflow_source("v2"))

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions[0].params["message"] == "v2"

    def test_deleted_workflow_is_unloaded(self) -> None:
        """Deleting a workflow file should unregister its hooks on reload."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflow_file = _create_workflow(
                workflows_dir, "tdw", _workflow_source("hello")
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            # Load workflow
            daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert "tdw" in daemon._workflows

            # Delete the workflow file
            workflow_file.unlink()
            daemon.reload_changed()

            assert "tdw" not in daemon._workflows

            # Hooks should no longer fire
            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions == []
            assert not result.default_prevented


class TestEventDispatch:
    """Tests for event dispatch to workflows."""

    def test_dispatch_to_matching_hook(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_opened(ctx):\n"
                    "    return [ctx.actions.post_to_channel(\n"
                    "        f\"PR #{ctx.pr_number} opened\"\n"
                    "    )]\n"
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 123}, channel_workflow="tdw"
            )

            assert len(result.actions) == 1
            assert result.actions[0].method == "channel.post"
            assert "123" in result.actions[0].params["message"]
            assert not result.default_prevented

    def test_dispatch_unknown_event_returns_empty(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "unknown.event", {}, channel_workflow="tdw"
            )
            assert result.actions == []
            assert not result.default_prevented

    def test_dispatch_no_matching_hook_returns_empty(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.merged", {}, channel_workflow="tdw"
            )
            assert result.actions == []

    def test_plugin_returning_none_is_skipped(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_opened(ctx):\n"
                    "    return None\n"
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )
            assert result.actions == []

    def test_action_serialization(self) -> None:
        """Dispatch returns DaemonAction objects with method and params."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_task_completed(ctx):\n"
                    "    return [\n"
                    '        ctx.actions.post_to_channel("done!"),\n'
                    "        ctx.actions.check_pending(),\n"
                    "    ]\n"
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "task.completed",
                {"task_id": "42"},
                channel_workflow="tdw",
                task_id="42",
            )

            assert len(result.actions) == 2
            assert result.actions[0].method == "channel.post"
            assert result.actions[0].params == {"message": "done!"}
            assert result.actions[1].method == "daemon.check-pending"
            assert result.actions[1].params == {}

    def test_on_event_hook_fires_for_all_events(self) -> None:
        """The global on_event hook should fire alongside specific hooks."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_event(ctx):\n"
                    '    return [ctx.actions.post_to_channel("global")]\n'
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_opened(ctx):\n"
                    '    return [ctx.actions.post_to_channel("specific")]\n'
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )

            assert len(result.actions) == 2
            messages = {a.params["message"] for a in result.actions}
            assert "global" in messages
            assert "specific" in messages

    def test_context_populated_from_event(self) -> None:
        """HookContext fields should be populated from event and kwargs."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_opened(ctx):\n"
                    "    msg = f'{ctx.event_type}:{ctx.pr_number}:{ctx.task_id}'\n"
                    "    return [ctx.actions.post_to_channel(msg)]\n"
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened",
                {"pr_number": 42},
                channel_workflow="tdw",
                task_id="7",
            )

            assert len(result.actions) == 1
            assert result.actions[0].params["message"] == "pr.opened:42:7"


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
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=Path(tmpdir) / "workflows"
            )

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
        """Events dispatched over the socket should return workflow actions."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("socket works"))

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=workflows_dir
            )

            server_task = await _start_daemon(daemon)
            try:
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 42},
                        "channel_workflow": "tdw",
                    },
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
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=Path(tmpdir) / "workflows"
            )

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
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=Path(tmpdir) / "workflows"
            )

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
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("counted"))

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=workflows_dir
            )

            server_task = await _start_daemon(daemon)
            try:
                for i in range(3):
                    response = await _send_request(
                        sock_path,
                        {
                            "type": "pr.opened",
                            "event": {"pr_number": i},
                            "channel_workflow": "tdw",
                        },
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
        """A workflow that raises should not crash the server."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "bad",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_opened(ctx):\n"
                    '    raise RuntimeError("plugin exploded")\n'
                ),
            )

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=workflows_dir
            )

            server_task = await _start_daemon(daemon)
            try:
                # The request should not crash the server
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 1},
                        "channel_workflow": "bad",
                    },
                )
                # Server should still be alive for next request
                response2 = await _send_request(
                    sock_path,
                    {
                        "type": "pr.merged",
                        "event": {},
                        "channel_workflow": "bad",
                    },
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
        """Workflows modified while the server is running should be reloaded."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflow_file = _create_workflow(
                workflows_dir, "tdw", _workflow_source("v1")
            )

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=workflows_dir
            )

            server_task = await _start_daemon(daemon)
            try:
                # First request should see v1
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 1},
                        "channel_workflow": "tdw",
                    },
                )
                assert response["actions"][0]["params"]["message"] == "v1"

                # Update workflow file
                time.sleep(0.05)
                workflow_file.write_text(_workflow_source("v2"))

                # Next request should trigger hot-reload and see v2
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 2},
                        "channel_workflow": "tdw",
                    },
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

            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=Path(tmpdir) / "workflows"
            )

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
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_opened(ctx):\n"
                    "    msg = f'{ctx.task_id}:{ctx.task_state}'\n"
                    "    return [ctx.actions.post_to_channel(msg)]\n"
                ),
            )

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=workflows_dir
            )

            server_task = await _start_daemon(daemon)
            try:
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 42},
                        "channel_workflow": "tdw",
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


class TestPreventDefault:
    """Tests for the prevent_default() / is_default_prevented() API."""

    def test_prevent_default(self) -> None:
        """Plugin calling prevent_default() sets default_prevented in result."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_auto_merge(ctx):\n"
                    "    ctx.prevent_default()\n"
                    "    return []\n"
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.auto_merge", {"pr_number": 1}, channel_workflow="tdw"
            )

            assert result.default_prevented is True
            assert result.actions == []

    def test_prevent_default_with_replacement_actions(self) -> None:
        """Plugin can prevent_default and return replacement actions."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(
                workflows_dir,
                "tdw",
                (
                    "from midtown.hooks import hookimpl\n"
                    "\n"
                    "@hookimpl\n"
                    "def on_pr_opened(ctx):\n"
                    "    ctx.prevent_default()\n"
                    '    return [ctx.actions.post_to_channel("custom handling")]\n'
                ),
            )

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )

            assert result.default_prevented is True
            assert len(result.actions) == 1
            assert result.actions[0].params["message"] == "custom handling"

    def test_no_prevent_default_by_default(self) -> None:
        """Default dispatch without prevent_default keeps default_prevented False."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("hello"))

            daemon = WorkflowDaemon(
                socket_path="/tmp/test.sock",
                workflows_dir=workflows_dir,
            )

            result = daemon.dispatch_event(
                "pr.opened", {"pr_number": 1}, channel_workflow="tdw"
            )

            assert result.default_prevented is False
            assert len(result.actions) == 1


class TestReloadCommand:
    """Tests for the reload command over the socket."""

    @pytest.mark.asyncio
    async def test_reload_command_returns_loaded_workflows(self) -> None:
        """The reload command should return the list of loaded workflows."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            _create_workflow(workflows_dir, "tdw", _workflow_source("hello"))

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=workflows_dir
            )

            server_task = await _start_daemon(daemon)
            try:
                # First load the workflow via a dispatch
                await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 1},
                        "channel_workflow": "tdw",
                    },
                )

                response = await _send_request(sock_path, {"type": "reload"})
                assert response["ok"] is True
                assert response["reloaded"] is True
                assert "tdw" in response["loaded_workflows"]
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass

    @pytest.mark.asyncio
    async def test_reload_unloads_deleted_workflow(self) -> None:
        """A reload command should unload a deleted workflow."""
        with tempfile.TemporaryDirectory() as tmpdir:
            workflows_dir = Path(tmpdir) / "workflows"
            workflow_file = _create_workflow(
                workflows_dir, "tdw", _workflow_source("hello")
            )

            sock_path = str(Path(tmpdir) / "daemon.sock")
            daemon = WorkflowDaemon(
                socket_path=sock_path, workflows_dir=workflows_dir
            )

            server_task = await _start_daemon(daemon)
            try:
                # Load workflow via dispatch
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 1},
                        "channel_workflow": "tdw",
                    },
                )
                assert len(response["actions"]) == 1

                # Delete the workflow
                workflow_file.unlink()

                # Send reload command
                response = await _send_request(sock_path, {"type": "reload"})
                assert response["ok"] is True
                assert response["loaded_workflows"] == []

                # Workflow should no longer fire
                response = await _send_request(
                    sock_path,
                    {
                        "type": "pr.opened",
                        "event": {"pr_number": 2},
                        "channel_workflow": "tdw",
                    },
                )
                assert response["actions"] == []
            finally:
                server_task.cancel()
                try:
                    await server_task
                except asyncio.CancelledError:
                    pass
