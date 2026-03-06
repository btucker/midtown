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
from midtown.skill import SkillMetadata, parse_skill_file

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

        # Track AgentSkills metadata (keyed by plugin directory)
        self._skill_metadata: dict[Path, SkillMetadata] = {}
        # Reverse map: hooks file path → plugin directory (for cleanup)
        self._hooks_to_skill_dir: dict[Path, Path] = {}

        # Load initial plugins, sorted by midtown_order so that
        # lower-order plugins execute first (pluggy uses LIFO).
        self._load_all_plugins(plugin_dirs)

    def _load_all_plugins(self, plugin_dirs: list[Path]) -> None:
        """Discover all plugins, sort by midtown_order, and register.

        Pluggy dispatches hooks in LIFO order (last registered = first called),
        so we register in *descending* order so that plugins with a lower
        ``midtown_order`` execute first.

        Bare ``.py`` files get a default order of 1000 (same as AgentSkills
        plugins without an explicit ``midtown_order``).
        """
        # Collect (order, kind, path_or_dir) tuples
        pending: list[tuple[int, str, Path, Path | None]] = []

        for plugin_dir in plugin_dirs:
            if not plugin_dir.exists():
                continue
            for entry in sorted(plugin_dir.iterdir()):
                if entry.is_file() and entry.suffix == ".py" and not entry.name.startswith("_"):
                    pending.append((1000, "bare", entry, None))
                elif entry.is_dir():
                    skill_md = entry / "SKILL.md"
                    if skill_md.exists():
                        metadata = parse_skill_file(skill_md)
                        pending.append((metadata.order, "agentskills", entry, skill_md))

        # Sort descending by order so that lowest-order plugins register
        # last → called first by pluggy's LIFO dispatch. Python's stable
        # sort preserves alphabetical ordering for equal-order plugins.
        pending.sort(key=lambda t: t[0], reverse=True)
        for _order, kind, path, skill_md in pending:
            if kind == "bare":
                self.load_plugin(path)
            else:
                self._load_agentskills_plugin(path, skill_md)  # type: ignore[arg-type]

    def load_plugins_from(self, directory: Path) -> None:
        """Load all plugins from *directory*.

        Supports two formats:

        1. **Bare ``.py`` files** — loaded directly as plugin modules.
           Skips files whose names start with ``_`` (e.g. ``__init__.py``).

        2. **AgentSkills directories** — subdirectories containing a
           ``SKILL.md`` file with YAML frontmatter.  The ``midtown_hooks``
           metadata field specifies the hooks module path (defaults to
           ``scripts/hooks.py``).

        Non-existent directories are silently ignored.
        """
        if not directory.exists():
            return

        for entry in sorted(directory.iterdir()):
            if entry.is_file() and entry.suffix == ".py" and not entry.name.startswith("_"):
                # Bare .py plugin file
                self.load_plugin(entry)
            elif entry.is_dir():
                # Check for AgentSkills format (directory with SKILL.md)
                skill_md = entry / "SKILL.md"
                if skill_md.exists():
                    self._load_agentskills_plugin(entry, skill_md)

    def load_plugin(self, path: Path, *, plugin_name: str | None = None) -> None:
        """Load and register a single plugin file.

        *plugin_name* overrides the default module name (``path.stem``).
        This is needed when multiple plugins share the same filename
        (e.g. ``scripts/hooks.py``) to avoid pluggy duplicate name errors.
        """
        try:
            # Read source directly and compile to bypass bytecode caching.
            # importlib.util.spec_from_file_location keys .pyc files by
            # source path, so unique module names don't help on hot-reload.
            source = path.read_text()
            code = compile(source, str(path), "exec")
            name = plugin_name or path.stem
            module = types.ModuleType(name)
            module.__file__ = str(path)
            exec(code, module.__dict__)  # noqa: S102
            self.pm.register(module, name=name)
            self._loaded_plugins[path] = module
            self._mtimes[path] = path.stat().st_mtime
            logger.info("Loaded plugin: %s", path)
        except Exception:
            logger.exception("Failed to load plugin %s", path)

    def _load_agentskills_plugin(self, plugin_dir: Path, skill_md: Path) -> None:
        """Load an AgentSkills-format plugin from a directory with SKILL.md.

        Parses the SKILL.md frontmatter to determine the hooks module path
        and execution order, then loads the hooks module as a plugin.
        """
        metadata = parse_skill_file(skill_md)
        hooks_path = plugin_dir / metadata.hooks_path

        if not hooks_path.exists():
            logger.warning(
                "AgentSkills plugin %s: hooks file not found at %s",
                plugin_dir.name,
                hooks_path,
            )
            return

        self._skill_metadata[plugin_dir] = metadata
        self._hooks_to_skill_dir[hooks_path] = plugin_dir
        # Use a unique plugin name to avoid pluggy duplicate name errors
        # when multiple AgentSkills share the same hooks filename.
        unique_name = f"agentskills_{metadata.name or plugin_dir.name}"
        self.load_plugin(hooks_path, plugin_name=unique_name)
        logger.info(
            "Loaded AgentSkills plugin: %s (order=%d, hooks=%s)",
            metadata.name or plugin_dir.name,
            metadata.order,
            metadata.hooks_path,
        )

    def get_skill_metadata(self) -> dict[Path, SkillMetadata]:
        """Return metadata for all loaded AgentSkills plugins."""
        return dict(self._skill_metadata)

    def unload_plugin(self, path: Path) -> None:
        """Unregister a previously loaded plugin."""
        if path in self._loaded_plugins:
            self.pm.unregister(self._loaded_plugins[path])
            del self._loaded_plugins[path]
            del self._mtimes[path]
            # Clean up AgentSkills metadata if this was an AgentSkills hook
            skill_dir = self._hooks_to_skill_dir.pop(path, None)
            if skill_dir is not None:
                self._skill_metadata.pop(skill_dir, None)
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

    def _get_plugin_order(self, path: Path) -> int:
        """Return the midtown_order for a loaded plugin.

        AgentSkills plugins use their SKILL.md order; bare .py plugins
        default to 1000.
        """
        skill_dir = self._hooks_to_skill_dir.get(path)
        if skill_dir is not None:
            meta = self._skill_metadata.get(skill_dir)
            if meta is not None:
                return meta.order
        return 1000

    def _reorder_plugins(self) -> None:
        """Unregister all plugins from pluggy and re-register in order.

        Pluggy dispatches in LIFO order, so we register in descending
        midtown_order so that lower-order plugins run first.
        """
        # Build list of (order, path, module) for all loaded plugins.
        entries = []
        for path, module in list(self._loaded_plugins.items()):
            order = self._get_plugin_order(path)
            entries.append((order, path, module))

        # Unregister all from pluggy (but keep our tracking dicts intact).
        for _, _, module in entries:
            self.pm.unregister(module)

        # Re-register in descending order (lowest order registers last → runs first).
        entries.sort(key=lambda t: t[0], reverse=True)
        for _, path, module in entries:
            plugin_name = getattr(module, "__name__", path.stem)
            self.pm.register(module, name=plugin_name)

    def scan_for_new_plugins(self) -> list[Path]:
        """Scan plugin directories for new plugins not yet loaded.

        Returns a list of newly discovered plugin paths that were loaded.
        This complements :meth:`check_for_changes` which only watches
        already-tracked files.
        """
        newly_loaded: list[Path] = []
        for directory in self.plugin_dirs:
            if not directory.exists():
                continue
            for entry in sorted(directory.iterdir()):
                if entry.is_file() and entry.suffix == ".py" and not entry.name.startswith("_"):
                    if entry not in self._loaded_plugins and entry not in self._mtimes:
                        self.load_plugin(entry)
                        if entry in self._loaded_plugins:
                            newly_loaded.append(entry)
                elif entry.is_dir():
                    skill_md = entry / "SKILL.md"
                    if skill_md.exists() and entry not in self._skill_metadata:
                        # Check if the hooks file for this skill is already loaded
                        metadata = parse_skill_file(skill_md)
                        hooks_path = entry / metadata.hooks_path
                        if hooks_path not in self._loaded_plugins:
                            self._load_agentskills_plugin(entry, skill_md)
                            if hooks_path in self._loaded_plugins:
                                newly_loaded.append(hooks_path)
        return newly_loaded

    def reload_changed(self) -> None:
        """Hot-reload any plugins whose files have changed on disk.

        After reloading, all plugins are re-registered in midtown_order
        to preserve correct execution order (pluggy LIFO).

        Also scans for new plugins that appeared in the plugin directories.
        """
        # Unload deleted plugins
        for path in self._find_deleted():
            self.unload_plugin(path)

        changed = self.check_for_changes()
        if not changed:
            return

        for path in changed:
            old_mtime = self._mtimes.get(path)
            # Remember if this was an AgentSkills plugin so we can
            # re-load it correctly (re-parsing SKILL.md for metadata).
            skill_dir = self._hooks_to_skill_dir.get(path)
            self.unload_plugin(path)
            if skill_dir is not None:
                skill_md = skill_dir / "SKILL.md"
                if skill_md.exists():
                    self._load_agentskills_plugin(skill_dir, skill_md)
            else:
                self.load_plugin(path)
            # If load failed, preserve tracking so the plugin is retried
            # on the next check when the file is fixed.
            if path not in self._mtimes and old_mtime is not None:
                self._mtimes[path] = old_mtime

        # Re-register all plugins in correct midtown_order to maintain
        # execution order after LIFO disruption from individual reloads.
        self._reorder_plugins()

        # Discover newly added plugins
        self.scan_for_new_plugins()

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
        """Process a single request and return the response dict.

        Handles two request types:

        - **``"reload"``** — Forces a full reload check (mtime changes,
          deleted files, and new plugin discovery).  Sent by the Rust daemon
          when it detects ``.midtown/`` file-system changes.

        - **Event dispatch** (any other ``type`` value) — Hot-reloads changed
          plugins, then dispatches the event to all registered hooks.
        """
        request_type = request.get("type", "")

        # Handle reload command
        if request_type == "reload":
            self.reload_changed()
            loaded = list(self._loaded_plugins.keys())
            return {
                "ok": True,
                "reloaded": True,
                "loaded_plugins": [str(p) for p in loaded],
            }

        self.reload_changed()

        event_type = request_type
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
