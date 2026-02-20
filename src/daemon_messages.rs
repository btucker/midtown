//! Daemon message templates for spawn and shutdown events.
//!
//! Each function returns a canonical message string for the given event.

// ---------------------------------------------------------------------------
// Spawn / call-in messages
// ---------------------------------------------------------------------------

/// Called in {name} to address {issue_type} on PR #{pr_number}.
pub fn called_in_pr_issue(name: &str, issue_type: &str, pr_number: u64) -> String {
    format!(
        "\u{1f680} Called in {} to address {} on PR #{}",
        name, issue_type, pr_number
    )
}

/// Called in {name} to address review feedback on PR #{pr_number}.
pub fn called_in_review_feedback(name: &str, pr_number: u64) -> String {
    format!(
        "\u{1f680} Called in {} to address review feedback on PR #{}",
        name, pr_number
    )
}

/// Called in {name} to review PR #{pr_number}.
pub fn called_in_reviewer(name: &str, pr_number: u64) -> String {
    format!(
        "\u{1f50d} Called in {} to review PR #{}",
        name, pr_number
    )
}

/// Called in coworker {name} for pending task !{task_id}.
pub fn called_in_pending_task(name: &str, task_id: &str) -> String {
    format!(
        "\u{1f680} Called in coworker {} for pending task !{}",
        name, task_id
    )
}

/// Called in coworker {name} for assigned task !{task_id}: {subject}.
pub fn called_in_assigned_task(name: &str, task_id: &str, subject: &str) -> String {
    format!(
        "\u{1f680} Called in coworker {} for assigned task !{}: {}",
        name, task_id, subject
    )
}

// ---------------------------------------------------------------------------
// Idle / waiting messages (used by coworker hooks)
// ---------------------------------------------------------------------------

/// Idle message for a coworker waiting for input.
/// The message must contain the keyword `waiting` for daemon status parsing.
/// Returns only the action content — the coworker name is set via `Message::action()`.
pub fn idle_waiting() -> String {
    "waiting for input".to_string()
}

// ---------------------------------------------------------------------------
// Break / shutdown messages
// ---------------------------------------------------------------------------

/// Letting {name} take a break (review complete for PR #{pr}).
pub fn break_review_complete(name: &str, pr_number: u64) -> String {
    format!(
        "\u{2615} Letting {} take a break (review complete for PR #{})",
        name, pr_number
    )
}

/// Letting {name} take a break (no PR assignment found).
pub fn break_no_pr(name: &str) -> String {
    format!(
        "\u{2615} Letting {} take a break (no PR assignment found)",
        name
    )
}

/// Letting {name} take a break (work merged).
pub fn break_work_merged(name: &str) -> String {
    format!(
        "\u{2615} Letting {} take a break (work's all merged)",
        name
    )
}

/// Letting {name} take a break (PR CI passed — session saved for resume).
pub fn break_pr_ci_passed(name: &str) -> String {
    format!(
        "\u{2615} Letting {} take a break (CI is green, will resume if needed)",
        name
    )
}

/// Letting {name} take a break (generic idle).
pub fn break_idle(name: &str) -> String {
    format!("\u{2615} Letting {} take a break", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_canonical_messages() {
        assert_eq!(
            break_idle("bob"),
            "\u{2615} Letting bob take a break"
        );
        assert_eq!(
            break_review_complete("carol", 42),
            "\u{2615} Letting carol take a break (review complete for PR #42)"
        );
        assert_eq!(
            break_work_merged("alice"),
            "\u{2615} Letting alice take a break (work's all merged)"
        );
        assert_eq!(
            called_in_reviewer("dave", 99),
            "\u{1f50d} Called in dave to review PR #99"
        );
        assert_eq!(
            called_in_assigned_task("eve", "5", "Fix bug"),
            "\u{1f680} Called in coworker eve for assigned task !5: Fix bug"
        );
        assert_eq!(idle_waiting(), "waiting for input");
    }

    #[test]
    fn messages_contain_expected_names_and_numbers() {
        let name = "eve";
        let msg = called_in_pr_issue(name, "CI failure", 10);
        assert!(msg.contains(name) && msg.contains("10"), "{msg}");

        let msg = called_in_review_feedback(name, 20);
        assert!(msg.contains(name) && msg.contains("20"), "{msg}");

        let msg = called_in_reviewer(name, 30);
        assert!(msg.contains(name) && msg.contains("30"), "{msg}");

        let msg = called_in_pending_task(name, "5");
        assert!(msg.contains(name) && msg.contains("5"), "{msg}");

        let msg = called_in_assigned_task(name, "6", "Fix bug");
        assert!(
            msg.contains(name) && msg.contains("6") && msg.contains("Fix bug"),
            "{msg}"
        );

        let msg = break_review_complete(name, 40);
        assert!(msg.contains(name) && msg.contains("40"), "{msg}");

        let msg = break_work_merged(name);
        assert!(msg.contains(name), "{msg}");

        let msg = break_no_pr(name);
        assert!(msg.contains(name), "{msg}");

        let msg = break_idle(name);
        assert!(msg.contains(name), "{msg}");

        let msg = idle_waiting();
        assert!(
            msg.contains("waiting"),
            "idle message must contain 'waiting' keyword: {msg}"
        );
    }
}
