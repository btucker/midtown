## Channel Monitoring

Every time you read the channel, scan the **full output** for anomalies — don't just look for the specific thing that triggered the read. Watch for:

- **Loops**: Same coworker + same task appearing repeatedly in quick succession
- **Stale tasks**: Tasks referencing already-merged PRs or completed work
- **Failed spawns**: "Called in coworker X" with no follow-up activity from that coworker
- **Repeated errors**: The same warning or error appearing multiple times

This is for catching **daemon bugs and failure modes** — not for overriding normal orchestration. When you spot a malfunction, act immediately:

1. **Break the loop** if urgent (send coworker on break, complete stale tasks)
2. **Communicate what you did** — your text will be auto-posted to the channel
3. **Follow the debugging workflow** below — capture a snapshot, create a task

Don't get tunnel-visioned on the message that triggered the read. The channel is your window into team health — read it like a dashboard, not a message queue.

## Debugging Unexpected Daemon Behavior

Act **proactively** whenever you notice misbehavior — don't wait to be asked.

1. **Capture state immediately:** `midtown e2e capture --label <bug-description>`
2. **Move snapshot to fixtures:** `mv tests/fixtures/snapshot/captured/<file> tests/fixtures/snapshot/`
3. **Create a task** for a coworker to write a failing test and fix the bug, referencing the snapshot
4. **Post to the channel** so the team is aware

The coworker's failing test should load the captured snapshot and assert expected behavior:

```rust
#[test]
fn test_bug_description() {
    let fixture = include_str!("fixtures/snapshot/snapshot-<label>-<timestamp>.json");
    let snapshot: WorldSnapshot = serde_json::from_str(fixture).unwrap();
    // Assert the expected behavior against the captured state
}
```

### Daemon Log

Check the daemon log first when debugging: `~/.midtown/projects/<repo>/logs/daemon.log`

```bash
tail -100 ~/.midtown/projects/<repo>/logs/daemon.log   # recent activity
tail -f ~/.midtown/projects/<repo>/logs/daemon.log      # follow live
```

`MIDTOWN_LOG_LEVEL=debug` for task assignments and spawns; `trace` for full pane content and serialized snapshots.

## Lead Maintenance

Whenever a PR is merged into main, pull, rebuild, and restart so the running daemon and coworkers pick up the changes:

```bash
git pull && cargo install --path . && midtown restart
```

Post to the channel when done so the team knows the new code is live.

