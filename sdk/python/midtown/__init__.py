"""Midtown Python SDK.

Provides the ``run()`` and ``run_loop()`` entry points for workflow scripts
and the ``MidtownRPC`` client for calling the daemon's JSON-RPC API over a
Unix socket.

**Single-shot mode** (``run()``)::

    from midtown import run, MidtownRPC

    def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
        if event["type"] == "coworker.idle":
            rpc.post_to_channel(f"{event['coworker']} finished — looking for more work")

    if __name__ == "__main__":
        run(handle)

**Persistent sidecar mode** (``run_loop()``)::

    from midtown import run_loop, MidtownRPC

    def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
        if event["type"] == "coworker.idle":
            rpc.post_to_channel(f"{event['coworker']} finished — looking for more work")

    if __name__ == "__main__":
        run_loop(handle)

The persistent mode reads newline-delimited JSON events from stdin and keeps
the process alive, avoiding the ~300-800ms Python startup overhead per event.
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

    # ------------------------------------------------------------------
    # PR methods
    # ------------------------------------------------------------------

    def spawn_reviewer(self, pr_number: int) -> Any:
        """Request the daemon to spawn a reviewer for a pull request.

        The daemon handles all reviewer setup: worktree creation, name
        selection, assignment tracking, and launch configuration.  The
        workflow script controls *when* to spawn — the daemon controls *how*.

        Parameters
        ----------
        pr_number:
            The GitHub PR number to review.

        Returns
        -------
        A dict with a ``"message"`` key describing the outcome (e.g.
        ``"Reviewer assigned: lexington (PR #42)"``).

        Raises
        ------
        RpcError
            If the PR is not open, already reviewed, or no coworker slots
            are available.
        """
        return self._call("pr.review", {"pr": pr_number})

    # ------------------------------------------------------------------
    # Daemon methods
    # ------------------------------------------------------------------

    def check_pending(self) -> Any:
        """Trigger immediate dispatch of pending tasks.

        Asks the daemon to run its task-dispatch loop right now rather than
        waiting for the next ``TaskDispatchTick``.  Useful after a new task is
        created (``task.created``) or a coworker goes idle (``coworker.idle``)
        so that pending work starts immediately.

        This is an optimisation — the daemon will dispatch eventually on its
        own.  Safe to call from ``try/except`` blocks; a failure here should
        not interrupt the handler.
        """
        return self._call("daemon.check-pending")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _load_state(state_path: Path) -> dict:
    """Load workflow state from disk, returning empty dict if absent.

    Raises on I/O or parse errors so callers can decide how to handle them
    (single-shot ``run()`` exits; persistent ``run_loop()`` reports the error
    and continues processing events).
    """
    if state_path.exists():
        return json.loads(state_path.read_text())
    return {}


def _persist_state(state: dict, state_path: Path) -> None:
    """Atomically persist workflow state to disk (temp file + rename).

    Raises on I/O errors so callers can decide how to handle them.
    """
    state_dir = state_path.parent
    state_dir.mkdir(parents=True, exist_ok=True)
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


# ---------------------------------------------------------------------------
# Entry point — single-shot mode
# ---------------------------------------------------------------------------


def run(
    handler: Callable[[dict, MidtownRPC, dict], None],
) -> None:
    """Parse CLI args, load state, call *handler*, then save state.

    Supports two modes, selected automatically by the daemon:

    **Single-shot mode** (default)::

        uv run workflow.py --event '{"type":"pr.opened",...}' \\
            --state /path/to/workflow-state.json \\
            --socket /path/to/daemon.sock

    **Persistent sidecar mode** (when ``--sidecar`` is passed)::

        uv run workflow.py --sidecar

    In sidecar mode the script stays alive, reading events from stdin.
    Existing workflow scripts work without changes — the daemon tries
    sidecar mode first and falls back to single-shot if the script
    doesn't respond with ``{"ready":true}``.

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
    # Check for sidecar mode early (before argparse, which would reject
    # unknown args or fail on missing --event).
    if "--sidecar" in sys.argv:
        run_loop(handler)
        return

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

    state_path = Path(args.state)
    try:
        state = _load_state(state_path)
    except (json.JSONDecodeError, OSError) as exc:
        print(f"midtown: failed to load state from {state_path}: {exc}", file=sys.stderr)
        sys.exit(1)

    rpc = MidtownRPC(args.socket)
    handler(event, rpc, state)

    try:
        _persist_state(state, state_path)
    except OSError as exc:
        print(f"midtown: failed to save state to {state_path}: {exc}", file=sys.stderr)
        sys.exit(1)


# ---------------------------------------------------------------------------
# Entry point — persistent sidecar mode
# ---------------------------------------------------------------------------


def run_loop(
    handler: Callable[[dict, MidtownRPC, dict], None],
) -> None:
    """Run as a long-lived sidecar, reading events from stdin.

    The daemon spawns the script once and sends newline-delimited JSON
    messages on stdin.  Each message is an envelope::

        {"event": {...}, "state_file": "/path/to/state.json", "socket": "/path/to/daemon.sock"}

    For each message the SDK:

    1. Decodes the event from the envelope.
    2. Loads state from ``state_file`` (if it exists).
    3. Creates an :class:`MidtownRPC` client from ``socket``.
    4. Calls ``handler(event, rpc, state)``.
    5. Persists state back to ``state_file``.
    6. Writes ``{"ok": true}`` to stdout to acknowledge processing.

    On handler errors, writes ``{"ok": false, "error": "..."}`` to stdout
    so the daemon knows the event failed without killing the sidecar.

    The process exits cleanly when stdin is closed (daemon shutdown).

    Parameters
    ----------
    handler:
        Callable with signature ``(event, rpc, state) -> None``.
    """
    # Signal readiness to the daemon so it knows imports are done.
    sys.stdout.write('{"ready":true}\n')
    sys.stdout.flush()

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue

        try:
            envelope = json.loads(line)
        except json.JSONDecodeError as exc:
            _write_response(ok=False, error=f"invalid JSON: {exc}")
            continue

        event = envelope.get("event")
        state_file = envelope.get("state_file", "")
        socket_path = envelope.get("socket", "")

        if not event or not isinstance(event, dict):
            _write_response(ok=False, error="missing or invalid 'event' in envelope")
            continue

        state_path = Path(state_file) if state_file else None
        rpc = MidtownRPC(socket_path)

        try:
            state = _load_state(state_path) if state_path else {}
            handler(event, rpc, state)
            if state_path:
                _persist_state(state, state_path)
            _write_response(ok=True)
        except Exception as exc:
            # Don't crash the sidecar on handler/state errors — report and continue.
            _write_response(ok=False, error=str(exc))


def _write_response(*, ok: bool, error: str | None = None) -> None:
    """Write a JSON ack/nack line to stdout for the daemon."""
    resp: dict[str, Any] = {"ok": ok}
    if error is not None:
        resp["error"] = error
    sys.stdout.write(json.dumps(resp, separators=(",", ":")) + "\n")
    sys.stdout.flush()
