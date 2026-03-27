# Session Resume E2E Testing

## Problem

Session resuming (`claude --resume <session_id>`) fails too often in production. Workers and fork sessions are most affected — they exit immediately after resume (< 30s), triggering the fallback path. We have unit tests for the daemon's resume decision logic but no tests that exercise actual CLI resume behavior end-to-end.

## Goal

Add E2E tests to the "full" containerized test suite that spawn real Claude sessions, stop them, resume them, and verify the resume actually works. Capture diagnostic data to understand *why* resumes fail.

## Design

### Test file

`tests/resume_e2e.rs` — added to the "full" suite in `scripts/e2e-entrypoint.sh`.

All tests use `#[test] #[ignore]` and require real Claude CLI + OAuth auth (same as `full_stack_e2e.rs`). Tests use the `FullStackFixture` pattern for daemon lifecycle management.

### Helper infrastructure

A `ResumeTestHelper` struct wrapping `FullStackFixture` with resume-specific utilities:

- `spawn_coworker_and_wait()` — spawns a coworker via RPC, polls daemon state until `is_running=true`, returns the session_id
- `stop_session(name)` — kills the coworker process directly (SIGTERM via PID from daemon state), then polls until the daemon detects it stopped (`is_running=false`). Note: there is no `session.stop` RPC — `session.cancel` exists but it auto-resumes, which we don't want for controlled testing.
- `read_session_record(session_id)` — calls the `snapshot` RPC and extracts the `SessionRecord` for a given session_id (no need to read JSON files directly)
- `get_session_list()` — calls `session.list` RPC to get current session status
- `resume_coworker(session_id)` — spawns with `resume: true` targeting a specific session_id via `coworker.spawn` RPC
- `capture_session_stderr(name)` — captures stderr output from the session for diagnostics
- `session_age_seconds(name)` — returns how long the session ran before stopping

### Test cases

#### Test 1: `test_basic_session_resume`

The happy path — spawn, stop, resume, verify alive.

1. Start daemon
2. Spawn a headless coworker via `coworker.spawn` RPC
3. Wait for session to be `is_running=true` in daemon state
4. Record `session_id` from daemon state
5. Kill the coworker process (SIGTERM via PID from daemon state)
6. Wait for daemon to detect session stopped (`is_running=false`)
7. Resume via `coworker.spawn` with `resume: true` (daemon uses `SessionMode::ResumeSession`)
8. **Assert:** session comes back, `is_running=true`, runs > 30s without exiting
9. **Log:** CLI args, session age, stderr output

#### Test 2: `test_resume_preserves_context`

Does the resumed session retain conversation history?

1. Spawn coworker with initial prompt containing a distinctive marker: "Remember the code word: FALCON-7. Respond with just 'acknowledged'."
2. Wait for session to process the prompt (poll for output containing "acknowledged")
3. Kill the session process
4. Resume with follow-up prompt: "What was the code word I told you earlier? Reply with just the code word."
5. **Assert:** session output contains "FALCON"
6. **Log:** full session output for both phases

#### Test 3: `test_resume_fallback_on_invalid_session_id`

Validates `spawn_with_resume_fallback` works when the session ID is stale.

1. Start daemon
2. Inject a fake `SessionRecord` into daemon state with a bogus `session_id`
3. Trigger dispatch that would resume this session (or call spawn directly with the bad ID)
4. **Assert:** fallback fires — a fresh session spawns instead
5. **Assert:** fresh session is running (`is_running=true`)
6. **Assert:** the stale `session_id` was cleared from daemon state
7. **Log:** stderr from the failed resume attempt, the handoff prompt content

#### Test 4: `test_rapid_resume_after_short_session`

Reproduces the most common failure mode: session killed quickly, then resumed.

1. Spawn coworker
2. Kill it almost immediately (< 5s after spawn, before it finishes processing)
3. Attempt resume
4. **Assert one of:**
   - Resume succeeds (session alive, runs > 30s) — the happy case
   - Resume fails AND daemon correctly detects `was_failed_resume`, clears session_id, does NOT crash-loop
5. **Log:** time between original spawn and kill, time between resume and exit, stderr

This test is explicitly designed to be informational — even a "failure" that's handled correctly is a pass. The test fails only if the daemon mishandles the situation (crash loop, stale state).

#### Test 5: `test_daemon_restart_resumes_sessions`

Full daemon restart with session recovery.

1. Start daemon, spawn coworker, wait for `is_running=true`
2. Verify `resume_on_startup=true` in the session record
3. Stop daemon gracefully via `shutdown` RPC
4. Restart daemon (same fixture, same state directory)
5. **Assert:** daemon automatically resumes the session (`ResumeCoworker` effect fires)
6. **Assert:** resumed session is alive, runs > 30s
7. **Log:** recovery timing, any stderr from resumed session

### Diagnostic output

Every test captures and logs (via `eprintln!`):

| Data point | Purpose |
|---|---|
| CLI args used for resume | Verify correct flags passed |
| Session stderr | Contains error messages on failure |
| Session age (spawn to exit) | Validate 30s threshold, spot patterns |
| Daemon state snapshots (before/after) | Track state transitions |
| Whether fallback fired | Track fallback frequency |

This data is visible in container test output and CI logs, giving us forensics even on intermittent failures.

### Integration with e2e-entrypoint.sh

Add `resume_e2e` to the "full" test suite block in `scripts/e2e-entrypoint.sh`. These tests run sequentially (`--test-threads=1`) since they share daemon state. Timeout: 5 minutes per test (300s), matching `full_stack_e2e.rs`.

### What this does NOT test

- Codex provider resume (different protocol, separate concern)
- Fork session resume specifically (same CLI mechanism as workers, can add later)
- Context window exhaustion recovery (separate failure mode, needs very long sessions)

### Success criteria

- All 5 tests pass in containerized E2E ("full" suite)
- Tests produce enough diagnostic output to identify resume failure root causes
- Tests catch regressions in resume behavior (CI gate)
