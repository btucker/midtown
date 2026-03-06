"""Tests for SKILL.md frontmatter parsing."""

from __future__ import annotations

import tempfile
from pathlib import Path

from midtown.skill import SkillMetadata, parse_skill_file, parse_skill_frontmatter


class TestParseFrontmatter:
    """Tests for parse_skill_frontmatter."""

    def test_basic_frontmatter(self) -> None:
        text = (
            "---\n"
            "name: tdw\n"
            "description: Test-Driven Writing\n"
            "metadata:\n"
            "  midtown_hooks: scripts/hooks.py\n"
            "  midtown_order: 50\n"
            "---\n"
            "# TDW Plugin\n"
        )
        meta = parse_skill_frontmatter(text)
        assert meta.name == "tdw"
        assert meta.description == "Test-Driven Writing"
        assert meta.hooks_path == "scripts/hooks.py"
        assert meta.order == 50

    def test_defaults_when_no_metadata_block(self) -> None:
        text = "---\nname: simple\n---\n# Simple\n"
        meta = parse_skill_frontmatter(text)
        assert meta.name == "simple"
        assert meta.hooks_path == "scripts/hooks.py"
        assert meta.order == 1000

    def test_no_frontmatter(self) -> None:
        text = "# Just markdown, no frontmatter\n"
        meta = parse_skill_frontmatter(text)
        assert meta.name == ""
        assert meta.hooks_path == "scripts/hooks.py"
        assert meta.order == 1000

    def test_custom_hooks_path(self) -> None:
        text = (
            "---\n"
            "name: custom\n"
            "metadata:\n"
            "  midtown_hooks: my/custom/hooks.py\n"
            "---\n"
            "# Custom\n"
        )
        meta = parse_skill_frontmatter(text)
        assert meta.hooks_path == "my/custom/hooks.py"

    def test_invalid_order_defaults_to_1000(self) -> None:
        text = (
            "---\n"
            "name: bad-order\n"
            "metadata:\n"
            "  midtown_order: not-a-number\n"
            "---\n"
            "# Bad\n"
        )
        meta = parse_skill_frontmatter(text)
        assert meta.order == 1000

    def test_raw_dict_preserved(self) -> None:
        text = (
            "---\n"
            "name: test\n"
            "compatibility: Midtown\n"
            "---\n"
            "# Test\n"
        )
        meta = parse_skill_frontmatter(text)
        assert meta.raw["name"] == "test"
        assert meta.raw["compatibility"] == "Midtown"

    def test_boolean_coercion(self) -> None:
        text = "---\nenabled: true\ndisabled: false\n---\n"
        meta = parse_skill_frontmatter(text)
        assert meta.raw["enabled"] is True
        assert meta.raw["disabled"] is False

    def test_numeric_coercion(self) -> None:
        text = "---\ncount: 42\nratio: 3.14\n---\n"
        meta = parse_skill_frontmatter(text)
        assert meta.raw["count"] == 42
        assert meta.raw["ratio"] == 3.14

    def test_no_trailing_newline(self) -> None:
        """Frontmatter without trailing newline after closing --- should parse."""
        text = "---\nname: test\nmetadata:\n  midtown_order: 42\n---"
        meta = parse_skill_frontmatter(text)
        assert meta.name == "test"
        assert meta.order == 42


class TestParseSkillFile:
    """Tests for parse_skill_file."""

    def test_reads_and_parses_file(self) -> None:
        with tempfile.TemporaryDirectory() as tmpdir:
            skill_md = Path(tmpdir) / "SKILL.md"
            skill_md.write_text(
                "---\nname: from-file\nmetadata:\n  midtown_order: 10\n---\n# From File\n"
            )
            meta = parse_skill_file(skill_md)
            assert meta.name == "from-file"
            assert meta.order == 10

    def test_nonexistent_file(self) -> None:
        meta = parse_skill_file(Path("/nonexistent/SKILL.md"))
        assert meta.name == ""
        assert meta.order == 1000
