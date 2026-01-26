---
name: status
description: Show midtown system status including daemon, coworkers, channel, and tasks
---

# Midtown Status Command

Display the current state of the midtown coordination system.

## What to do

Run the midtown status command and present a summary to the user:

```bash
midtown status
```

## Output sections

The status command shows:

1. **Daemon** - Whether the midtown daemon is running
2. **Coworkers** - List of active coworker agents with their status
3. **Channel** - Recent channel messages and unread count
4. **Tasks** - Summary of task status (open, claimed, completed)
5. **PRs** - Open pull requests and their CI/review status

Present this information in a clear, organized format for the user.
