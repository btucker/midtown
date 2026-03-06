"""Tests for TDW hooks -- AgentSkills-format hook implementations."""

from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import MagicMock

from hooks import (
    DEFAULT_CRITERIA,
    DEFAULT_PATTERNS,
    _build_critique_prompt,
    _find_active_task_id,
    on_channel_message,
    on_coworker_message,
    on_task_created,
)


def _make_context(state: dict | None = None) -> MagicMock:
    """Create a mock HookContext with an in-memory state store."""
    ctx = MagicMock()
    store: dict = state if state is not None else {}

    def get_state(key: str):
        parts = key.split(".")
        current = store
        for part in parts:
            if not isinstance(current, dict) or part not in current:
                return None
            current = current[part]
        return current

    def set_state(key: str, value):
        parts = key.split(".")
        current = store
        for part in parts[:-1]:
            current = current.setdefault(part, {})
        current[parts[-1]] = value

    ctx.rpc.get_state = MagicMock(side_effect=get_state)
    ctx.rpc.set_state = MagicMock(side_effect=set_state)
    ctx._store = store  # expose for assertions
    return ctx


def _make_event(**kwargs) -> SimpleNamespace:
    """Create a simple event object with given attributes."""
    return SimpleNamespace(**kwargs)


def _action_messages(actions):
    """Extract message strings from a list of DaemonActions."""
    if actions is None:
        return []
    return [a.params.get("message", "") for a in actions if a.method == "channel.post"]


def _has_spawn(actions):
    """Check if any action is a coworker spawn."""
    if actions is None:
        return False
    return any(a.method == "coworker.spawn" for a in actions)


# ---------------------------------------------------------------------------
# on_task_created
# ---------------------------------------------------------------------------


class TestOnTaskCreated:
    def test_initialises_state_and_returns_actions(self) -> None:
        ctx = _make_context()
        event = _make_event(task_id="10", subject="Write blog post")

        actions = on_task_created(event, ctx)

        assert ctx._store["tasks"]["10"]["stage"] == "research"
        assert ctx._store["tasks"]["10"]["criteria"] == DEFAULT_CRITERIA
        assert ctx._store["tasks"]["10"]["patterns"] == DEFAULT_PATTERNS
        assert ctx._store["tasks"]["10"]["revision_count"] == 0

        msgs = _action_messages(actions)
        assert len(msgs) == 1
        assert "Write blog post" in msgs[0]
        assert "Research" in msgs[0]

    def test_no_task_id_returns_none(self) -> None:
        ctx = _make_context()
        event = _make_event(subject="No ID")

        actions = on_task_created(event, ctx)

        assert actions is None
        assert "tasks" not in ctx._store

    def test_copies_defaults(self) -> None:
        """Ensure criteria/patterns are copies, not shared references."""
        ctx = _make_context()
        event = _make_event(task_id="42", subject="Test")

        on_task_created(event, ctx)
        ctx._store["tasks"]["42"]["criteria"].append("extra")

        assert "extra" not in DEFAULT_CRITERIA


# ---------------------------------------------------------------------------
# on_coworker_message -- stage transitions
# ---------------------------------------------------------------------------


class TestResearchToOutline:
    def test_research_complete_advances_stage(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)

        actions = on_coworker_message(
            _make_event(task_id="10", message="Research complete, moving on"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "outline"
        msgs = _action_messages(actions)
        assert any("Outline" in m for m in msgs)

    def test_research_complete_case_insensitive(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)

        on_coworker_message(
            _make_event(task_id="10", message="RESEARCH COMPLETE"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "outline"


class TestOutlineToDraft:
    def test_outline_ready_advances_stage(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "outline"

        actions = on_coworker_message(
            _make_event(task_id="10", message="Outline ready for review"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "draft"
        msgs = _action_messages(actions)
        assert any("Draft" in m for m in msgs)


class TestDraftToCritique:
    def test_draft_complete_spawns_critic(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "draft"

        actions = on_coworker_message(
            _make_event(task_id="10", message="Draft complete"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "critique"
        assert _has_spawn(actions)

        # Verify the critique prompt includes criteria
        spawn_action = [a for a in actions if a.method == "coworker.spawn"][0]
        prompt = spawn_action.params["prompt"]
        assert "CRITERIA" in prompt
        assert "PATTERNS" in prompt
        for criterion in DEFAULT_CRITERIA:
            assert criterion in prompt

    def test_draft_complete_uses_custom_criteria(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "draft"
        ctx._store["tasks"]["10"]["criteria"] = ["Custom rule 1"]
        ctx._store["tasks"]["10"]["patterns"] = ["Custom pattern 1"]

        actions = on_coworker_message(
            _make_event(task_id="10", message="Draft complete"), ctx
        )

        spawn_action = [a for a in actions if a.method == "coworker.spawn"][0]
        prompt = spawn_action.params["prompt"]
        assert "Custom rule 1" in prompt
        assert "Custom pattern 1" in prompt


class TestCritiqueResults:
    def test_all_criteria_pass_goes_to_final(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "critique"

        actions = on_coworker_message(
            _make_event(task_id="10", message="CRITIQUE COMPLETE - 0 criteria failed"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "final"
        msgs = _action_messages(actions)
        assert any("All criteria passed" in m for m in msgs)
        assert any("Final Review" in m for m in msgs)

    def test_failures_go_to_revise(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "critique"

        actions = on_coworker_message(
            _make_event(task_id="10", message="CRITIQUE COMPLETE - 3 criteria failed"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "revise"
        msgs = _action_messages(actions)
        assert any("3 criteria failed" in m for m in msgs)
        assert any("Revise" in m for m in msgs)

    def test_malformed_critique_stays_in_critique(self) -> None:
        """If critique message doesn't match expected format, stay in critique (fail-closed)."""
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "critique"

        actions = on_coworker_message(
            _make_event(task_id="10", message="critique complete - all good"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "critique"
        msgs = _action_messages(actions)
        assert any("Unable to parse" in m for m in msgs)


class TestRevisionLoop:
    def test_revision_complete_triggers_re_critique(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "revise"

        actions = on_coworker_message(
            _make_event(task_id="10", message="Revision complete"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "critique"
        assert ctx._store["tasks"]["10"]["revision_count"] == 1
        assert _has_spawn(actions)
        msgs = _action_messages(actions)
        assert any("revision #1" in m for m in msgs)

    def test_multiple_revisions_increment_count(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "revise"
        ctx._store["tasks"]["10"]["revision_count"] = 2

        actions = on_coworker_message(
            _make_event(task_id="10", message="Revision complete"), ctx
        )

        assert ctx._store["tasks"]["10"]["revision_count"] == 3
        msgs = _action_messages(actions)
        assert any("revision #3" in m for m in msgs)

    def test_full_sdoh_cycle(self) -> None:
        """End-to-end: research -> outline -> draft -> critique -> revise -> critique -> final."""
        ctx = _make_context()

        # task created
        on_task_created(_make_event(task_id="1", subject="Blog post"), ctx)
        assert ctx._store["tasks"]["1"]["stage"] == "research"

        # research -> outline
        on_coworker_message(
            _make_event(task_id="1", message="research complete"), ctx
        )
        assert ctx._store["tasks"]["1"]["stage"] == "outline"

        # outline -> draft
        on_coworker_message(
            _make_event(task_id="1", message="outline ready"), ctx
        )
        assert ctx._store["tasks"]["1"]["stage"] == "draft"

        # draft -> critique (spawns critic)
        actions = on_coworker_message(
            _make_event(task_id="1", message="draft complete"), ctx
        )
        assert ctx._store["tasks"]["1"]["stage"] == "critique"
        assert _has_spawn(actions)

        # critique with failures -> revise
        on_coworker_message(
            _make_event(task_id="1", message="CRITIQUE COMPLETE - 2 criteria failed"), ctx
        )
        assert ctx._store["tasks"]["1"]["stage"] == "revise"

        # revise -> re-critique (spawns critic again)
        actions = on_coworker_message(
            _make_event(task_id="1", message="revision complete"), ctx
        )
        assert ctx._store["tasks"]["1"]["stage"] == "critique"
        assert ctx._store["tasks"]["1"]["revision_count"] == 1
        assert _has_spawn(actions)

        # critique passes -> final
        on_coworker_message(
            _make_event(task_id="1", message="CRITIQUE COMPLETE - 0 criteria failed"), ctx
        )
        assert ctx._store["tasks"]["1"]["stage"] == "final"


# ---------------------------------------------------------------------------
# on_channel_message -- human learning loop
# ---------------------------------------------------------------------------


class TestAddCriterion:
    def test_add_criterion_to_active_task(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "draft"

        actions = on_channel_message(
            _make_event(message="add criterion: No passive voice in opening"), ctx
        )

        assert "No passive voice in opening" in ctx._store["tasks"]["10"]["criteria"]
        msgs = _action_messages(actions)
        assert any("No passive voice in opening" in m for m in msgs)

    def test_new_rule_alias(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "critique"

        on_channel_message(
            _make_event(message="new rule: Every paragraph under 5 sentences"), ctx
        )

        assert "Every paragraph under 5 sentences" in ctx._store["tasks"]["10"]["criteria"]

    def test_no_active_task_returns_none(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        # research stage is not "active" for criteria changes
        ctx._store["tasks"]["10"]["stage"] = "research"

        actions = on_channel_message(
            _make_event(message="add criterion: something"), ctx
        )

        assert actions is None


class TestAddPattern:
    def test_add_pattern_to_active_task(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "draft"

        actions = on_channel_message(
            _make_event(message="add pattern: Use metaphors sparingly"), ctx
        )

        assert "Use metaphors sparingly" in ctx._store["tasks"]["10"]["patterns"]
        msgs = _action_messages(actions)
        assert len(msgs) == 1

    def test_add_pattern_inactive_stage_returns_none(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)
        ctx._store["tasks"]["10"]["stage"] = "research"

        actions = on_channel_message(
            _make_event(message="add pattern: something"), ctx
        )

        assert actions is None

    def test_empty_state_returns_none(self) -> None:
        ctx = _make_context()

        actions = on_channel_message(
            _make_event(message="add pattern: something"), ctx
        )

        assert actions is None


# ---------------------------------------------------------------------------
# Edge cases
# ---------------------------------------------------------------------------


class TestEdgeCases:
    def test_coworker_message_without_task_id_returns_none(self) -> None:
        ctx = _make_context()

        actions = on_coworker_message(
            _make_event(message="research complete"), ctx
        )

        assert actions is None

    def test_coworker_message_unknown_task_returns_none(self) -> None:
        ctx = _make_context()

        actions = on_coworker_message(
            _make_event(task_id="999", message="research complete"), ctx
        )

        assert actions is None
        # No state pollution
        assert "tasks" not in ctx._store

    def test_wrong_stage_transition_returns_none(self) -> None:
        """Sending 'draft complete' during research stage does nothing."""
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)

        actions = on_coworker_message(
            _make_event(task_id="10", message="draft complete"), ctx
        )

        assert ctx._store["tasks"]["10"]["stage"] == "research"
        assert actions is None

    def test_missing_message_field(self) -> None:
        ctx = _make_context()
        on_task_created(_make_event(task_id="10", subject="test"), ctx)

        actions = on_coworker_message(
            _make_event(task_id="10"), ctx
        )

        assert actions is None


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


class TestBuildCritiquePrompt:
    def test_includes_all_criteria_and_patterns(self) -> None:
        criteria = ["Rule A", "Rule B"]
        patterns = ["Pattern X"]
        prompt = _build_critique_prompt(criteria, patterns)

        assert "Rule A" in prompt
        assert "Rule B" in prompt
        assert "Pattern X" in prompt
        assert "CRITIQUE COMPLETE" in prompt

    def test_empty_lists(self) -> None:
        prompt = _build_critique_prompt([], [])
        assert "CRITERIA" in prompt
        assert "PATTERNS" in prompt


class TestFindActiveTaskId:
    def test_finds_draft_stage(self) -> None:
        ctx = _make_context({"tasks": {"1": {"stage": "draft"}}})
        assert _find_active_task_id(ctx) == "1"

    def test_finds_critique_stage(self) -> None:
        ctx = _make_context({"tasks": {"2": {"stage": "critique"}}})
        assert _find_active_task_id(ctx) == "2"

    def test_finds_revise_stage(self) -> None:
        ctx = _make_context({"tasks": {"3": {"stage": "revise"}}})
        assert _find_active_task_id(ctx) == "3"

    def test_none_in_research(self) -> None:
        ctx = _make_context({"tasks": {"1": {"stage": "research"}}})
        assert _find_active_task_id(ctx) is None

    def test_none_in_final(self) -> None:
        ctx = _make_context({"tasks": {"1": {"stage": "final"}}})
        assert _find_active_task_id(ctx) is None

    def test_empty_state(self) -> None:
        ctx = _make_context({})
        assert _find_active_task_id(ctx) is None
