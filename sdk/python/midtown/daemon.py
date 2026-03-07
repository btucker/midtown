"""Long-running Python daemon for workflow plugins."""

from __future__ import annotations

import asyncio
import dataclasses
import json
import logging
import os
import sys
import types
from pathlib import Path
from typing import Any

from dataclasses import dataclass, field

from midtown.actions import Actions
from midtown.hooks import DaemonAction, HookContext, get_plugin_manager

logger = logging.getLogger(__name__)


@dataclass
class LoadedWorkflow:
    """Per-workflow state tracking."""

    pm: Any  # pluggy.PluginManager
    mtime: float
    module: types.ModuleType


@dataclass
class DispatchResult:
    """Result of dispatching an event through the plugin system.

    Contains the collected actions from all plugins and whether any plugin
    called ``ctx.prevent_default()`` to suppress daemon defaults.
    """

    actions: list[DaemonAction] = field(default_factory=list)
    """All actions returned by plugins, concatenated."""

    default_prevented: bool = False
    """Whether any plugin called ``ctx.prevent_default()``."""


class WorkflowDaemon:
    """Manages workflows and dispatches events.

    The daemon loads Python workflow modules from a workflows directory,
    where each workflow lives at ``workflows/<name>/workflow.py``.
    Each workflow gets its own pluggy ``PluginManager`` and is loaded
    lazily on first dispatch.

    Hot-reload is supported via mtime tracking — workflows are reloaded
    when their ``workflow.py`` file changes on disk.
    """

    def __init__(self, socket_path: str, workflows_dir: str | Path) -> None:
        self.socket_path = socket_path
        self.workflows_dir = Path(workflows_dir)
        self._workflows: dict[str, LoadedWorkflow] = {}  # name -> loaded state

    def _ensure_loaded(self, name: str) -> Any | None:
        """Load or hot-reload a workflow by name.

        Looks for ``self.workflows_dir / name / "workflow.py"``.
        Tracks mtime per workflow, reloads on change.
        Creates a fresh ``PluginManager`` per workflow (not shared).

        Returns the workflow's ``PluginManager``, or ``None`` if the
        workflow file does not exist or fails to load.
        """
        workflow_file = self.workflows_dir / name / "workflow.py"
        if not workflow_file.exists():
            return None

        current_mtime = workflow_file.stat().st_mtime

        existing = self._workflows.get(name)
        if existing is not None and existing.mtime == current_mtime:
            return existing.pm

        # Need to load or reload
        module = self._load_module(workflow_file, name)
        if module is None:
            # Load failed — keep old version if it exists
            if existing is not None:
                return existing.pm
            return None

        pm = get_plugin_manager()
        pm.register(module, name=name)
        self._workflows[name] = LoadedWorkflow(
            pm=pm,
            mtime=current_mtime,
            module=module,
        )
        logger.info("Loaded workflow: %s (from %s)", name, workflow_file)
        return pm

    def _load_module(self, path: Path, name: str) -> types.ModuleType | None:
        """Load a Python module from a file path.

        Returns the loaded module, or ``None`` on failure.
        """
        try:
            source = path.read_text()
            code = compile(source, str(path), "exec")
            module = types.ModuleType(name)
            module.__file__ = str(path)
            exec(code, module.__dict__)  # noqa: S102
            return module
        except Exception:
            logger.exception("Failed to load workflow module %s", path)
            return None

    def unload_workflow(self, name: str) -> None:
        """Unregister a previously loaded workflow."""
        if name in self._workflows:
            del self._workflows[name]
            logger.info("Unloaded workflow: %s", name)

    def check_for_changes(self) -> list[str]:
        """Return names of tracked workflows whose mtime has changed."""
        changed: list[str] = []
        for name, loaded in list(self._workflows.items()):
            workflow_file = self.workflows_dir / name / "workflow.py"
            if not workflow_file.exists():
                continue
            if workflow_file.stat().st_mtime != loaded.mtime:
                changed.append(name)
        return changed

    def _find_deleted(self) -> list[str]:
        """Return names of tracked workflows whose files no longer exist."""
        deleted: list[str] = []
        for name in self._workflows:
            workflow_file = self.workflows_dir / name / "workflow.py"
            if not workflow_file.exists():
                deleted.append(name)
        return deleted

    def reload_changed(self) -> None:
        """Hot-reload any workflows whose files have changed on disk.

        Checks already-tracked workflows for mtime changes and removes
        deleted workflows. Workflows are loaded lazily on first dispatch,
        so no scanning for new workflows is needed.
        """
        # Unload deleted workflows
        for name in self._find_deleted():
            self.unload_workflow(name)

        # Reload changed workflows
        for name in self.check_for_changes():
            # _ensure_loaded will detect the mtime change and reload
            self._ensure_loaded(name)

    def dispatch_event(
        self,
        event_type: str,
        event: dict[str, Any],
        *,
        channel_workflow: str = "",
        task_id: str | None = None,
        task_state: str | None = None,
        prev_task_state: str | None = None,
        state: dict[str, Any] | None = None,
    ) -> DispatchResult:
        """Dispatch an event to the workflow's hook implementations.

        Uses ``_ensure_loaded(channel_workflow)`` to get the right
        PluginManager. If no workflow is loaded, returns empty
        DispatchResult.
        """
        if not channel_workflow:
            return DispatchResult()

        pm = self._ensure_loaded(channel_workflow)
        if pm is None:
            return DispatchResult()

        ctx = HookContext(
            event_type=event_type,
            event=event,
            task_id=task_id,
            task_state=task_state,
            prev_task_state=prev_task_state,
            coworker=event.get("coworker", ""),
            pr_number=event.get("pr_number"),
            channel=event.get("channel", ""),
            state=state or {},
            actions=Actions(),
        )

        all_actions: list[DaemonAction] = []

        # Global on_event hook
        for result in pm.hook.on_event(ctx=ctx):
            if result:
                all_actions.extend(result)

        # Event-specific hook
        hook_name = f"on_{event_type.replace('.', '_')}"
        hook = getattr(pm.hook, hook_name, None)
        if hook:
            for result in hook(ctx=ctx):
                if result:
                    all_actions.extend(result)

        return DispatchResult(
            actions=all_actions,
            default_prevented=ctx.is_default_prevented(),
        )

    async def run(self) -> None:
        """Start the Unix socket server and process events.

        Listens on :attr:`socket_path` for connections from the Rust daemon.
        Each connection sends one newline-delimited JSON request and receives
        one newline-delimited JSON response.

        Request format::

            {"type": "pr.opened", "event": {...}, "channel_workflow": "tdw", ...}

        Response format::

            {"ok": true, "actions": [...], "default_prevented": false}

        On startup, writes ``{"ready":true}`` to stdout so the Rust daemon
        knows the Python process is initialised and accepting connections.

        Checks for workflow file changes before each dispatch (hot-reload).
        """
        # Clean up stale socket file
        try:
            os.unlink(self.socket_path)
        except FileNotFoundError:
            pass

        server = await asyncio.start_unix_server(
            self._handle_connection,
            path=self.socket_path,
        )
        self._server = server

        logger.info("Workflow daemon listening on %s", self.socket_path)

        # Signal readiness to the Rust daemon
        sys.stdout.write('{"ready":true}\n')
        sys.stdout.flush()

        async with server:
            await server.serve_forever()

    async def _handle_connection(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        """Handle a single connection from the Rust daemon."""
        try:
            line = await reader.readline()
            if not line:
                return

            request = json.loads(line)
            response = self._process_request(request)

            writer.write(json.dumps(response, separators=(",", ":")).encode() + b"\n")
            await writer.drain()
        except json.JSONDecodeError as exc:
            error_resp = {"ok": False, "error": f"invalid JSON: {exc}"}
            writer.write(json.dumps(error_resp, separators=(",", ":")).encode() + b"\n")
            await writer.drain()
        except Exception:
            logger.exception("Error handling connection")
            try:
                error_resp = {"ok": False, "error": "internal error"}
                writer.write(
                    json.dumps(error_resp, separators=(",", ":")).encode() + b"\n"
                )
                await writer.drain()
            except Exception:
                pass
        finally:
            writer.close()
            await writer.wait_closed()

    def _process_request(self, request: dict[str, Any]) -> dict[str, Any]:
        """Process a single request and return the response dict.

        Handles two request types:

        - **``"reload"``** — Forces a reload check (mtime changes,
          deleted workflows).  Sent by the Rust daemon when it detects
          ``.midtown/`` file-system changes.

        - **Event dispatch** (any other ``type`` value) — Hot-reloads changed
          workflows, then dispatches the event to the channel's workflow.
        """
        request_type = request.get("type", "")

        # Handle reload command
        if request_type == "reload":
            self.reload_changed()
            loaded = list(self._workflows.keys())
            return {
                "ok": True,
                "reloaded": True,
                "loaded_workflows": loaded,
            }

        self.reload_changed()

        event_type = request_type
        event = request.get("event", {})
        channel_workflow = request.get("channel_workflow", "")
        task_id = request.get("task_id")
        task_state = request.get("task_state")
        prev_task_state = request.get("prev_task_state")
        state = request.get("state")

        if not event_type:
            return {"ok": False, "error": "missing 'type' field"}

        result = self.dispatch_event(
            event_type,
            event,
            channel_workflow=channel_workflow,
            task_id=task_id,
            task_state=task_state,
            prev_task_state=prev_task_state,
            state=state,
        )

        return {
            "ok": True,
            "actions": [dataclasses.asdict(a) for a in result.actions],
            "default_prevented": result.default_prevented,
        }


if __name__ == "__main__":
    from midtown.__main__ import main

    main()
