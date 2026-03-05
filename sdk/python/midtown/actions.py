"""Action types returned by workflow hooks."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


@dataclass
class Action:
    """Base class for actions. Use factory methods to create."""

    type: str
    args: dict[str, Any]

    @classmethod
    def post_to_channel(cls, message: str, thread_id: str | None = None) -> Action:
        return cls("post_to_channel", {"message": message, "thread_id": thread_id})

    @classmethod
    def nudge_coworker(cls, name: str, message: str) -> Action:
        return cls("nudge_coworker", {"name": name, "message": message})

    @classmethod
    def spawn_reviewer(cls, pr_number: int) -> Action:
        return cls("spawn_reviewer", {"pr_number": pr_number})

    @classmethod
    def spawn_coworker(cls, prompt: str, different_from: str | None = None) -> Action:
        return cls("spawn_coworker", {"prompt": prompt, "different_from": different_from})

    @classmethod
    def fork_lead(cls, role: str, prompt: str) -> Action:
        return cls("fork_lead", {"role": role, "prompt": prompt})

    @classmethod
    def complete_task(cls, task_id: str) -> Action:
        return cls("complete_task", {"task_id": task_id})

    @classmethod
    def enable_auto_merge(cls, pr_number: int) -> Action:
        return cls("enable_auto_merge", {"pr_number": pr_number})

    @classmethod
    def check_pending(cls) -> Action:
        return cls("check_pending", {})

    @classmethod
    def http_post(cls, url: str, body: dict[str, Any]) -> Action:
        return cls("http_post", {"url": url, "body": body})
