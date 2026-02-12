//! Tests for content-based channel filtering.
//!
//! Verifies that messages with certain content patterns (e.g., @user mentions,
//! task lifecycle events, errors) are routed to the main channel even when
//! the sender has a task channel assignment.

#[cfg(test)]
mod tests {
    use midtown::daemon::helpers::{contains_at_user, should_route_to_main_channel};

    #[test]
    fn test_at_user_mention_routes_to_main() {
        assert!(should_route_to_main_channel("@user can you help?"));
        assert!(should_route_to_main_channel("hey @user, looking at this"));
        assert!(should_route_to_main_channel("@user I found a bug"));
    }

    #[test]
    fn test_at_user_case_insensitive() {
        assert!(should_route_to_main_channel("@USER check this"));
        assert!(should_route_to_main_channel("@UsEr something weird"));
    }

    #[test]
    fn test_task_lifecycle_events_route_to_main() {
        // Task request (already uses @lead which routes to main)
        assert!(should_route_to_main_channel(
            "@lead [Task Request] from park: \"Add validation\""
        ));

        // Task completion messages
        assert!(should_route_to_main_channel("task !42 completed"));
        assert!(should_route_to_main_channel("completed task !123"));
        assert!(should_route_to_main_channel("Task !999 is completed"));
    }

    #[test]
    fn test_error_messages_route_to_main() {
        assert!(should_route_to_main_channel("Error: connection failed"));
        assert!(should_route_to_main_channel("⚠️ Warning: API rate limit"));
        assert!(should_route_to_main_channel("❌ Tests failed"));
        assert!(should_route_to_main_channel("Failed to build"));
    }

    #[test]
    fn test_escalation_keywords_route_to_main() {
        assert!(should_route_to_main_channel("blocked on dependency"));
        assert!(should_route_to_main_channel(
            "I'm blocked waiting for approval"
        ));
        assert!(should_route_to_main_channel("help needed with this issue"));
        assert!(should_route_to_main_channel("Need help understanding this"));
    }

    #[test]
    fn test_regular_messages_do_not_route_to_main() {
        // Regular /me actions
        assert!(!should_route_to_main_channel("working on auth module"));
        assert!(!should_route_to_main_channel("refactoring the validator"));
        assert!(!should_route_to_main_channel("running tests"));

        // Regular text
        assert!(!should_route_to_main_channel("Added validation logic"));
        assert!(!should_route_to_main_channel("Fixed the bug"));
    }

    #[test]
    fn test_word_boundaries_for_user_mention() {
        // Should match
        assert!(contains_at_user("@user test"));
        assert!(contains_at_user("Hey @user!"));
        assert!(contains_at_user("@user."));

        // Should NOT match (part of larger word)
        assert!(!contains_at_user("unusual@user.com"));
        assert!(!contains_at_user("@username"));
        assert!(!contains_at_user("@users"));
    }

    #[test]
    fn test_task_created_routes_to_main() {
        // System messages about task creation should go to main
        assert!(should_route_to_main_channel(
            "📋 Created task !42: Add auth"
        ));
        assert!(should_route_to_main_channel("task !123 created"));
    }

    #[test]
    fn test_false_positives_do_not_trigger() {
        // "error" in different contexts should not trigger
        assert!(!should_route_to_main_channel("error handling is tricky"));
        assert!(!should_route_to_main_channel("the error rate decreased"));

        // "blocked" in different contexts
        assert!(!should_route_to_main_channel(
            "the request was blocked by CORS"
        ));
        assert!(!should_route_to_main_channel("blocked requests counter"));

        // "task" without lifecycle indicators
        assert!(!should_route_to_main_channel("task looks straightforward"));
        assert!(!should_route_to_main_channel("working on the task"));
    }
}
