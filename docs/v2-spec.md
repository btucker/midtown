# Daemon V2 — Requirements

**Status:** In progress
**Last updated:** 2026-03-29

## User Stories

### US-1: Daemon Startup
As a developer, I want to start the v2 daemon with a single command, so that all agent coordination begins automatically.

**Acceptance Criteria:**
- WHEN the user runs `MIDTOWN_DAEMON_V2=1 midtown start` THEN the system SHALL recover state from the event store (snapshot + replay)
- WHEN the daemon starts AND agents were previously running THEN the system SHALL check PIDs AND resume agents with valid session IDs
- WHEN the daemon starts THEN the system SHALL bind the same Unix socket as v1 so CLI and web UI work transparently
- WHEN a web port is configured THEN the system SHALL start an HTTP server for the web UI

### US-2: Agent Lifecycle
As a developer, I want agents to be spawned, stopped, and resumed reliably, so that work continues without manual intervention.

**Acceptance Criteria:**
- WHEN a worker agent is spawned THEN the system SHALL create an isolated git worktree with a task-specific branch
- WHEN a lead or fork agent is spawned THEN the system SHALL use the shared lead worktree
- WHEN an agent process dies THEN the system SHALL detect it via process health polling AND emit an AgentStopped event
- WHEN a stopped agent has a session ID AND receives a nudge THEN the system SHALL resume the session before delivering the message
- WHEN an agent has been stopped for more than 24 hours AND is not a lead THEN the system SHALL garbage-collect its record

### US-3: Task Dispatch
As a developer, I want tasks to be automatically assigned to workers, so that pending work is picked up without manual coordination.

**Acceptance Criteria:**
- WHEN a task is pending AND unblocked AND fewer than max concurrent tasks are in progress THEN the system SHALL spawn a worker agent for it
- WHEN a worker dies while its task is in progress THEN the system SHALL reset the task to pending for re-dispatch
- WHEN two agents are assigned to the same task THEN the system SHALL stop the older one
- WHEN a worker has no task for more than 5 minutes THEN the system SHALL stop it
- WHEN a task declares `blocked_by` dependencies THEN the system SHALL NOT dispatch it until all blockers are completed

### US-4: Message Routing
As a developer, I want messages to reach the right agent automatically, so that communication flows without explicit addressing.

**Acceptance Criteria:**
- WHEN a message is posted to a channel THEN the system SHALL nudge the channel lead
- WHEN a thread reply is posted AND an agent is bound to that thread THEN the system SHALL nudge the thread-bound agent instead of the channel lead
- WHEN a thread reply is posted AND no agent is bound to that thread THEN the system SHALL nudge the channel lead
- WHEN a message contains `@agent-name` THEN the system SHALL nudge the named agent
- WHEN a message contains `@all` THEN the system SHALL nudge every agent in the channel except the sender
- WHEN a message contains `@lead` or `@channel-name` THEN the system SHALL nudge the channel lead
- WHEN a message contains `!N` THEN the system SHALL nudge the agent assigned to task N
- WHEN the nudge target is stopped AND has a session ID THEN the system SHALL resume the agent before delivering the message
- WHEN the sender is the same as the nudge target THEN the system SHALL NOT nudge (self-nudge suppression)
- WHEN multiple routing rules match the same agent THEN the system SHALL nudge it exactly once (deduplication)

### US-5: PR Integration
As a developer, I want PRs to be monitored and reviewed automatically, so that the review cycle doesn't stall.

**Acceptance Criteria:**
- WHEN the system polls GitHub AND a new PR is open THEN the system SHALL emit a PrOpened event
- WHEN a PR needs review AND no reviewer agent is running for it THEN the system SHALL spawn a reviewer agent
- WHEN a reviewer agent dies without posting a review AND fewer than 3 attempts have been made THEN the system SHALL spawn a new reviewer
- WHEN a reviewer agent has failed 3 times for a PR THEN the system SHALL post an escalation message to the ops channel AND SHALL NOT spawn another reviewer
- WHEN a PR merges AND a task is linked to it THEN the system SHALL complete the task AND clean up the worktree
- WHEN a PR merges THEN the system SHALL nudge workers with other open PRs to rebase (at most once per hour per agent)
- WHEN a worker's task has an open PR awaiting review THEN the system SHALL stop the worker (it's waiting)

### US-6: Channel Leads
As a developer, I want every active channel to have a lead agent, so that messages are always handled.

**Acceptance Criteria:**
- WHEN a non-archived channel exists AND has no running lead THEN the system SHALL spawn a lead agent for it
- WHEN the channel is the default channel THEN the system SHALL use `midtown-project-lead` as the agent type
- WHEN the channel is a topic channel THEN the system SHALL use `midtown-channel-lead` as the agent type
- WHEN a channel has a `directory` setting THEN the system SHALL pass it as the lead's working directory so AGENTS.md loads from that subdirectory
- WHEN a channel has `lead_driven: true` THEN the system SHALL NOT auto-dispatch tasks for that channel

### US-7: Session Persistence
As a developer, I want agent sessions to survive daemon restarts, so that work isn't lost.

**Acceptance Criteria:**
- WHEN the daemon restarts AND an agent was previously running with a session ID THEN the system SHALL resume the agent session
- WHEN an agent is resumed THEN the system SHALL reset its `started_at` timestamp so idle checks use the resume time
- WHEN a fork agent stops THEN the system SHALL preserve its thread binding so it can be resumed on future thread activity
- WHEN an agent is garbage-collected THEN the system SHALL remove its thread binding AND all index entries

### US-8: Web UI
As a developer, I want a web interface for monitoring and interacting with the system.

**Acceptance Criteria:**
- WHEN a domain event occurs THEN the system SHALL broadcast it to all connected WebSocket clients
- WHEN the web UI requests channel history THEN the system SHALL return messages with optional thread filtering
- WHEN the web UI searches with `GET /api/search?q=...` THEN the system SHALL return matches across all channels
- WHEN the web UI requests status THEN the system SHALL return agent counts, task list, and open PRs

### US-9: Cooldowns
As a developer, I want rate-limiting on repeated actions, so that the system doesn't spam agents or APIs.

**Acceptance Criteria:**
- WHEN a rebase nudge has been sent to an agent THEN the system SHALL NOT send another for 1 hour
- WHEN a spawn fails THEN the system SHALL NOT retry for 2 minutes
- WHEN an orphan spawn occurs THEN the system SHALL NOT spawn another for 1 minute

## Unchanged Behavior

These behaviors from v1 SHALL CONTINUE TO work identically:

- WHEN the user runs `midtown status` THEN the system SHALL CONTINUE TO return daemon status via the same RPC protocol
- WHEN the user runs `midtown channel post` THEN the system SHALL CONTINUE TO write messages to channel JSONL files
- WHEN the user runs `midtown task create` THEN the system SHALL CONTINUE TO accept the same parameters
- WHEN v1 RPC methods (`coworker.spawn`, `coworker.break`, `ping`, `version`) are called THEN the system SHALL CONTINUE TO handle them via compatibility aliases

## Not Yet Implemented

The following v1 capabilities are not yet ported:

- Webhook forwarder watchdog (`gh webhook forward` process management)
- Background chat monitor (tail loop on channel JSONL)
- GitHub API rate limit monitoring
- Auth profile pooling (multi-account rotation)
- Reminder system (cron + event-based triggers)
- Workflow system (assignment, state machine)
- Task prompt / handoff between agents
- Session attach/detach (interactive takeover)
- CI issue detection (stale checks, auto-rerun)

## Revision History

| Date | Change |
|------|--------|
| 2026-03-29 | Initial spec covering US-1 through US-9. |
