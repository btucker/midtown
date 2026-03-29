# Daemon V2 — Requirements

**Status:** In progress — requirements under review
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

### 2.2 Task Lifecycle
- WHEN a worker dies while its task is InProgress THEN the system SHALL resume the worker
- WHEN a worker cannot be resumed (no session ID) THEN the system SHALL spawn a replacement worker with the same task configuration
- WHEN two agents are assigned to the same task THEN the system SHALL stop the newer one
- WHEN a worker reports its state as idle via `midtown state` THEN the system SHALL stop it after 2 minutes if it remains idle
- WHEN a worker has not reported a state change for 5 minutes THEN the system SHALL nudge it
- WHEN a running worker's task is Completed THEN the system SHALL stop the worker
- WHEN a task declares `blocked_by` dependencies THEN the system SHALL NOT dispatch it until all blockers are completed

---

## 3. PR Integration

### 3.1 Event Sources
- WHEN a GitHub webhook reports a PR opened THEN the system SHALL emit a PrOpened event
- WHEN polling detects a new open PR not already tracked THEN the system SHALL emit a PrOpened event as a backstop
- WHEN a GitHub webhook reports CI or review state change THEN the system SHALL emit a PrUpdated event
- WHEN polling detects a CI or review state change not already reflected THEN the system SHALL emit a PrUpdated event as a backstop
- WHEN a GitHub webhook reports a PR merged THEN the system SHALL emit a PrMerged event
- WHEN polling detects a merged PR not already tracked THEN the system SHALL emit a PrMerged event as a backstop
- WHEN polling fails THEN the system SHALL log the error and return no events

### 3.2 Reviewer Spawning
- WHEN a PR needs review AND no reviewer is running for it THEN the system SHALL spawn a reviewer named `reviewer-{pr_num}`
- WHEN a reviewer dies AND fewer than 3 attempts have been made THEN the system SHALL spawn a new reviewer
- WHEN a reviewer has failed 3 times THEN the system SHALL post to the ops channel AND SHALL NOT spawn another
- WHEN spawning a reviewer THEN the initial prompt SHALL be `Review PR #{pr_num}: {branch}`

### 3.3 PR Lifecycle
- WHEN a PR merges AND has a linked InProgress task THEN the system SHALL complete the task
- WHEN a PR merges THEN the system SHALL nudge workers with other open PRs to rebase (1hr cooldown per agent)
- WHEN a worker's task has an open PR awaiting review THEN the system SHALL stop the worker

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
- WHEN a worker is spawned THEN the system SHALL auto-create a DM channel `dm-{agent_name}`
- WHEN spawning succeeds THEN AgentCreated and AgentStarted events SHALL be emitted
- WHEN spawning fails THEN the error SHALL be logged and no events emitted
- WHEN a session is spawned THEN stdout/stderr SHALL be drained in a background task

### 4.2 Stopping
- WHEN StopAgent is executed THEN the session SHALL be killed and removed from the sessions map
- WHEN the kill fails THEN the error SHALL be logged but AgentStopped SHALL still be emitted
- WHEN an agent process exits (detected by try_wait) THEN AgentStopped SHALL be emitted

### 4.3 Resuming
- WHEN ResumeAgent is executed THEN the session SHALL be spawned with `resume_session_id` set
- WHEN resume succeeds THEN AgentResumed SHALL be emitted with the new PID
- WHEN an agent is resumed THEN `started_at` SHALL be reset to now
- WHEN resume fails THEN the error SHALL be logged and no events emitted

### 4.4 Garbage Collection
- WHEN an agent has been stopped for more than 24 hours AND is not a Lead THEN it SHALL be garbage-collected
- WHEN an agent is garbage-collected THEN it SHALL be removed from all indexes (by_id, by_name, by_task, by_channel, by_thread)

### 4.5 Fork Sessions
- WHEN a fork session spawns THEN it SHALL be bound to a thread via `bound_thread_id`
- WHEN a fork session stops THEN its thread binding SHALL persist (NOT cleared)
- WHEN a fork has `fork_from_session` THEN it SHALL inherit the parent session's context
- WHEN a session.fork request arrives AND a running fork exists for the thread THEN the existing fork ID SHALL be returned

---

## 5. Channel Management

### 5.1 Channel Leads
- WHEN a non-archived channel exists AND has no running lead THEN the system SHALL spawn one
- WHEN spawning a lead for the default channel THEN agent_type SHALL be `midtown-project-lead`
- WHEN spawning a lead for a topic channel THEN agent_type SHALL be `midtown-channel-lead`
- WHEN a channel has a `directory` setting THEN the lead's working_dir SHALL be set to that subdirectory

### 5.2 Channel Settings
- WHEN `lead_driven` is set to true THEN automatic task dispatch SHALL be skipped for that channel
- WHEN `directory` is set THEN the lead SHALL load AGENTS.md/CLAUDE.md from that subdirectory
- WHEN a channel is archived THEN its directory SHALL be renamed with `.archived` suffix
- WHEN a channel is unarchived THEN the `.archived` suffix SHALL be removed

### 5.3 Channel I/O
- WHEN a message is posted THEN it SHALL be written to the channel's JSONL file
- WHEN messages are read with a limit THEN the last N messages SHALL be returned
- WHEN a system message is posted THEN sender SHALL be `midtown`

---

## 6. Projections

### 6.1 AgentIndex
- WHEN AgentCreated is applied THEN the agent SHALL be indexed by id, name, task, channel, and thread
- WHEN AgentStarted is applied THEN pid and session_id SHALL be set, agent added to running set, started_at set to now
- WHEN AgentStopped is applied THEN agent removed from running set, stopped_at set, thread binding preserved
- WHEN AgentResumed is applied THEN pid updated, started_at reset, stopped_at cleared, added back to running set
- WHEN AgentGarbageCollected is applied THEN agent removed from all indexes

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
- WHEN pending resumes exist THEN they SHALL be executed before entering the main loop

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
| ensure_channel_leads_alive | 30s |
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
- `task.create` — emit TaskCreated (required: id, subject, channel)
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
- `lead.spawn` → ok (leads auto-spawned by scheduler)

### 10.3 Stubbed Methods
- `reminder.create`, `reminder.list`, `reminder.cancel`
- `workflow.set_state`, `workflow.list`
- `coworker.report-state`
- `session.detach`
- `task.prompt`, `task.handoff`
- `pr.review`, `pr.merge`, `pr.list-external`, `pr.allow`
- `daemon.check-pending`

### 10.4 Error Handling
- WHEN required params are missing THEN error -32602 SHALL be returned
- WHEN method is unknown THEN error -32601 SHALL be returned
- WHEN a referenced resource (task, PR, agent) is not found THEN error -32000 or -32001 SHALL be returned

---

## 11. Web API

### 11.1 REST Endpoints
- `GET /api/health` → "ok"
- `GET /api/status` → agent/task/PR dashboard data
- `GET /api/channels` → channel list (with optional `include_archived`)
- `GET /api/channels/history` → messages (params: channel, limit, thread_parent_id)
- `POST /api/channels/create` → create channel
- `GET /api/channels/{channel}/settings` → channel settings
- `PUT /api/channels/{channel}/settings` → update settings
- `GET /api/channels/{channel}/agents-md` → read AGENTS.md
- `PUT /api/channels/{channel}/agents-md` → write AGENTS.md
- `GET /api/channels/{channel}/directory` → channel directory
- `PUT /api/channels/{channel}/directory` → set directory
- `POST /api/channels/{channel}/archive` → archive channel
- `POST /api/channels/{channel}/unarchive` → unarchive channel
- `GET /api/search` → full-text search (params: q, limit)
- `GET /api/read-state` → stub (empty object)
- `PUT /api/read-state/{type}/{id}` → stub (204)
- `GET /api/usage` → auth profile usage data
- `GET /api/questions` → stub (empty array)
- `GET /api/auth/profiles` → current auth profile
- `POST /api/auth/switch` → switch auth profile
- `POST /api/auth/login` → login request
- `GET /api/directories` → stub (empty array)
- `GET /api/push/vapid-key` → VAPID public key
- `POST /api/push/subscribe` → register push subscription
- `POST /api/push/unsubscribe` → remove push subscription
- `POST /api/upload` → file upload (sanitized filename)
- `GET /api/uploads/{filename}` → serve uploaded file

### 11.2 WebSocket (`GET /api/ws`)
- WHEN a domain event occurs THEN it SHALL be broadcast to all connected clients as JSON
- WHEN client sends `send_message` THEN message SHALL be posted and confirmation returned
- WHEN client sends `fork_thread` THEN thread_ownership response SHALL be returned
- WHEN client sends `answer_question` THEN answer SHALL be posted to the coworker's DM channel
- WHEN client sends `nudge` THEN message SHALL be posted to the target's DM channel
- WHEN client sends Ping THEN Pong SHALL be returned
- WHEN client disconnects THEN the WebSocket loop SHALL terminate

### 11.3 Response Transformations
- WHEN channel history contains a `message` field THEN it SHALL be renamed to `content` for web UI compatibility
- WHEN search results contain a `message` field THEN it SHALL be renamed to `content`

---

## 12. Webhook Integration

- WHEN a GitHub webhook reports a PR merged THEN PrMerged event SHALL be produced
- WHEN a GitHub webhook reports a PR needs review THEN PrReviewRequested event SHALL be produced
- WHEN a GitHub webhook reports a PR opened THEN PrOpened event SHALL be produced
- WHEN pr_opened has author_coworker THEN it SHALL be used as author; otherwise `unknown`
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
- Webhook forwarder watchdog (`gh webhook forward` process management)
- Background chat monitor (tail loop on channel JSONL for ambient mention routing)
- GitHub API rate limit monitoring and adaptive throttling
- Auth profile pooling (multi-account rotation)

### Important
- Reminder system (cron + all-work-merged triggers)
- Workflow system (assignment, state machine, event emission)
- Task prompt / handoff between agents
- Session attach/detach (interactive takeover)
- CI issue detection (stale checks, auto-rerun)

### Nice to Have
- Channel rename/merge
- Oneshot execute
- Daemon exec-restart / draining mode
- Push notifications (VAPID web push delivery)
- RPC response caching

---

## Revision History

| Date | Change |
|------|--------|
| 2026-03-29 | Initial spec. Requirements under review — sections 1-3.1 approved. |
