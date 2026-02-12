//! Pane content pattern detection for usage limits, API errors, and UI chrome.
//!
//! These functions analyze Claude Code pane output to detect transient states
//! (usage limits, API errors) and filter UI chrome from significant content.

/// Patterns that indicate a coworker has hit a usage/rate limit (case-insensitive).
///
/// When Claude Code hits a usage limit, it displays a message with "/upgrade"
/// or "/extra-usage" as an action option. We look for contextual patterns to
/// avoid false positives when coworkers edit code containing these in strings:
/// - "- /upgrade" (menu option format in the usage limit screen)
/// - "/upgrade to" (instruction format: "/upgrade to increase your limit")
/// - "/upgrade or" (options format: "/upgrade or wait")
/// - "/extra-usage" (Claude Code v2.1.33+: "/extra-usage to finish what you're working on")
///
/// Previous patterns like "usage limit" caused false positives when coworkers
/// were editing code with those strings in comments.
const USAGE_LIMIT_PATTERNS: &[&str] = &["- /upgrade", "/upgrade to", "/upgrade or", "/extra-usage"];

/// Patterns that indicate a Claude API error in pane content.
///
/// API errors are transient failures (500s, network issues, etc.) that may resolve
/// on retry. Unlike usage limits which have a known reset time, API errors should
/// trigger periodic nudges to encourage retry.
///
/// Patterns detected:
/// - `API Error: 500` - HTTP 500 status code
/// - `"type":"api_error"` - JSON response type field
/// - `"type":"error"` with `api_error` - Structured error response
/// - `Internal server error` - Common error message
#[allow(dead_code)]
const API_ERROR_PATTERNS: &[&str] = &[
    "API Error: 500",
    "API Error: 502",
    "API Error: 503",
    "API Error: 529",
    r#""type":"api_error""#,
    r#""type":"overloaded_error""#,
    "Internal server error",
];

/// Check if pane content has an active (not recovered) match for any pattern.
///
/// Finds the last occurrence of any pattern (case-insensitive) and counts
/// significant lines after it. Returns true if the pattern is present and
/// there are ≤ 5 significant lines after it (i.e., the coworker hasn't
/// recovered).
fn is_at_pattern(content: &str, patterns: &[&str]) -> bool {
    let content_lower = content.to_lowercase();

    // Find the last occurrence of any pattern (case-insensitive)
    let Some((match_pos, pattern_len)) = patterns
        .iter()
        .filter_map(|pattern| {
            content_lower
                .rfind(&pattern.to_lowercase())
                .map(|pos| (pos, pattern.len()))
        })
        .max_by_key(|(pos, _)| *pos)
    else {
        return false;
    };

    // Count significant lines after the match
    let after_match = &content[match_pos + pattern_len..];
    let significant_lines = after_match
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !is_ui_chrome(trimmed)
        })
        .count();

    // If there are more than 5 significant lines, the coworker has recovered
    significant_lines <= 5
}

/// Returns `true` if `c` is a UI chrome character (box-drawing, bullets, prompts, rules).
fn is_ui_chrome_char(c: char) -> bool {
    matches!(
        c,
        // Horizontal rules
        '─' | '━' | '=' | '-'
        // Box-drawing
        | '│' | '┌' | '├' | '└' | '┐' | '┤' | '┘' | '┬' | '┴' | '┼'
        | '╭' | '╮' | '╯' | '╰'
        // Bullet / task indicators
        | '◼' | '◻' | '✔' | '●' | '○' | '■' | '□' | '▪' | '▫'
        // Cursor prompts
        | '❯' | '>' | '$' | '%'
        // Whitespace (counted toward chrome ratio)
        | ' '
    )
}

/// Check if a line is UI chrome (visual elements, not meaningful content).
///
/// Matches horizontal rules, box-drawing lines, Claude Code task list items
/// (◼/◻/✔), cogitation indicators (✻/⏵), and UI key hints (ctrl+… to …).
/// Lines where ≥80% of non-whitespace chars are chrome characters also match.
fn is_ui_chrome(line: &str) -> bool {
    // Lines that are entirely horizontal rules / chrome chars
    if line.chars().all(is_ui_chrome_char) {
        return true;
    }

    // Claude Code task list lines or cogitation/status indicators
    let first_non_ws = line.trim_start();
    if first_non_ws.starts_with(['◼', '◻', '✔', '✻', '⏵']) {
        return true;
    }

    // Lines containing Claude Code UI key hints
    if first_non_ws.contains("ctrl+") && first_non_ws.contains(" to ") {
        return true;
    }

    // If ≥80% of non-whitespace chars are chrome, consider it chrome
    let non_ws_count = line.chars().filter(|c| !c.is_whitespace()).count();
    non_ws_count > 0
        && line.chars().filter(|c| is_ui_chrome_char(*c)).count() * 100 / non_ws_count >= 80
}

/// Check if pane content indicates an active (not recovered) usage limit.
///
/// Returns true only if the usage limit pattern is present AND the coworker
/// hasn't recovered (no significant activity after the limit message).
///
/// Used in `decide_usage_limit_detection` and snapshot collection.
/// Public (not `pub(crate)`) because integration tests in `dispatch_e2e.rs` call
/// this to verify usage limit detection against captured snapshot pane contents.
pub fn has_usage_limit_pattern(pane_content: &str) -> bool {
    is_at_pattern(pane_content, USAGE_LIMIT_PATTERNS)
}

/// Check if pane content indicates an API error (transient failure).
///
/// Returns true if an API error pattern is present AND the coworker hasn't
/// recovered (no significant activity after the error message).
///
/// API errors differ from usage limits:
/// - Usage limits have a known reset time; API errors are transient
/// - Usage limit nudges happen once at reset; API error nudges are periodic
/// - Both should skip stuck detection and idle shutdown
#[allow(dead_code)]
pub(crate) fn has_api_error_pattern(pane_content: &str) -> bool {
    is_at_pattern(pane_content, API_ERROR_PATTERNS)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Usage limit detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn usage_limit_code_content_should_not_trigger_detection() {
        let code_content = r#"
            // Health checks: idle shutdown, stuck detection, usage limits.
            fn check_health() {
                // Handle rate limit errors gracefully
                if self.rate_limit_exceeded {
                    return Err("rate limit hit");
                }
            }
        "#;

        assert!(
            !has_usage_limit_pattern(code_content),
            "code containing 'usage limits' in comments should NOT trigger detection"
        );
    }

    #[test]
    fn usage_limit_actual_screen_should_trigger_detection() {
        let actual_usage_limit_screen = r#"
            You've reached your usage limit for Claude Opus 4.5.

            Your limit will reset in 2 hours 30 minutes.

            Options:
            - /upgrade to increase your limit
            - /compact to reduce context
            - Wait for the limit to reset
        "#;

        assert!(
            has_usage_limit_pattern(actual_usage_limit_screen),
            "actual usage limit screen with '/upgrade' should trigger detection"
        );
    }

    #[test]
    fn usage_limit_recovery_detected_after_activity() {
        let recovered_pane = r#"
You've reached your usage limit. /upgrade to increase.
Your limit will reset in 2 hours.

> User response resumed

⏺ I'll continue with the task.

Let me read the file first.

⏺ Read(file_path: "/src/main.rs")

Now I'll implement the fix.

⏺ Edit(file_path: "/src/main.rs")
"#;

        assert!(
            !has_usage_limit_pattern(recovered_pane),
            "coworker with significant activity after usage limit should NOT be detected as limited"
        );
    }

    #[test]
    fn usage_limit_still_stuck_at_limit() {
        let stuck_at_limit = r#"
You've reached your usage limit for Claude Opus 4.5.

Your limit will reset in 2 hours.

Options:
- /upgrade to increase your limit
- /compact to reduce context
"#;

        assert!(
            has_usage_limit_pattern(stuck_at_limit),
            "coworker still at usage limit screen should be detected as limited"
        );
    }

    #[test]
    fn usage_limit_minimal_activity_still_limited() {
        let minimal_after = r#"
- /upgrade to increase your limit

(waiting for limit to reset)
"#;

        assert!(
            has_usage_limit_pattern(minimal_after),
            "minimal activity after limit should still be considered limited"
        );
    }

    #[test]
    fn usage_limit_case_insensitive() {
        let uppercase = "Your limit reached. - /UPGRADE to increase your limit.";
        let mixed_case = "Your limit reached. - /Upgrade to increase your limit.";

        assert!(
            has_usage_limit_pattern(uppercase),
            "uppercase '/UPGRADE' should trigger detection"
        );
        assert!(
            has_usage_limit_pattern(mixed_case),
            "mixed case '/Upgrade' should trigger detection"
        );
    }

    #[test]
    fn usage_limit_code_with_upgrade_should_not_trigger() {
        let code_with_upgrade = r#"
            // Test fixture for usage limit detection
            const PATTERN: &str = "/upgrade";

            fn test_usage_limit() {
                let pane = "some content with /upgrade in it";
                assert!(has_pattern(pane));
            }
        "#;

        assert!(
            !has_usage_limit_pattern(code_with_upgrade),
            "code containing '/upgrade' without context should NOT trigger detection"
        );
    }

    #[test]
    fn usage_limit_ui_chrome_should_not_count_as_activity() {
        let limit_with_pure_chrome = r#"
You've reached your usage limit for Claude Opus 4.5.

- /upgrade to increase your limit

───────────────────────────
━━━━━━━━━━━━━━━━━━━━━━━━━━━
========================
❯
❯
"#;

        assert!(
            has_usage_limit_pattern(limit_with_pure_chrome),
            "pure UI chrome after usage limit should not count as recovery activity"
        );
    }

    #[test]
    fn usage_limit_real_activity_means_recovered() {
        let recovered_with_real_output = r#"
You've reached your usage limit for Claude Opus 4.5.

- /upgrade to increase your limit

OK I'll continue working.
Let me read the file.
⏺ Read(file_path: "/src/main.rs")
Got it, here are the contents.
Now implementing the fix.
⏺ Edit(file_path: "/src/main.rs")
"#;

        assert!(
            !has_usage_limit_pattern(recovered_with_real_output),
            "real activity after usage limit should indicate recovery"
        );
    }

    #[test]
    fn usage_limit_extra_usage_with_claude_code_ui() {
        let pane = r#"
  ⎿  You've hit your limit · resets 11pm (America/Chicago)
     /extra-usage to finish what you're working on.

✻ Worked for 1m 49s

  6 tasks (3 done, 1 in progress, 2 open) · ctrl+t to hide tasks
  ◼ Run 5 parallel code review agents
  ◻ Score and filter issues
  ◻ Post review comment on PR
  ✔ Check PR #702 eligibility
  ✔ Find relevant CLAUDE.md files
  ✔ Get PR #702 summary

─────────────────────────────────────────────
❯
─────────────────────────────────────────────
  ⏵⏵ bypass permissions on (shift+tab to cycle) · ctrl+t to hide tasks
"#;

        assert!(
            has_usage_limit_pattern(pane),
            "usage limit with Claude Code UI chrome after /extra-usage should be detected"
        );
    }

    // -----------------------------------------------------------------------
    // UI chrome detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn ui_chrome_detects_task_list_items() {
        assert!(is_ui_chrome("◼ Run 5 parallel code review agents"));
        assert!(is_ui_chrome("◻ Score and filter issues"));
        assert!(is_ui_chrome("✔ Check PR #702 eligibility"));
        assert!(is_ui_chrome("  ◼ Run 5 parallel code review agents"));
    }

    #[test]
    fn ui_chrome_detects_cogitation_and_status() {
        assert!(is_ui_chrome("✻ Worked for 1m 49s"));
        assert!(is_ui_chrome(
            "✻ Running parallel code reviews… (2m 4s · ↓ 4.1k tokens)"
        ));
        assert!(is_ui_chrome(
            "⏵⏵ bypass permissions on (shift+tab to cycle) · ctrl+t to hide tasks"
        ));
    }

    #[test]
    fn ui_chrome_detects_ctrl_key_hints() {
        assert!(is_ui_chrome(
            "6 tasks (3 done, 1 in progress, 2 open) · ctrl+t to hide tasks"
        ));
        assert!(is_ui_chrome("ctrl+b ctrl+b (twice) to run in background"));
    }

    #[test]
    fn ui_chrome_does_not_match_real_content() {
        assert!(!is_ui_chrome("Reading file src/main.rs"));
        assert!(!is_ui_chrome("OK I'll continue working."));
        assert!(!is_ui_chrome("Let me read the file."));
        assert!(!is_ui_chrome("Now implementing the fix."));
    }

    // -----------------------------------------------------------------------
    // API error detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn api_error_detects_500_error() {
        let api_error_pane = r#"
I'll read the file now.
⏺ Read(file_path: "/src/main.rs")

API Error: 500 {"type":"error","error":{"type":"api_error","message":"Internal server error"},"request_id":"req_123"}
"#;

        assert!(
            has_api_error_pattern(api_error_pane),
            "should detect API Error: 500 pattern"
        );
    }

    #[test]
    fn api_error_detects_overloaded_error() {
        let overloaded_pane = r#"
Working on the task.

API Error: 529 {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"},"request_id":"req_456"}
"#;

        assert!(
            has_api_error_pattern(overloaded_pane),
            "should detect overloaded_error pattern"
        );
    }

    #[test]
    fn api_error_detects_internal_server_error_message() {
        let internal_error_pane = "Something went wrong. Internal server error. Please try again.";

        assert!(
            has_api_error_pattern(internal_error_pane),
            "should detect 'Internal server error' message"
        );
    }

    #[test]
    fn api_error_detection_is_case_insensitive() {
        assert!(
            has_api_error_pattern("API ERROR: 500"),
            "should detect uppercase 'API ERROR'"
        );
        assert!(
            has_api_error_pattern("api error: 500"),
            "should detect lowercase 'api error'"
        );
        assert!(
            has_api_error_pattern("INTERNAL SERVER ERROR"),
            "should detect uppercase 'INTERNAL SERVER ERROR'"
        );
        assert!(
            has_api_error_pattern("internal server error"),
            "should detect lowercase 'internal server error'"
        );
    }

    #[test]
    fn api_error_code_content_should_not_trigger_detection() {
        let code_content = r#"
// Handle API errors gracefully
// API Error: 500 is a server error
fn handle_api_error(status: u16) {
    match status {
        500 => log!("Internal server error"),
        _ => log!("Unknown error"),
    }
}

// Now implement the actual handler
fn process_request() {
    let result = make_api_call();
    handle_response(result);
    validate_output();
    send_notification();
    cleanup_resources();
}
"#;

        assert!(
            !has_api_error_pattern(code_content),
            "code with API error strings followed by activity should NOT trigger detection"
        );
    }

    #[test]
    fn api_error_recovers_after_real_activity() {
        let recovered_pane = r#"
API Error: 500 {"type":"error","error":{"type":"api_error","message":"Internal server error"}}

Retrying the request...
⏺ Read(file_path: "/src/main.rs")
Got the file contents.
Now editing.
⏺ Edit(file_path: "/src/main.rs")
Done with the edit.
"#;

        assert!(
            !has_api_error_pattern(recovered_pane),
            "real activity after API error should indicate recovery"
        );
    }

    #[test]
    fn api_error_still_stuck_with_only_ui_chrome() {
        let stuck_with_chrome = r#"
API Error: 502 {"type":"error","error":{"type":"api_error","message":"Bad gateway"}}

───────────────────────────
❯
"#;

        assert!(
            has_api_error_pattern(stuck_with_chrome),
            "UI chrome after API error should not count as recovery"
        );
    }
}
