#!/usr/bin/env bash
#
# Tests for capture-insights.sh hook
#

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK_SCRIPT="$SCRIPT_DIR/capture-insights.sh"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

TESTS_PASSED=0
TESTS_FAILED=0

# Create temp directory for test files
TEST_DIR=$(mktemp -d)
trap "rm -rf $TEST_DIR" EXIT

# Mock midtown binary that captures posted messages
MOCK_MIDTOWN="$TEST_DIR/midtown"
POSTED_MESSAGES="$TEST_DIR/posted_messages.txt"

# Create mock midtown script (note: no single quotes to allow variable expansion)
cat > "$MOCK_MIDTOWN" << MOCK
#!/usr/bin/env bash
# Mock midtown - captures channel post messages
if [[ "\$1" == "channel" && "\$2" == "post" ]]; then
    echo "\$3" >> "$POSTED_MESSAGES"
    exit 0
fi
exit 1
MOCK
chmod +x "$MOCK_MIDTOWN"

run_test() {
    local test_name="$1"
    local transcript_content="$2"
    local expected_pattern="$3"
    local hook_input="$4"

    echo -n "Testing: $test_name... "

    # Create transcript file
    local transcript_file="$TEST_DIR/transcript.jsonl"
    echo -e "$transcript_content" > "$transcript_file"

    # Clear posted messages
    > "$POSTED_MESSAGES"

    # Run hook with mock midtown
    local full_input
    full_input=$(echo "$hook_input" | jq --arg tp "$transcript_file" '.transcript_path = $tp')

    # Export variables so the hook can access them
    export MIDTOWN_BIN="$MOCK_MIDTOWN"
    export POSTED_MESSAGES

    echo "$full_input" | "$HOOK_SCRIPT" 2>/dev/null || true

    # Check result
    if [[ -n "$expected_pattern" ]]; then
        if grep -q "$expected_pattern" "$POSTED_MESSAGES" 2>/dev/null; then
            echo -e "${GREEN}PASSED${NC}"
            TESTS_PASSED=$((TESTS_PASSED + 1))
        else
            echo -e "${RED}FAILED${NC}"
            echo "  Expected pattern: $expected_pattern"
            echo "  Got: $(cat "$POSTED_MESSAGES" 2>/dev/null || echo '(nothing)')"
            TESTS_FAILED=$((TESTS_FAILED + 1))
        fi
    else
        # Expected no output
        if [[ ! -s "$POSTED_MESSAGES" ]]; then
            echo -e "${GREEN}PASSED${NC}"
            TESTS_PASSED=$((TESTS_PASSED + 1))
        else
            echo -e "${RED}FAILED${NC}"
            echo "  Expected no output, got: $(cat "$POSTED_MESSAGES")"
            TESTS_FAILED=$((TESTS_FAILED + 1))
        fi
    fi
}

echo "=== capture-insights.sh tests ==="
echo ""

# Test 1: Basic insight extraction
# Note: Content must be valid JSON with properly escaped newlines
run_test "Basic insight extraction" \
    '{"type":"assistant","message":{"content":"Here is some text.\\n\\n★ Insight ─────────────────────────────────────\\nThis is a test insight with important info.\\n─────────────────────────────────────────────────\\n\\nMore text."}}' \
    "This is a test insight" \
    '{"cwd":"/tmp/test","transcript_path":""}'

# Test 2: No insights - should not post
run_test "No insights present" \
    '{"type":"assistant","message":{"content":"Just regular text without any insights."}}' \
    "" \
    '{"cwd":"/tmp/test","transcript_path":""}'

# Test 3: Content as array format
run_test "Content as array format" \
    '{"type":"assistant","message":{"content":[{"type":"text","text":"★ Insight ─────────────────────────────────────\\nArray format insight.\\n─────────────────────────────────────────────────"}]}}' \
    "Array format insight" \
    '{"cwd":"/tmp/test","transcript_path":""}'

# Test 4: Coworker attribution from cwd
run_test "Coworker attribution from cwd" \
    '{"type":"assistant","message":{"content":"★ Insight ─────────────────────────────────────\\nTest insight.\\n─────────────────────────────────────────────────"}}' \
    '\[myproject\]:' \
    '{"cwd":"/path/to/myproject","transcript_path":""}'

# Test 5: Multi-line insight
run_test "Multi-line insight content" \
    '{"type":"assistant","message":{"content":"★ Insight ─────────────────────────────────────\\nFirst line of insight.\\nSecond line continues.\\n─────────────────────────────────────────────────"}}' \
    "First line" \
    '{"cwd":"/tmp/test","transcript_path":""}'

# Test 6: User messages ignored
run_test "User messages ignored" \
    '{"type":"user","message":{"content":"★ Insight ─────────────────────────────────────\\nUser insight should be ignored.\\n─────────────────────────────────────────────────"}}' \
    "" \
    '{"cwd":"/tmp/test","transcript_path":""}'

# Test 7: Missing transcript path
run_test "Missing transcript path" \
    '{"type":"assistant","message":{"content":"text"}}' \
    "" \
    '{"cwd":"/tmp/test"}'

echo ""
echo "=== Results ==="
echo -e "Passed: ${GREEN}$TESTS_PASSED${NC}"
echo -e "Failed: ${RED}$TESTS_FAILED${NC}"

if [[ $TESTS_FAILED -gt 0 ]]; then
    exit 1
fi
