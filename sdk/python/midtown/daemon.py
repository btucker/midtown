"""Long-running Python daemon for workflow plugins."""

from __future__ import annotations

import asyncio
import logging
import types
from pathlib import Path
from typing import Any

from midtown.actions import Actions
from midtown.hooks import DaemonAction, HookContext, get_plugin_manager

logger = logging.getLogger(__name__)


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
    ) -> list[DaemonAction]:
        """Dispatch an event to all registered hook implementations.

        Constructs a :class:`HookContext` and invokes both the global
        ``on_event`` hook and the event-specific hook (e.g. ``on_pr_opened``).

        Returns a flat list of :class:`DaemonAction` objects from all plugins.
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

        return all_actions

    async def run(self) -> None:
        """Main event loop (placeholder).

        The unix socket server will be implemented in a later task.
        """
        logger.info("Starting workflow daemon on %s", self.socket_path)
        # TODO: Implement unix socket server
        while True:
            await asyncio.sleep(1)
