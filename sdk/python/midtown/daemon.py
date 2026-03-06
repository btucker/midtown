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
    """Manages plugins and dispatches events.

    The daemon loads Python plugin files from one or more directories,
    registers them with a pluggy ``PluginManager``, and dispatches events
    by converting event type strings (e.g. ``"pr.opened"``) to hook names
    (``on_pr_opened``).

    Plugins are regular Python modules containing ``@hookimpl``-decorated
    functions.  Files whose names start with ``_`` are skipped.

    Hot-reload is supported via :meth:`check_for_changes` and
    :meth:`reload_changed`, which detect mtime changes and re-register
    the affected modules.
    """

    def __init__(self, socket_path: str, plugin_dirs: list[Path]) -> None:
        self.socket_path = socket_path
        self.plugin_dirs = plugin_dirs

        # Set up plugin manager using the shared factory
        self.pm = get_plugin_manager()

        # Track loaded plugins for hot-reload
        self._loaded_plugins: dict[Path, Any] = {}
        self._mtimes: dict[Path, float] = {}

        # Load initial plugins
        for plugin_dir in plugin_dirs:
            self.load_plugins_from(plugin_dir)

    def load_plugins_from(self, directory: Path) -> None:
        """Load all plugin files from *directory* (recursively).

        Skips files whose names start with ``_`` (e.g. ``__init__.py``).
        Non-existent directories are silently ignored.
        """
        if not directory.exists():
            return

        for plugin_file in sorted(directory.glob("**/*.py")):
            if plugin_file.name.startswith("_"):
                continue
            self.load_plugin(plugin_file)

    def load_plugin(self, path: Path) -> None:
        """Load and register a single plugin file."""
        try:
            # Read source directly and compile to bypass bytecode caching.
            # importlib.util.spec_from_file_location keys .pyc files by
            # source path, so unique module names don't help on hot-reload.
            source = path.read_text()
            code = compile(source, str(path), "exec")
            module = types.ModuleType(path.stem)
            module.__file__ = str(path)
            exec(code, module.__dict__)  # noqa: S102
            self.pm.register(module)
            self._loaded_plugins[path] = module
            self._mtimes[path] = path.stat().st_mtime
            logger.info("Loaded plugin: %s", path)
        except Exception:
            logger.exception("Failed to load plugin %s", path)

    def unload_plugin(self, path: Path) -> None:
        """Unregister a previously loaded plugin."""
        if path in self._loaded_plugins:
            self.pm.unregister(self._loaded_plugins[path])
            del self._loaded_plugins[path]
            del self._mtimes[path]
            logger.info("Unloaded plugin: %s", path)

    def check_for_changes(self) -> list[Path]:
        """Return paths of plugin files whose mtime has changed."""
        changed: list[Path] = []
        for path, old_mtime in list(self._mtimes.items()):
            if not path.exists():
                continue
            if path.stat().st_mtime != old_mtime:
                changed.append(path)
        return changed

    def _find_deleted(self) -> list[Path]:
        """Return paths of tracked plugins whose files no longer exist."""
        return [p for p in self._mtimes if not p.exists()]

    def reload_changed(self) -> None:
        """Hot-reload any plugins whose files have changed on disk."""
        # Unload deleted plugins
        for path in self._find_deleted():
            self.unload_plugin(path)

        for path in self.check_for_changes():
            old_mtime = self._mtimes.get(path)
            self.unload_plugin(path)
            self.load_plugin(path)
            # If load failed, preserve tracking so the plugin is retried
            # on the next check when the file is fixed.
            if path not in self._mtimes and old_mtime is not None:
                self._mtimes[path] = old_mtime

    def dispatch_event(
        self,
        event_type: str,
        event: dict[str, Any],
        *,
        task_id: str | None = None,
        task_state: str | None = None,
        prev_task_state: str | None = None,
        state: dict[str, Any] | None = None,
    ) -> DispatchResult:
        """Dispatch an event to all registered hook implementations.

        Constructs a :class:`HookContext` and invokes both the global
        ``on_event`` hook and the event-specific hook (e.g. ``on_pr_opened``).

        Returns a :class:`DispatchResult` containing the collected actions
        and whether any plugin called ``ctx.prevent_default()``.
        """
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
        for result in self.pm.hook.on_event(ctx=ctx):
            if result:
                all_actions.extend(result)

        # Event-specific hook
        hook_name = f"on_{event_type.replace('.', '_')}"
        hook = getattr(self.pm.hook, hook_name, None)
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

            {"type": "pr.opened", "event": {...}, "task_id": "7", ...}

        Response format::

            {"ok": true, "actions": [...], "default_prevented": false}

        On startup, writes ``{"ready":true}`` to stdout so the Rust daemon
        knows the Python process is initialised and accepting connections.

        Checks for plugin file changes before each dispatch (hot-reload).
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
        """Process a single event request and return the response dict.

        Hot-reloads changed plugins before dispatching.
        """
        self.reload_changed()

        event_type = request.get("type", "")
        event = request.get("event", {})
        task_id = request.get("task_id")
        task_state = request.get("task_state")
        prev_task_state = request.get("prev_task_state")
        state = request.get("state")

        if not event_type:
            return {"ok": False, "error": "missing 'type' field"}

        result = self.dispatch_event(
            event_type,
            event,
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
