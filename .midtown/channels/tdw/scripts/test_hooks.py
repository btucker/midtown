"""Tests for TDW hooks -- AgentSkills-format hook implementations."""

from __future__ import annotations

from midtown.actions import Actions
from midtown.hooks import HookContext

from hooks import (
    ACTIVE_STAGES,
    DEFAULT_CRITERIA,
    DEFAULT_PATTERNS,
    _build_critique_prompt,
    _find_active_task_id,
    _resolve_target_task,
    _warn_no_target,
    on_channel_message,
    on_coworker_message,
    on_task_created,
)


def _make_ctx(
    state: dict | None = None,
    *,
    task_id: str | None = None,
    message: str = "",
    subject: str = "",
    **extra: object,
) -> HookContext:
    """Create a HookContext with the given state and event fields."""
    st = state if state is not None else {}
    event: dict = {"message": message}
    if task_id:
        event["task_id"] = task_id
    if subject:
        event["subject"] = subject
    event.update(extra)
    return HookContext(
        event_type="",
        event=event,
        task_id=task_id,
        state=st,
        actions=Actions,
    )


def _apply_actions(actions, state: dict) -> None:
    """Execute set_state actions against a nested state dict (for test setup)."""
    if actions is None:
        return
    for action in actions:
        if action.method == "state.set":
            key = action.params["key"]
            value = action.params["value"]
            parts = key.split(".")
            current = state
            for part in parts[:-1]:
                current = current.setdefault(part, {})
            current[parts[-1]] = value


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


def _init_task(state: dict, task_id: str) -> None:
    """Shorthand: run on_task_created and apply its state mutations."""
    ctx = _make_ctx(state, task_id=task_id, subject="test")
    actions = on_task_created(ctx)
    _apply_actions(actions, state)


# ---------------------------------------------------------------------------
# on_task_created
# ---------------------------------------------------------------------------


class TestOnTaskCreated:
    def test_initialises_state_and_returns_actions(self) -> None:
        state: dict = {}
        ctx = _make_ctx(state, task_id="10", subject="Write blog post")

        actions = on_task_created(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "research"
        assert state["tasks"]["10"]["criteria"] == DEFAULT_CRITERIA
        assert state["tasks"]["10"]["patterns"] == DEFAULT_PATTERNS
        assert state["tasks"]["10"]["revision_count"] == 0

        msgs = _action_messages(actions)
        assert len(msgs) == 1
        assert "Write blog post" in msgs[0]
        assert "Research" in msgs[0]

    def test_no_task_id_returns_none(self) -> None:
        state: dict = {}
        ctx = _make_ctx(state, subject="No ID")

        actions = on_task_created(ctx)

        assert actions is None
        assert "tasks" not in state

    def test_copies_defaults(self) -> None:
        """Ensure criteria/patterns are copies, not shared references."""
        state: dict = {}
        ctx = _make_ctx(state, task_id="42", subject="Test")

        actions = on_task_created(ctx)
        _apply_actions(actions, state)
        state["tasks"]["42"]["criteria"].append("extra")

        assert "extra" not in DEFAULT_CRITERIA


# ---------------------------------------------------------------------------
# on_coworker_message -- stage transitions
# ---------------------------------------------------------------------------


class TestResearchToOutline:
    def test_research_complete_advances_stage(self) -> None:
        state: dict = {}
        _init_task(state, "10")

        ctx = _make_ctx(state, task_id="10", message="Research complete, moving on")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "outline"
        msgs = _action_messages(actions)
        assert any("Outline" in m for m in msgs)

    def test_research_complete_case_insensitive(self) -> None:
        state: dict = {}
        _init_task(state, "10")

        ctx = _make_ctx(state, task_id="10", message="RESEARCH COMPLETE")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "outline"


class TestOutlineToDraft:
    def test_outline_ready_advances_stage(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "outline"

        ctx = _make_ctx(state, task_id="10", message="Outline ready for review")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "draft"
        msgs = _action_messages(actions)
        assert any("Draft" in m for m in msgs)


class TestDraftToCritique:
    def test_draft_complete_spawns_critic(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "draft"

        ctx = _make_ctx(state, task_id="10", message="Draft complete")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "critique"
        assert _has_spawn(actions)

        # Verify the critique prompt includes criteria
        spawn_action = [a for a in actions if a.method == "coworker.spawn"][0]
        prompt = spawn_action.params["prompt"]
        assert "CRITERIA" in prompt
        assert "PATTERNS" in prompt
        for criterion in DEFAULT_CRITERIA:
            assert criterion in prompt

    def test_draft_complete_uses_custom_criteria(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "draft"
        state["tasks"]["10"]["criteria"] = ["Custom rule 1"]
        state["tasks"]["10"]["patterns"] = ["Custom pattern 1"]

        ctx = _make_ctx(state, task_id="10", message="Draft complete")
        actions = on_coworker_message(ctx)

        spawn_action = [a for a in actions if a.method == "coworker.spawn"][0]
        prompt = spawn_action.params["prompt"]
        assert "Custom rule 1" in prompt
        assert "Custom pattern 1" in prompt


class TestCritiqueResults:
    def test_all_criteria_pass_goes_to_final(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "critique"

        ctx = _make_ctx(state, task_id="10", message="CRITIQUE COMPLETE - 0 criteria failed")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "final"
        msgs = _action_messages(actions)
        assert any("All criteria passed" in m for m in msgs)
        assert any("Final Review" in m for m in msgs)

    def test_failures_go_to_revise(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "critique"

        ctx = _make_ctx(state, task_id="10", message="CRITIQUE COMPLETE - 3 criteria failed")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "revise"
        msgs = _action_messages(actions)
        assert any("3 criteria failed" in m for m in msgs)
        assert any("Revise" in m for m in msgs)

    def test_malformed_critique_stays_in_critique(self) -> None:
        """If critique message doesn't match expected format, stay in critique (fail-closed)."""
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "critique"

        ctx = _make_ctx(state, task_id="10", message="critique complete - all good")
        actions = on_coworker_message(ctx)

        assert state["tasks"]["10"]["stage"] == "critique"
        msgs = _action_messages(actions)
        assert any("Unable to parse" in m for m in msgs)


class TestRevisionLoop:
    def test_revision_complete_triggers_re_critique(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "revise"

        ctx = _make_ctx(state, task_id="10", message="Revision complete")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["stage"] == "critique"
        assert state["tasks"]["10"]["revision_count"] == 1
        assert _has_spawn(actions)
        msgs = _action_messages(actions)
        assert any("revision #1" in m for m in msgs)

    def test_multiple_revisions_increment_count(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "revise"
        state["tasks"]["10"]["revision_count"] = 2

        ctx = _make_ctx(state, task_id="10", message="Revision complete")
        actions = on_coworker_message(ctx)
        _apply_actions(actions, state)

        assert state["tasks"]["10"]["revision_count"] == 3
        msgs = _action_messages(actions)
        assert any("revision #3" in m for m in msgs)

    def test_full_sdoh_cycle(self) -> None:
        """End-to-end: research -> outline -> draft -> critique -> revise -> critique -> final."""
        state: dict = {}

        # task created
        _init_task(state, "1")
        assert state["tasks"]["1"]["stage"] == "research"

        # research -> outline
        actions = on_coworker_message(
            _make_ctx(state, task_id="1", message="research complete")
        )
        _apply_actions(actions, state)
        assert state["tasks"]["1"]["stage"] == "outline"

        # outline -> draft
        actions = on_coworker_message(
            _make_ctx(state, task_id="1", message="outline ready")
        )
        _apply_actions(actions, state)
        assert state["tasks"]["1"]["stage"] == "draft"

        # draft -> critique (spawns critic)
        actions = on_coworker_message(
            _make_ctx(state, task_id="1", message="draft complete")
        )
        _apply_actions(actions, state)
        assert state["tasks"]["1"]["stage"] == "critique"
        assert _has_spawn(actions)

        # critique with failures -> revise
        actions = on_coworker_message(
            _make_ctx(state, task_id="1", message="CRITIQUE COMPLETE - 2 criteria failed")
        )
        _apply_actions(actions, state)
        assert state["tasks"]["1"]["stage"] == "revise"

        # revise -> re-critique (spawns critic again)
        actions = on_coworker_message(
            _make_ctx(state, task_id="1", message="revision complete")
        )
        _apply_actions(actions, state)
        assert state["tasks"]["1"]["stage"] == "critique"
        assert state["tasks"]["1"]["revision_count"] == 1
        assert _has_spawn(actions)

        # critique passes -> final
        actions = on_coworker_message(
            _make_ctx(state, task_id="1", message="CRITIQUE COMPLETE - 0 criteria failed")
        )
        _apply_actions(actions, state)
        assert state["tasks"]["1"]["stage"] == "final"


# ---------------------------------------------------------------------------
# on_channel_message -- human learning loop
# ---------------------------------------------------------------------------


class TestAddCriterion:
    def test_add_criterion_to_active_task(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "draft"

        ctx = _make_ctx(state, message="add criterion: No passive voice in opening")
        actions = on_channel_message(ctx)
        _apply_actions(actions, state)

        assert "No passive voice in opening" in state["tasks"]["10"]["criteria"]
        msgs = _action_messages(actions)
        assert any("No passive voice in opening" in m for m in msgs)

    def test_add_criterion_uses_task_id_from_context(self) -> None:
        """With multiple active tasks, task_id from context targets the correct task."""
        state: dict = {}
        _init_task(state, "10")
        _init_task(state, "20")
        state["tasks"]["10"]["stage"] = "draft"
        state["tasks"]["20"]["stage"] = "draft"

        ctx = _make_ctx(state, task_id="20", message="add criterion: Must include a quote")
        actions = on_channel_message(ctx)
        _apply_actions(actions, state)

        # Criterion should be added to task 20 (from context), not task 10
        assert "Must include a quote" in state["tasks"]["20"]["criteria"]
        assert "Must include a quote" not in state["tasks"]["10"]["criteria"]

    def test_add_criterion_ambiguous_multiple_active_tasks(self) -> None:
        """Without task_id context, multiple active tasks should warn about ambiguity."""
        state: dict = {}
        _init_task(state, "10")
        _init_task(state, "20")
        state["tasks"]["10"]["stage"] = "draft"
        state["tasks"]["20"]["stage"] = "critique"

        ctx = _make_ctx(state, message="add criterion: something")
        actions = on_channel_message(ctx)

        # Should NOT silently modify a random task — should warn about ambiguity
        assert "something" not in state["tasks"]["10"].get("criteria", [])
        assert "something" not in state["tasks"]["20"].get("criteria", [])
        msgs = _action_messages(actions)
        assert len(msgs) == 1
        assert "Multiple active tasks" in msgs[0]
        assert "--task" in msgs[0]

    def test_add_criterion_explicit_task_id_inactive_stage(self) -> None:
        """Explicit task_id targeting an inactive task should warn the user."""
        state: dict = {}
        _init_task(state, "10")
        _init_task(state, "20")
        state["tasks"]["10"]["stage"] = "draft"
        state["tasks"]["20"]["stage"] = "final"

        ctx = _make_ctx(state, task_id="20", message="add criterion: something")
        actions = on_channel_message(ctx)

        # Neither task should have the criterion added
        assert "something" not in state["tasks"]["10"].get("criteria", [])
        assert "something" not in state["tasks"]["20"].get("criteria", [])
        # Should warn that the targeted task is not active
        msgs = _action_messages(actions)
        assert len(msgs) == 1
        assert "20" in msgs[0]
        assert "not in an active stage" in msgs[0]

    def test_new_rule_alias(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "critique"

        ctx = _make_ctx(state, message="new rule: Every paragraph under 5 sentences")
        actions = on_channel_message(ctx)
        _apply_actions(actions, state)

        assert "Every paragraph under 5 sentences" in state["tasks"]["10"]["criteria"]

    def test_no_active_task_returns_none(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        # research stage is not "active" for criteria changes
        state["tasks"]["10"]["stage"] = "research"

        ctx = _make_ctx(state, message="add criterion: something")
        actions = on_channel_message(ctx)

        assert actions is None


class TestAddPattern:
    def test_add_pattern_to_active_task(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "draft"

        ctx = _make_ctx(state, message="add pattern: Use metaphors sparingly")
        actions = on_channel_message(ctx)
        _apply_actions(actions, state)

        assert "Use metaphors sparingly" in state["tasks"]["10"]["patterns"]
        msgs = _action_messages(actions)
        assert len(msgs) == 1

    def test_add_pattern_uses_task_id_from_context(self) -> None:
        """With multiple active tasks, task_id from context targets the correct task."""
        state: dict = {}
        _init_task(state, "10")
        _init_task(state, "20")
        state["tasks"]["10"]["stage"] = "draft"
        state["tasks"]["20"]["stage"] = "revise"

        ctx = _make_ctx(state, task_id="20", message="add pattern: Vary sentence length")
        actions = on_channel_message(ctx)
        _apply_actions(actions, state)

        assert "Vary sentence length" in state["tasks"]["20"]["patterns"]
        assert "Vary sentence length" not in state["tasks"]["10"]["patterns"]

    def test_add_pattern_inactive_stage_returns_none(self) -> None:
        state: dict = {}
        _init_task(state, "10")
        state["tasks"]["10"]["stage"] = "research"

        ctx = _make_ctx(state, message="add pattern: something")
        actions = on_channel_message(ctx)

        assert actions is None

    def test_empty_state_returns_none(self) -> None:
        state: dict = {}

        ctx = _make_ctx(state, message="add pattern: something")
        actions = on_channel_message(ctx)

        assert actions is None


# ---------------------------------------------------------------------------
# Edge cases
# ---------------------------------------------------------------------------


class TestEdgeCases:
    def test_coworker_message_without_task_id_returns_none(self) -> None:
        state: dict = {}

        ctx = _make_ctx(state, message="research complete")
        actions = on_coworker_message(ctx)

        assert actions is None

    def test_coworker_message_unknown_task_returns_none(self) -> None:
        state: dict = {}

        ctx = _make_ctx(state, task_id="999", message="research complete")
        actions = on_coworker_message(ctx)

        assert actions is None
        # No state pollution
        assert "tasks" not in state

    def test_wrong_stage_transition_returns_none(self) -> None:
        """Sending 'draft complete' during research stage does nothing."""
        state: dict = {}
        _init_task(state, "10")

        ctx = _make_ctx(state, task_id="10", message="draft complete")
        actions = on_coworker_message(ctx)

        assert state["tasks"]["10"]["stage"] == "research"
        assert actions is None

    def test_missing_message_field(self) -> None:
        state: dict = {}
        _init_task(state, "10")

        ctx = _make_ctx(state, task_id="10")
        actions = on_coworker_message(ctx)

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
        ctx = _make_ctx({"tasks": {"1": {"stage": "draft"}}})
        assert _find_active_task_id(ctx) == "1"

    def test_finds_critique_stage(self) -> None:
        ctx = _make_ctx({"tasks": {"2": {"stage": "critique"}}})
        assert _find_active_task_id(ctx) == "2"

    def test_finds_revise_stage(self) -> None:
        ctx = _make_ctx({"tasks": {"3": {"stage": "revise"}}})
        assert _find_active_task_id(ctx) == "3"

    def test_none_in_research(self) -> None:
        ctx = _make_ctx({"tasks": {"1": {"stage": "research"}}})
        assert _find_active_task_id(ctx) is None

    def test_none_in_final(self) -> None:
        ctx = _make_ctx({"tasks": {"1": {"stage": "final"}}})
        assert _find_active_task_id(ctx) is None

    def test_empty_state(self) -> None:
        ctx = _make_ctx({})
        assert _find_active_task_id(ctx) is None

    def test_none_when_multiple_active(self) -> None:
        """Multiple active tasks should return None (ambiguous)."""
        ctx = _make_ctx({"tasks": {"1": {"stage": "draft"}, "2": {"stage": "revise"}}})
        assert _find_active_task_id(ctx) is None


class TestResolveTargetTask:
    def test_explicit_task_id_active(self) -> None:
        """Explicit task_id targeting an active task returns it."""
        state = {"tasks": {"10": {"stage": "draft"}, "20": {"stage": "revise"}}}
        ctx = _make_ctx(state, task_id="20")
        tid, td = _resolve_target_task(ctx)
        assert tid == "20"
        assert td.get("stage") == "revise"

    def test_explicit_task_id_inactive(self) -> None:
        """Explicit task_id targeting an inactive task returns (None, {})."""
        state = {"tasks": {"10": {"stage": "draft"}, "20": {"stage": "final"}}}
        ctx = _make_ctx(state, task_id="20")
        tid, td = _resolve_target_task(ctx)
        assert tid is None
        assert td == {}

    def test_fallback_single_active(self) -> None:
        """No task_id, single active task: returns it."""
        state = {"tasks": {"10": {"stage": "draft"}}}
        ctx = _make_ctx(state)
        tid, td = _resolve_target_task(ctx)
        assert tid == "10"

    def test_fallback_multiple_active(self) -> None:
        """No task_id, multiple active tasks: returns (None, {})."""
        state = {"tasks": {"10": {"stage": "draft"}, "20": {"stage": "revise"}}}
        ctx = _make_ctx(state)
        tid, td = _resolve_target_task(ctx)
        assert tid is None
        assert td == {}


class TestWarnNoTarget:
    def test_warns_about_inactive_explicit_task(self) -> None:
        state = {"tasks": {"10": {"stage": "draft"}, "20": {"stage": "final"}}}
        ctx = _make_ctx(state, task_id="20")
        actions = _warn_no_target(ctx)
        msgs = _action_messages(actions)
        assert len(msgs) == 1
        assert "20" in msgs[0]
        assert "not in an active stage" in msgs[0]

    def test_warns_about_ambiguity(self) -> None:
        state = {"tasks": {"10": {"stage": "draft"}, "20": {"stage": "revise"}}}
        ctx = _make_ctx(state)
        actions = _warn_no_target(ctx)
        msgs = _action_messages(actions)
        assert len(msgs) == 1
        assert "Multiple active tasks" in msgs[0]
        assert "--task" in msgs[0]

    def test_no_warning_when_no_active_tasks(self) -> None:
        state = {"tasks": {"10": {"stage": "research"}}}
        ctx = _make_ctx(state)
        actions = _warn_no_target(ctx)
        assert actions == []
