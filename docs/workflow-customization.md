# Writing Custom Workflow Scripts

Midtown's workflow system lets you customize how the daemon responds to events in your project — PR lifecycle, coworker health, task transitions, CI results, and more. Each channel can have its own `workflow.py` script that replaces or extends the default behavior.

This guide covers everything you need to write, test, and deploy custom workflow scripts.

## Table of Contents

- [Quick Start](#quick-start)
- [How It Works](#how-it-works)
- [Available Events](#available-events)
- [RPC Methods](#rpc-methods)
- [State Management](#state-management)
- [Script Resolution Order](#script-resolution-order)
- [Examples](#examples)
- [Testing](#testing)
- [Troubleshooting](#troubleshooting)

---

## Quick Start

The fastest way to get started is to copy the reference implementation and modify it:

```bash
# 1. Copy the reference workflow to your project (applies to all channels)
mkdir -p .midtown
cp $(python -c "import midtown, os; print(os.path.dirname(midtown.__file__))")/default_workflow.py \
   .midtown/workflow.py

# 2. Edit to taste
$EDITOR .midtown/workflow.py
```

Or write a minimal script from scratch:

```python
# .midtown/workflow.py
from midtown import run, MidtownRPC

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    if event["type"] == "pr.approved":
        author = event.get("coworker", "someone")
        rpc.post_to_channel(f"🎉 PR #{event['pr_number']} approved!")

if __name__ == "__main__":
    run(handle)
```

That's it. The daemon picks up `workflow.py` on the next tick — no restart needed.

---

## How It Works

The daemon invokes your workflow script as a **short-lived subprocess** for each event:

```
uv run workflow.py \
    --event '{"type":"pr.opened", "channel":"proj-auth", "task_id":"42", ...}' \
    --state ~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json \
    --socket ~/.local/state/midtown/<repo>/daemon.sock
```

The Midtown Python SDK (`from midtown import run`) handles CLI parsing, state loading/saving, and socket setup. Your handler function receives three arguments:

| Argument | Type | Description |
|----------|------|-------------|
| `event` | `dict` | The decoded event JSON (always has `"type"` and `"channel"`) |
| `rpc` | `MidtownRPC` | Client for calling back to the daemon |
| `state` | `dict` | Mutable workflow state, loaded before and saved after your handler |

**Key properties:**

- **One subprocess per event** — scripts are stateless between invocations; use `state` for persistence
- **30-second timeout** — if your script takes longer, it's killed and an error is posted to the channel
- **Errors are visible** — non-zero exit codes post `stderr` to the channel so failures don't go unnoticed
- **No restart needed** — the daemon re-resolves the script path on each event, so changes take effect immediately
- **Authoritative for PR lifecycle** — when a workflow script exists for a channel, it is the *sole authority* for 5 PR events (`pr.approved`, `pr.changes_requested`, `pr.ci_failed`, `pr.ci_passed`, `pr.conflict`). The daemon's compiled-in behavior is fully replaced, not supplemented

### Script Dependencies

Workflow scripts are invoked via `uv run`, which handles dependency resolution automatically. Declare dependencies using [PEP 723 inline script metadata](https://peps.python.org/pep-0723/) at the top of your script:

```python
# /// script
# requires-python = ">=3.9"
# dependencies = [
#   "transitions>=0.9",
#   "midtown-sdk",
#   "requests",           # add your own dependencies here
# ]
# ///
```

The `midtown-sdk` package provides the `run()` entry point and `MidtownRPC` client. Additional packages (HTTP clients, Slack SDKs, etc.) can be added as needed.

---

## Available Events

Every event is a JSON object with a `"type"` field (dotted name) and a `"channel"` field. Other fields vary by event type.

> **Important:** Optional fields are **omitted entirely** when absent — they are not serialized as `null`. Always use `event.get("field")` instead of `event["field"]` for optional fields.

### Task Lifecycle

#### `task.created`

A new task was created in the channel.

```json
{
  "type": "task.created",
  "channel": "proj-auth",
  "task_id": "42",
  "subject": "Fix auth timeout"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel the task belongs to |
| `task_id` | string | yes | The new task's ID |
| `subject` | string | yes | Task subject line |

#### `task.assigned`

A coworker claimed or was assigned a task.

```json
{
  "type": "task.assigned",
  "channel": "proj-auth",
  "task_id": "42",
  "coworker": "lexington"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel the task belongs to |
| `task_id` | string | yes | The assigned task's ID |
| `coworker` | string | yes | Name of the coworker who claimed it |

#### `task.completed`

A task was marked as completed.

```json
{
  "type": "task.completed",
  "channel": "proj-auth",
  "task_id": "42",
  "coworker": "lexington"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel the task belongs to |
| `task_id` | string | yes | The completed task's ID |
| `coworker` | string | optional | The coworker who completed it (omitted if unknown) |

### PR Lifecycle

All PR events include `channel`, `task_id`, and `pr_number`.

#### `pr.opened`

A coworker opened a pull request for a task.

```json
{
  "type": "pr.opened",
  "channel": "proj-auth",
  "task_id": "42",
  "pr_number": 123,
  "coworker": "lexington"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel |
| `task_id` | string | yes | Associated task ID |
| `pr_number` | integer | yes | GitHub PR number |
| `coworker` | string | yes | Coworker who opened the PR |

#### `pr.approved`

A PR received an approving review.

```json
{
  "type": "pr.approved",
  "channel": "proj-auth",
  "task_id": "42",
  "pr_number": 123
}
```

#### `pr.changes_requested`

A reviewer requested changes on a PR.

```json
{
  "type": "pr.changes_requested",
  "channel": "proj-auth",
  "task_id": "42",
  "pr_number": 123
}
```

#### `pr.merged`

A PR was merged.

```json
{
  "type": "pr.merged",
  "channel": "proj-auth",
  "task_id": "42",
  "pr_number": 123
}
```

#### `pr.ci_passed`

All CI checks passed on a PR.

```json
{
  "type": "pr.ci_passed",
  "channel": "proj-auth",
  "task_id": "42",
  "pr_number": 123
}
```

#### `pr.ci_failed`

A CI check failed on a PR.

```json
{
  "type": "pr.ci_failed",
  "channel": "proj-auth",
  "task_id": "42",
  "pr_number": 123,
  "check_name": "clippy"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `check_name` | string | optional | Name of the failing check (omitted if unavailable) |

#### `pr.conflict`

A PR has a merge conflict.

```json
{
  "type": "pr.conflict",
  "channel": "proj-auth",
  "task_id": "42",
  "pr_number": 123
}
```

### Coworker Lifecycle

#### `coworker.idle`

A coworker finished its current turn and is now idle.

```json
{
  "type": "coworker.idle",
  "channel": "proj-auth",
  "task_id": "42",
  "coworker": "lexington"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel |
| `task_id` | string | optional | Task the coworker was working on (omitted if none) |
| `coworker` | string | yes | Coworker name |

#### `coworker.stuck`

The daemon detected that a coworker appears stuck (no progress for an extended period).

```json
{
  "type": "coworker.stuck",
  "channel": "proj-auth",
  "task_id": "42",
  "coworker": "lexington"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel |
| `task_id` | string | optional | Task the coworker was working on (omitted if none) |
| `coworker` | string | yes | Coworker name |

#### `coworker.message`

A coworker posted a message to the channel.

```json
{
  "type": "coworker.message",
  "channel": "proj-auth",
  "task_id": "42",
  "coworker": "lexington",
  "message": "Found the root cause in auth.rs"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel |
| `task_id` | string | optional | Associated task (omitted if none) |
| `coworker` | string | yes | Coworker name |
| `message` | string | yes | Message content |

### Channel

#### `channel.message`

A human (non-coworker) posted a message to the channel.

```json
{
  "type": "channel.message",
  "channel": "proj-auth",
  "sender": "midtown",
  "message": "Let's prioritize the auth bug"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel |
| `sender` | string | yes | Message author name |
| `message` | string | yes | Message content |

### Timer

#### `timer.tick`

Periodic heartbeat emitted on each `TaskDispatchTick` cycle. Use this for reconciliation, deadline checking, or other periodic bookkeeping.

```json
{
  "type": "timer.tick",
  "channel": "proj-auth"
}
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `channel` | string | yes | Channel |

---

## RPC Methods

Your handler receives an `rpc` object (`MidtownRPC`) that can call back to the daemon. All methods communicate over a Unix socket using JSON-RPC 2.0.

### Channel

#### `rpc.post_to_channel(message, *, channel=None, sender=None, thread_parent_id=None)`

Post a message to a channel.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `message` | `str` | (required) | Message text |
| `channel` | `str` | event's channel | Target channel (defaults to daemon's default) |
| `sender` | `str` | repo name | Display name for the message author |
| `thread_parent_id` | `str` | `None` | Post as a thread reply under this message ID |

```python
rpc.post_to_channel("Build complete — all tests passing ✅")
rpc.post_to_channel("Detailed results...", thread_parent_id="msg-123")
```

### Tasks

#### `rpc.create_task(subject, *, description="", channel=None, blocked_by=None, model=None)`

Create a new task.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `subject` | `str` | (required) | One-line imperative task title |
| `description` | `str` | `""` | Multi-line task body |
| `channel` | `str` | `None` | Channel to associate the task with |
| `blocked_by` | `list[str]` | `None` | Task IDs that must complete first |
| `model` | `str` | `None` | Provider/model (e.g. `"claude/sonnet"`) |

```python
rpc.create_task(
    "Add rate limiting to auth endpoint",
    description="Limit to 10 requests per minute per IP",
    channel="proj-auth",
    blocked_by=["41"],  # wait for task 41 to finish first
)
```

#### `rpc.update_task(task_id, *, owner=None, status=None, description=None, blocked_by=None, channel=None, model=None, pr=None)`

Update an existing task.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `task_id` | `str` | (required) | Task ID to update |
| `owner` | `str` | `None` | Assign to this coworker |
| `status` | `str` | `None` | `"pending"`, `"in_progress"`, or `"completed"` |
| `description` | `str` | `None` | Replace task description |
| `blocked_by` | `list[str]` | `None` | Replace blocked-by list |
| `channel` | `str` | `None` | Reassign to a different channel |
| `model` | `str` | `None` | Change execution model |
| `pr` | `int` | `None` | Associate a GitHub PR number |

```python
rpc.update_task("42", status="in_progress", owner="lexington")
```

#### `rpc.complete_task(task_id)`

Mark a task as done. This unblocks any downstream tasks that have this task in their `blocked_by` list.

```python
rpc.complete_task("42")
```

#### `rpc.list_tasks()`

Return the current task list (kanban data) as a dict.

```python
tasks = rpc.list_tasks()
```

### Coworkers

#### `rpc.spawn_coworker(*, prompt=None, resume=False)`

Spawn a new coworker session.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `prompt` | `str` | `None` | Initial prompt for the coworker |
| `resume` | `bool` | `False` | Resume the most-recently-stopped session instead |

```python
rpc.spawn_coworker(
    prompt="Please review PR #123. Use the code-review skill."
)
```

#### `rpc.nudge_coworker(name, message, *, sender=None)`

Send a message to an existing coworker, waking them up if idle.

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `name` | `str` | (required) | Coworker name (e.g. `"lexington"`) |
| `message` | `str` | (required) | Nudge content |
| `sender` | `str` | repo name | Display name for the sender |

```python
rpc.nudge_coworker(
    "lexington",
    "PR #123 is approved — please merge when ready"
)
```

### Daemon

#### `rpc.check_pending()`

Trigger immediate dispatch of pending tasks. The daemon dispatches on its own schedule, so this is an optimization to reduce latency after creating tasks or when coworkers go idle.

```python
try:
    rpc.check_pending()
except Exception:
    pass  # non-critical, daemon will dispatch eventually
```

### Error Handling

RPC methods raise `midtown.RpcError` on failure:

```python
from midtown import RpcError

try:
    rpc.nudge_coworker("nonexistent", "hello")
except RpcError as e:
    print(f"RPC failed: {e}")  # RPC error -32000: coworker not found
    print(f"Code: {e.code}, Data: {e.data}")
```

---

## State Management

Since workflow scripts are short-lived subprocesses (one `uv run` per event), the `state` dict provides persistent storage between invocations.

### How It Works

1. Before your handler is called, the SDK loads state from `~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json` (empty dict `{}` if the file doesn't exist yet)
2. Your handler mutates `state` freely
3. After your handler returns, the SDK saves `state` atomically (write to temp file, then rename)

You control the state structure — it can be whatever your workflow needs.

### Example: Tracking Task States

The reference implementation uses a per-task state machine:

```python
def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    task_id = event.get("task_id")
    if not task_id:
        return

    # Initialize task state on first event
    tasks = state.setdefault("tasks", {})
    task = tasks.setdefault(task_id, {"status": "pending"})

    if event["type"] == "task.assigned":
        task["status"] = "in_progress"
        task["coworker"] = event["coworker"]

    elif event["type"] == "pr.opened":
        task["status"] = "in_review"
        task["pr_author"] = event["coworker"]
        task["pr_number"] = event["pr_number"]

    elif event["type"] == "pr.merged":
        task["status"] = "merged"
        rpc.complete_task(task_id)
```

### Example: Custom Counters

```python
def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    # Track CI failure count per PR
    if event["type"] == "pr.ci_failed":
        pr = str(event["pr_number"])
        failures = state.setdefault("ci_failures", {})
        failures[pr] = failures.get(pr, 0) + 1

        if failures[pr] >= 3:
            rpc.post_to_channel(
                f"⚠️ PR #{pr} has failed CI {failures[pr]} times"
            )
```

### State File Location

State is always stored at:

```
~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json
```

This is a **local** file (never committed to the repo). Each channel gets its own independent state file, even if multiple channels share the same `workflow.py` script.

---

## Script Resolution Order

The daemon resolves workflow scripts using a 4-level priority order. The first file found wins:

| Priority | Path | Scope | Committed? |
|----------|------|-------|------------|
| 1 | `<project>/.midtown/channels/<channel>/workflow.py` | Channel-specific | Yes (shared with team) |
| 2 | `~/.midtown/projects/<repo>/channels/<channel>/workflow.py` | Channel-specific | No (local only) |
| 3 | `<project>/.midtown/workflow.py` | All channels | Yes (shared with team) |
| 4 | `~/.midtown/projects/<repo>/workflow.py` | All channels | No (local only) |

If no script is found at any level, the daemon falls back to its compiled-in default behavior.

### When to Use Each Level

**Channel-specific, committed** (level 1) — Your team agreed on a custom workflow for the `proj-auth` channel that should be version-controlled:

```
.midtown/channels/proj-auth/workflow.py
```

**Channel-specific, local** (level 2) — You want to override behavior for a channel on your machine only (e.g., extra logging during debugging):

```
~/.midtown/projects/myrepo/channels/proj-auth/workflow.py
```

**Project default, committed** (level 3) — One workflow for all channels, shared with your team:

```
.midtown/workflow.py
```

**Project default, local** (level 4) — A personal default workflow for all channels on your machine:

```
~/.midtown/projects/myrepo/workflow.py
```

### Resolution in Practice

The layered system means you can:

- Commit a project-wide workflow (level 3) as the team default
- Override specific channels with custom behavior (level 1) — also committed
- Apply local machine-specific tweaks (levels 2 and 4) without affecting teammates

---

## Examples

### Skip Reviewer Spawning for a Channel

If a channel handles reviews externally (e.g., human reviewers), you can skip the automatic reviewer spawn:

```python
# .midtown/channels/proj-docs/workflow.py
# /// script
# requires-python = ">=3.9"
# dependencies = ["midtown-sdk"]
# ///
from midtown import run, MidtownRPC

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    if event["type"] == "pr.opened":
        rpc.post_to_channel(
            f"PR #{event['pr_number']} opened by {event['coworker']} — "
            "skipping auto-review (human review required for docs)"
        )
        # No rpc.spawn_coworker() call → no reviewer spawned

    elif event["type"] == "pr.approved":
        author = event.get("coworker", "")
        if author:
            rpc.nudge_coworker(author, f"PR #{event['pr_number']} approved — merge when ready")

    elif event["type"] == "pr.merged":
        task_id = event.get("task_id")
        if task_id:
            rpc.complete_task(task_id)

if __name__ == "__main__":
    run(handle)
```

### Post to Slack on PR Approval

Replace the default nudge-the-author behavior with a Slack notification:

```python
# .midtown/channels/proj-api/workflow.py
# /// script
# requires-python = ">=3.9"
# dependencies = [
#   "midtown-sdk",
#   "requests",
# ]
# ///
import os
import requests
from midtown import run, MidtownRPC

SLACK_WEBHOOK = os.environ.get("SLACK_WEBHOOK_URL", "")

def post_slack(text: str) -> None:
    if SLACK_WEBHOOK:
        requests.post(SLACK_WEBHOOK, json={"text": text}, timeout=10)

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    if event["type"] == "pr.approved":
        pr = event["pr_number"]
        post_slack(f"✅ PR #{pr} approved and ready to merge")
        rpc.post_to_channel(f"Slack notified about PR #{pr} approval")

    elif event["type"] == "pr.ci_failed":
        pr = event["pr_number"]
        check = event.get("check_name", "unknown")
        post_slack(f"❌ CI failed on PR #{pr}: {check}")

if __name__ == "__main__":
    run(handle)
```

### Add a Senior Approval Gate Before Merge

Require a senior reviewer's approval before allowing merge, instead of merging on any approval:

```python
# .midtown/channels/proj-core/workflow.py
# /// script
# requires-python = ">=3.9"
# dependencies = ["midtown-sdk"]
# ///
from midtown import run, MidtownRPC

# Only these reviewers' approvals count for auto-merge
SENIOR_REVIEWERS = {"alice", "bob", "carol"}

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    task_id = event.get("task_id")

    if event["type"] == "pr.opened" and task_id:
        pr = event["pr_number"]
        coworker = event["coworker"]
        # Track the PR author for later nudging
        tasks = state.setdefault("tasks", {})
        tasks[task_id] = {
            "pr_author": coworker,
            "pr_number": pr,
            "senior_approved": False,
        }
        rpc.post_to_channel(
            f"PR #{pr} opened by {coworker} — requires senior review "
            f"({', '.join(SENIOR_REVIEWERS)})"
        )
        rpc.spawn_coworker(
            prompt=f"Review PR #{pr} opened by {coworker}."
        )

    elif event["type"] == "pr.approved" and task_id:
        task_data = state.get("tasks", {}).get(task_id, {})
        pr = event.get("pr_number")
        author = task_data.get("pr_author", "")

        # Check if the approver is a senior reviewer
        # (In a real implementation, you'd get the reviewer name from the event
        # or the GitHub API. This example uses a simplified check.)
        task_data["senior_approved"] = True
        state.setdefault("tasks", {})[task_id] = task_data

        if author:
            rpc.nudge_coworker(
                author,
                f"PR #{pr} has senior approval — clear to merge"
            )

    elif event["type"] == "pr.merged" and task_id:
        rpc.complete_task(task_id)
        # Clean up state
        state.get("tasks", {}).pop(task_id, None)

if __name__ == "__main__":
    run(handle)
```

### Disable Stuck Coworker Warnings

For channels with long-running tasks (e.g., large migrations), suppress the "stuck" notifications:

```python
# .midtown/channels/proj-migration/workflow.py
# /// script
# requires-python = ">=3.9"
# dependencies = ["midtown-sdk"]
# ///
from midtown import run, MidtownRPC

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    if event["type"] == "coworker.stuck":
        # Silently ignore — no channel post, no restart notification.
        # The daemon still handles restarts; we just skip the message.
        return

    # For all other events, replicate the default behavior you want.
    # Here's a minimal set for PR lifecycle:
    if event["type"] == "pr.opened":
        rpc.post_to_channel(
            f"PR #{event['pr_number']} opened by {event['coworker']}"
        )
        rpc.spawn_coworker(
            prompt=f"Review PR #{event['pr_number']} by {event['coworker']}."
        )

    elif event["type"] == "pr.approved":
        task_id = event.get("task_id")
        author = state.get("tasks", {}).get(task_id, {}).get("pr_author") if task_id else None
        if author:
            rpc.nudge_coworker(author, f"PR #{event['pr_number']} approved")

    elif event["type"] == "pr.merged":
        task_id = event.get("task_id")
        if task_id:
            rpc.complete_task(task_id)

    elif event["type"] == "task.created":
        try:
            rpc.check_pending()
        except Exception:
            pass

    elif event["type"] == "coworker.idle":
        try:
            rpc.check_pending()
        except Exception:
            pass

if __name__ == "__main__":
    run(handle)
```

### Use `timer.tick` for Deadline Alerts

```python
# .midtown/workflow.py (project-wide)
# /// script
# requires-python = ">=3.9"
# dependencies = ["midtown-sdk"]
# ///
import time
from midtown import run, MidtownRPC

# Alert if a task has been in_progress for more than 2 hours
STALE_THRESHOLD_SECS = 2 * 60 * 60

def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    task_id = event.get("task_id")
    now = time.time()

    if event["type"] == "task.assigned" and task_id:
        tasks = state.setdefault("tasks", {})
        tasks[task_id] = {"started_at": now, "alerted": False}

    elif event["type"] == "timer.tick":
        tasks = state.get("tasks", {})
        for tid, data in tasks.items():
            started = data.get("started_at", now)
            if now - started > STALE_THRESHOLD_SECS and not data.get("alerted"):
                hours = (now - started) / 3600
                rpc.post_to_channel(
                    f"⏰ Task !{tid} has been in progress for {hours:.1f}h"
                )
                data["alerted"] = True

    elif event["type"] in ("pr.merged", "task.completed") and task_id:
        state.get("tasks", {}).pop(task_id, None)

if __name__ == "__main__":
    run(handle)
```

---

## Testing

### Test Locally with the CLI

You can invoke your workflow script directly to test event handling without waiting for real daemon events:

```bash
# Test a pr.approved event
uv run .midtown/workflow.py \
    --event '{"type":"pr.approved","channel":"proj-auth","task_id":"42","pr_number":123}' \
    --state /tmp/test-workflow-state.json \
    --socket /path/to/daemon.sock
```

To find the daemon socket path for your project:

```bash
ls ~/.local/state/midtown/*/daemon.sock
```

### Dry-Run Mode

For testing without making real RPC calls, you can wrap the handler to intercept RPC calls:

```python
# test_workflow.py
import json
import sys

# Import your workflow handler
sys.path.insert(0, ".midtown")
from workflow import handle

class MockRPC:
    """Records RPC calls instead of executing them."""
    def __init__(self):
        self.calls = []

    def __getattr__(self, name):
        def recorder(*args, **kwargs):
            self.calls.append((name, args, kwargs))
            print(f"  RPC: {name}({args}, {kwargs})")
        return recorder

# Test events
events = [
    {"type": "pr.opened", "channel": "test", "task_id": "1", "pr_number": 99, "coworker": "lex"},
    {"type": "pr.approved", "channel": "test", "task_id": "1", "pr_number": 99},
    {"type": "pr.merged", "channel": "test", "task_id": "1", "pr_number": 99},
]

state = {}
rpc = MockRPC()

for event in events:
    print(f"\n→ {event['type']}")
    handle(event, rpc, state)

print(f"\nFinal state: {json.dumps(state, indent=2)}")
```

```bash
uv run test_workflow.py
```

### Test Against a Running Daemon

If you have a running Midtown session, you can trigger events through normal operations:

1. Create a task: `midtown task create "Test task" --channel proj-test`
2. Watch the channel: `midtown channel read`
3. Verify your script's response appears in the channel

### Checking for Errors

If your script has a bug, the daemon posts the error to the channel:

```
⚠️ workflow.py error: NameError: name 'undefined_var' is not defined
```

Check the daemon logs for more detail:

```bash
tail -f ~/.local/state/midtown/<repo>/daemon.log | grep workflow
```

---

## Troubleshooting

### Script Not Being Picked Up

- Verify the file path matches the [resolution order](#script-resolution-order)
- The file must be named exactly `workflow.py`
- Check file permissions — the daemon runs `uv run`, which needs read access

### Script Timing Out

Scripts have a 30-second timeout. If you need external API calls, use short timeouts:

```python
requests.post(url, json=data, timeout=5)  # don't use the full 30s
```

### State File Corruption

The SDK uses atomic writes (temp file + rename), so corruption is rare. If it happens:

```bash
# Reset state for a channel
rm ~/.midtown/projects/<repo>/channels/<channel>/workflow-state.json
```

### RPC Errors

If `rpc.nudge_coworker()` fails, the coworker might not exist or might have already shut down. Wrap non-critical calls in try/except:

```python
try:
    rpc.nudge_coworker(author, "PR approved")
except Exception:
    rpc.post_to_channel(f"Could not reach {author} — they may have shut down")
```

### Debugging Event Payloads

Add temporary logging to see exactly what events your script receives:

```python
def handle(event: dict, rpc: MidtownRPC, state: dict) -> None:
    import json, sys
    print(json.dumps(event, indent=2), file=sys.stderr)
    # ... rest of handler
```

Stderr output from successful runs is captured but not posted to the channel — check the daemon log to see it. Non-zero exit codes will post stderr to the channel.
