"""Tests for default_workflow.py — PR age guard on pr.ci_passed reviewer spawn."""

from __future__ import annotations

import time
from unittest.mock import MagicMock

from midtown.default_workflow import PR_REVIEW_DELAY_SECS, handle


def _make_rpc() -> MagicMock:
    """Create a mock MidtownRPC with all expected methods."""
    rpc = MagicMock()
    rpc.spawn_reviewer.return_value = {"message": "Reviewer assigned"}
    rpc.nudge_coworker.return_value = None
    rpc.post_to_channel.return_value = None
    rpc.check_pending.return_value = None
    rpc.complete_task.return_value = None
    return rpc


def _setup_pr_opened(state: dict, task_id: str = "100", opened_at: float | None = None) -> None:
    """Simulate a PR that was opened, recording pr_opened_at in state."""
    state.setdefault("tasks", {}).setdefault(task_id, {})
    state["tasks"][task_id]["state"] = "in_review"
    state["tasks"][task_id]["pr_author"] = "park"
    if opened_at is not None:
        state["tasks"][task_id]["pr_opened_at"] = opened_at


class TestCiPassedAgeGuard:
    """pr.ci_passed should only call spawn_reviewer when the PR is old enough."""

    def test_spawn_blocked_when_pr_too_new(self) -> None:
        """A PR opened moments ago should NOT trigger spawn_reviewer."""
        rpc = _make_rpc()
        state: dict = {}
        _setup_pr_opened(state, "100", opened_at=time.time())  # just opened

        handle(
            {"type": "pr.ci_passed", "task_id": "100", "pr_number": 42, "channel": "test"},
            rpc,
            state,
        )

        rpc.spawn_reviewer.assert_not_called()

    def test_spawn_allowed_when_pr_old_enough(self) -> None:
        """A PR opened long ago should trigger spawn_reviewer."""
        rpc = _make_rpc()
        state: dict = {}
        # Opened well before the delay threshold
        _setup_pr_opened(state, "100", opened_at=time.time() - PR_REVIEW_DELAY_SECS - 10)

        handle(
            {"type": "pr.ci_passed", "task_id": "100", "pr_number": 42, "channel": "test"},
            rpc,
            state,
        )

        rpc.spawn_reviewer.assert_called_once_with(42)

    def test_spawn_allowed_when_no_opened_at(self) -> None:
        """If pr_opened_at is missing (e.g. state was lost), spawn_reviewer proceeds."""
        rpc = _make_rpc()
        state: dict = {}
        _setup_pr_opened(state, "100", opened_at=None)  # no timestamp

        handle(
            {"type": "pr.ci_passed", "task_id": "100", "pr_number": 42, "channel": "test"},
            rpc,
            state,
        )

        rpc.spawn_reviewer.assert_called_once_with(42)

    def test_spawn_allowed_when_no_task_data(self) -> None:
        """If no task data exists at all, spawn_reviewer proceeds."""
        rpc = _make_rpc()
        state: dict = {}

        handle(
            {"type": "pr.ci_passed", "task_id": "999", "pr_number": 42, "channel": "test"},
            rpc,
            state,
        )

        rpc.spawn_reviewer.assert_called_once_with(42)

    def test_spawn_blocked_at_exact_boundary(self) -> None:
        """A PR opened exactly PR_REVIEW_DELAY_SECS - 1 ago should be blocked."""
        rpc = _make_rpc()
        state: dict = {}
        _setup_pr_opened(state, "100", opened_at=time.time() - PR_REVIEW_DELAY_SECS + 1)

        handle(
            {"type": "pr.ci_passed", "task_id": "100", "pr_number": 42, "channel": "test"},
            rpc,
            state,
        )

        rpc.spawn_reviewer.assert_not_called()


class TestPrOpenedRecordsTimestamp:
    """pr.opened should store pr_opened_at in task state."""

    def test_pr_opened_records_timestamp(self) -> None:
        """When pr.opened fires, pr_opened_at is saved in task state."""
        rpc = _make_rpc()
        state: dict = {}
        # Start with a task in in_progress state
        state.setdefault("tasks", {}).setdefault("100", {})["state"] = "in_progress"

        before = time.time()
        handle(
            {
                "type": "pr.opened",
                "task_id": "100",
                "pr_number": 42,
                "coworker": "park",
                "channel": "test",
            },
            rpc,
            state,
        )
        after = time.time()

        opened_at = state["tasks"]["100"].get("pr_opened_at")
        assert opened_at is not None
        assert before <= opened_at <= after
