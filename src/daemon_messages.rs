//! Randomized daemon message templates for spawn and shutdown events.
//!
//! Instead of always posting the same message when calling in or sending
//! coworkers on break, the daemon picks from a pool of variants. The
//! personality setting gates the behaviour:
//!
//! - **Normal**: always uses the first (canonical) template.
//! - **Fun / Wild**: picks uniformly at random from all templates.

use crate::config::Personality;

/// Pick a random template from `pool` respecting the personality setting.
/// Normal personality always returns index 0 (the canonical message).
fn pick<'a>(pool: &'a [&'a str], personality: Personality) -> &'a str {
    match personality {
        Personality::Normal => pool[0],
        Personality::Fun | Personality::Wild => {
            let idx = fastrand::usize(..pool.len());
            pool[idx]
        }
    }
}

// ---------------------------------------------------------------------------
// Spawn / call-in messages
// ---------------------------------------------------------------------------

/// Called in {name} to address {issue_type} on PR #{pr_number}.
pub fn called_in_pr_issue(
    name: &str,
    issue_type: &str,
    pr_number: u64,
    personality: Personality,
) -> String {
    let templates: &[&str] = &[
        "🚀 Called in {name} to address {issue} on PR #{pr}",
        "📞 Paging {name} — {issue} on PR #{pr} needs attention",
        "🏃 {name} is heading over to handle {issue} on PR #{pr}",
        "👋 {name} just walked in for {issue} on PR #{pr}",
        "🎯 Tapped {name} to sort out {issue} on PR #{pr}",
    ];
    pick(templates, personality)
        .replace("{name}", name)
        .replace("{issue}", issue_type)
        .replace("{pr}", &pr_number.to_string())
}

/// Called in {name} to address review feedback on PR #{pr_number}.
pub fn called_in_review_feedback(name: &str, pr_number: u64, personality: Personality) -> String {
    let templates: &[&str] = &[
        "🚀 Called in {name} to address review feedback on PR #{pr}",
        "📞 Paging {name} — review feedback landed on PR #{pr}",
        "🏃 {name} is heading back to tackle review notes on PR #{pr}",
        "👋 {name} just walked in for review feedback on PR #{pr}",
        "🎯 Tapped {name} to address the review on PR #{pr}",
    ];
    pick(templates, personality)
        .replace("{name}", name)
        .replace("{pr}", &pr_number.to_string())
}

/// Called in {name} to review PR #{pr_number}.
pub fn called_in_reviewer(name: &str, pr_number: u64, personality: Personality) -> String {
    let templates: &[&str] = &[
        "🔍 Called in {name} to review PR #{pr}",
        "🔍 Paging {name} — PR #{pr} is ready for review",
        "🔍 {name} is on the way to review PR #{pr}",
        "🔍 {name} just walked in to review PR #{pr}",
        "🔍 Tapped {name} for a review of PR #{pr}",
    ];
    pick(templates, personality)
        .replace("{name}", name)
        .replace("{pr}", &pr_number.to_string())
}

/// Called in coworker {name} for pending task #{task_id}.
pub fn called_in_pending_task(name: &str, task_id: &str, personality: Personality) -> String {
    let templates: &[&str] = &[
        "🚀 Called in coworker {name} for pending task #{task}",
        "📞 Paging {name} — task #{task} is waiting",
        "🏃 {name} is on the way for task #{task}",
        "👋 {name} just walked in for task #{task}",
        "🎯 Tapped {name} for task #{task}",
    ];
    pick(templates, personality)
        .replace("{name}", name)
        .replace("{task}", task_id)
}

/// Called in coworker {name} for assigned task #{task_id}: {subject}.
pub fn called_in_assigned_task(
    name: &str,
    task_id: &str,
    subject: &str,
    personality: Personality,
) -> String {
    let templates: &[&str] = &[
        "🚀 Called in coworker {name} for assigned task #{task}: {subject}",
        "📞 Paging {name} — task #{task}: {subject}",
        "🏃 {name} is on the way for task #{task}: {subject}",
        "👋 {name} just walked in for task #{task}: {subject}",
        "🎯 Tapped {name} for task #{task}: {subject}",
    ];
    pick(templates, personality)
        .replace("{name}", name)
        .replace("{task}", task_id)
        .replace("{subject}", subject)
}

// ---------------------------------------------------------------------------
// Idle / waiting messages (used by coworker hooks)
// ---------------------------------------------------------------------------

/// Personality-flavored idle message for a coworker waiting for input.
/// The message must contain the keyword `waiting` for daemon status parsing.
/// Returns only the action content — the coworker name is set via `Message::action()`.
pub fn idle_waiting(personality: Personality) -> String {
    let templates: &[&str] = &[
        "waiting for input",
        "waiting for input — all is calm",
        "waiting for the next task to drift in",
        "waiting patiently, the river keeps flowing",
        "waiting for input — idle and at peace",
    ];
    pick(templates, personality).to_string()
}

// ---------------------------------------------------------------------------
// Break / shutdown messages
// ---------------------------------------------------------------------------

/// Letting {name} take a break (review complete for PR #{pr}).
pub fn break_review_complete(name: &str, pr_number: u64, personality: Personality) -> String {
    let templates: &[&str] = &[
        "☕ Letting {name} take a break (review complete for PR #{pr})",
        "🔍 {name} wrapped up their review of PR #{pr} — taking a break",
        "✅ {name} finished the review on PR #{pr}, heading out",
        "📝 Review done on PR #{pr} — {name} is free",
        "✌️ {name} is clocking out after reviewing PR #{pr}",
    ];
    pick(templates, personality)
        .replace("{name}", name)
        .replace("{pr}", &pr_number.to_string())
}

/// Letting {name} take a break (no PR assignment found).
pub fn break_no_pr(name: &str, personality: Personality) -> String {
    let templates: &[&str] = &[
        "☕ Letting {name} take a break (no PR assignment found)",
        "🌴 {name} is stepping out — no PR assignment on file",
        "💤 {name} is taking five — nothing assigned",
        "🚶 {name} is heading out — no PR in sight",
        "✌️ {name} is clocking out for now",
    ];
    pick(templates, personality).replace("{name}", name)
}

/// Letting {name} take a break (work merged).
pub fn break_work_merged(name: &str, personality: Personality) -> String {
    let templates: &[&str] = &[
        "☕ Letting {name} take a break (work's all merged)",
        "🎉 {name}'s PR landed — heading out on a high note",
        "✅ {name}'s work is merged, taking a well-earned break",
        "🚀 {name} shipped it! Stepping out now",
        "🏁 {name} crossed the finish line — PR merged, signing off",
    ];
    pick(templates, personality).replace("{name}", name)
}

/// Letting {name} take a break (PR CI passed — session saved for resume).
pub fn break_pr_ci_passed(name: &str, personality: Personality) -> String {
    let templates: &[&str] = &[
        "☕ Letting {name} take a break (CI is green, will resume if needed)",
        "✅ {name}'s CI passed — saving session and stepping out",
        "🟢 {name}'s checks are green, parking session for later",
        "💾 {name}'s PR looks good — saving context for when review lands",
        "🌿 {name} is taking a break (CI passed, session saved)",
    ];
    pick(templates, personality).replace("{name}", name)
}

/// Letting {name} take a break (generic idle).
pub fn break_idle(name: &str, personality: Personality) -> String {
    let templates: &[&str] = &[
        "☕ Letting {name} take a break",
        "🌴 {name} is stepping out for a bit",
        "💤 {name} is taking five",
        "🚶 {name} is heading out — nothing on the board",
        "✌️ {name} is clocking out for now",
    ];
    pick(templates, personality).replace("{name}", name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_personality_always_returns_canonical() {
        // Run many times to confirm determinism
        for _ in 0..50 {
            assert_eq!(
                break_idle("bob", Personality::Normal),
                "☕ Letting bob take a break"
            );
            assert_eq!(
                break_review_complete("carol", 42, Personality::Normal),
                "☕ Letting carol take a break (review complete for PR #42)"
            );
            assert_eq!(
                break_work_merged("alice", Personality::Normal),
                "☕ Letting alice take a break (work's all merged)"
            );
            assert_eq!(
                called_in_reviewer("dave", 99, Personality::Normal),
                "🔍 Called in dave to review PR #99"
            );
            assert_eq!(
                called_in_assigned_task("eve", "5", "Fix bug", Personality::Normal),
                "🚀 Called in coworker eve for assigned task #5: Fix bug"
            );
            assert_eq!(idle_waiting(Personality::Normal), "waiting for input");
        }
    }

    #[test]
    fn fun_personality_produces_valid_messages() {
        // Just verify every function returns a non-empty string containing the name
        let name = "eve";
        let msg = called_in_pr_issue(name, "CI failure", 10, Personality::Fun);
        assert!(msg.contains(name) && msg.contains("10"), "{msg}");

        let msg = called_in_review_feedback(name, 20, Personality::Fun);
        assert!(msg.contains(name) && msg.contains("20"), "{msg}");

        let msg = called_in_reviewer(name, 30, Personality::Fun);
        assert!(msg.contains(name) && msg.contains("30"), "{msg}");

        let msg = called_in_pending_task(name, "5", Personality::Fun);
        assert!(msg.contains(name) && msg.contains("5"), "{msg}");

        let msg = called_in_assigned_task(name, "6", "Fix bug", Personality::Fun);
        assert!(
            msg.contains(name) && msg.contains("6") && msg.contains("Fix bug"),
            "{msg}"
        );

        let msg = break_review_complete(name, 40, Personality::Fun);
        assert!(msg.contains(name) && msg.contains("40"), "{msg}");

        let msg = break_work_merged(name, Personality::Fun);
        assert!(msg.contains(name), "{msg}");

        let msg = break_no_pr(name, Personality::Fun);
        assert!(msg.contains(name), "{msg}");

        let msg = break_idle(name, Personality::Fun);
        assert!(msg.contains(name), "{msg}");

        let msg = idle_waiting(Personality::Fun);
        assert!(
            msg.contains("waiting"),
            "idle message must contain 'waiting' keyword: {msg}"
        );
    }

    #[test]
    fn wild_personality_produces_valid_messages() {
        let name = "frank";
        for _ in 0..20 {
            let msg = called_in_pending_task(name, "7", Personality::Wild);
            assert!(msg.contains(name) && msg.contains("7"), "{msg}");
            let msg = break_idle(name, Personality::Wild);
            assert!(msg.contains(name), "{msg}");
            let msg = idle_waiting(Personality::Wild);
            assert!(
                msg.contains("waiting"),
                "idle message must contain 'waiting' keyword: {msg}"
            );
        }
    }

    #[test]
    fn idle_waiting_always_contains_waiting_keyword() {
        // All personality variants must include "waiting" for daemon status parsing
        for personality in &[Personality::Normal, Personality::Fun, Personality::Wild] {
            for _ in 0..50 {
                let msg = idle_waiting(*personality);
                assert!(
                    msg.contains("waiting"),
                    "idle_waiting with {:?} must contain 'waiting': {}",
                    personality,
                    msg
                );
            }
        }
    }
}
