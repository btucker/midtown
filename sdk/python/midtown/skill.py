"""SKILL.md frontmatter parser for AgentSkills-format plugins.

Each AgentSkills plugin is a directory containing:
- ``SKILL.md`` — YAML frontmatter with metadata + markdown body
- ``scripts/hooks.py`` — Python hook implementations (or a custom path via frontmatter)

The frontmatter provides metadata used by the plugin system::

    ---
    name: tdw
    description: Test-Driven Writing workflow
    metadata:
      midtown_hooks: scripts/hooks.py
      midtown_order: 50
    ---
    # Markdown body (loaded into channel-lead prompt)

The ``midtown_hooks`` field specifies the path (relative to SKILL.md) to the
Python module containing ``@hookimpl`` functions.  Defaults to ``scripts/hooks.py``.

The ``midtown_order`` field controls plugin execution order (lower = earlier).
Defaults to 1000.
"""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# Match YAML frontmatter delimited by --- on its own line.
_FRONTMATTER_RE = re.compile(r"\A---\s*\n(.*?)\n---\s*\n", re.DOTALL)


@dataclass
class SkillMetadata:
    """Parsed metadata from a SKILL.md frontmatter block."""

    name: str = ""
    """Plugin name from frontmatter."""

    description: str = ""
    """Human-readable description."""

    hooks_path: str = "scripts/hooks.py"
    """Relative path to the Python hooks module."""

    order: int = 1000
    """Execution order (lower = earlier). Default 1000."""

    raw: dict[str, Any] = field(default_factory=dict)
    """The full parsed frontmatter dict."""


def parse_skill_frontmatter(text: str) -> SkillMetadata:
    """Parse YAML frontmatter from a SKILL.md file's text content.

    Uses a minimal YAML subset parser to avoid adding a PyYAML dependency.
    Handles the fields needed by Midtown: ``name``, ``description``, and
    nested ``metadata.midtown_hooks`` / ``metadata.midtown_order``.
    """
    match = _FRONTMATTER_RE.match(text)
    if not match:
        return SkillMetadata()

    raw = _parse_simple_yaml(match.group(1))
    metadata_block = raw.get("metadata", {})
    if not isinstance(metadata_block, dict):
        metadata_block = {}

    hooks_path = metadata_block.get("midtown_hooks", "scripts/hooks.py")
    order_raw = metadata_block.get("midtown_order", 1000)
    try:
        order = int(order_raw)
    except (TypeError, ValueError):
        order = 1000

    return SkillMetadata(
        name=str(raw.get("name", "")),
        description=str(raw.get("description", "")),
        hooks_path=str(hooks_path),
        order=order,
        raw=raw,
    )


def parse_skill_file(path: Path) -> SkillMetadata:
    """Read and parse a SKILL.md file, returning its metadata."""
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        logger.warning("Failed to read SKILL.md: %s", path)
        return SkillMetadata()
    return parse_skill_frontmatter(text)


def _parse_simple_yaml(text: str) -> dict[str, Any]:
    """Parse a minimal YAML subset (flat keys + one level of nesting).

    Supports:
    - ``key: value`` (strings, numbers)
    - ``key:`` followed by indented ``subkey: value`` (one-level dict)

    This avoids requiring PyYAML for the small subset we need.
    """
    result: dict[str, Any] = {}
    current_key: str | None = None
    current_dict: dict[str, Any] | None = None

    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue

        indent = len(line) - len(line.lstrip())

        if indent > 0 and current_key is not None and current_dict is not None:
            # Indented line under a parent key
            if ":" in stripped:
                k, _, v = stripped.partition(":")
                current_dict[k.strip()] = _coerce_value(v.strip())
            continue

        # Top-level key
        if ":" in stripped:
            k, _, v = stripped.partition(":")
            k = k.strip()
            v = v.strip()
            if v:
                result[k] = _coerce_value(v)
                current_key = None
                current_dict = None
            else:
                # Key with no inline value — start collecting nested dict
                current_key = k
                current_dict = {}
                result[k] = current_dict

    return result


def _coerce_value(v: str) -> Any:
    """Coerce a YAML string value to int, bool, or str."""
    if v.lower() in ("true", "yes"):
        return True
    if v.lower() in ("false", "no"):
        return False
    try:
        return int(v)
    except ValueError:
        pass
    try:
        return float(v)
    except ValueError:
        pass
    return v
