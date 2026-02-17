# Investigation: Task !1395 - Webhook State Interaction Issues

**Date:** 2026-02-17
**Investigator:** park
**Status:** Root cause identified

## Summary

The disabled test `_test_full_pr_lifecycle_webhook_effects_disabled()` in `tests/webhook_effect_pipeline_e2e.rs` was failing because the test's RPC calls to `channel.read` were returning only a single message despite multiple messages being present in the channel file.

**Key Finding:** This is NOT a webhook processing issue. Webhooks are processed correctly by the daemon. The bug is in the `channel.read` RPC handler or underlying `Channel::read_all()` implementation.

## Investigation Timeline

### 1. Initial Hypothesis: Webhook Deduplication
- Suspected that rapid successive webhooks for the same PR were being deduplicated
- Found `record_webhook_event()` in `src/github_state.rs` that tracks webhook timestamps per PR
- This was a red herring - the dedup mechanism is for polling, not webhook-to-webhook interaction

### 2. Test Reproduction
- Enabled the disabled test by removing `#[allow(dead_code)]` and adding `#[test]` `#[ignore]` attributes
- Test sequence:
  1. Send PR #100 opened webhook
  2. Verify message appears (✅ passes)
  3. Send PR #101 merged webhook
  4. Verify merge messages appear (❌ fails)

### 3. Daemon Log Analysis
Found that BOTH webhooks were processed successfully:
```
2026-02-17T05:45:18.776186Z  INFO Webhook: PR #100 queued for review spawn in 45s
2026-02-17T05:45:19.290643Z  INFO Nudged lead about PR #101 merge
```

### 4. Channel File Verification
Direct inspection of `channels/midtown.jsonl` confirmed all 5 messages were written:
1. "@columbus opened PR #100"
2. "Called in columbus in response to @mention"
3. "@columbus merged PR #101"
4. "PR #101 merged into main."
5. "✅ Auto-completed task !51"

### 5. RPC Response Analysis
Added debug logging to test fixture's `rpc_call()` method. Found that ALL calls to `channel.read` with `{"all": true}` returned only the FIRST message:

```json
{
  "id": 1,
  "jsonrpc": "2.0",
  "result": {
    "messages": [
      {
        "from": "github",
        "message": "@columbus opened PR #100: feat: Implement feature [Midtown !50]",
        "timestamp": "2026-02-17T05:49:04.166455+00:00"
      }
    ]
  }
}
```

## Root Cause

**Bug Location:** `src/daemon/rpc_channel.rs::handle_channel_read()` or `src/channel.rs::Channel::read_all()`

**Symptom:** When `channel.read` RPC is called (regardless of parameters), it returns only the first message in the channel file, even though the file contains multiple messages.

**Evidence:**
- Test sent 3 separate RPC requests with `{"all": true}`
- All 3 requests returned identical responses with only 1 message
- Channel file (`channels/midtown.jsonl`) confirmed to have 5 messages
- Daemon logs show the webhook events were processed and messages were posted

## Next Steps

1. **Investigate channel read implementation:**
   - Check if there's a cursor/cache issue in `Channel::read_all()`
   - Check if `ChannelRouter` has state that limits message retrieval
   - Check for file locking issues that might truncate reads

2. **Fix the bug:**
   - Once root cause in channel read logic is identified, implement fix
   - Ensure fix handles concurrent writes during reads (the lock retry logic at line 430-448 in `channel.rs` suggests this is a concern)

3. **Re-enable the test:**
   - Once channel read is fixed, the test should pass
   - Test provides good coverage of multi-webhook PR lifecycle

4. **Regression prevention:**
   - Add unit tests for `Channel::read_all()` with multiple messages
   - Add test for `handle_channel_read()` RPC with `all=true` parameter

## Files Modified

- `tests/webhook_effect_pipeline_e2e.rs`:
  - Enabled `test_full_pr_lifecycle_webhook_effects()`
  - Fixed test bug: changed `{"limit": 100}` to `{"all": true}` (limit param doesn't exist)
  - Added debug logging to `rpc_call()` and `read_channel_messages()`
  - Temporarily disabled cleanup in `Drop` impl for debugging

## Related Code Paths

### Webhook Processing (WORKING CORRECTLY)
- `src/webhook.rs::handle_pull_request()` - Translates GitHub webhook to `WebhookEvent`
- `src/daemon/mod.rs:2452` - Main event loop receives webhook from mpsc channel
- `src/daemon/mod.rs:2560-2590` - Processes PR merged webhook, posts messages, nudges lead
- All of this is working correctly - messages ARE being written to channel file

### Channel Read (BUG HERE)
- `src/daemon/rpc_channel.rs::handle_channel_read()` - RPC handler
- `src/channel.rs::Channel::read_all()` - Reads messages from JSONL file
- Bug causes only first message to be returned despite multiple messages in file

## Conclusion

The "webhook state interaction" issue described in the task is actually a **channel read bug** that was exposed by a test attempting to verify webhook effects. The webhooks themselves are processing correctly. The daemon receives both webhooks, processes them, posts messages to the channel file, and executes all effects (nudges, task completion, etc.).

The test fails because it cannot observe the second webhook's effects via `channel.read` RPC, not because those effects didn't occur.
