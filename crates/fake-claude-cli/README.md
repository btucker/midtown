# fake-claude-cli

Deterministic Claude CLI simulator for protocol and E2E testing.

## What it simulates

- `claude plugin ...` commands used by startup hooks
- Stream JSON mode (`--output-format stream-json`, `--input-format stream-json`)
- `system/init`, `assistant`, and `result` events

## Behavior controls

- `FAKE_CLAUDE_MODE`
  - `echo` (default)
  - `tool`
  - `error`
  - `init-only`
  - `no-response`
  - `hang-turn`
- `FAKE_CLAUDE_DELAY_MS` (optional per-turn delay)
- `FAKE_CLAUDE_SESSION_ID` (default: `fake-claude-session`)
- `FAKE_CLAUDE_MODEL` (default: `fake-claude-model`)
- `FAKE_CLAUDE_RESPONSE_TEXT` (static response)
- `FAKE_CLAUDE_RESPONSE_TEMPLATE` (supports `{prompt}` substitution)

## Example

```bash
FAKE_CLAUDE_MODE=echo cargo run -p fake-claude-cli -- \
  --model sonnet --output-format stream-json --input-format stream-json
```

To use as a drop-in `claude` binary in tests, place a wrapper script named `claude`
on `PATH` that execs `fake-claude-cli`.
