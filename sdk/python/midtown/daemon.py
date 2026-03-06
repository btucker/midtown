"""Long-running Python daemon for workflow plugins."""

from __future__ import annotations

import asyncio
import importlib.util
import logging
import uuid
from pathlib import Path
from typing import Any

import pluggy

from midtown.hooks import HookContext, TaskHooks, WorkflowHooks

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

        # Set up plugin manager
        self.pm = pluggy.PluginManager("midtown")
        self.pm.add_hookspecs(WorkflowHooks)
        self.pm.add_hookspecs(TaskHooks)

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
            # Use a unique module name each time to bypass __pycache__
            # bytecode caching, which can serve stale code on hot-reload.
            module_name = f"{path.stem}_{uuid.uuid4().hex[:8]}"
            spec = importlib.util.spec_from_file_location(module_name, path)
            if spec and spec.loader:
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
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

    def reload_changed(self) -> None:
        """Hot-reload any plugins whose files have changed on disk."""
        for path in self.check_for_changes():
            self.unload_plugin(path)
            self.load_plugin(path)

    def dispatch_event(
        self,
        event_type: str,
        event: dict[str, Any],
        context: HookContext,
    ) -> tuple[list[dict[str, Any]], bool]:
        """Dispatch an event to all registered hook implementations.

        Converts *event_type* (e.g. ``"pr.opened"``) to a hook method name
        (``on_pr_opened``) and calls all implementations, collecting returned
        :class:`Action` objects.

        Returns ``(actions, default_prevented)`` where *actions* is a list of
        serialised action dicts and *default_prevented* indicates whether any
        plugin called :meth:`HookContext.prevent_default`.
        """
        hook_name = f"on_{event_type.replace('.', '_')}"

        hook = getattr(self.pm.hook, hook_name, None)
        if not hook:
            return [], False

        results = hook(event=event, context=context)

        all_actions: list[dict[str, Any]] = []
        for result in results:
            if result:
                for action in result:
                    all_actions.append({
                        "type": action.type,
                        "args": action.args,
                    })

        return all_actions, context.is_default_prevented()

    async def run(self) -> None:
        """Main event loop (placeholder).

        The unix socket server will be implemented in a later task.
        """
        logger.info("Starting workflow daemon on %s", self.socket_path)
        # TODO: Implement unix socket server
        while True:
            await asyncio.sleep(1)
