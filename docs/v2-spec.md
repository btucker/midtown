# Daemon V2 — Requirements

**Status:** Reviewed
**Last updated:** 2026-03-29

---

## 1. Message Routing

### 1.1 Thread Routing
- WHEN a message is posted to a thread THEN the system SHALL nudge the thread-bound agent if one exists, OTHERWISE the channel lead
- WHEN a top-level message is posted THEN the system SHALL nudge the channel lead

### 1.2 @Mentions
- WHEN a message contains `@agent-name` THEN the system SHALL nudge the named agent
- WHEN a message contains `@all` in the main channel THEN the system SHALL nudge ALL channel leads AND ALL agents bound to in-progress tasks across ALL channels, excluding the sender
- WHEN a message contains `@all` in a topic channel THEN the system SHALL nudge the channel lead AND every agent in that channel bound to an in-progress task, excluding the sender
- WHEN a message contains `@channel-name` THEN the system SHALL nudge that channel's lead
- WHEN a @mention refers to an unknown agent THEN the system SHALL NOT emit a nudge
- WHEN a @mention contains trailing punctuation THEN the system SHALL strip it before lookup

### 1.3 Task References
- WHEN a message contains `!N` THEN the system SHALL nudge the agent assigned to task N AND agents assigned to all descendant tasks of N
- WHEN a task is created with a `parent` field THEN the system SHALL record the parent-child relationship
- WHEN a task reference has no assigned agent THEN the system SHALL NOT emit a nudge for that reference

### 1.4 Nudge Invariants
- WHEN the sender is the same as the nudge target THEN the system SHALL suppress the nudge
- WHEN multiple routing rules match the same agent for the same message THEN the system SHALL nudge it exactly once
- WHEN the nudge target is stopped AND has a session ID THEN the system SHALL resume the agent before delivering the message
- WHEN the nudge target is stopped AND has no session ID THEN the system SHALL spawn a new agent with the same configuration (kind, agent_type, channel, task_id, bound_thread_id) and deliver the nudge to it
- WHEN the nudge target is unknown THEN the system SHALL drop the nudge

---

## 2. Task Dispatch

### 2.1 Spawning Workers
- WHEN pending unblocked tasks exist AND fewer than max_in_progress tasks are running THEN the system SHALL spawn a worker for each available slot
- WHEN a task has no `agent_type` THEN the system SHALL use `midtown-code-author` as default
- WHEN a task is in a `lead_driven` channel THEN the system SHALL NOT auto-dispatch it
- WHEN spawning a worker THEN the system SHALL use name/icon/color from the task if set, OTHERWISE from the spawn command if set, OTHERWISE generate random values
- WHEN spawning a worker THEN the worker's auto-output channel SHALL be `dm-{agent_name}`, NOT the task's channel
- WHEN a task has a `thread_id` THEN the worker SHALL be spawned with `bound_thread_id` set to that thread

### 2.2 Task Lifecycle
- WHEN a worker dies while its task is InProgress THEN the system SHALL resume the worker
- WHEN a worker cannot be resumed (no session ID) THEN the system SHALL spawn a replacement worker with the same task configuration
- WHEN a worker has failed 3 consecutive spawn attempts (died within 60s of start) THEN the system SHALL stop retrying AND post to the ops channel
- WHEN two agents are assigned to the same task THEN the system SHALL stop the newer one
- WHEN a worker reports its state as idle via `midtown state` THEN the system SHALL stop it after 2 minutes if it remains idle
- WHEN a worker has not reported a state change for 5 minutes THEN the system SHALL nudge it
- WHEN a running worker's task is Completed THEN the system SHALL stop the worker
- WHEN a task declares `blocked_by` dependencies THEN the system SHALL NOT dispatch it until all blockers are completed
- WHEN a blocker task completes THEN the system SHALL remove it from the blocked_by lists of all dependent tasks — if a dependent task's blocked_by list becomes empty, it SHALL become eligible for dispatch
- WHEN a task is updated via `task.update` AND it has an assigned agent THEN the system SHALL nudge that agent with a message describing the update

---

## 3. PR Integration

### 3.1 Event Sources
- WHEN a GitHub webhook reports a PR opened THEN the system SHALL emit a PrOpened event
- WHEN polling detects a new open PR not already tracked THEN the system SHALL emit a PrOpened event AND a PrReviewRequested event (if not draft) as a backstop
- WHEN a PrOpened event is processed AND the author matches a running worker THEN the system SHALL emit PrLinkedToTask to associate the PR with that worker's task
- WHEN a GitHub webhook reports CI or review state change THEN the system SHALL emit a PrUpdated event
- WHEN polling detects a CI or review state change not already reflected THEN the system SHALL emit a PrUpdated event as a backstop
- WHEN a GitHub webhook reports a PR merged THEN the system SHALL emit a PrMerged event
- WHEN polling detects a merged PR not already tracked THEN the system SHALL emit a PrMerged event as a backstop
- WHEN polling fails THEN the system SHALL log the error and return no events

### 3.2 Reviewer Spawning
- WHEN a new non-draft PR is opened (via webhook or polling backstop) THEN the system SHALL emit PrReviewRequested
- WHEN a PR needs review AND no reviewer is running for it THEN the system SHALL spawn a reviewer named `{author_name}-reviewer`
- WHEN a reviewer dies THEN the system SHALL resume it
- WHEN a reviewer cannot be resumed (no session ID) AND fewer than 3 attempts have been made THEN the system SHALL spawn a replacement reviewer
- WHEN a reviewer has failed 3 times THEN the system SHALL post to the ops channel AND SHALL NOT spawn another
- WHEN spawning a reviewer THEN the initial prompt SHALL be `Review PR #{pr_num}: {branch}`

### 3.3 PR Lifecycle
- WHEN a PR merges AND has a linked InProgress task THEN the system SHALL complete the task
- WHEN a PR merges THEN the system SHALL nudge workers with other open PRs to rebase (1hr cooldown per agent)
- WHEN a worker's task has an open PR awaiting review THEN the system SHALL stop the worker
- WHEN a new comment is posted on a PR (top-level or inline) THEN the system SHALL post the comment to the task's channel thread AND nudge the author agent, UNLESS the comment's frontmatter identifies the commenter as the author agent

### 3.4 CI Status Parsing
- WHEN statusCheckRollup contains any FAILURE/TIMED_OUT/CANCELLED THEN CI status SHALL be Failed
- WHEN statusCheckRollup contains PENDING/QUEUED/IN_PROGRESS THEN CI status SHALL be Running
- WHEN statusCheckRollup is empty or all SUCCESS THEN CI status SHALL be Passed
- WHEN a PR is draft THEN needs_review SHALL be false regardless of reviewDecision

---

## 4. Agent Lifecycle

### 4.1 Spawning
- WHEN a worker is spawned THEN the system SHALL create an isolated git worktree with a task-specific branch
- WHEN a lead or fork is spawned THEN the system SHALL use the shared lead worktree
- WHEN a lead already has a working_dir override (channel directory) THEN the worktree manager SHALL NOT override it
- WHEN a task already has a worktree from a previous dispatch THEN the system SHALL reuse it
- WHEN an agent is resumed THEN the system SHALL derive and set its worktree path (same as fresh spawn)
- WHEN an agent is spawned AND its output is not bound to a channel or thread THEN the system SHALL auto-create a DM channel `dm-{agent_name}`
- WHEN spawning succeeds THEN AgentCreated and AgentStarted events SHALL be emitted
- WHEN spawning fails THEN the system SHALL emit an AgentSpawnFailed event with the agent configuration and error reason
- WHEN a session is spawned THEN stdout/stderr SHALL be drained in a background task
- WHEN an agent produces assistant text on stdout THEN the system SHALL auto-post it to the agent's bound channel
- WHEN an agent has no bound channel THEN stdout text SHALL be posted to the agent's DM channel
- WHEN multiple stdout events accumulate THEN the system SHALL flush and post at most every 2 seconds

### 4.2 Stopping
- WHEN StopAgent is executed THEN the session process SHALL be killed AND AgentStopped emitted, but the session ID SHALL be preserved for potential resume
- WHEN the kill fails THEN the system SHALL emit AgentStopFailed with the error reason
- WHEN an agent process exits (detected by try_wait) THEN AgentStopped SHALL be emitted

### 4.3 Resuming
- WHEN ResumeAgent is executed THEN the session SHALL be spawned with `resume_session_id` set
- WHEN resume succeeds THEN AgentResumed SHALL be emitted with the new PID
- WHEN an agent is resumed THEN `started_at` SHALL be reset to now
- WHEN resume fails THEN the system SHALL emit an AgentSpawnFailed event with the agent configuration and error reason

### 4.4 Spawn Failure Cooldown
- WHEN any agent (lead OR worker) dies within 60 seconds of starting THEN the system SHALL record a SpawnFailure cooldown
- WHEN a SpawnFailure cooldown is active for a worker's task THEN health checks and dispatch SHALL NOT re-spawn that worker until the cooldown expires (120 seconds)
- WHEN a SpawnFailure cooldown is active for a channel THEN lead spawning SHALL NOT occur until the cooldown expires
- The cooldown key for leads is the channel name; the cooldown key for workers is the task ID

### 4.5 Garbage Collection
- WHEN the number of stopped agent records exceeds a threshold THEN the system SHALL garbage-collect the oldest stopped agents (non-Lead), retaining at least 24 hours of history
- WHEN an agent is garbage-collected THEN it SHALL be marked as GC'd AND excluded from routing, dispatch, and active queries, but its record SHALL be preserved

### 4.6 Fork Sessions
- WHEN a fork session spawns THEN it SHALL be bound to a thread via `bound_thread_id`
- WHEN a fork session stops THEN its thread binding SHALL persist AND any subsequent nudge to the thread SHALL resume the fork
- WHEN a fork is spawned THEN it SHALL inherit the parent lead's session context by using the underlying Claude Code `--fork-session` feature
- WHEN a session.fork request arrives AND a running fork exists for the thread THEN the existing fork ID SHALL be returned

---

## 5. Channel Management

### 5.1 Channel Leads
- WHEN a user messages a channel AND no lead agent exists for that channel THEN the system SHALL spawn one
- WHEN spawning a lead for the default channel THEN agent_type SHALL be `midtown-project-lead`
- WHEN spawning a lead for a topic channel THEN agent_type SHALL be `midtown-channel-lead`
- WHEN a channel has a `directory` setting THEN the lead's working_dir SHALL be set to that subdirectory
- WHEN looking up a channel's lead AND multiple lead agents exist for that channel THEN the system SHALL prefer the running one
- WHEN looking up a channel's lead AND no lead is running THEN the system SHALL return the most recently created non-GC'd lead (for resume-on-nudge)

### 5.2 Channel Settings
- WHEN `lead_driven` is set to true THEN automatic task dispatch SHALL be skipped for that channel
- WHEN a channel is archived THEN its directory SHALL be renamed with `.archived` suffix
- WHEN a channel is unarchived THEN the `.archived` suffix SHALL be removed

### 5.3 Channel I/O
- WHEN a message is posted THEN it SHALL be written to the channel's JSONL file
- WHEN messages are read from a channel THEN thread replies SHALL be excluded UNLESS the read request specifies a thread_parent_id
- WHEN messages are read with a thread_parent_id THEN only messages in that thread SHALL be returned
- WHEN messages are read with a limit THEN the last N messages SHALL be returned
- WHEN a system message is posted THEN sender SHALL be `midtown`
- WHEN a channel's JSONL file exceeds 10MB THEN the system SHALL roll to a new file
- WHEN reading or searching channel messages THEN the system SHALL operate across all message files for that channel

---

## 6. Projections

### 6.1 AgentIndex
- WHEN AgentCreated is applied THEN the agent SHALL be indexed by id, name, task, channel, and thread
- WHEN AgentStarted is applied THEN pid and session_id SHALL be set, agent added to running set, started_at set to now
- WHEN AgentStopped is applied THEN agent removed from running set, stopped_at set, thread binding preserved
- WHEN AgentResumed is applied THEN pid updated, started_at reset, stopped_at cleared, added back to running set
- WHEN AgentGarbageCollected is applied THEN agent marked as GC'd, excluded from routing and active queries

### 6.2 WorkIndex
- WHEN TaskCreated is applied THEN task added to tasks map and pending_tasks list
- WHEN TaskCreated has blocked_by THEN task added to blocked map
- WHEN TaskAssigned is applied THEN status changes to InProgress, moved from pending to in_progress list
- WHEN TaskCompleted is applied THEN status changes to Completed, removed from in_progress, completed_at set
- WHEN TaskReset is applied THEN status reverts to Pending, moved back to pending list
- WHEN TaskUnblocked is applied THEN task removed from blocked map
- WHEN PrOpened is applied THEN PR added to prs map and open_prs list
- WHEN PrUpdated is applied THEN ci_status and review_state updated
- WHEN PrMerged is applied THEN is_merged/is_closed set, removed from open_prs and needing_review
- WHEN PrClosed is applied THEN is_closed set, removed from open_prs and needing_review
- WHEN PrReviewRequested is applied THEN needs_review set, added to needing_review list
- WHEN PrLinkedToTask is applied THEN task's pr_number set

### 6.3 ChannelIndex
- WHEN MessagePosted is applied THEN channel ensured to exist, last_message_at updated
- WHEN MessagePosted has thread_id THEN thread added to known_threads and thread_count incremented
- WHEN ChannelLeadDrivenSet is applied THEN lead_driven setting updated
- WHEN ChannelDirectorySet is applied THEN directory setting updated

### 6.4 CooldownTracker
- WHEN OrphanSpawn cooldown recorded THEN 60s cooldown active
- WHEN AgentDispatch cooldown recorded THEN 30s cooldown active
- WHEN SpawnFailure cooldown recorded THEN 120s cooldown active
- WHEN MergeRebaseNudge cooldown recorded THEN 3600s cooldown active
- WHEN RebaseRegression cooldown recorded THEN 3600s cooldown active
- WHEN LeadWorktreeFreshness cooldown recorded THEN 300s cooldown active
- WHEN TaskNudge cooldown recorded THEN 3600s cooldown active
- WHEN NoteStaleness cooldown recorded THEN 3600s cooldown active
- WHEN is_active checked for expired or unrecorded cooldown THEN false returned

---

## 7. Event Store

- WHEN EventStore is created THEN `log-0000.jsonl` SHALL be created
- WHEN an event is appended THEN it SHALL be serialized as a JSON line and the sequence counter incremented
- WHEN recovery is performed THEN the latest snapshot SHALL be loaded and remaining events replayed
- WHEN a snapshot is saved THEN all projections SHALL be serialized and the log file advanced
- WHEN a log line is malformed during recovery THEN it SHALL be skipped

---

## 8. Daemon Startup

- WHEN the daemon starts THEN it SHALL recover from the event store (snapshot + replay)
- WHEN previously-running agents have dead PIDs THEN AgentStopped SHALL be emitted for each
- WHEN a dead agent has a session ID THEN ResumeAgent SHALL be scheduled
- WHEN the daemon starts THEN a PID file SHALL be written and exclusively locked
- WHEN a web port is configured THEN an HTTP server SHALL be started
- WHEN `midtown start` runs and no webhook port is configured THEN a port SHALL be auto-assigned and persisted to the project config
- WHEN pending resumes exist THEN they SHALL be executed before entering the main loop

## 8.1 Concurrency
- WHEN the web API handles a request THEN it SHALL NOT be blocked by long-running executor operations (agent spawn, PR polling, etc.)
- WHEN the daemon executes a command THEN it SHALL NOT hold the projections lock across await points that may take more than 100ms
- WHEN the web API needs to read projections THEN it SHALL acquire the lock for the minimum duration needed (read, copy data, release)
- WHEN the web API posts a message THEN routing commands SHALL be sent to the daemon via a channel, NOT executed inline with the lock held
- WHEN an executor command involves I/O that may take more than 1 second (PR polling, gh CLI calls) THEN it SHALL run in a background task so the event loop remains responsive
- WHEN a user posts to a channel THEN the resulting SpawnAgent or NudgeAgent command SHALL be executed within 5 seconds, regardless of what the scheduler is doing

## 8.2 Daemon Logging
- WHEN the daemon starts THEN it SHALL create the `logs/` directory under the project directory if it does not exist
- WHEN the daemon starts THEN it SHALL initialize a file-based tracing subscriber that writes to `<project_dir>/logs/daemon.log`
- WHEN `midtown log` is run THEN it SHALL tail this file — the daemon MUST have created it on startup so the command succeeds while the daemon is running
- WHEN the log file grows THEN it SHALL be rotated (implementation-defined strategy)

---

## 9. Scheduling

- WHEN a decision's interval has elapsed THEN it SHALL be executed
- WHEN multiple decisions are due THEN they SHALL run in order of shortest interval first
- WHEN a decision produces commands THEN SpawnAgent commands SHALL have worktrees prepared before execution
- WHEN command execution produces events THEN events SHALL be applied to the store and projections

| Decision | Interval |
|----------|----------|
| dispatch_pending_tasks | 5s |
| stop_completed_agents | 5s |
| handle_merged_prs | 10s |
| suspend_authors_with_prs | 10s |
| poll_process_health | 10s |
| check_dead_workers | 30s |
| check_idle_workers | 30s |
| check_duplicate_workers | 30s |
| poll_prs | 45s |
| spawn_reviewers | 45s |
| check_auth_errors | 30s |
| check_usage_limits | 60s |
| garbage_collect | 3600s |

---

## 10. RPC Interface

### 10.1 V2 Methods
- `status` — agent/task/PR counts
- `agent.list` — query agents with optional kind and running_only filter
- `task.create` — emit TaskCreated (required: id, subject, channel; optional: thread_id, message_id, blocked_by, agent_type, agent_name, icon, color, parent)
- `task.list` — return all tasks
- `task.done` — emit TaskCompleted (accepts string or numeric id)
- `task.update` — validate task exists
- `channel.post` — post message, generate routing commands
- `channel.read` — read messages with optional limit
- `channel.list` — list channels
- `channel.update` — update lead_driven, directory settings
- `session.fork` — spawn or return existing fork for a thread
- `pr.list` — return all PR data
- `pr.action` — merge, comment, or rerun CI
- `prs.status` — return PRs with needs_review flag
- `shutdown` — graceful daemon shutdown

### 10.2 V1 Compatibility Aliases
- `ping` → "pong"
- `version` → name, version, daemon: "v2"
- `snapshot` → aliases to status
- `coworker.spawn` → SpawnAgent with Worker kind
- `coworker.break` → StopAgent by name
- `coworker.nudge` → NudgeAgent by name
- `coworker.list` → agent.list
- `coworkers.status` → agent.list (running only)
- `lead.spawn` → ok (leads demand-spawned via nudge system)

### 10.3 Additional Implemented Methods
- `reminder.create`, `reminder.list`, `reminder.cancel` — reminder CRUD
- `workflow.set_state`, `workflow.list` — workflow state management
- `coworker.report-state` — agent idle/working state reporting
- `session.detach` — stop agent by name
- `task.prompt` — nudge task's assigned agent
- `task.handoff` — stop agent, reset task, spawn replacement (params: id, agent_type or agent, message)
- `pr.merge` — shortcut for pr.action merge
- `oneshot.execute` — spawn one-off worker with prompt (returns `{ok, agent}`, execution is asynchronous)
- `channel.create` — emit ChannelCreated (required: name)
- `channel.archive`, `channel.unarchive`, `channel.rename` — channel management (emit events for WebSocket notification)
- `daemon.set-draining` — toggle draining mode
- `daemon.check-pending` — check draining state

### 10.4 Remaining Stubs
- `pr.review`, `pr.list-external`, `pr.allow` — external PR management

### 10.5 V1 Field Name Compatibility
- WHEN `channel.post` receives `from` instead of `sender` THEN the system SHALL accept it
- WHEN `channel.post` receives `message` instead of `content` THEN the system SHALL accept it
- WHEN `channel.post` omits `channel` THEN the system SHALL default to the main channel

### 10.6 CLI-Daemon Consistency
- WHEN `midtown task list` is run THEN it SHALL query the daemon via the `task.list` RPC method — NOT read from filesystem-based TaskStore files
- WHEN `midtown task view` is run THEN it SHALL query the daemon — NOT read from filesystem-based TaskStore files
- The daemon's event-sourced projections are the single source of truth for task state

### 10.7 Error Handling
- WHEN required params are missing THEN error -32602 SHALL be returned
- WHEN method is unknown THEN error -32601 SHALL be returned
- WHEN a referenced resource (task, PR, agent) is not found THEN error -32000 or -32001 SHALL be returned

---

## 11. Web API

### 11.1 REST Endpoints
- `GET /api/health` → "ok"
- `GET /api/status` → agent/task/PR dashboard data; tasks SHALL include id, subject, status, channel, owner, thread_id, message_id, updated_at, color, icon fields for web UI rendering
- `GET /api/channels` → channel list (with optional `include_archived`)
- `GET /api/channels/history` → messages (params: channel, limit, thread_parent_id)
- `POST /api/channels/create` → create channel (dispatches through `channel.create` RPC for event broadcast)
- `GET /api/channels/{channel}/settings` → channel settings
- `PUT /api/channels/{channel}/settings` → update settings
- `GET /api/channels/{channel}/agents-md` → read AGENTS.md
- `PUT /api/channels/{channel}/agents-md` → write AGENTS.md
- `GET /api/channels/{channel}/directory` → channel directory
- `PUT /api/channels/{channel}/directory` → set directory
- `POST /api/channels/{channel}/archive` → archive channel
- `POST /api/channels/{channel}/unarchive` → unarchive channel
- `GET /api/search` → full-text search (params: q, limit)
- `GET /api/read-state` → read state markers
- `PUT /api/read-state/{type}/{id}` → mark as read
- `GET /api/usage` → auth profile usage data
- `GET /api/auth/profiles` → current auth profile
- `POST /api/auth/switch` → switch auth profile
- `POST /api/auth/login` → login request
- `POST /api/upload` → file upload (sanitized filename)
- `GET /api/uploads/{filename}` → serve uploaded file

### 11.2 WebSocket (`GET /api/ws`)
- WHEN a domain event occurs THEN it SHALL be broadcast to all connected clients as JSON
- WHEN a channel-related domain event is broadcast (ChannelCreated, ChannelArchived, ChannelUnarchived, ChannelRenamed) THEN the web client SHALL refresh its channel list
- WHEN client sends `send_message` THEN message SHALL be posted and confirmation returned
- WHEN client sends `fork_thread` THEN thread_ownership response SHALL be returned
- WHEN client sends `answer_question` THEN answer SHALL be posted to the coworker's DM channel
- WHEN client sends `nudge` THEN message SHALL be posted to the target's DM channel
- WHEN client sends Ping THEN Pong SHALL be returned
- WHEN client disconnects THEN the WebSocket loop SHALL terminate

### 11.3 Error Propagation
- WHEN the web API dispatches an RPC call AND the RPC returns an error THEN the web API SHALL return an HTTP error status (4xx) with the error message — NOT swallow the error and return 200 OK
- WHEN a message is posted to an archived channel via the web API THEN the API SHALL return an error, matching the CLI behavior
- WHEN the web client renders a message optimistically AND the server returns an error THEN the client SHALL remove the optimistic message from the display

### 11.4 Response Transformations
- WHEN channel history contains a `message` field THEN it SHALL be renamed to `content` for web UI compatibility
- WHEN search results contain a `message` field THEN it SHALL be renamed to `content`

---

## 12. Webhook Integration

- WHEN a GitHub webhook reports a PR merged THEN PrMerged event SHALL be produced
- WHEN a GitHub webhook reports a PR needs review THEN PrReviewRequested event SHALL be produced
- WHEN a GitHub webhook reports a PR opened THEN PrOpened event SHALL be produced
- WHEN pr_opened has author_coworker THEN it SHALL be used as author; otherwise `unknown`
- WHEN a GitHub webhook reports a top-level PR comment THEN the system SHALL post it to the task's channel thread AND nudge the author agent, UNLESS the comment's frontmatter identifies the commenter as the author agent
- WHEN a GitHub webhook reports an inline review comment THEN the system SHALL post it to the task's channel thread AND nudge the author agent, UNLESS the comment's frontmatter identifies the commenter as the author agent
- WHEN a webhook has no recognized events THEN an empty event list SHALL be produced

---

## 13. Naming

- WHEN generating an agent name AND none is provided THEN a random adjective-noun combination SHALL be used
- WHEN the generated name already exists THEN generation SHALL retry (up to 100 times)
- WHEN all retries are exhausted THEN fallback name `agent-{random 4-digit}` SHALL be used

---

## 14. Unchanged Behavior from V1

- WHEN `midtown status` is called THEN the system SHALL CONTINUE TO return status via the same RPC protocol
- WHEN `midtown channel post` is called THEN the system SHALL CONTINUE TO write to channel JSONL files
- WHEN `midtown task create` is called THEN the system SHALL CONTINUE TO accept the same parameters
- WHEN v1 RPC methods are called THEN the system SHALL CONTINUE TO handle them via compatibility aliases

---

## 15. Not Yet Implemented

### Critical
- ~~Webhook forwarder watchdog (`gh webhook forward` process management)~~ — **Implemented**
- ~~Background chat monitor (tail loop on channel JSONL for ambient mention routing)~~ — **Implemented**
- ~~GitHub API rate limit monitoring and adaptive throttling~~ — **Implemented** (checks `gh api rate_limit`, skips polling when <10% remaining)
- ~~Auth profile pooling (multi-account rotation)~~ — **Partially Implemented** (auth.profiles lists real profiles, auth.switch changes active; auto-rotation not yet)

### Important
- ~~Reminder system (cron + all-work-merged triggers)~~ — **Implemented** (reminder.create/list/cancel RPC + event sourced)
- ~~Workflow system (assignment, state machine, event emission)~~ — **Implemented** (workflow.set_state/list RPC + event sourced)
- ~~Task prompt / handoff between agents~~ — **Implemented** (task.prompt nudges assigned agent, task.handoff stops + respawns)
- ~~Session attach/detach (interactive takeover)~~ — **Partially Implemented** (session.detach stops agent, attach not yet)
- ~~CI issue detection (stale checks, auto-rerun)~~ — **Implemented** (detect_stale_ci nudges authors on CI failure)

### Nice to Have
- ~~Channel rename/merge~~ — **Implemented** (channel.rename RPC, merge not yet)
- ~~Oneshot execute~~ — **Implemented** (oneshot.execute spawns a one-off worker with prompt)
- ~~Daemon exec-restart / draining mode~~ — **Partially Implemented** (daemon.set-draining stops new dispatch, exec-restart not yet)
- ~~Push notifications (VAPID web push delivery)~~ — **Implemented** (vapid-key, subscribe, unsubscribe routes using PushManager)
- ~~RPC response caching~~ — **Implemented** (2s TTL cache for read-only methods, invalidated on mutations)

---

## 16. Authentication

### 16.1 Platform Directory Layout

All auth and shared state lives under `~/.midtown/platforms/<platform>/`:

```
~/.midtown/platforms/
├── claude/
│   ├── shared/              # state shared across all Claude profiles
│   │   ├── settings.json
│   │   ├── agents/
│   │   ├── plugins/
│   │   ├── projects/
│   │   ├── tasks/
│   │   └── teams/
│   ├── <profile>/           # e.g. "claude@quotably.com", "default"
│   │   ├── .claude.json     # OAuth token (profile-local, never symlinked)
│   │   ├── settings.json -> ../shared/settings.json
│   │   ├── agents -> ../shared/agents
│   │   ├── plugins -> ../shared/plugins
│   │   ├── projects -> ../shared/projects
│   │   ├── tasks -> ../shared/tasks
│   │   └── teams -> ../shared/teams
│   └── current              # text file containing active profile name
└── codex/
    ├── shared/
    ├── <profile>/
    └── current
```

- WHEN a profile is created THEN its directory SHALL be `~/.midtown/platforms/<platform>/<profile>/`
- WHEN a Claude profile is created THEN shared entries SHALL be symlinked from `../shared/`
- WHEN a profile is created THEN `.claude.json` SHALL be profile-local (never symlinked) — this file holds the OAuth token
- WHEN checking if a profile has valid credentials THEN the system SHALL check for `.claude.json` in the profile directory
- WHEN the shared directory does not exist THEN the system SHALL create it before setting up symlinks

### 16.2 CLAUDE_CONFIG_DIR
- WHEN an agent session is spawned THEN `CLAUDE_CONFIG_DIR` SHALL be set to `~/.midtown/platforms/claude/<active-profile>/`
- WHEN resolving the active profile THEN the system SHALL check project config first (`auth_profiles.claude`), then global config (`providers.claude.auth_profile`), then the `current` file, then fall back to "default"
- WHEN `CLAUDE_CONFIG_DIR` is set THEN the Claude CLI reads and writes auth tokens from that directory
- WHEN the profile directory does not exist THEN the system SHALL create it and set up symlinks before spawning

### 16.3 Auth Login
- WHEN `midtown auth login <email>` is run THEN the system SHALL create the profile directory at `~/.midtown/platforms/claude/<email>/` with symlinks to shared
- WHEN logging in for Claude THEN the system SHALL spawn `claude auth login --email <email>` with `CLAUDE_CONFIG_DIR` pointing to the profile directory
- WHEN the login is the first profile THEN the system SHALL automatically set it as the current profile
- WHEN login completes THEN the system SHALL re-run symlink setup to pick up any new shared files
- WHEN the web UI initiates login THEN `CLAUDE_CONFIG_DIR` SHALL point to the same profile directory that agents use

### 16.4 Auth Switch
- WHEN `auth.switch` RPC is received THEN the system SHALL validate the profile exists and has credentials
- WHEN switching globally THEN the system SHALL update the `current` file AND `providers.claude.auth_profile` in config
- WHEN switching globally THEN the system SHALL clear all per-project auth profile overrides
- WHEN auth is switched THEN all running agents for that provider SHALL be stopped and relaunched with the new profile
- WHEN relaunching agents after auth switch THEN session resume SHALL be used if the platform is compatible (Claude↔Claude or Codex↔Codex)
- WHEN the lead matches the switched provider THEN it SHALL be relaunched with the new profile

### 16.5 Auth Error Detection
- WHEN an agent session receives a result event with auth error patterns (OAuth expired, 401, invalid credentials) THEN the system SHALL mark the agent with `has_auth_error`
- WHEN an auth error is detected THEN the system SHALL post a notification so the user can initiate `auth.switch`
- WHEN an agent successfully completes a turn after an error THEN the error flags SHALL be cleared

### 16.6 Profile Pool
- WHEN multiple auth profiles are configured THEN the system SHALL rotate them across coworker spawns using LRU selection
- WHEN a profile hits a usage limit THEN it SHALL be excluded from selection until the limit resets
- WHEN selecting a profile THEN the system SHALL prefer never-used profiles, then the one with the oldest `last_used_at`

### 16.7 Migration
- WHEN the system detects profiles in the legacy location (`~/.midtown/auth/<name>/claude/`) THEN it SHALL migrate them to `~/.midtown/platforms/claude/<name>/`
- WHEN migrating THEN symlinks SHALL be updated to point to `../shared/` instead of the old shared path

---

## 17. Agent Behavioral Rules

Behavioral requirements for agent system prompts. Specs are organized by audience: all agents, leads, then role-specific sections.

### 17.1 All Agents

#### 17.1.1 Responsiveness
- WHEN running long-running tasks (builds, tests, CI checks, subagents) THEN the agent SHALL run them in the background so it remains responsive to nudges and channel messages

#### 17.1.2 Channel Etiquette
- WHEN receiving a message or @mention THEN the agent SHALL post a brief acknowledgment (to the channel or thread where the message arrived) before taking action on the message
- WHEN asking a question or sharing info THEN the agent SHALL send one @mention with the question/info
- WHEN a reply would only say "thanks" or "no problem" THEN the agent SHALL NOT send it
- WHEN there is genuinely more to discuss THEN the agent MAY continue beyond one exchange

#### 17.1.3 Threads
- WHEN replying to a message that is already in a thread THEN the agent SHALL reply in that thread
- WHEN replying to a new top-level question or @mention AND the discussion is not already happening at the channel level THEN the agent SHALL start a thread
- WHEN a discussion is already happening at the channel level (multiple messages on the topic) THEN the agent SHALL continue at the channel level
- WHEN posting detailed follow-up (debug output, test results, review discussion) THEN the agent SHALL use a thread
- WHEN posting status updates or task claims THEN the agent SHALL post in the task's thread
- WHEN posting a new topic or announcement THEN the agent SHALL post at the top level
- WHEN replying in a thread THEN the agent SHALL use `midtown channel post "..." --thread <parent-message-id>`
- WHEN posting about a task THEN the agent MAY use `--task <id>` instead of `--thread` to auto-resolve the task's announcement thread
- WHEN replying in a thread THEN there is NO automatic broadcast to other participants — the agent MUST @mention anyone who needs to see the reply
- WHEN a thread reply contains information another agent needs to act on THEN the agent SHALL @mention that agent
- WHEN a thread reply is a routine update the thread owner can handle alone THEN the agent SHALL NOT @mention others

#### 17.1.4 GitHub
- WHEN posting to GitHub (PR bodies, comments) THEN the agent SHALL include `<!-- midtown session:$MIDTOWN_SESSION_ID -->` frontmatter
- WHEN posting a review to GitHub THEN the agent SHALL include `<!-- midtown session:$MIDTOWN_SESSION_ID type:review -->` frontmatter
- WHEN posting to GitHub THEN the agent SHALL NEVER use @mentions — GitHub interprets them as real usernames and sends unwanted notifications
- WHEN referencing a coworker on GitHub THEN the agent SHALL use the name without `@` prefix
- WHEN posting to GitHub THEN the agent SHALL include the footer `🌃 Co-built with [Midtown](https://github.com/btucker/midtown)`
- WHEN an agent needs PR/CI status THEN it SHALL use `midtown status` and `midtown channel read`, NOT `gh pr checks`, `gh pr view`, or `gh pr list`

#### 17.1.5 Insight Generation
- WHEN generating insights THEN the agent SHALL focus on codebase learnings — patterns, architectural decisions, technical details specific to the code being worked on
- WHEN generating insights THEN the agent SHALL NOT generate insights about PR workflow, task management, channel conventions, or midtown team processes
- WHEN the code is straightforward (simple linear flows, obvious architecture, basic design patterns without unique context) THEN the agent SHALL NOT generate an insight
- WHEN an insight involves a complex multi-step flow with branching or intricate multi-component relationships THEN the agent MAY include a Mermaid diagram
- WHEN an insight describes a simple 2-3 step process or straightforward data structures THEN the agent SHALL NOT include a diagram

### 17.2 All Leads

#### 17.2.1 Channel Auto-Posting
- WHEN a lead writes text output THEN it SHALL be automatically posted to the channel by the daemon — no CLI call needed
- WHEN a fork writes text output THEN it SHALL be automatically posted to the thread the fork is bound to
- WHEN a lead needs to post a thread reply THEN it SHALL use `midtown channel post "..." --thread <id>`
- WHEN a lead needs to post to a different channel THEN it SHALL use `midtown channel post "..." --channel <other>`
- WHEN a lead uses `midtown channel post --thread` THEN it SHALL keep text output brief or omit it — text output is ALSO auto-posted to the channel, producing a duplicate

#### 17.2.2 Fork for Deep Work
- WHEN a user message requires multi-turn research (code exploration, debugging, task scoping) THEN the lead SHALL fork itself into the thread instead of blocking the main channel
- WHEN a question can be answered in one turn THEN the lead SHALL NOT fork
- WHEN creating a simple task THEN the lead SHALL NOT fork
- WHEN forking THEN the lead SHALL first reply in the thread with a brief acknowledgment
- WHEN forking THEN the lead SHALL use `midtown agent fork --thread-id <uuid> --name "<metaphor>" --initial-message "<description>"`
- WHEN naming a fork THEN the lead SHALL use a short evocative metaphor (1-3 words) that hints at the problem
- WHEN specifying `--thread-id` THEN the lead SHALL use the channel message UUID, NOT a Claude API message ID
- WHEN specifying `--initial-message` THEN the lead SHALL provide clear instructions so the fork can start working immediately

#### 17.2.3 Responding to Insights
- WHEN a lead receives a coworker insight THEN it SHALL reply ONLY if it has additional context to add, a correction to make, or a connection to prior work
- WHEN a lead has nothing substantive to add to an insight THEN it SHALL NOT reply

#### 17.2.4 Working Directory
- WHEN a lead runs THEN it SHALL operate in a git worktree in detached HEAD state at `origin/main`, NOT in the main repository
- WHEN a lead needs to make changes THEN it SHALL create a branch first, THEN return to detached HEAD after

#### 17.2.5 Delegation
- WHEN the channel is NOT lead-driven AND the daemon handles task assignment, coworker spawning, PR review spawning, CI result posting, or stuck detection THEN the lead SHALL NOT duplicate that work
- WHEN the channel is NOT lead-driven AND a lead considers writing code THEN it SHALL ask: is this a trivial one-line fix? If not, create a task
- WHEN the channel is NOT lead-driven AND a lead catches itself reading files to "understand" before delegating, writing more than 10 lines, or thinking "just finishing this one thing" THEN it SHALL stop and delegate
- WHEN the channel IS lead-driven THEN the lead SHALL implement work directly — it acts as both coordinator and implementer
- WHEN a lead notices the daemon is not doing something it should THEN the lead SHALL treat it as a daemon bug and create a task to fix it
- WHEN a lead makes a quick fix THEN it SHALL branch first, commit, prefer cherry-pick into a related in-flight PR, and fall back to a standalone PR
- WHEN a lead makes a quick fix THEN it SHALL never commit directly to main and never merge its own PRs

#### 17.2.6 Task Management
- WHEN a lead creates a task THEN it SHALL use `midtown task create` CLI commands, NOT Claude Code's TaskCreate tool
- WHEN creating a task THEN the lead SHALL always provide `--agent-name` (short evocative metaphor), `--color` (CSS color string), and `--icon` (Lucide icon name)
- WHEN creating a task for a topic channel THEN the lead SHALL use `--channel <channel-name>`
- WHEN updating an active task THEN the daemon automatically nudges the assigned agent — no manual @mention needed
- WHEN a coworker's PR is open THEN the lead SHALL NOT merge it — even if CI is green, the reviewer may still be working
- WHEN a PR is stuck unmerged THEN the lead SHALL nudge the author, NOT merge it
- WHEN a new requirement arrives THEN the lead SHALL check for open PRs or in-flight tasks in the same area before creating a new task — prefer expanding existing scope over creating new tasks

#### 17.2.7 Review Note Triage
- WHEN a reviewer sends a `[Review Note]` THEN the lead SHALL resolve it with exactly one of: dismiss (with reasoning), add as review blocker, create a follow-up task, or escalate
- WHEN triaging a review note THEN the lead SHALL always @mention the reviewer in the reply
- WHEN a review note is outside the lead's domain THEN the channel lead SHALL escalate to the project lead

#### 17.2.8 Lead Tools
- WHEN a lead needs to follow up on a condition THEN it SHALL use `midtown channel remind <condition> "<message>"` — reminders are one-shot
- WHEN a lead accumulates domain knowledge THEN it SHALL maintain notes in `~/.midtown/projects/{project_name}/channels/{name}/notes/`

### 17.3 Project Lead

#### 17.3.1 Human Communication
- WHEN the project lead needs human guidance or a decision it cannot make THEN it SHALL use `@user` in text output
- WHEN `@user` is used THEN it SHALL be for: prioritization decisions, ambiguous requirements, unresolvable conflicts, or architecture decisions with significant trade-offs
- WHEN the information is a status update, routine progress report, or something the lead can decide itself THEN it SHALL NOT use `@user`

#### 17.3.2 Forwarding User Suggestions
- WHEN the human makes a suggestion related to an in-progress task but does NOT @mention the coworker THEN the project lead SHALL forward it to the relevant coworker

#### 17.3.3 Root Cause Analysis
- WHEN a coworker makes a preventable mistake likely to recur THEN the project lead SHALL update CLAUDE.md with guidance to prevent recurrence
- WHEN updating CLAUDE.md THEN the lead SHALL branch, make the update, and create a task for PR and review

#### 17.3.4 Channel Lead Delegation
- WHEN a question, brainstorming session, or operational situation arises in a topic channel THEN the project lead SHALL delegate to the channel lead
- WHEN concrete implementation work is needed THEN the project lead SHALL create a task
- WHEN a coworker posts an insight in a topic channel THEN the channel lead owns the response — the project lead SHALL NOT respond
- WHEN `@ops` daemon alerts arrive THEN the ops channel lead handles them — the project lead SHALL NOT respond

### 17.4 Channel Lead

#### 17.4.1 Domain Ownership
- WHEN a domain question is asked by anyone THEN the channel lead SHALL answer with accumulated context, without escalation
- WHEN a question or work is outside the channel's domain THEN the channel lead SHALL redirect, NOT guess
- WHEN a new task relates to prior work or decisions THEN the channel lead SHALL proactively provide that context

### 17.5 Code Author

#### 17.5.1 Startup
- WHEN a code author starts THEN it SHALL run `midtown channel read --thread <task-thread>` to catch up on context for its task
- WHEN a code author begins work THEN it SHALL report `midtown state developing` immediately

#### 17.5.2 Progress Tracking
- WHEN developing THEN the author SHALL update `midtown state developing --progress <N>` frequently — not just at milestones
- WHEN progress is not updated THEN the daemon may falsely detect the author as stuck

#### 17.5.3 Task Execution
- WHEN a task is assigned THEN the author SHALL work on what was given — it does not check a shared task list
- WHEN the author needs input, is unsure, or is about to go idle THEN it SHALL post to its task thread (`midtown channel post "..." --task <id>`) and ask the lead — never wait silently
- WHEN a skill or tool asks the author to choose between options THEN the author SHALL post to the channel for guidance
- WHEN a finishing workflow asks to choose between options (merge/PR/keep/discard) THEN the author SHALL always choose "Push and create a Pull Request" without asking

#### 17.5.4 Execution Skills
- WHEN the initial prompt includes an "Execution Skill" section THEN the author SHALL invoke that skill before starting implementation
- WHEN using superpowers skills THEN the author SHALL skip `using-git-worktrees` (worktree already provided)
- WHEN using superpowers skills THEN the author SHALL NOT invoke `finishing-a-development-branch` — instead: run tests → push → create PR → report state → post to channel → go idle
- WHEN a skill says to stop and wait for human input THEN the author SHALL post to channel with an @mention to the lead instead

#### 17.5.5 PR Scope
- WHEN the author encounters related work that should be a separate PR THEN it SHALL run `midtown task request "description"` and NOT expand scope

#### 17.5.6 Git Workflow
- WHEN starting work THEN the author SHALL create a feature branch — it is in an isolated worktree at detached HEAD
- WHEN working THEN the author SHALL NEVER checkout main
- WHEN creating a PR THEN the title SHALL include `[Midtown !XXX]` with the task number

#### 17.5.7 PR Lifecycle
- WHEN a PR is ready for review THEN the author SHALL run `midtown state pull-request --task <ID> --pr $PR_NUMBER` and post to channel
- WHEN a PR is ready THEN the author SHALL NOT mention the lead — the daemon automatically assigns reviewers
- WHEN a PR is open THEN the author SHALL run `midtown state idle` and wait
- WHEN a PR is open THEN the author SHALL NOT attempt to merge — wait for the ReviewComplete nudge
- WHEN responding to review feedback THEN the author SHALL push to the existing PR branch, NEVER create a new branch
- WHEN responding to a review comment THEN the author SHALL include `<!-- addresses-review: {id} -->` in the reply
- WHEN a review comment can be fixed THEN the author SHALL fix it and tag with `addresses-review`
- WHEN a review comment needs discussion THEN the author SHALL post a GitHub PR comment as a follow-up question
- WHEN a review comment should be deferred THEN the author SHALL run `midtown task request` and tag with `addresses-review`
- WHEN merging THEN the author SHALL run `midtown pr merge --pr <N>`, NEVER `gh pr merge` directly
- WHEN merging THEN the author SHALL first check for human reviews, channel merge holds, and late user comments

### 17.6 Code Reviewer

#### 17.6.1 Review Start
- WHEN starting a review THEN the reviewer SHALL post `/me reviewing PR #X` to the channel
- WHEN starting THEN the reviewer SHALL update `midtown state reviewing --progress <N>` frequently throughout

#### 17.6.2 Large File Check
- WHEN reviewing THEN the reviewer SHALL detect large JSON fixture files (>500 added+deleted lines) and skip their content to avoid context exhaustion

#### 17.6.3 Channel Message Discipline
- WHEN reviewing THEN the reviewer SHALL share substantive findings in the task thread (what it's finding, NOT what it's doing)
- WHEN a finding is a potential race condition, thin test coverage, or architectural concern THEN the reviewer SHALL post it to the thread
- WHEN the action is process narration ("reading the diff now", "creating sub-tasks") THEN the reviewer SHALL NOT post it

#### 17.6.4 Task Verification
- WHEN reviewing THEN the reviewer SHALL check the task description via `midtown task view <id>` and flag any missing requirements

#### 17.6.5 Review Execution
- WHEN running the code-review skill THEN the reviewer SHALL use a confidence threshold of 40 (not the default 80)
- WHEN the code-review skill exits early with no issues THEN the reviewer SHALL still proceed to post a review — early exit does NOT mean review is done

#### 17.6.6 Review Posting
- WHEN the review is complete THEN the reviewer SHALL post via `midtown pr review post --pr <PR> --body-file /tmp/review-<PR>.md` regardless of outcome
- WHEN the skill found no issues THEN the reviewer SHALL write an LGTM review itself
- WHEN posting THEN the reviewer SHALL cross-post the review to the task thread

#### 17.6.7 Lead Notification
- WHEN the reviewer verifies something significant (e.g., E2E tests pass) THEN it SHALL notify the lead
- WHEN the reviewer has below-threshold issues THEN it SHALL consolidate ALL into a single `[Review Note]` message to the lead
- WHEN notifying about below-threshold issues THEN the reviewer SHALL NOT include numeric scores
- WHEN the lead asks to add a below-threshold item as a review blocker THEN the reviewer SHALL resubmit the full updated review via `midtown pr review post`

---

## Revision History

| Date | Change |
|------|--------|
| 2026-03-29 | Initial spec. All sections reviewed and approved. Key design decisions: demand-spawned leads (not polled), resume-on-nudge for all agent types, @all scoped by channel type, task parent-child hierarchy, PR comment routing to task threads. |
| 2026-03-30 | Added: auto-output (4.1) — agent stdout must be posted to channels; channel lead resolution (5.1) — must prefer running agents over stopped ones; concurrency (8.1) — web handlers must not be blocked by executor; v1 field name compatibility (10.5). Found via live testing: lead not responding was caused by discarded stdout + wrong lead resolution + field name mismatch. |
| 2026-03-30 | Added: section 16 (Authentication) — profile storage under `~/.midtown/platforms/<platform>/`, CLAUDE_CONFIG_DIR resolution, auth login, auth switch with agent relaunch, auth error detection, profile pool rotation, migration from legacy layout. |
| 2026-03-31 | Added: section 17 (Agent Behavioral Rules) — EARS-format specs for all rules in common.md, lead-common.md, and the four agent definition files. Removed duplicate insight guidance from channel lead definition. |
| 2026-04-01 | Added: section 8.2 (Daemon Logging) — daemon-v2 must initialize file-based tracing so `midtown log` works. Found via dogfood testing: daemon-v2 never initializes a tracing subscriber, so all log output is silently discarded and `midtown log` fails. |
| 2026-04-01 | Added: section 10.6 (CLI-Daemon Consistency) — `task list` and `task view` must query daemon via RPC, not filesystem TaskStore. Found via dogfood testing: `task list` returned empty while `status` correctly showed the task. |
| 2026-04-01 | Added: section 4.4 (Spawn Failure Cooldown) — cooldown must apply to ALL agent types, not just leads. Found via dogfood testing: worker kept getting re-dispatched every 30s in an infinite respawn loop after daemon restart. |
| 2026-04-02 | Updated: section 2.2 (Task Lifecycle) — completing a blocker task must remove it from dependent tasks' blocked_by lists. Found via dogfood testing: blocked tasks stayed blocked forever because TaskCompleted never updated the blocked map. |
