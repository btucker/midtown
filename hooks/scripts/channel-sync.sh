#!/bin/bash
# Midtown Channel Sync Hook
# Runs on Stop events to sync channel messages and check for unclaimed tasks

set -e

# Read channel messages (advances cursor)
CHANNEL_OUTPUT=$(midtown channel read --format json 2>/dev/null || echo '{"messages":[]}')

# Check for new messages
MESSAGE_COUNT=$(echo "$CHANNEL_OUTPUT" | jq -r '.messages | length' 2>/dev/null || echo "0")

if [ "$MESSAGE_COUNT" -gt 0 ]; then
    echo "📨 $MESSAGE_COUNT new channel message(s)"
    echo "$CHANNEL_OUTPUT" | jq -r '.messages[] | "[\(.timestamp)] \(.author): \(.content)"' 2>/dev/null || true
fi

# Check for unclaimed tasks
TASK_OUTPUT=$(midtown task list --format json 2>/dev/null || echo '{"tasks":[]}')
UNCLAIMED=$(echo "$TASK_OUTPUT" | jq -r '[.tasks[] | select(.owner == null or .owner == "")] | length' 2>/dev/null || echo "0")

if [ "$UNCLAIMED" -gt 0 ]; then
    echo "📋 $UNCLAIMED unclaimed task(s) available"
fi

exit 0
