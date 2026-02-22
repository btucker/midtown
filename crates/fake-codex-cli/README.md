# fake-codex-cli

Deterministic Codex CLI simulator for protocol and E2E testing.

## What it simulates

- `codex app-server` JSON-RPC over stdio
- `initialize`, `thread/start|resume|fork`, and `turn/start` request handling
- Notifications used by Midtown translation logic:
  - `thread/started`
  - `item/agentMessage/delta`
  - `item/started` / `item/completed` for command execution
  - `turn/completed`

## Behavior controls

- `FAKE_CODEX_MODE`
  - `echo` (default)
  - `tool`
  - `error`
  - `no-response`
  - `hang-start`
  - `hang-turn`
- `FAKE_CODEX_DELAY_MS` (optional per-request delay)
- `FAKE_CODEX_THREAD_ID` (default: `fake-codex-thread`)
- `FAKE_CODEX_RESPONSE_TEXT` (static response)
- `FAKE_CODEX_RESPONSE_TEMPLATE` (supports `{prompt}` substitution)

## Example

```bash
FAKE_CODEX_MODE=echo cargo run -p fake-codex-cli -- app-server
```

To use as a drop-in `codex` binary in tests, place a wrapper script named `codex`
on `PATH` that execs `fake-codex-cli`.
