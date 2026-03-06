"""Tests for tdw_workflow.py — TDW (Test-Driven Writing) workflow plugin."""

from __future__ import annotations

from unittest.mock import MagicMock

from midtown.tdw_workflow import (
    DEFAULT_CRITERIA,
    DEFAULT_PATTERNS,
    _build_critique_prompt,
    _find_active_task,
    _get_task,
    _init_task,
    handle,
)


def _make_rpc() -> MagicMock:
    """Create a mock MidtownRPC with all expected methods."""
    rpc = MagicMock()
    rpc.post_to_channel.return_value = None
    rpc.spawn_coworker.return_value = None
    return rpc


# ---------------------------------------------------------------------------
# State helpers
# ---------------------------------------------------------------------------


class TestStateHelpers:
    def test_get_task_creates_if_absent(self) -> None:
        state: dict = {}
        task = _get_task(state, "42")
        assert task == {}
        assert "42" in state["tdw_tasks"]

    def test_get_task_returns_existing(self) -> None:
        state = {"tdw_tasks": {"42": {"stage": "draft"}}}
        task = _get_task(state, "42")
        assert task["stage"] == "draft"

    def test_init_task_sets_defaults(self) -> None:
        state: dict = {}
        task = _init_task(state, "42")
        assert task["stage"] == "research"
        assert task["criteria"] == DEFAULT_CRITERIA
        assert task["patterns"] == DEFAULT_PATTERNS
        assert task["revision_count"] == 0

    def test_init_task_copies_defaults(self) -> None:
        """Ensure criteria/patterns are copies, not shared references."""
        state: dict = {}
        task = _init_task(state, "42")
        task["criteria"].append("extra")
        assert "extra" not in DEFAULT_CRITERIA

    def test_find_active_task_in_draft(self) -> None:
        state = {"tdw_tasks": {"1": {"stage": "draft"}}}
        task_id, data = _find_active_task(state)
        assert task_id == "1"
        assert data["stage"] == "draft"

    def test_find_active_task_in_critique(self) -> None:
        state = {"tdw_tasks": {"2": {"stage": "critique"}}}
        task_id, _ = _find_active_task(state)
        assert task_id == "2"

    def test_find_active_task_in_revise(self) -> None:
        state = {"tdw_tasks": {"3": {"stage": "revise"}}}
        task_id, _ = _find_active_task(state)
        assert task_id == "3"

    def test_find_active_task_none_in_research(self) -> None:
        state = {"tdw_tasks": {"1": {"stage": "research"}}}
        task_id, _ = _find_active_task(state)
        assert task_id is None

    def test_find_active_task_none_in_final(self) -> None:
        state = {"tdw_tasks": {"1": {"stage": "final"}}}
        task_id, _ = _find_active_task(state)
        assert task_id is None

    def test_find_active_task_empty(self) -> None:
        task_id, _ = _find_active_task({})
        assert task_id is None


# ---------------------------------------------------------------------------
# task.created
# ---------------------------------------------------------------------------


class TestTaskCreated:
    def test_initialises_state_and_posts(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        handle(
            {"type": "task.created", "task_id": "10", "subject": "Write blog post"},
            rpc,
            state,
        )

        task = state["tdw_tasks"]["10"]
        assert task["stage"] == "research"
        assert task["criteria"] == DEFAULT_CRITERIA
        rpc.post_to_channel.assert_called_once()
        msg = rpc.post_to_channel.call_args[0][0]
        assert "Write blog post" in msg
        assert "Research" in msg

    def test_no_task_id_is_noop(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        handle({"type": "task.created", "subject": "No ID"}, rpc, state)
        rpc.post_to_channel.assert_not_called()
        assert "tdw_tasks" not in state


# ---------------------------------------------------------------------------
# Stage transitions via coworker.message
# ---------------------------------------------------------------------------


class TestResearchToOutline:
    def test_research_complete_advances_stage(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "Research complete, moving on",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "outline"
        msg = rpc.post_to_channel.call_args[0][0]
        assert "Outline" in msg

    def test_research_complete_case_insensitive(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "RESEARCH COMPLETE",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "outline"


class TestOutlineToDraft:
    def test_outline_ready_advances_stage(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "outline"

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "Outline ready for review",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "draft"
        msg = rpc.post_to_channel.call_args[0][0]
        assert "Draft" in msg


class TestDraftToCritique:
    def test_draft_complete_spawns_critic(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "draft"

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "Draft complete",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "critique"
        rpc.post_to_channel.assert_called_once()
        rpc.spawn_coworker.assert_called_once()

        # Verify the critique prompt includes criteria
        prompt = rpc.spawn_coworker.call_args[1]["prompt"]
        assert "CRITERIA" in prompt
        assert "PATTERNS" in prompt
        for criterion in DEFAULT_CRITERIA:
            assert criterion in prompt

    def test_draft_complete_uses_custom_criteria(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "draft"
        state["tdw_tasks"]["10"]["criteria"] = ["Custom rule 1"]
        state["tdw_tasks"]["10"]["patterns"] = ["Custom pattern 1"]

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "Draft complete",
            },
            rpc,
            state,
        )

        prompt = rpc.spawn_coworker.call_args[1]["prompt"]
        assert "Custom rule 1" in prompt
        assert "Custom pattern 1" in prompt


class TestCritiqueResults:
    def test_all_criteria_pass_goes_to_final(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "critique"

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "CRITIQUE COMPLETE - 0 criteria failed",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "final"
        msg = rpc.post_to_channel.call_args[0][0]
        assert "All criteria passed" in msg
        assert "Final Review" in msg

    def test_failures_go_to_revise(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "critique"

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "CRITIQUE COMPLETE - 3 criteria failed",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "revise"
        msg = rpc.post_to_channel.call_args[0][0]
        assert "3 criteria failed" in msg
        assert "Revise" in msg

    def test_no_number_defaults_to_zero_failures(self) -> None:
        """If critique message doesn't match the pattern, assume 0 failures."""
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "critique"

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "critique complete - all good",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "final"


class TestRevisionLoop:
    def test_revision_complete_triggers_re_critique(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "revise"

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "Revision complete",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "critique"
        assert state["tdw_tasks"]["10"]["revision_count"] == 1
        rpc.spawn_coworker.assert_called_once()
        msg = rpc.post_to_channel.call_args[0][0]
        assert "revision #1" in msg

    def test_multiple_revisions_increment_count(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "revise"
        state["tdw_tasks"]["10"]["revision_count"] = 2

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "Revision complete",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["revision_count"] == 3
        msg = rpc.post_to_channel.call_args[0][0]
        assert "revision #3" in msg

    def test_full_sdoh_cycle(self) -> None:
        """End-to-end test: research → outline → draft → critique → revise → critique → final."""
        rpc = _make_rpc()
        state: dict = {}

        # task.created
        handle(
            {"type": "task.created", "task_id": "1", "subject": "Blog post"},
            rpc,
            state,
        )
        assert state["tdw_tasks"]["1"]["stage"] == "research"

        # research → outline
        handle(
            {"type": "coworker.message", "task_id": "1", "message": "research complete"},
            rpc,
            state,
        )
        assert state["tdw_tasks"]["1"]["stage"] == "outline"

        # outline → draft
        handle(
            {"type": "coworker.message", "task_id": "1", "message": "outline ready"},
            rpc,
            state,
        )
        assert state["tdw_tasks"]["1"]["stage"] == "draft"

        # draft → critique (spawns critic)
        handle(
            {"type": "coworker.message", "task_id": "1", "message": "draft complete"},
            rpc,
            state,
        )
        assert state["tdw_tasks"]["1"]["stage"] == "critique"
        rpc.spawn_coworker.assert_called_once()

        # critique with failures → revise
        handle(
            {
                "type": "coworker.message",
                "task_id": "1",
                "message": "CRITIQUE COMPLETE - 2 criteria failed",
            },
            rpc,
            state,
        )
        assert state["tdw_tasks"]["1"]["stage"] == "revise"

        # revise → re-critique (spawns critic again)
        rpc.spawn_coworker.reset_mock()
        handle(
            {"type": "coworker.message", "task_id": "1", "message": "revision complete"},
            rpc,
            state,
        )
        assert state["tdw_tasks"]["1"]["stage"] == "critique"
        assert state["tdw_tasks"]["1"]["revision_count"] == 1
        rpc.spawn_coworker.assert_called_once()

        # critique passes → final
        handle(
            {
                "type": "coworker.message",
                "task_id": "1",
                "message": "CRITIQUE COMPLETE - 0 criteria failed",
            },
            rpc,
            state,
        )
        assert state["tdw_tasks"]["1"]["stage"] == "final"


# ---------------------------------------------------------------------------
# channel.message — human learning loop
# ---------------------------------------------------------------------------


class TestAddCriterion:
    def test_add_criterion_to_active_task(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "draft"

        handle(
            {
                "type": "channel.message",
                "message": "add criterion: No passive voice in opening",
            },
            rpc,
            state,
        )

        assert "No passive voice in opening" in state["tdw_tasks"]["10"]["criteria"]
        rpc.post_to_channel.assert_called_once()
        msg = rpc.post_to_channel.call_args[0][0]
        assert "No passive voice in opening" in msg

    def test_new_rule_alias(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "critique"

        handle(
            {
                "type": "channel.message",
                "message": "new rule: Every paragraph under 5 sentences",
            },
            rpc,
            state,
        )

        assert "Every paragraph under 5 sentences" in state["tdw_tasks"]["10"]["criteria"]

    def test_no_active_task_is_silent(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        # research stage is not "active" for criteria changes
        state["tdw_tasks"]["10"]["stage"] = "research"

        handle(
            {
                "type": "channel.message",
                "message": "add criterion: something",
            },
            rpc,
            state,
        )

        rpc.post_to_channel.assert_not_called()


class TestAddPattern:
    def test_add_pattern_to_any_active_task(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        state["tdw_tasks"]["10"]["stage"] = "research"

        handle(
            {
                "type": "channel.message",
                "message": "add pattern: Use metaphors sparingly",
            },
            rpc,
            state,
        )

        assert "Use metaphors sparingly" in state["tdw_tasks"]["10"]["patterns"]
        rpc.post_to_channel.assert_called_once()

    def test_empty_state_is_silent(self) -> None:
        rpc = _make_rpc()
        state: dict = {}

        handle(
            {
                "type": "channel.message",
                "message": "add pattern: something",
            },
            rpc,
            state,
        )

        rpc.post_to_channel.assert_not_called()


# ---------------------------------------------------------------------------
# Edge cases
# ---------------------------------------------------------------------------


class TestEdgeCases:
    def test_coworker_message_without_task_id_is_noop(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        handle(
            {"type": "coworker.message", "message": "research complete"},
            rpc,
            state,
        )
        rpc.post_to_channel.assert_not_called()

    def test_coworker_message_unknown_task_is_noop(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        handle(
            {"type": "coworker.message", "task_id": "999", "message": "research complete"},
            rpc,
            state,
        )
        # _get_task creates the entry but stage is None so no transition
        rpc.post_to_channel.assert_not_called()

    def test_wrong_stage_transition_is_noop(self) -> None:
        """Sending 'draft complete' during research stage does nothing."""
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")

        handle(
            {
                "type": "coworker.message",
                "task_id": "10",
                "message": "draft complete",
            },
            rpc,
            state,
        )

        assert state["tdw_tasks"]["10"]["stage"] == "research"
        rpc.post_to_channel.assert_not_called()

    def test_unrelated_event_type_is_noop(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        handle({"type": "pr.opened", "task_id": "10"}, rpc, state)
        rpc.post_to_channel.assert_not_called()

    def test_missing_message_field(self) -> None:
        rpc = _make_rpc()
        state: dict = {}
        _init_task(state, "10")
        # coworker.message without message field
        handle(
            {"type": "coworker.message", "task_id": "10"},
            rpc,
            state,
        )
        rpc.post_to_channel.assert_not_called()


# ---------------------------------------------------------------------------
# Critique prompt builder
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
