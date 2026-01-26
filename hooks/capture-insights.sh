#!/usr/bin/env bash
#
# Claude Code Stop Hook: Capture and share insight blocks to midtown channel
#
# This hook monitors Claude's output for insight blocks (★ Insight) and
# posts them to the shared midtown channel so coworkers can learn from
# each other's discoveries.
#
# Installation:
#   Add to ~/.claude/settings.json or .claude/settings.json:
#   {
#     "hooks": {
#       "Stop": [{
#         "hooks": [{
#           "type": "command",
#           "command": "/path/to/midtown/hooks/capture-insights.sh"
#         }]
#       }]
#     }
#   }
#
# Environment variables:
#   MIDTOWN_COWORKER - Coworker name for attribution (defaults to directory name)
#   MIDTOWN_BIN      - Path to midtown binary (defaults to 'midtown' in PATH)
#

set -euo pipefail

# Read hook input from stdin
INPUT=$(cat)

# Extract transcript path from JSON input
TRANSCRIPT_PATH=$(echo "$INPUT" | jq -r '.transcript_path // empty')

if [[ -z "$TRANSCRIPT_PATH" || ! -f "$TRANSCRIPT_PATH" ]]; then
    # No transcript available, nothing to do
    exit 0
fi

# Get coworker name for attribution
# Priority: MIDTOWN_COWORKER env var > basename of cwd
CWD=$(echo "$INPUT" | jq -r '.cwd // empty')
COWORKER="${MIDTOWN_COWORKER:-$(basename "${CWD:-unknown}")}"

# Path to midtown binary
MIDTOWN="${MIDTOWN_BIN:-midtown}"

# Check if midtown is available (use full path if MIDTOWN_BIN set, otherwise check PATH)
if [[ "$MIDTOWN" != /* ]] && ! command -v "$MIDTOWN" &>/dev/null; then
    # Midtown not available, skip silently
    exit 0
fi

# Pattern for insight blocks:
# ★ Insight ─────────────────────────────────────
# [content - may be multiple lines]
# ─────────────────────────────────────────────────
#
# We look for assistant messages containing this pattern

# Extract all assistant message content from transcript
# The transcript is JSONL - each line is a JSON object
# Assistant messages have type "assistant" and content with the text

# Process the transcript to find insight blocks
extract_insights() {
    local content="$1"

    # Use perl for multiline regex matching with proper UTF-8 support
    # Match content between the insight header and footer lines
    # The ─ character (U+2500) is a box-drawing character used in the delimiter
    echo "$content" | perl -0777 -CSD -ne '
        use utf8;
        while (/★ Insight ─+\n(.*?)\n─+/sg) {
            my $insight = $1;
            $insight =~ s/^\s+|\s+$//g;  # Trim whitespace
            print "$insight\n---INSIGHT_SEPARATOR---\n" if $insight;
        }
    '
}

# Read transcript and extract assistant content
ASSISTANT_CONTENT=""
while IFS= read -r line; do
    # Skip empty lines
    [[ -z "$line" ]] && continue

    # Parse JSON line
    MSG_TYPE=$(echo "$line" | jq -r '.type // empty' 2>/dev/null || true)

    if [[ "$MSG_TYPE" == "assistant" ]]; then
        # Extract content - handle both string and array formats
        MSG_CONTENT=$(echo "$line" | jq -r '
            if .message.content | type == "string" then
                .message.content
            elif .message.content | type == "array" then
                [.message.content[] | select(.type == "text") | .text] | join("\n")
            else
                empty
            end
        ' 2>/dev/null || true)

        if [[ -n "$MSG_CONTENT" ]]; then
            ASSISTANT_CONTENT+="$MSG_CONTENT"$'\n'
        fi
    fi
done < "$TRANSCRIPT_PATH"

# Extract insights from the combined content
if [[ -n "$ASSISTANT_CONTENT" ]]; then
    INSIGHTS=$(extract_insights "$ASSISTANT_CONTENT")

    # Post each insight to the channel
    while IFS= read -r insight; do
        # Skip separators and empty lines
        [[ "$insight" == "---INSIGHT_SEPARATOR---" ]] && continue
        [[ -z "$insight" ]] && continue

        # Format: [coworker]: ★ [insight content]
        MESSAGE="[$COWORKER]: ★ $insight"

        # Post to channel (suppress output, ignore errors)
        "$MIDTOWN" channel post "$MESSAGE" &>/dev/null || true
    done <<< "$INSIGHTS"
fi

exit 0
