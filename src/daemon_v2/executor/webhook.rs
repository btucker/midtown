use crate::daemon_v2::events::DomainEvent;
use crate::webhook::WebhookEvent;

/// Convert a webhook event into zero or more domain events.
pub fn webhook_to_events(event: &WebhookEvent) -> Vec<DomainEvent> {
    let mut events = Vec::new();

    // PR merged
    if let Some(pr_num) = event.merged_pr {
        events.push(DomainEvent::PrMerged {
            number: pr_num,
            // PrMergedInfo carries the title but not the branch; branch is not
            // available from the webhook payload without an extra API call, so
            // we leave it empty here.  The polling path (diff_pr_state) fills
            // this field when it has the full PR object.
            branch: String::new(),
        });
    }

    // PR needs review (opened or ready_for_review)
    if let Some(pr_num) = event.needs_review {
        events.push(DomainEvent::PrReviewRequested { number: pr_num });
    }

    // PR opened
    if let Some(ref opened) = event.pr_opened {
        events.push(DomainEvent::PrOpened {
            number: opened.pr_number,
            branch: opened.branch.clone(),
            github_author: opened.github_author.clone(),
        });
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::webhook::{PrMergedInfo, PrOpenedInfo, WebhookEvent};

    fn noop_event() -> WebhookEvent {
        WebhookEvent::github("test")
    }

    /// Spec 12: WHEN a webhook has no recognized events THEN an empty event list
    /// SHALL be produced
    #[test]
    fn empty_webhook_produces_no_events() {
        let we = noop_event();
        assert!(webhook_to_events(&we).is_empty());
    }

    /// Spec 12: WHEN a GitHub webhook reports a PR merged THEN PrMerged event
    /// SHALL be produced
    #[test]
    fn merged_pr_webhook_produces_pr_merged_event() {
        let we = WebhookEvent {
            merged_pr: Some(42),
            pr_merged_info: Some(PrMergedInfo {
                pr_number: 42,
                title: "feat: something".to_string(),
            }),
            ..noop_event()
        };
        let events = webhook_to_events(&we);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            DomainEvent::PrMerged { number: 42, .. }
        ));
    }

    /// Spec 12: WHEN a GitHub webhook reports a PR needs review THEN
    /// PrReviewRequested event SHALL be produced
    #[test]
    fn needs_review_webhook_produces_pr_review_requested_event() {
        let we = WebhookEvent {
            needs_review: Some(7),
            ..noop_event()
        };
        let events = webhook_to_events(&we);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            DomainEvent::PrReviewRequested { number: 7 }
        ));
    }

    /// Spec 12: WHEN a GitHub webhook reports a PR opened THEN PrOpened event
    /// SHALL be produced
    #[test]
    fn pr_opened_webhook_produces_pr_opened_event() {
        let we = WebhookEvent {
            pr_opened: Some(PrOpenedInfo {
                pr_number: 99,
                branch: "lexington/my-feature".to_string(),
                author_coworker: Some("lexington".to_string()),
                title: "My feature".to_string(),
                github_author: "lex-gh".to_string(),
            }),
            ..noop_event()
        };
        let events = webhook_to_events(&we);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            DomainEvent::PrOpened { number: 99, .. }
        ));
        if let DomainEvent::PrOpened {
            branch,
            github_author,
            ..
        } = &events[0]
        {
            assert_eq!(branch, "lexington/my-feature");
            assert_eq!(github_author, "lex-gh");
        }
    }

    #[test]
    fn multiple_fields_produce_multiple_events() {
        let we = WebhookEvent {
            merged_pr: Some(10),
            needs_review: Some(11),
            ..noop_event()
        };
        let events = webhook_to_events(&we);
        assert_eq!(events.len(), 2);
    }

    /// Spec 12: WHEN pr_opened has github_author THEN it SHALL be used
    #[test]
    fn pr_opened_uses_github_author() {
        let we = WebhookEvent {
            pr_opened: Some(PrOpenedInfo {
                pr_number: 5,
                branch: "feature/x".to_string(),
                author_coworker: None,
                title: "X".to_string(),
                github_author: "octocat".to_string(),
            }),
            ..noop_event()
        };
        let events = webhook_to_events(&we);
        if let DomainEvent::PrOpened { github_author, .. } = &events[0] {
            assert_eq!(github_author, "octocat");
        } else {
            panic!("expected PrOpened");
        }
    }
}
