"""Midtown Python SDK.

Provides the ``run()`` entry point for workflow scripts and the
``MidtownRPC`` client for calling the daemon's JSON-RPC API over a
Unix socket.

Typical usage in a ``workflow.py``::

    from midtown import run, MidtownRPC

    def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
        if event["type"] == "coworker.idle":
            rpc.post_to_channel(f"{event['coworker']} finished — looking for more work")

    if __name__ == "__main__":
        run(handle)
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import sys
import tempfile
import uuid
from pathlib import Path
from typing import Any, Callable


class RpcError(Exception):
    """Raised when the daemon returns a JSON-RPC error response."""

    def __init__(self, code: int, message: str, data: Any = None) -> None:
        super().__init__(f"RPC error {code}: {message}")
        self.code = code
        self.data = data


class MidtownRPC:
    """JSON-RPC 2.0 client that talks to the midtown daemon over a Unix socket.

    Each method call opens a connection, sends one request, reads one response,
    and closes the connection.  This keeps the implementation simple and avoids
    connection-state issues in the subprocess-per-event model.

    Parameters
    ----------
    socket_path:
        Path to the daemon's Unix domain socket (passed via ``--socket``).
    """

    def __init__(self, socket_path: str) -> None:
        self._socket_path = socket_path

    # ------------------------------------------------------------------
    # Transport
    # ------------------------------------------------------------------

    def _call(self, method: str, params: dict[str, Any] | None = None) -> Any:
        """Send a JSON-RPC request and return the result.

        Raises :class:`RpcError` if the daemon responds with an error object.

        Request IDs are UUID4 strings to avoid cross-process collisions.  The
        daemon caches successful RPC responses by request ID for 60 seconds
        (``src/daemon/rpc.rs``); a simple per-process counter resets to ``1``
        on every ``uv run`` invocation, so two different workflow calls could
        share the same ID within the cache TTL and one would receive a stale
        result.  UUIDs are effectively unique across processes.
        """
        request_id = str(uuid.uuid4())

        request = {
            "jsonrpc": "2.0",
            "method": method,
            "id": request_id,
        }
        if params:
            request["params"] = params

        line = json.dumps(request, separators=(",", ":")) + "\n"

        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.connect(self._socket_path)
            sock.sendall(line.encode())

            # Read until newline — responses are newline-delimited JSON
            buf = b""
            while True:
                chunk = sock.recv(4096)
                if not chunk:
                    break
                buf += chunk
                if b"\n" in buf:
                    break

        response_line = buf.split(b"\n", 1)[0]
        response = json.loads(response_line)

        if "error" in response and response["error"] is not None:
            err = response["error"]
            raise RpcError(err["code"], err["message"], err.get("data"))

        return response.get("result")

    # ------------------------------------------------------------------
    # Channel methods
    # ------------------------------------------------------------------

    def post_to_channel(
        self,
        message: str,
        *,
        channel: str | None = None,
        sender: str | None = None,
        thread_parent_id: str | None = None,
    ) -> Any:
        """Post a message to a channel.

        Parameters
        ----------
        message:
            The message text to post.
        channel:
            Target channel name.  Uses the daemon's default channel when omitted.
        sender:
            Display name for the message author.  Defaults to the repo name.
        thread_parent_id:
            If set, posts as a reply in the thread rooted at this message ID.
        """
        params: dict[str, Any] = {"message": message}
        if channel is not None:
            params["channel"] = channel
        if sender is not None:
            params["from"] = sender
        if thread_parent_id is not None:
            params["thread_parent_id"] = thread_parent_id
        return self._call("channel.post", params)

    # ------------------------------------------------------------------
    # Task methods
    # ------------------------------------------------------------------

    def create_task(
        self,
        subject: str,
        *,
        description: str = "",
        channel: str | None = None,
        blocked_by: list[str] | None = None,
        model: str | None = None,
    ) -> Any:
        """Create a new task.

        Parameters
        ----------
        subject:
            One-line imperative task title (e.g. ``"Fix auth timeout"``).
        description:
            Optional multi-line task body.
        channel:
            Channel to associate the task with.
        blocked_by:
            List of task IDs that must complete before this one is dispatched.
        model:
            Provider/model string (e.g. ``"claude/sonnet"``).
        """
        params: dict[str, Any] = {"subject": subject}
        if description:
            params["description"] = description
        if channel is not None:
            params["channel"] = channel
        if blocked_by is not None:
            params["blocked_by"] = blocked_by
        if model is not None:
            params["model"] = model
        return self._call("task.create", params)

    def update_task(
        self,
        task_id: str,
        *,
        owner: str | None = None,
        status: str | None = None,
        description: str | None = None,
        blocked_by: list[str] | None = None,
        channel: str | None = None,
        model: str | None = None,
        pr: int | None = None,
    ) -> Any:
        """Update an existing task.

        Parameters
        ----------
        task_id:
            The task ID to update.
        owner:
            Assign the task to this coworker name.
        status:
            New status (``"pending"``, ``"in_progress"``, or ``"completed"``).
        description:
            Replace the task description.
        blocked_by:
            Replace the blocked-by list.
        channel:
            Reassign to a different channel.
        model:
            Change the execution model.
        pr:
            Associate a GitHub PR number.
        """
        params: dict[str, Any] = {"id": task_id}
        if owner is not None:
            params["owner"] = owner
        if status is not None:
            params["status"] = status
        if description is not None:
            params["description"] = description
        if blocked_by is not None:
            params["blocked_by"] = blocked_by
        if channel is not None:
            params["channel"] = channel
        if model is not None:
            params["model"] = model
        if pr is not None:
            params["pr"] = pr
        return self._call("task.update", params)

    def complete_task(self, task_id: str) -> Any:
        """Mark a task as done.

        Parameters
        ----------
        task_id:
            The task ID to complete.
        """
        return self._call("task.done", {"id": task_id})

    def list_tasks(self) -> Any:
        """Return the current task list (kanban data)."""
        return self._call("kanban.data")

    # ------------------------------------------------------------------
    # Coworker methods
    # ------------------------------------------------------------------

    def spawn_coworker(
        self,
        *,
        prompt: str | None = None,
        resume: bool = False,
    ) -> Any:
        """Spawn a new coworker session.

        Parameters
        ----------
        prompt:
            Initial prompt to send to the coworker after spawning.
        resume:
            If ``True``, resume the most-recently-stopped coworker session.
        """
        params: dict[str, Any] = {"resume": resume}
        if prompt is not None:
            params["prompt"] = prompt
        return self._call("coworker.spawn", params)

    def nudge_coworker(
        self,
        name: str,
        message: str,
        *,
        sender: str | None = None,
    ) -> Any:
        """Send a nudge message to an existing coworker.

        Parameters
        ----------
        name:
            The coworker's name (e.g. ``"lexington"``).
        message:
            The nudge content.
        sender:
            Display name for the nudge sender.  Defaults to the repo name.
        """
        params: dict[str, Any] = {"name": name, "message": message}
        if sender is not None:
            params["from"] = sender
        return self._call("coworker.nudge", params)


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def run(
    handler: Callable[[dict, MidtownRPC, dict], None],
) -> None:
    """Parse CLI args, load state, call *handler*, then save state.

    This is the standard entry point for workflow scripts.  The daemon
    invokes the script as::

        uv run workflow.py --event '{"type":"pr.opened",...}' \\
            --state /path/to/workflow-state.json \\
            --socket /path/to/daemon.sock

    The handler receives:

    - ``event`` — the decoded event dict (always contains ``"type"`` and
      ``"channel"``; other fields depend on the event type).
    - ``rpc`` — a :class:`MidtownRPC` instance connected to the daemon.
    - ``state`` — the mutable workflow state dict, pre-loaded from the
      state file.  Mutate it freely; it will be persisted after the
      handler returns.

    Parameters
    ----------
    handler:
        Callable with signature ``(event, rpc, state) -> None``.
    """
    parser = argparse.ArgumentParser(description="Midtown workflow script runner")
    parser.add_argument(
        "--event",
        required=True,
        help="JSON-encoded event object from the daemon",
    )
    parser.add_argument(
        "--state",
        required=True,
        help="Path to the persistent workflow state JSON file",
    )
    parser.add_argument(
        "--socket",
        required=True,
        help="Path to the midtown daemon Unix socket",
    )
    args = parser.parse_args()

    # Decode event
    try:
        event: dict = json.loads(args.event)
    except json.JSONDecodeError as exc:
        print(f"midtown: failed to parse --event JSON: {exc}", file=sys.stderr)
        sys.exit(1)

    # Load state (empty dict if file doesn't exist yet)
    state_path = Path(args.state)
    if state_path.exists():
        try:
            state: dict = json.loads(state_path.read_text())
        except (json.JSONDecodeError, OSError) as exc:
            print(
                f"midtown: failed to load state from {state_path}: {exc}",
                file=sys.stderr,
            )
            sys.exit(1)
    else:
        state = {}

    rpc = MidtownRPC(args.socket)

    # Invoke handler
    handler(event, rpc, state)

    # Persist state atomically: write to a temp file next to the target,
    # then rename.  This prevents partial writes from corrupting state.
    state_dir = state_path.parent
    state_dir.mkdir(parents=True, exist_ok=True)
    try:
        fd, tmp_path = tempfile.mkstemp(dir=state_dir, suffix=".tmp")
        try:
            with os.fdopen(fd, "w") as f:
                json.dump(state, f, indent=2)
            os.replace(tmp_path, state_path)
        except Exception:
            try:
                os.unlink(tmp_path)
            except OSError:
                pass
            raise
    except OSError as exc:
        print(
            f"midtown: failed to save state to {state_path}: {exc}",
            file=sys.stderr,
        )
        sys.exit(1)
